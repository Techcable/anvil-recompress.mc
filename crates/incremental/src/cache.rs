use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anvil_recompress_engine::RecompressFileOptions;
use anvil_recompress_engine::spec::OpenedFile;
use camino::{Utf8Path, Utf8PathBuf};
use indoc::indoc;
use relative_path::{RelativePath, RelativePathBuf};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, ToSql};
use serde::Serializer;
use slog::{Logger, debug, info, trace};
use time::UtcDateTime;

/// The version of the database.
///
/// v1 - Initial format
const CACHE_DATABASE_VERSION: u32 = 1;
const OUTPUT_FILE_TABLE: &str = "cached_output_files";
const INPUT_FILE_TABLE: &str = "cached_input_files";
const KNOWN_TABLES: &[&str] = &[INPUT_FILE_TABLE, OUTPUT_FILE_TABLE];
const CACHE_SUBDIR: &str = ".cache/anvil-recompress";

pub struct IncrementalCache {
    logger: Logger,
    output_dir: PathBuf,
    /// Lockfile for the cache.
    ///
    /// Locked with [`File::lock_shared`] as the per-file locks
    /// should be sufficient to prevent misbehavior.
    _lockfile: File,
    database: Mutex<Connection>,
}
impl IncrementalCache {
    /// Open the cache for the specified output directory.
    ///
    /// May block if other processes are using the cache.
    pub fn open(logger: &Logger, location: &Path) -> Result<Self, CacheOpenError> {
        let logger = logger.new(slog::o!(
            "output_dir" => location.display().to_string(),
        ));
        let cache_subdir = location.join(CACHE_SUBDIR);
        std::fs::create_dir_all(&cache_subdir).map_err(|cause| CacheOpenErrorReason::CreateCacheDir {
            cache_subdir: cache_subdir.clone(),
            cause,
        })?;
        let tag_file = location.join("CACHEDIR.TAG");
        if !tag_file.is_file() {
            std::fs::write(&tag_file, CACHEDIR_TAG_CONTENTS).map_err(|cause| CacheOpenErrorReason::CreateCacheTag {
                cause,
                cache_subdir: cache_subdir.clone(),
            })?;
        }
        let lockfile_path = cache_subdir.join("cache.lock");
        debug!(
            logger,
            "Acquiring cache lock";
            "lockfile_path" => lockfile_path.display(),
        );
        let lockfile = File::create(&lockfile_path).map_err(|cause| CacheOpenErrorReason::LockfileCreate {
            cause,
            lockfile_path: lockfile_path.clone(),
        })?;
        lockfile
            .lock_shared()
            .map_err(|cause| CacheOpenErrorReason::LockfileLock {
                lockfile_path: lockfile_path.clone(),
                cause,
            })?;
        let database_file = cache_subdir.join("cache.sqlite");
        let database = Connection::open(&database_file).map_err(|cause| CacheOpenErrorReason::DatabaseOpen {
            database_file: database_file.clone(),
            cause,
        })?;
        let db_setup_error = |cause| CacheOpenErrorReason::DatabaseSetupError {
            cause,
            database_file: database_file.clone(),
        };
        let actual_user_version: u32 = database
            .query_one("PRAGMA user_version;", (), |row| row.get(0))
            .map_err(&db_setup_error)?;
        database
            .execute_batch(indoc::indoc!(
                r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            -- wait up to 1s if the db is locked (better than immediate error)
            PRAGMA busy_timeout = 1000;
        "#
            ))
            .map_err(&db_setup_error)?;
        match actual_user_version.cmp(&CACHE_DATABASE_VERSION) {
            Ordering::Greater => {
                return Err(CacheOpenErrorReason::UnsupportedDatabaseVersion {
                    actual_version: actual_user_version,
                    expected_version: CACHE_DATABASE_VERSION,
                    database_file,
                }
                .into());
            }
            Ordering::Equal => {} // do nothing
            Ordering::Less => {
                debug!(
                    logger,
                    "Initializing cache database";
                    "old_version" => actual_user_version
                );
                for table in KNOWN_TABLES {
                    // delete existing tables
                    database
                        .execute(&format!("DROP TABLE IF EXISTS {table}"), ())
                        .map_err(&db_setup_error)?;
                }
                database
                    .execute_batch(indoc::indoc! {r#"
                    CREATE TABLE cached_input_files (
                        path TEXT PRIMARY KEY,
                        hash TEXT NOT NULL,
                        modified_time BLOB NOT NULL,
                        last_used_time TEXT
                    );
                    CREATE INDEX inputs_by_hash ON cached_input_files(hash);
                    CREATE TABLE cached_output_files (
                        relative_path TEXT PRIMARY KEY,
                        hash TEXT NOT NULL,
                        modified_time BLOB NOT NULL,
                        compression_options BLOB,
                        input_hash TEXT NOT NULL
                    );
                    -- this is needed to allow efficient gc
                    CREATE INDEX outputs_by_input_hash ON cached_output_files(input_hash);
                "#})
                    .map_err(&db_setup_error)?;
                trace!(logger, "Setting db version"; "new_version" => CACHE_DATABASE_VERSION);
                // Apparently sqlite can't handle params in pragma statements
                database
                    .execute(&format!("PRAGMA user_version = {CACHE_DATABASE_VERSION};"), ())
                    .map_err(&db_setup_error)?;
            }
        }
        Ok(IncrementalCache {
            logger,
            database: Mutex::new(database),
            _lockfile: lockfile,
            output_dir: location.to_owned(),
        })
    }

    /// Lock the output file with the specified relative path.
    pub fn lock_out_file(&self, output_path: &RelativePath) -> Result<LockedOutputFile<'_>, FileLockError> {
        let output_path = output_path.to_owned();
        let create_error = |cause| FileLockError {
            cause,
            entry: output_path.clone(),
            output_dir: self.output_dir.clone(),
        };
        let resolved_output_path: Utf8PathBuf = output_path
            .to_path(&self.output_dir)
            .try_into()
            .map_err(FileLockErrorReason::OutputInvalidUtf8)
            .map_err(&create_error)?;
        let output_file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&resolved_output_path)
            .map_err(|cause| {
                create_error(FileLockErrorReason::OpenFile {
                    cause,
                    path: resolved_output_path.to_path_buf(),
                })
            })?;
        trace!(
            self.logger,
            "Locking output file";
            "output_path" => output_path.as_str(),
        );
        output_file.lock().map_err(|cause| {
            create_error(FileLockErrorReason::AcquireLock {
                cause,
                path: resolved_output_path.clone(),
            })
        })?;
        Ok(LockedOutputFile {
            logger: self.logger.new(slog::o!(
                "output_path" => output_path.as_str().to_owned(),
            )),
            output_path,
            cache: self,
            output_file,
            resolved_output_path,
        })
    }
    fn lock_database(&self) -> MutexGuard<'_, Connection> {
        self.database.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn load_cached_hash(&self, kind: HashFileKind, path: &Path) -> Result<Option<CachedHash>, HashFileError> {
        let database = self.lock_database();
        let create_error = |reason| HashFileError {
            reason,
            kind,
            path: path.to_owned(),
        };
        let path = Utf8PathBuf::try_from(path.to_owned())
            .map_err(HashFileErrorReason::InvalidUTF8)
            .map_err(&create_error)?;
        database
            .query_one(
                &format!(
                    "SELECT * FROM {table_name} WHERE {path_field} = ?",
                    table_name = kind.table_name(),
                    path_field = kind.path_field_name(),
                ),
                rusqlite::params![path.as_str()],
                |row| {
                    let timestamp = row.get("modified_time")?;
                    let hash: Checksum = row.get("hash")?;
                    Ok(CachedHash { timestamp, hash })
                },
            )
            .optional()
            .map_err(HashFileErrorReason::DatabaseLoad)
            .map_err(&create_error)
    }
    /// Hash the input file, reusing a cached hash whenever possible
    fn hash_input_file(&self, path: &Path) -> Result<Checksum, HashFileError> {
        let create_error = |reason| HashFileError {
            reason,
            path: path.to_owned(),
            kind: HashFileKind::Input,
        };
        let cached = self.load_cached_hash(HashFileKind::Input, path)?;
        let path = Utf8PathBuf::try_from(path.to_owned())
            .map_err(HashFileErrorReason::InvalidUTF8)
            .map_err(&create_error)?;
        let current_mtime = FileModificationTime::for_file(&path)
            .map_err(HashFileErrorReason::Timestamp)
            .map_err(&create_error)?;
        let updated_hash = if let Some(ref cached) = cached
            && cached.timestamp == current_mtime
        {
            // no need to update as the timestamp matches
            cached.clone()
        } else {
            trace!(
                self.logger,
                "Recomputing hash for input file";
                "cached" => slog::Serde(cached.clone()),
                "current_mtime" => slog::Serde(current_mtime.clone()),
            );
            CachedHash {
                hash: hash_unconditionally(path.as_std_path())
                    .map_err(HashFileErrorReason::Hash)
                    .map_err(&create_error)?,
                timestamp: current_mtime.clone(),
            }
        };
        let database = self.lock_database();
        database
            .execute(
                indoc! {r#"
                INSERT INTO cached_input_files(path, hash, modified_time, last_used_time)
                VALUES (:path, :hash, :modified_time, datetime('now', 'subsec'))
                ON CONFLICT(path) DO UPDATE SET
                    hash = excluded.hash,
                    modified_time = excluded.modified_time,
                    last_used_time = excluded.last_used_time;
                "#},
                rusqlite::named_params! {
                    ":path": path.as_str(),
                    ":hash": updated_hash.hash,
                    ":modified_time": &current_mtime,
                },
            )
            .map_err(HashFileErrorReason::DatabaseUpdate)
            .map_err(&create_error)?;
        Ok(updated_hash.hash)
    }
    /// Automatically remove old input entries from the cache database.
    pub fn garbage_collect(&self, opts: &CacheGcOpts) -> Result<(), CacheGcError> {
        let database = self.lock_database();
        info!(self.logger, "Garbage collecting the incremental cache database");
        if let Some(ref outputs_to_keep) = opts.remove_all_outputs_except {
            database
                .execute_batch(indoc! {r#"
                CREATE TEMPORARY TABLE output_files_to_keep(
                    relative_path TEXT
                );
            "#})
                .map_err(CacheGcError)?;
            for keep in outputs_to_keep {
                let Some(keep) = Utf8Path::from_path(keep) else {
                    // just skip non-UTF8 paths, as they can never match
                    continue;
                };
                database
                    .execute("INSERT INTO output_files_to_keep VALUES (?);", (keep.as_str(),))
                    .map_err(CacheGcError)?;
            }
            info!(
                self.logger,
                "Garbage collecting unused output files";
                "files_to_keep" => outputs_to_keep.len(),
            );
            database
                .execute_batch(indoc! {r#"
                DELETE FROM cached_output_files
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM output_files_to_keep k
                    WHERE k.relative_path = cached_output_files.relative_path
                );
                DROP TABLE output_files_to_keep;
            "#})
                .map_err(CacheGcError)?;
        }
        // Written mostly by claude
        database
            .execute_batch(indoc! {r#"
            -- delete input files whose hash is not used
            DELETE FROM cached_input_files AS input
            WHERE NOT EXISTS (
                SELECT 1
                FROM cached_output_files AS output
                WHERE output.input_hash = input.hash
            );
            -- for any given input hash, restrict to the 5 most recently used entries
            DELETE FROM cached_input_files
            WHERE path IN (
                SELECT path FROM (
                    SELECT path,
                        ROW_NUMBER() OVER (
                            PARTITION BY hash
                            ORDER BY last_used_time DESC
                        ) AS rn
                    FROM cached_input_files
                )
                WHERE rn > 5
            );
        "#})
            .map_err(CacheGcError)?;
        Ok(())
    }
}
#[derive(Default)]
#[non_exhaustive]
pub struct CacheGcOpts {
    pub remove_all_outputs_except: Option<Vec<PathBuf>>,
}
/// Wraps a [`blake3::Hash`].
///
/// This newtype implements [`FromSql`]
/// and does [`serde::Serialize`] in terms of strings.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
struct Checksum(blake3::Hash);
impl FromSql for Checksum {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse::<blake3::Hash>()
            .map_err(FromSqlError::other)
            .map(Checksum)
    }
}
impl serde::Serialize for Checksum {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl ToSql for Checksum {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.0.to_string().into())
    }
}
#[derive(Clone, serde::Serialize)]
struct CachedHash {
    hash: Checksum,
    timestamp: FileModificationTime,
}
trait HashTarget {
    type Read: Read;
    fn open(self) -> std::io::Result<Self::Read>;
}
impl HashTarget for &mut File {
    type Read = Self;
    fn open(self) -> std::io::Result<Self::Read> {
        Ok(self)
    }
}
impl HashTarget for &Path {
    type Read = File;
    fn open(self) -> std::io::Result<Self::Read> {
        File::open(self)
    }
}
fn hash_unconditionally(target: impl HashTarget) -> std::io::Result<Checksum> {
    let mut hasher = blake3::Hasher::new();
    let file = target.open()?;
    hasher.update_reader(file)?;
    Ok(Checksum(hasher.finalize()))
}

#[derive(thiserror::Error, Debug)]
#[error("Failed to GC cache database")]
pub struct CacheGcError(#[from] rusqlite::Error);

/// An output file locked via [`IncrementalCache::lock_out_file`].
///
/// Locking prevents the anvil-recompress tool from making changes
/// and may or may not affect other tools.
pub struct LockedOutputFile<'a> {
    cache: &'a IncrementalCache,
    logger: Logger,
    output_path: RelativePathBuf,
    resolved_output_path: Utf8PathBuf,
    /// The output file on which we own the lock.
    output_file: File,
}
impl LockedOutputFile<'_> {
    fn rewind_output_file(&mut self) -> Result<(), FileRewindError> {
        self.output_file.rewind().map_err(|cause| FileRewindError {
            cause,
            path: self.resolved_output_path.clone().into_std_path_buf(),
        })
    }
    /// Get the information on the cached file if present.
    ///
    /// Returns `None` if this file has no known input.
    fn load_cached_info(&self) -> Result<Option<CachedOutputFileInfo>, CacheLookupError> {
        let database = self
            .cache
            .database
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        database
            .query_one(
                "SELECT * FROM cached_output_files WHERE relative_path = :path",
                rusqlite::named_params! {
                    ":path": self.output_path.as_str(),
                },
                |row| {
                    Ok(CachedOutputFileInfo {
                        input_hash: row.get("input_hash")?,
                        output_hash: row.get("hash")?,
                        compression_opts: {
                            let mut opts = row.get::<_, serde_json::Value>("compression_options")?;
                            opts.sort_all_objects();
                            opts
                        },
                        output_modified: row.get("modified_time")?,
                    })
                },
            )
            .optional()
            .map_err(|cause| CacheLookupError { cause })
    }
    fn determine_output_hash(
        &mut self,
        cached_info: Option<&CachedOutputFileInfo>,
    ) -> Result<Option<CachedHash>, HashFileError> {
        let resolved_output_path = self.resolved_output_path.clone();
        let create_error = |reason| HashFileError {
            kind: HashFileKind::Output,
            path: resolved_output_path.clone().into_std_path_buf(),
            reason,
        };
        let current_mtime = match FileModificationTime::for_file(&self.resolved_output_path) {
            Ok(time) => time,
            Err(e) if matches!(e.cause.kind(), std::io::ErrorKind::NotFound) => {
                // file isn't found => hash is None
                return Ok(None);
            }
            Err(e) => {
                return Err(create_error(HashFileErrorReason::Timestamp(e)));
            }
        };
        let cached_hash = cached_info.map(|info| CachedHash {
            hash: info.output_hash,
            timestamp: info.output_modified.clone(),
        });
        if let Some(ref cached_hash) = cached_hash
            && cached_hash.timestamp == current_mtime
        {
            Ok(Some(cached_hash.clone()))
        } else {
            trace!(
                self.logger,
                "Recomputing hash for output file";
                "cached" => slog::Serde(cached_hash.clone()),
                "current_mtime" => slog::Serde(current_mtime.clone()),
            );
            self.rewind_output_file()
                .map_err(HashFileErrorReason::Rewind)
                .map_err(&create_error)?;
            Ok(Some(CachedHash {
                hash: hash_unconditionally(&mut self.output_file)
                    .map_err(HashFileErrorReason::Hash)
                    .map_err(&create_error)?,
                timestamp: current_mtime.clone(),
            }))
        }
    }
    pub fn recompress_region_file(
        &mut self,
        input_file: &Path,
        opts: &RecompressFileOptions,
    ) -> Result<(), CachedFileRecompressError> {
        let output_path = self.output_path.clone();
        let create_error = |reason| CachedFileRecompressError {
            reason: Box::new(reason),
            input_file: input_file.to_owned(),
            output_file: output_path.clone(),
        };
        let cached_output = self
            .load_cached_info()
            .map_err(CachedRecompressErrorReason::LoadCachedOutputInfo)
            .map_err(&create_error)?;
        let input_hash = self
            .cache
            .hash_input_file(input_file)
            .map_err(CachedRecompressErrorReason::HashInput)
            .map_err(&create_error)?;
        let existing_output_hash = self
            .determine_output_hash(cached_output.as_ref())
            .map_err(CachedRecompressErrorReason::HashOutput)
            .map_err(&create_error)?;
        let actual_compress_options = {
            let mut val = serde_json::to_value(opts)
                .map_err(CachedRecompressErrorReason::SerializeOptions)
                .map_err(&create_error)?;
            val.sort_all_objects();
            val
        };
        if let Some(ref cached_output) = cached_output
            && Some(cached_output.output_hash) == existing_output_hash.as_ref().map(|cached| cached.hash)
            && actual_compress_options == cached_output.compression_opts
            && cached_output.input_hash == input_hash
        {
            debug!(
                self.logger,
                "Reusing cached output file (hash and options match)";
                "cached_output" => slog::Serde(cached_output.clone()),
                "input_hash" => slog::Serde(input_hash),
            );
            Ok(())
        } else {
            debug!(
                self.logger,
                "Recomputing region file output";
                "cached_output" => slog::Serde(cached_output.clone()),
                "actual_output_hash" => slog::Serde(existing_output_hash.clone().map(|cached| cached.hash)),
                "input_hash" => slog::Serde(input_hash),
                "actual_compress_options" => slog::Serde(actual_compress_options.clone()),
            );
            self.rewind_output_file()
                .map_err(CachedRecompressErrorReason::Rewind)
                .map_err(&create_error)?;
            // Need to truncate here since we open file with truncate(false)
            self.output_file
                .set_len(0)
                .map_err(CachedRecompressErrorReason::TruncateOutput)
                .map_err(&create_error)?;
            anvil_recompress_engine::recompress_region_file(
                input_file,
                OpenedFile {
                    path: self.resolved_output_path.clone().into(),
                    file: &mut self.output_file,
                },
                opts,
            )
            .map_err(CachedRecompressErrorReason::Recompress)
            .map_err(&create_error)?;
            let new_output_hash = self
                .determine_output_hash(None)
                .map_err(CachedRecompressErrorReason::HashOutput)
                .map_err(&create_error)?
                .ok_or_else(|| {
                    create_error(CachedRecompressErrorReason::DeletedOutputFile {
                        path: self.resolved_output_path.clone(),
                    })
                })?;
            {
                let connection = self.cache.lock_database();
                connection
                    .execute(
                        indoc! {r#"
                        INSERT OR REPLACE INTO cached_output_files
                        (relative_path, hash, modified_time, compression_options, input_hash)
                        VALUES (:rel_path, :output_hash, :modified_time, :compress_opts, :input_hash);
                    "#},
                        rusqlite::named_params! {
                            ":rel_path": RelativePath::as_str(&self.output_path),
                            ":output_hash": new_output_hash.hash,
                            ":modified_time": new_output_hash.timestamp,
                            ":compress_opts": actual_compress_options.clone(),
                            ":input_hash": input_hash,
                        },
                    )
                    .map_err(CachedRecompressErrorReason::UpdateCachedOutput)
                    .map_err(&create_error)?;
            }
            Ok(())
        }
    }
}
#[derive(thiserror::Error, Debug)]
#[error("Failed incremental recompress of {input_file:?} into {output_file:?}")]
pub struct CachedFileRecompressError {
    input_file: PathBuf,
    output_file: RelativePathBuf,
    #[source]
    reason: Box<CachedRecompressErrorReason>,
}
#[derive(thiserror::Error, Debug)]
enum CachedRecompressErrorReason {
    #[error(transparent)]
    Recompress(anvil_recompress_engine::FileRecompressError),
    #[error("Failed to load cached output info")]
    LoadCachedOutputInfo(#[source] CacheLookupError),
    #[error("Failed to hash output file")]
    HashOutput(#[source] HashFileError),
    #[error("Failed to hash input file")]
    HashInput(#[source] HashFileError),
    #[error("Failed to serialize options")]
    SerializeOptions(#[source] serde_json::error::Error),
    #[error("Output file removed while running ({path:?})")]
    DeletedOutputFile { path: Utf8PathBuf },
    #[error("Failed to update cached output in database")]
    UpdateCachedOutput(#[source] rusqlite::Error),
    #[error(transparent)]
    Rewind(FileRewindError),
    #[error("Failed to truncate output file")]
    TruncateOutput(#[source] std::io::Error),
}
#[derive(thiserror::Error, Debug)]
#[error("Failed to rewind file handle for {path:?}")]
struct FileRewindError {
    path: PathBuf,
    #[source]
    cause: std::io::Error,
}
#[derive(Debug, Copy, Clone)]
enum HashFileKind {
    Input,
    Output,
}
impl HashFileKind {
    fn name(&self) -> &'static str {
        match self {
            HashFileKind::Input => "input",
            HashFileKind::Output => "output",
        }
    }
    fn table_name(&self) -> &'static str {
        match self {
            HashFileKind::Input => INPUT_FILE_TABLE,
            HashFileKind::Output => OUTPUT_FILE_TABLE,
        }
    }
    fn path_field_name(&self) -> &'static str {
        match self {
            HashFileKind::Input => "path",
            HashFileKind::Output => "relative_path",
        }
    }
}
impl Display for HashFileKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
#[derive(thiserror::Error, Debug)]
#[error("Failed to hash {kind} file {path:?}")]
struct HashFileError {
    path: PathBuf,
    kind: HashFileKind,
    #[source]
    reason: HashFileErrorReason,
}
#[derive(thiserror::Error, Debug)]
enum HashFileErrorReason {
    #[error("Failed to load from database")]
    DatabaseLoad(rusqlite::Error),
    #[error("Failed to update database")]
    DatabaseUpdate(rusqlite::Error),
    #[error(transparent)]
    Timestamp(ModTimeResolveError),
    #[error(transparent)]
    Hash(std::io::Error),
    #[error(transparent)]
    InvalidUTF8(camino::FromPathBufError),
    #[error(transparent)]
    Rewind(FileRewindError),
}

#[derive(thiserror::Error, Debug)]
#[error("Failed to lookup data from cache")]
pub struct CacheLookupError {
    #[source]
    cause: rusqlite::Error,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CachedOutputFileInfo {
    /// The hash of the input file.
    input_hash: Checksum,
    compression_opts: serde_json::Value,
    /// The hash of the output file.
    output_hash: Checksum,
    /// When the output file was last modified.
    ///
    /// Used to avoid hash recalculations wherever possible.
    output_modified: FileModificationTime,
}

/// A timestamp of when a certain file was modified.
///
/// On unix we use ctime instead of mtime as ctime is set by the kernel
/// and not user-modifiable.
/// This reasoning comes from borg backup.
/// False positive changes are fine as we fallback to comparing hashes.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct FileModificationTime(time::Timestamp);
impl FileModificationTime {
    const FORMAT: time::format_description::well_known::Iso8601 =
        { time::format_description::well_known::Iso8601::DATE_TIME_OFFSET };
}
impl FromSql for FileModificationTime {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        use rusqlite::types::Type;
        let tp = value.data_type();
        match tp {
            Type::Blob => {
                // old version
                let nanos = <i128 as FromSql>::column_result(value)?;
                time::Timestamp::from_nanoseconds(nanos)
                    .map_err(FromSqlError::other)
                    .map(FileModificationTime)
            }
            Type::Text => {
                let text = value.as_str()?;
                UtcDateTime::parse(text, &Self::FORMAT)
                    .map_err(FromSqlError::other)
                    .map(|date| date.into())
                    .map(FileModificationTime)
            }
            _ => Err(FromSqlError::InvalidType),
        }
    }
}
impl serde::Serialize for FileModificationTime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let text = self
            .to_timestamp()
            .format(&Self::FORMAT)
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&text)
    }
}
impl Display for FileModificationTime {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.format(&Self::FORMAT).expect("format failure"))
    }
}
impl ToSql for FileModificationTime {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let text = self.to_timestamp().format(&Self::FORMAT).expect("time format error");
        Ok(text.into())
    }
}
impl From<time::Timestamp> for FileModificationTime {
    fn from(value: time::Timestamp) -> Self {
        FileModificationTime(value)
    }
}
impl FileModificationTime {
    /// The timestamp for when the file was modified.
    #[inline]
    pub fn to_timestamp(&self) -> time::Timestamp {
        self.0
    }
    /// Gettthe tim of the specified file,
    /// including both its name and time modified.
    pub fn for_file(path: impl AsRef<Path>) -> Result<Self, ModTimeResolveError> {
        let path = path.as_ref();
        std::fs::metadata(path)
            .and_then(Self::for_meta)
            .map_err(|cause| ModTimeResolveError {
                cause,
                path: path.to_owned(),
            })
    }
    pub fn for_meta(meta: std::fs::Metadata) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(FileModificationTime(Self::ctime_unix(meta)?))
        }
        #[cfg(not(unix))]
        {
            Ok(crate::cache::FileModificationTime(Self::ctime_fallback(meta)?))
        }
    }
    #[cfg(unix)]
    fn ctime_unix(meta: std::fs::Metadata) -> std::io::Result<time::Timestamp> {
        use std::os::unix::fs::MetadataExt;
        let secs = meta.ctime();
        let nsecs = meta.ctime_nsec();
        let create_error =
            |cause: &dyn Display| std::io::Error::other(format!("ctime ({secs}, {nsecs}) is not valid: {cause}"));
        time::Timestamp::new(
            secs,
            u32::try_from(nsecs).map_err(|_| create_error(&"nanoseconds don't fit in u32"))?,
        )
        .map_err(|cause| create_error(&cause))
    }
    #[allow(unused, reason = "Only used on-non unix systems")]
    fn ctime_fallback(meta: std::fs::Metadata) -> std::io::Result<time::Timestamp> {
        Ok(meta.modified()?.into())
    }
}
#[derive(thiserror::Error, Debug)]
#[error("Failed to get ctime/mtime for {path:?}")]
pub struct ModTimeResolveError {
    path: PathBuf,
    #[source]
    cause: std::io::Error,
}

