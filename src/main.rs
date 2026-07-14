#![warn(clippy::pedantic)]
#![allow(clippy::unnecessary_debug_formatting)]
use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use walkdir::WalkDir;

#[derive(Copy, Clone, Debug, Eq, PartialEq, clap::ValueEnum)]
enum CompressionChoice {
    Zlib,
    Gzip,
    None,
    Lz4,
}
impl CompressionChoice {
    fn fastanvil_scheme(self) -> fastanvil::CompressionScheme {
        use fastanvil::CompressionScheme;
        match self {
            CompressionChoice::Zlib => CompressionScheme::Zlib,
            CompressionChoice::Gzip => CompressionScheme::Gzip,
            CompressionChoice::None => CompressionScheme::Uncompressed,
            CompressionChoice::Lz4 => CompressionScheme::Lz4,
        }
    }
}

/// Recompress minecraft region files.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// Recursively process directories.
    #[arg(short = 'r', long)]
    recurse: bool,
    #[arg(long, required = true)]
    compression: CompressionChoice,
    #[arg(long = "level")]
    compression_level: Option<u32>,
    /// The file or directory to recompress.
    ///
    /// If using --recurse, this can only have one entry.
    #[arg(num_args(1..))]
    targets: Vec<PathBuf>,
    #[command(flatten)]
    output: OutputSpec,
    /// Ignore and override existing `.bak` files.
    #[arg(long, requires = "inplace")]
    override_backups: bool,
    /// Suppress the normal printing of progress.
    #[arg(long, short)]
    quiet: bool,
}
impl Cli {
    fn compress<'a>(&self, input: &'a [u8]) -> anyhow::Result<Cow<'a, [u8]>> {
        let mut buffer = Vec::new();
        match self.compression {
            CompressionChoice::Zlib => {
                let level = self.compression_level.map(flate2::Compression::new).unwrap_or_default();
                let mut encoder = flate2::write::ZlibEncoder::new(&mut buffer, level);
                encoder.write_all(input).context("zlib compression error")?;
            }
            CompressionChoice::Gzip => {
                let level = self.compression_level.map(flate2::Compression::new).unwrap_or_default();
                let mut encoder = flate2::write::GzEncoder::new(&mut buffer, level);
                encoder.write_all(input).context("gzip compression error")?;
            }
            CompressionChoice::None => return Ok(Cow::Borrowed(input)),
            CompressionChoice::Lz4 => {
                let level = self
                    .compression_level
                    .map(|x| i32::try_from(x).expect("level overflow"))
                    .map(lz4::block::CompressionMode::HIGHCOMPRESSION)
                    .unwrap_or_default();
                lz4::block::compress_to_buffer(input, Some(level), false, &mut buffer)
                    .context("lz4 compression error")?;
            }
        }
        Ok(Cow::Owned(buffer))
    }
}

#[derive(clap::Args, Debug)]
#[group(required = true, multiple = false)]
struct OutputSpec {
    /// Replace the original files with the output files.
    ///
    /// Renames the original file as a `.bak` file,
    /// erroring if that file already exists.
    #[arg(long)]
    inplace: bool,
    /// Place the output in this directory.
    ///
    /// Requires all targets to use relative paths or to be directories.
    #[arg(long)]
    dest: Option<PathBuf>,
}

const REGION_FILE_EXTENSION: &str = ".mca";

