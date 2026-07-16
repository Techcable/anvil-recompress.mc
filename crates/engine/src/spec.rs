//! Experimental API for reusing open file handles.
use std::fs::File;
use std::path::{Path, PathBuf};

pub(crate) mod internal {
    use std::fs::File;
    use std::io::{Read, Seek, Write};
    use std::path::Path;

    use crate::RecompressFileOptions;
    use crate::errors::{FileRecompressErrorKind, OpenOutputError};
    use crate::spec::FileSpec;

    pub struct FileSpecOpenError(pub(crate) FileRecompressErrorKind);
    pub trait FileSpecInternal {
        type Opened: Read + Write + Seek;
        fn open_input_file(self, opts: &RecompressFileOptions) -> Result<Self::Opened, FileSpecOpenError>;
        fn open_output_file(self, opts: &RecompressFileOptions) -> Result<Self::Opened, FileSpecOpenError>;
    }
    impl<'a> FileSpecInternal for super::OpenedFile<'a> {
        type Opened = &'a mut File;
        fn open_input_file(self, _opts: &RecompressFileOptions) -> Result<&'a mut File, FileSpecOpenError> {
            Ok(self.file)
        }
        fn open_output_file(self, _opts: &RecompressFileOptions) -> Result<&'a mut File, FileSpecOpenError> {
            Ok(self.file)
        }
    }
    impl<P: AsRef<Path>> FileSpecInternal for P {
        type Opened = File;
        fn open_input_file(self, _opts: &RecompressFileOptions) -> Result<File, FileSpecOpenError> {
            File::open(self.path())
                .map_err(|cause| FileRecompressErrorKind::OpenInputFile { cause })
                .map_err(FileSpecOpenError)
        }
        fn open_output_file(self, opts: &RecompressFileOptions) -> Result<File, FileSpecOpenError> {
            let create_new = !opts.override_existing;
            File::options()
                .create(true)
                .write(true)
                .read(true)
                .truncate(!create_new)
                .create_new(create_new)
                .open(self.path())
                .map_err(|cause| FileRecompressErrorKind::OpenOutput(OpenOutputError { create_new, cause }))
                .map_err(FileSpecOpenError)
        }
    }
}

/// A file that has already been opened.
///
/// Errors may be unclear if the file is not appropriately writable/readable.
/// Note that the "output" file must be readable as well as writable.
///
/// *WARNING*: This API is experimental.
#[derive(Debug)]
pub struct OpenedFile<'a> {
    /// A handle to the opened file.
    pub file: &'a mut File,
    /// The path of the opened file.
    pub path: PathBuf,
}

/// A file that can be passed to [`crate::recompress_region_file`].
pub trait FileSpec: internal::FileSpecInternal {
    /// The path that this file refers to.
    fn path(&self) -> &Path;
}
impl<P: AsRef<Path>> FileSpec for P {
    fn path(&self) -> &Path {
        self.as_ref()
    }
}
impl FileSpec for OpenedFile<'_> {
    fn path(&self) -> &Path {
        &self.path
    }
}