#[derive(thiserror::Error, Debug)]
#[error("Failed to lock output file {entry:?} in directory {output_dir:?}")]
pub struct FileLockError {
    entry: RelativePathBuf,
    output_dir: PathBuf,
    #[source]
    cause: FileLockErrorReason,
}
#[derive(thiserror::Error, Debug)]
enum FileLockErrorReason {
    #[error("Failed to open file {path:?}")]
    OpenFile {
        path: Utf8PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("Failed to acquire lock")]
    AcquireLock {
        path: Utf8PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("Resolved path contains invalid UtF8")]
    OutputInvalidUtf8(#[source] camino::FromPathBufError),
}

#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct CacheOpenError(#[from] CacheOpenErrorReason);

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
enum CacheOpenErrorReason {
    #[error("Failed to create cache directory {cache_subdir:?}")]
    CreateCacheDir {
        cache_subdir: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("Failed to create CACHEDIR.TAG in {cache_subdir:?}")]
    CreateCacheTag {
        cache_subdir: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("Failed to create lockfile at {lockfile_path:?}")]
    LockfileCreate {
        lockfile_path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("Failed to lock the cache (lockfile: {lockfile_path:?})")]
    LockfileLock {
        lockfile_path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("Failed to open database at {database_file:?}")]
    DatabaseOpen {
        database_file: PathBuf,
        #[source]
        cause: rusqlite::Error,
    },
    #[error("Failed to setup database at {database_file:?}")]
    DatabaseSetupError {
        database_file: PathBuf,
        #[source]
        cause: rusqlite::Error,
    },
    #[error("Database file has version {actual_version}, newer then expected {expected_version} at {database_file:?}")]
    UnsupportedDatabaseVersion {
        actual_version: u32,
        expected_version: u32,
        database_file: PathBuf,
    },
}

const CACHEDIR_TAG_CONTENTS: &str = indoc::indoc!(
    r#"
    Signature: 8a477f597d28d172789f06886806bc55
    # This directory is a cache for the anvil-recompress tool
    # See https://github.com/Techcable/anvil-recompress.mc/
"#
);

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn cachedir_tag_no_leading_whitespace() {
        // ensures the signature line has no leading whitespace
        assert!(CACHEDIR_TAG_CONTENTS.starts_with("Signature: 8a477"));
    }
}