fn process_entry(root: &Path, relative_target: &Path, cli: &Cli) -> anyhow::Result<()> {
    let (input_file, output_file) = if cli.output.inplace {
        let target = root.join(relative_target);
        let backup_file = target.with_added_extension(".bak");
        assert_ne!(backup_file, target);
        // making fs::rename  atomic and catching the FileExists error is actually very difficult,
        // so we just check the old fashioned way and hope we don't race
        if backup_file.exists() {
            if cli.override_backups {
                // warn and continue, as fs::rename will override the old backup
                eprintln!("WARN: Overriding backup file {backup_file:?}");
            } else {
                anyhow::bail!("Cannot process {target:?} inplace as a backup file already exists")
            }
        }
        std::fs::rename(&target, &backup_file).context("Failed to create backup")?;
        (backup_file, target)
    } else {
        let dest = cli.output.dest.clone().expect("neither --inplace nor --dest");
        anyhow::ensure!(
            relative_target.is_relative(),
            "A relative path is required (got {relative_target:?})"
        );
        (root.join(relative_target), dest.join(relative_target))
    };
    anyhow::ensure!(
        !same_file::is_same_file(&input_file, &output_file).unwrap_or(false),
        "Internal Error: Cannot process file {input_file:?} as output {output_file:?} is actually the same file"
    );
    let mut input_region = std::fs::File::open(&input_file)
        .map_err(anyhow::Error::new)
        .and_then(|file| Ok(fastanvil::Region::from_stream(file)?))
        .with_context(|| format!("Failed to open region file {input_file:?}"))?;
    if !cli.output.inplace
        && let Some(parent) = output_file.parent()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent {parent:?} for {output_file:?}"))?;
    }
    // Using File::create is write-only.
    // This doesn't work due to the need to seek
    let mut output_region = std::fs::File::options()
        .create(true)
        .write(true)
        .read(true)
        .truncate(true)
        .open(&output_file)
        .map_err(anyhow::Error::new)
        .and_then(|file| Ok(fastanvil::Region::create(file)?))
        .with_context(|| format!("Failed to create region file {output_file:?}"))?;
    // We are careful to use deterministic iter order here,
    // although this currently matches what fastanvil does by default
    for z in 0..32 {
        for x in 0..32 {
            let chunk = input_region
                .read_chunk(x, z)
                .with_context(|| format!("Failed to read chunk ({x}, {z}) from {input_file:?}"))?;
            let Some(chunk) = chunk else { continue };
            let compressed_data = cli
                .compress(&chunk)
                .with_context(|| format!("Compression failure for chunk ({x}, {z})"))?;
            output_region
                .write_compressed_chunk(x, z, cli.compression.fastanvil_scheme(), &compressed_data)
                .with_context(|| format!("Failed to write chunk ({x}, {z}) to {output_file:?}"))?;
        }
    }
    if !cli.quiet {
        // finished
        println!("{}", relative_target.display());
    }
    Ok(())
}

fn process_entries_recursive(root: &Path, cli: &Cli) -> anyhow::Result<()> {
    // NOTE: This implicitly handles the non-recursive case by simply yielding the root
    let walk = WalkDir::new(root).sort_by_file_name();
    for entry in walk {
        let entry = entry?;
        // only consider allow files that have the proper extension
        if !entry.file_name().to_string_lossy().ends_with(REGION_FILE_EXTENSION) {
            continue;
        }
        // only consider actual files (comes after name check to maybe avoid stat call)
        if !entry.file_type().is_file() {
            continue;
        }
        let relative_path = entry
            .path()
            .strip_prefix(root)
            .with_context(|| format!("Failed to strip prefix of {entry:?} while searching {root:?}"))?;
        process_entry(root, relative_path, cli)
            .with_context(|| format!("Failed to process {relative_path:?} while searching {root:?}"))?;
    }
    Ok(())
}

fn process_entries_standard(cli: &Cli) -> anyhow::Result<()> {
    // do some basic validation of args ahead of time
    for entry in &cli.targets {
        if !cli.output.inplace {
            anyhow::ensure!(
                entry.is_relative(),
                "When using --dest option, only relative paths are allowed: {entry:?}"
            );
        }
        anyhow::ensure!(entry.is_file(), "Target must be an existing file: {entry:?}");
    }
    for entry in &cli.targets {
        process_entry(&PathBuf::new(), entry, cli).with_context(|| format!("Failed to process {entry:?}"))?;
    }
    Ok(())
}

pub fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.compression != CompressionChoice::Lz4 {
        eprintln!("WARN: Compression choice 'lz4' has not been tested.");
    }
    if cli.recurse {
        anyhow::ensure!(
            cli.targets.len() == 1,
            "When recursing, cannot have more than one target"
        );
        process_entries_recursive(&cli.targets[0], &cli)
    } else {
        process_entries_standard(&cli)
    }
}
