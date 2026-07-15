//! Options for compression.
use std::fmt;
use std::fmt::{Display, Formatter};

use crate::options::OptionsValidateErrorReason;

/// A supported compression algorithm for minecraft region files.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[non_exhaustive]
pub enum CompressionAlgorithm {
    /// Use [zlib](https://en.wikipedia.org/wiki/Zlib) compression.
    ///
    /// This is the default behavior for minecraft.
    #[default]
    Zlib,
    /// Use [gzip](https://en.wikipedia.org/wiki/Gzip#File_format) for compression.
    Gzip,
    /// Do not use any compression algorithm.
    None,
    /// Use the [lz4](https://lz4.org/) compression format.
    Lz4,
}
impl CompressionAlgorithm {
    /// The name of this compression algorithm.
    pub fn name(&self) -> &'static str {
        match self {
            CompressionAlgorithm::Zlib => "zlib",
            CompressionAlgorithm::Gzip => "gzip",
            CompressionAlgorithm::None => "none",
            CompressionAlgorithm::Lz4 => "lz4",
        }
    }
    pub(crate) fn fastanvil_scheme(self) -> fastanvil::CompressionScheme {
        use fastanvil::CompressionScheme;
        match self {
            CompressionAlgorithm::Zlib => CompressionScheme::Zlib,
            CompressionAlgorithm::Gzip => CompressionScheme::Gzip,
            CompressionAlgorithm::None => CompressionScheme::Uncompressed,
            CompressionAlgorithm::Lz4 => CompressionScheme::Lz4,
        }
    }
}
impl Display for CompressionAlgorithm {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Controls the speed and quality of compression.
///
/// Using a level that an algorithm doesn't support
/// will trigger an [`OptionsValidateErrorReason::InappropriateCompressionLevel`].
///
/// The [`Display`] implementation will print the level in the
/// style of a command line option (ex. `--best` `--fast=3`, `-9`)
/// The exception is the "default" level which prints as "default"
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum CompressionLevel {
    /// Use the default compression level for the algorithm.
    ///
    /// This is the only compression level that is valid for [`CompressionAlgorithm::None`].
    #[default]
    Default,
    /// Use the best standard compression level for the algorithm.
    ///
    /// This excludes the extra high levels than zstd/lz4 offers.
    Best,
    /// Use the fastest standard compression level for the algorithm.
    ///
    /// Should not include extra fast levels ([`Self::ExtraFast`]),
    /// so the zstd and lz4 `--fast` options should not be used.
    /// This is somewhat confusing as the [`Display`] implementation prints this as `--fast`.
    Fast,
    /// Use a specific integer compression level.
    Standard(u32),
    /// Use alternate faster compression with a specific level.
    ///
    /// Currently only applicable to lz4 compression.
    ExtraFast(u32),
}
impl CompressionLevel {
    pub(crate) fn is_applicable_for(&self, algorithm: CompressionAlgorithm) -> bool {
        match (self, algorithm) {
            // a "default" level is always valid
            (CompressionLevel::Default, _) => true,
            // the "none" algorithm cannot accept non-default compression levels
            (_, CompressionAlgorithm::None) => false,
            // all non-none algorithms accept "best", "fast", and "explicit"
            (CompressionLevel::Best | CompressionLevel::Fast | CompressionLevel::Standard(_), _) => true,
            // only lz4 accepts extra fast levels
            (CompressionLevel::ExtraFast(_), _) => matches!(algorithm, CompressionAlgorithm::Lz4),
        }
    }
    pub(crate) fn int_value(&self) -> Option<u32> {
        match *self {
            CompressionLevel::Default | CompressionLevel::Best | CompressionLevel::Fast => None,
            CompressionLevel::Standard(value) | CompressionLevel::ExtraFast(value) => Some(value),
        }
    }
    pub(crate) fn validate(&self, algorithm: CompressionAlgorithm) -> Result<(), OptionsValidateErrorReason> {
        if !self.is_applicable_for(algorithm) {
            return Err(OptionsValidateErrorReason::InappropriateCompressionLevel {
                algorithm,
                level: *self,
            });
        }
        if let Some(int_value) = self.int_value() {
            // ensure the integer values always fit in an i32
            let _ = i32::try_from(int_value).map_err(|_| OptionsValidateErrorReason::OverflowCompressionLevel {
                target_type: "i32",
                level: *self,
            })?;
        }
        Ok(())
    }
}
impl Display for CompressionLevel {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            CompressionLevel::Default => f.write_str("default"),
            CompressionLevel::ExtraFast(x) => write!(f, "--fast={x}"),
            CompressionLevel::Standard(x) => write!(f, "-{x}"),
            CompressionLevel::Best => f.write_str("--best"),
            CompressionLevel::Fast => f.write_str("--fast"),
        }
    }
}
