//! Defines option types to control recompression.

use std::borrow::Cow;
use std::io::Write;

mod compress;

pub use self::compress::{CompressionAlgorithm, CompressionLevel};

/// Options to control [`crate::recompress_region_file`].
///
/// Specifying a [`CompressionAlgorithm`] is mandatory and must be passed to [`RecompressFileOptions::new`].
/// All other options have sensible defaults.
///
/// # Errors
/// If an invalid config is attempted to be used, a [`OptionsValidateError`] could be returned.
///
/// This is possible if compression options are not appropriate for a specific algorithm.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RecompressFileOptions {
    /// The algorithm to use.
    pub compression_algorithm: CompressionAlgorithm,
    /// The level of compression to use.
    ///
    /// If this is not valid for a certain algorithm,
    /// an error will be returned when it is used.
    pub compression_level: CompressionLevel,
    /// If true, this will override existing output files.
    pub override_existing: bool,
}
impl RecompressFileOptions {
    /// Create the default set of options for the specified algorithm.
    pub fn new(algorithm: CompressionAlgorithm) -> Self {
        RecompressFileOptions {
            compression_algorithm: algorithm,
            compression_level: CompressionLevel::default(),
            override_existing: false,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), OptionsValidateError> {
        self.compression_level
            .validate(self.compression_algorithm)
            .map_err(|reason| OptionsValidateError { reason })?;
        Ok(())
    }
    pub(crate) fn zlib_level(&self) -> flate2::Compression {
        use flate2::Compression;
        match self.compression_level {
            CompressionLevel::Standard(value) => Compression::new(value),
            CompressionLevel::Best => Compression::best(),
            CompressionLevel::Default => Compression::default(),
            CompressionLevel::Fast => Compression::fast(),
            level @ CompressionLevel::ExtraFast(_) => unreachable!("{level:?} should have failed validation"),
        }
    }
    pub(crate) fn lz4_level(&self) -> lz4::block::CompressionMode {
        #[track_caller]
        fn cast_i32(x: u32) -> i32 {
            i32::try_from(x).expect("overflowing integer should not have passed validation")
        }
        use lz4::block::CompressionMode;
        match self.compression_level {
            CompressionLevel::Standard(value) => CompressionMode::HIGHCOMPRESSION(cast_i32(value)),
            CompressionLevel::Best => CompressionMode::HIGHCOMPRESSION(12),
            CompressionLevel::Fast => CompressionMode::HIGHCOMPRESSION(0),
            CompressionLevel::ExtraFast(fast) => CompressionMode::FAST(cast_i32(fast)),
            CompressionLevel::Default => CompressionMode::DEFAULT,
        }
    }
    pub(crate) fn compress<'a>(&self, input: &'a [u8]) -> anyhow::Result<Cow<'a, [u8]>> {
        let mut buffer = Vec::new();
        match self.compression_algorithm {
            CompressionAlgorithm::Zlib => {
                let mut encoder = flate2::write::ZlibEncoder::new(&mut buffer, self.zlib_level());
                encoder.write_all(input)?;
            }
            CompressionAlgorithm::Gzip => {
                let mut encoder = flate2::write::GzEncoder::new(&mut buffer, self.zlib_level());
                encoder.write_all(input)?;
            }
            CompressionAlgorithm::None => return Ok(Cow::Borrowed(input)),
            CompressionAlgorithm::Lz4 => {
                lz4::block::compress_to_buffer(input, Some(self.lz4_level()), false, &mut buffer)?;
            }
        }
        Ok(Cow::Owned(buffer))
    }
}

/// Indicates that an instance of [`RecompressFileOptions`] is not valid.
#[derive(thiserror::Error, Debug)]
#[error("Specified invalid options")]
#[non_exhaustive]
pub struct OptionsValidateError {
    /// The reason the errors are invalid.
    #[source]
    pub reason: OptionsValidateErrorReason,
}

/// Indicates the underlying reason for an [`OptionsValidateError`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OptionsValidateErrorReason {
    /// Indicates a [`CompressionLevel`] is specified that is not appropriate for a certain algorithm.
    #[error("Compression level {level} is not appropriate for algorithm {algorithm}")]
    #[non_exhaustive]
    InappropriateCompressionLevel {
        /// The compression level that was attempted to be used.
        level: CompressionLevel,
        /// The algorithm for which the level was used with.
        algorithm: CompressionAlgorithm,
    },
    /// Indicates that a [`CompressionLevel`] overflowed an integer.
    #[error("Compression level {level} overflowed a {target_type}")]
    #[non_exhaustive]
    OverflowCompressionLevel {
        /// The compression level that overflowed.
        level: CompressionLevel,
        /// The type of integer that the level overflowed.
        target_type: &'static str,
    },
}
