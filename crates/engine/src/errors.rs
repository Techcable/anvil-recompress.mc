//! Defines the errors that can occur.
use std::fmt;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

pub use crate::options::OptionsValidateError;
use crate::{CompressionAlgorithm, RelativeChunkPos};

/// Failure in [`crate::recompress_region_file`].
#[derive(Debug, thiserror::Error)]
#[error("Failed to recompress region file {input_file:?} into {output_file:?}")]
pub struct FileRecompressError {
    pub(crate) input_file: PathBuf,
    pub(crate) output_file: PathBuf,
    #[source]
    pub(crate) kind: FileRecompressErrorKind,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum FileRecompressErrorKind {
    #[error(transparent)]
    InvalidOptions(OptionsValidateError),
    #[error("Failed to open input file")]
    #[non_exhaustive]
    OpenInputFile {
        #[source]
        cause: std::io::Error,
    },
    #[error("Failed to read input region")]
    #[non_exhaustive]
    ReadInput {
        #[source]
        cause: fastanvil::Error,
    },
    #[error("Failed to read chunk {chunk}")]
    #[non_exhaustive]
    ReadChunk {
        chunk: RelativeChunkPos,
        #[source]
        cause: fastanvil::Error,
    },
    #[error("Failed to compress chunk {chunk} with {algorithm}")]
    #[non_exhaustive]
    CompressChunk {
        chunk: RelativeChunkPos,
        algorithm: CompressionAlgorithm,
        #[source]
        cause: anyhow::Error,
    },
    #[error("Failed to write chunk {chunk}")]
    WriteChunk {
        chunk: RelativeChunkPos,
        #[source]
        cause: fastanvil::Error,
    },
    #[error("Failed to write output region")]
    #[non_exhaustive]
    WriteOutput {
        #[source]
        cause: fastanvil::Error,
    },
    #[error(transparent)]
    OpenOutput(OpenOutputError),
    #[error("Input and output files are actually the same file")]
    #[non_exhaustive]
    SameFile,
}

#[derive(Debug, thiserror::Error)]
pub(crate) struct OpenOutputError {
    pub create_new: bool,
    #[source]
    pub cause: std::io::Error,
}
impl Display for OpenOutputError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str("Failed to ")?;
        f.write_str(if self.create_new { "create new" } else { "open" })?;
        f.write_str(" output file")?;
        if self.create_new {
            f.write_str(" (is there already an existing file?)")?;
        }
        Ok(())
    }
}
