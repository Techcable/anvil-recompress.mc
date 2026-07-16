# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- *EXPERIMENTAL*: Add incremental processing/caching functionality.
  Avoids recompressing files when inputs haven't changed.
  Uses a combination of ctime and blake3 hashing to detect changes.
- Add `--override-existing-dest` option to CLI, allowing override without needing `--inplace`.
- *EXPERIMENTAL*: Allow passing already opened file handles to `recompress_region_file`.
  This API is not intended for public use yet, as it has some footguns.

### Changed
- Split library crate `anvil-recompress-engine` from the CLI crate `anvil-recompress`.
  - Library crate is re-exported from the CLI crate, so backwards compatibility is preserved.
- Use `slog_term` for logging

## 0.1.1 - 2026-07-15

### Added
- Offer functionality through library in addition to command line.
  - The CLI functionality requires the on-by-default "cli" feature.

### Changed
- Make support for lz4 compression optional (on by default).
- Set LTO for release builds by default

## 0.1.0 - 2026-07-14
Initial release.

