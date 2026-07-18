#![warn(clippy::pedantic)]
#![allow(clippy::unnecessary_debug_formatting, clippy::missing_errors_doc)]
use std::path::{Path, PathBuf};

use anvil_recompress_engine::{CompressionAlgorithm, CompressionLevel, RecompressFileOptions};
use anyhow::Context;
use clap::Parser;
use walkdir::WalkDir;

/// Recompress minecraft region files.
#[derive(Parser, Debug)]
#[command(version)]
#[allow(clippy::struct_excessive_bools, reason = "these options are named")]
struct Cli {
    /// Recursively process directories.
    #[arg(short = 'r', long)]
    recurse: bool,
    #[arg(long, required = true)]
    compression: CompressionAlgorithm,
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
    /// Override existing destination options.
    #[arg(long, requires = "dest")]
    override_existing_dest: bool,
    /// Suppress the normal printing of progress.
    #[arg(long, short)]
    quiet: bool,
}
impl Cli {
    fn recompression_opts(&self) -> RecompressFileOptions {
        let mut opts = RecompressFileOptions::new(self.compression);
        opts.compression_level = self
            .compression_level
            .map(CompressionLevel::Standard)
            .unwrap_or_default();
        opts.override_existing = (self.output.inplace && self.override_backups)
            || (self.output.dest.is_some() && self.override_existing_dest);
        opts
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
    if !cli.output.inplace
        && let Some(parent) = output_file.parent()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent {parent:?} for {output_file:?}"))?;
    }
    // no context needs to be added as the error already includes that
    anvil_recompress_engine::recompress_region_file(&input_file, &output_file, &cli.recompression_opts())?;
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
    if cli.compression == CompressionAlgorithm::Lz4 {
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
