# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed
- Split library crate `anvil-recompress-engine` from the CLI crate `anvil-recompress`.
  - Library crate is re-exported from the CLI crate, so backwards compatibility is preserved.

## 0.1.1 - 2026-07-15

### Added
- Offer functionality through library in addition to command line.
  - The CLI functionality requires the on-by-default "cli" feature.

### Changed
- Make support for lz4 compression optional (on by default).
- Set LTO for release builds by default

## 0.1.0 - 2026-07-14
Initial release.

