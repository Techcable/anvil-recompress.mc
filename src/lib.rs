//! Library interface to `anvil-recompress` functionality.
#![warn(clippy::pedantic, missing_docs)]
#![allow(
    clippy::must_use_candidate,
    clippy::trivially_copy_pass_by_ref,
    reason = "excessively pedantic",
)]

use std::fmt::{self, Debug, Display, Formatter};
use std::io::{Read, Seek, Write};
use std::ops::Range;
use std::path::Path;

use crate::errors::{FileRecompressErrorKind, OpenOutputError};

pub mod errors;
pub mod options;

pub use self::errors::FileRecompressError;
pub use self::options::{CompressionAlgorithm, CompressionLevel, RecompressFileOptions};

/// Recompress a region file.
///
/// # Errors
/// All error messages include the input and output file.
///
/// The input and output file must be distinct,
/// or an error will occur.
///
/// Unless [`RecompressFileOptions::override_existing`] is specified,
/// an existing output file will trigger an error.
pub fn recompress_region_file(
    input_file: &Path,
    output_file: &Path,
    opts: &RecompressFileOptions,
) -> Result<(), FileRecompressError> {
    let create_error = |kind| FileRecompressError {
        kind,
        input_file: input_file.into(),
        output_file: output_file.into(),
    };
    opts.validate()
        .map_err(FileRecompressErrorKind::InvalidOptions)
        .map_err(&create_error)?;
    if same_file::is_same_file(input_file, output_file).unwrap_or(false) {
        return Err(create_error(FileRecompressErrorKind::SameFile));
    }
    let mut input_region = std::fs::File::open(input_file)
        .map_err(|cause| FileRecompressErrorKind::OpenInputFile { cause })
        .and_then(|file| {
            fastanvil::Region::from_stream(file).map_err(|cause| FileRecompressErrorKind::ReadInput { cause })
        })
        .map_err(&create_error)?;
    let create_new = !opts.override_existing;
    // Using File::create does not work as fastanvil needs to be able to seek
    // and File::create is write-only.
    let mut output_region = std::fs::File::options()
        .create(true)
        .write(true)
        .read(true)
        .truncate(!create_new)
        .create_new(create_new)
        .open(output_file)
        .map_err(|cause| FileRecompressErrorKind::OpenOutput(OpenOutputError { create_new, cause }))
        .and_then(|file| {
            fastanvil::Region::create(file).map_err(|cause| FileRecompressErrorKind::WriteOutput { cause })
        })
        .map_err(&create_error)?;
    recompress_regions(&mut input_region, &mut output_region, opts).map_err(&create_error)?;
    Ok(())
}

fn recompress_regions<I: Read + Seek, O: Write + Seek + Read>(
    input_region: &mut fastanvil::Region<I>,
    output_region: &mut fastanvil::Region<O>,
    opts: &RecompressFileOptions,
) -> Result<(), errors::FileRecompressErrorKind> {
    // We are careful to use deterministic iter order here,
    // although this currently matches what fastanvil does by default
    for chunk in RelativeChunkPos::all() {
        let chunk_data = input_region
            .read_chunk(chunk.x(), chunk.z())
            .map_err(|cause| FileRecompressErrorKind::ReadChunk { chunk, cause })?;
        // skip missing chunks
        let Some(chunk_data) = chunk_data else { continue };
        let compressed_data = opts
            .compress(&chunk_data)
            .map_err(|cause| FileRecompressErrorKind::CompressChunk {
                chunk,
                cause,
                algorithm: opts.compression_algorithm,
            })?;
        output_region
            .write_compressed_chunk(
                chunk.x(),
                chunk.z(),
                opts.compression_algorithm.fastanvil_scheme(),
                &compressed_data,
            )
            .map_err(|cause| FileRecompressErrorKind::WriteChunk { chunk, cause })?;
    }
    Ok(())
}

/// The location of a chunk within a region file
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RelativeChunkPos(u8, u8);
impl RelativeChunkPos {
    /// Create a new [`RelativeChunkPos`] from a pair of coordinates.
    ///
    /// # Panics
    /// If the chunk coords would exceed the maximum bounds of a region file.
    #[track_caller]
    #[inline]
    pub fn new(x: u32, z: u32) -> Self {
        assert!(x < 32 && z < 32, "Invalid chunk coords overflow region ({x}, {z})");
        #[expect(clippy::cast_possible_truncation, reason = "just checked above")]
        RelativeChunkPos(x as u8, z as u8)
    }

    /// The x coordinate of the chunk relative to the containing region.
    #[inline]
    pub fn x(&self) -> usize {
        self.0 as usize
    }

    /// The z coordinate of the chunk relative to the containing region.
    #[inline]
    pub fn z(&self) -> usize {
        self.1 as usize
    }

    /// Iterate over all valid [`RelativeChunkPos`] in a deterministic fashion.
    #[inline]
    pub(crate) fn all() -> impl Iterator<Item = Self> + 'static {
        #[inline]
        fn indices() -> Range<u8> {
            0..32
        }
        // iterating over z first reflects how the region files are stored
        indices()
            .clone()
            .flat_map(|z| indices().clone().map(move |x| RelativeChunkPos(x, z)))
    }
}
impl Display for RelativeChunkPos {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "({x}, {z})", x = self.0, z = self.1)
    }
}
