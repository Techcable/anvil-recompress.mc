#![warn(clippy::pedantic)]
#![allow(clippy::unnecessary_debug_formatting, clippy::missing_errors_doc)]
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anvil_recompress_engine::{CompressionAlgorithm, CompressionLevel, RecompressFileOptions};
use anvil_recompress_incremental::cache::{CacheGcOpts, IncrementalCache};
use anyhow::{Context, anyhow, ensure};
use camino::Utf8PathBuf;
use clap::Parser;
use clap::builder::PossibleValue;
use relative_path::RelativePath;
use slog::{Drain, FilterLevel, Logger, warn};
use slog_term::{CompactFormat, TermDecorator};
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
    /// Enable the experimental incremental mode.
    #[arg(long)]
    incremental: bool,
    #[arg(long, default_value = "info")]
    log_level: LevelSpec,
    /// When using incremental compression, run GC afterwards.
    ///
    /// Has no effect unless `--incremental` is specified.
    #[arg(long)]
    gc: bool,
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
#[derive(Copy, Clone, Debug)]
struct LevelSpec(FilterLevel);
impl Default for LevelSpec {
    fn default() -> Self {
        LevelSpec(FilterLevel::Info)
    }
}
impl LevelSpec {
    const ALL: &[LevelSpec] = &[
        LevelSpec(FilterLevel::Off),
        LevelSpec(FilterLevel::Critical),
        LevelSpec(FilterLevel::Error),
        LevelSpec(FilterLevel::Warning),
        LevelSpec(FilterLevel::Info),
        LevelSpec(FilterLevel::Debug),
        LevelSpec(FilterLevel::Trace),
    ];
}
impl clap::ValueEnum for LevelSpec {
    fn value_variants<'a>() -> &'a [Self] {
        Self::ALL
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(self.0.as_str().to_lowercase()).alias(self.0.as_short_str().to_lowercase()))
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

struct ProcessContext<'a> {
    logger: Logger,
    root: Utf8PathBuf,
    incremental_cache: Option<Box<IncrementalCache>>,
    cli: &'a Cli,
}

fn process_entry(ctx: &ProcessContext, raw_target: &Path) -> anyhow::Result<()> {
    let cli = ctx.cli;
    let (input_file, output_file, relative_target) = if cli.output.inplace {
        let target = ctx.root.as_std_path().join(raw_target);
        let backup_file = target.with_added_extension(".bak");
        assert_ne!(backup_file, target);
        // making fs::rename  atomic and catching the FileExists error is actually very difficult,
        // so we just check the old fashioned way and hope we don't race
        if backup_file.exists() {
            if cli.override_backups {
                // warn and continue, as fs::rename will override the old backup
                warn!(
                    ctx.logger,
                    "Overriding backup file";
                    "backup_file" => backup_file.display(),
                );
            } else {
                anyhow::bail!("Cannot process {target:?} inplace as a backup file already exists")
            }
        }
        std::fs::rename(&target, &backup_file).context("Failed to create backup")?;
        (backup_file, target, None)
    } else {
        let relative_target = RelativePath::from_path(raw_target)
            .with_context(|| format!("When using --dest, a relative path is required (got {raw_target:?})"))?;
        let dest = Utf8PathBuf::try_from(cli.output.dest.clone().expect("neither --inplace nor --dest"))
            .context("Destination must be a UTF8 path")?;
        (
            relative_target.to_path(&ctx.root),
            relative_target.to_path(&dest),
            Some(relative_target),
        )
    };
    if !cli.output.inplace
        && let Some(parent) = output_file.parent()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent {parent:?} for {output_file:?}"))?;
    }
    if let Some(ref incremental) = ctx.incremental_cache {
        ensure!(
            !cli.output.inplace,
            "Inplace processing is not valid for --incremental mode"
        );
        let relative_target = relative_target.expect("Already checked for --inplace");
        incremental
            .lock_out_file(relative_target)
            .with_context(|| format!("Failed to lock output file {output_file:?}"))?
            .recompress_region_file(&input_file, &cli.recompression_opts())
            .with_context(|| format!("Incremental recompression of {input_file:?} failed"))?;
    } else {
        // no context needs to be added as the error already includes that
        anvil_recompress_engine::recompress_region_file(&input_file, &output_file, &cli.recompression_opts())?;
    }
    if !cli.quiet {
        // finished
        println!("{}", raw_target.display());
    }
    Ok(())
}

fn init_logger(cli: &Cli) -> Logger {
    let log_level = cli.log_level;
    let mut dec = TermDecorator::new();
    if std::env::var_os("FORCE_COLOR").is_some_and(|x| !x.is_empty()) {
        dec = dec.force_color();
    }
    let drain = CompactFormat::new(dec.build())
        .build()
        .filter(move |record| log_level.0.accepts(record.level()))
        .ignore_res();
    Logger::root(Mutex::new(drain).fuse(), slog::o!())
}

fn process_entries_recursive(logger: &Logger, root: &Path, cli: &Cli) -> anyhow::Result<()> {
    let ctx = ProcessContext {
        logger: logger.clone(),
        incremental_cache: if cli.incremental {
            let dest = cli
                .output
                .dest
                .clone()
                .ok_or_else(|| anyhow!("Must use --dest with --incremental"))?;
            Some(Box::new(
                IncrementalCache::open(logger, &dest).context("Failed to initialize incremental cache")?,
            ))
        } else {
            None
        },
        root: Utf8PathBuf::try_from(root.to_owned()).context("Root is not a UTF8 path")?,
        cli,
    };
    let mut entries_to_keep = Vec::new();
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
        process_entry(&ctx, relative_path)
            .with_context(|| format!("Failed to process {relative_path:?} while searching {root:?}"))?;
        entries_to_keep.push(relative_path.to_path_buf());
    }
    if let Some(ref incremental) = ctx.incremental_cache
        && cli.gc
    {
        let mut opts = CacheGcOpts::default();
        opts.remove_all_outputs_except = Some(entries_to_keep);
        incremental
            .garbage_collect(&opts)
            .context("Failed to run garbage collection")?;
    }
    Ok(())
}

fn process_entries_standard(logger: &Logger, cli: &Cli) -> anyhow::Result<()> {
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
    ensure!(
        !cli.incremental,
        "Incremental processing can not be used without --recursive"
    );
    let ctx = ProcessContext {
        logger: logger.clone(),
        incremental_cache: None,
        cli,
        root: Utf8PathBuf::new(),
    };
    for entry in &cli.targets {
        process_entry(&ctx, entry).with_context(|| format!("Failed to process {entry:?}"))?;
    }
    Ok(())
}

pub fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let logger = init_logger(&cli);
    if cli.compression == CompressionAlgorithm::Lz4 {
        warn!(logger, "Compression choice 'lz4' has not been tested.");
    }
    if cli.incremental {
        warn!(logger, "The --incremental option is highly experimental. Make backups!");
    }
    if cli.recurse {
        anyhow::ensure!(
            cli.targets.len() == 1,
            "When recursing, cannot have more than one target"
        );
        process_entries_recursive(&logger, &cli.targets[0], &cli)
    } else {
        process_entries_standard(&logger, &cli)
    }
}
