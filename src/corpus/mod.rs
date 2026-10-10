mod curate;
mod object_store;
mod query;
mod schema;
mod sync;

use std::{
    env, fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::{
    error::AppError,
    private_fs::private_file_options,
    reference::{ResourceRef, valid_id},
};

pub use crate::models::{
    ActivityEntry, ChangeEntry, ContentRecord, EditResult, HistoryEntry, LastSync, LibraryStatus,
    RelationResult, RetractionResult, SearchHit, SyncSummary,
};
pub use sync::SyncOptions;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryRef {
    Course(String),
    Resource(String),
    Representation(i64),
    Sha256(String),
    Assertion(i64),
    Relation(i64),
    Sync(i64),
}

impl FromStr for LibraryRef {
    type Err = AppError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let num = |prefix: &str| {
            value
                .strip_prefix(prefix)
                .and_then(|id| id.parse::<i64>().ok())
                .filter(|id| *id > 0)
        };
        num("representation:")
            .map(Self::Representation)
            .or_else(|| num("assertion:").map(Self::Assertion))
            .or_else(|| num("relation:").map(Self::Relation))
            .or_else(|| num("sync:").map(Self::Sync))
            .or_else(|| {
                (value.strip_prefix("course:").filter(|id| valid_id(id)))
                    .map(|id| Self::Course(id.into()))
            })
            .or_else(|| {
                (value.strip_prefix("sha256:").filter(|h| valid_hash(h, 64)))
                    .map(|hash| Self::Sha256(hash.to_ascii_lowercase()))
            })
            .or_else(|| valid_resource(value).then(|| Self::Resource(value.into())))
            .ok_or_else(|| AppError::usage(format!("invalid library reference {value:?}")))
    }
}

impl fmt::Display for LibraryRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Course(id) => write!(f, "course:{id}"),
            Self::Resource(reference) => f.write_str(reference),
            Self::Representation(id) => write!(f, "representation:{id}"),
            Self::Sha256(hash) => write!(f, "sha256:{hash}"),
            Self::Assertion(id) => write!(f, "assertion:{id}"),
            Self::Relation(id) => write!(f, "relation:{id}"),
            Self::Sync(id) => write!(f, "sync:{id}"),
        }
    }
}

fn valid_hash(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_resource(value: &str) -> bool {
    match value.strip_prefix("resource:") {
        Some(hash) => valid_hash(hash, 24),
        None => ResourceRef::parse(value)
            .is_ok_and(|reference| !matches!(reference, ResourceRef::Course(_))),
    }
}

#[derive(Debug)]
pub struct Corpus {
    connection: Connection,
    database: PathBuf,
    objects: PathBuf,
    created: bool,
}

#[derive(serde::Serialize)]
pub struct ContentPreview {
    #[serde(rename = "ref")]
    pub reference: String,
    pub byte_length: u64,
    pub mime: Option<String>,
    pub filename: String,
    pub text: Option<String>,
    pub truncated: bool,
}

impl Corpus {
    pub fn open() -> Result<Self, AppError> {
        let data_home = env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                (env::var_os("HOME").filter(|value| !value.is_empty()))
                    .map(|home| PathBuf::from(home).join(".local/share"))
            })
            .ok_or_else(|| {
                AppError::config("HOME or XDG_DATA_HOME is required for the local library")
            })?;
        Self::open_at(&data_home, Duration::from_secs(5))
    }

    fn open_at(data_home: &Path, timeout: Duration) -> Result<Self, AppError> {
        let root = data_home.join("klms");
        let database = root.join("library.db");
        let objects = root.join("objects/sha256");
        fs::create_dir_all(data_home).map_err(|e| io_error("create", data_home, e))?;
        for dir in [&root, &root.join("objects"), &objects] {
            private_dir(dir)?;
        }
        let created = create_database(&database)?;
        chmod(&database, 0o600)?;
        let sql = |error| sqlite_error(&database, error);
        let mut connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .and_then(|connection| {
            connection.busy_timeout(timeout)?;
            connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY;")?;
            Ok(connection)
        })
        .map_err(sql)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let version = (transaction
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0)))
        .map_err(sql)?;
        if version > schema::VERSION {
            return Err(AppError::migration_required(format!(
                "library schema {version} is newer than supported schema {}",
                schema::VERSION
            )));
        }
        if version == 0 {
            (transaction.execute_batch(schema::SCHEMA))
                .and_then(|()| transaction.pragma_update(None, "user_version", schema::VERSION))
                .map_err(sql)?;
        }
        transaction.commit().map_err(sql)?;
        Ok(Self {
            connection,
            database,
            objects,
            created,
        })
    }
}

fn create_database(path: &Path) -> Result<bool, AppError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(false),
        Ok(_) => Err(AppError::library_io(format!(
            "refusing unexpected database target {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let file = private_file_options()
                .read(true)
                .create_new(true)
                .open(path);
            file.and_then(|file| file.sync_all())
                .map_err(|e| io_error("create", path, e))?;
            Ok(true)
        }
        Err(error) => Err(io_error("inspect", path, error)),
    }
}

fn private_dir(path: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(AppError::library_io(format!(
                "refusing unexpected directory target {}",
                path.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|e| io_error("create", path, e))?;
        }
        Err(error) => return Err(io_error("inspect", path, error)),
    }
    chmod(path, 0o700)
}

fn chmod(path: &Path, mode: u32) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| io_error("secure", path, e))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn io_error(action: &str, path: &Path, error: io::Error) -> AppError {
    AppError::library_io(format!(
        "cannot {action} local library path {}: {error}",
        path.display()
    ))
}

fn sqlite_error(path: &Path, error: rusqlite::Error) -> AppError {
    let mut converted = AppError::from(error);
    converted.message = format!("{} ({})", converted.message, path.display());
    converted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library_db(temp: &tempfile::TempDir) -> PathBuf {
        let path = temp.path().join("klms/library.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    #[test]
    fn library_references_parse_and_display_round_trip() {
        for value in [
            "course:12",
            "file:9",
            "assign:5",
            "quiz:6",
            "board:7",
            "vod:8",
            "activity:page:8",
            "board-post:3:4",
            "resource:0123456789abcdef01234567",
            "representation:2",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "assertion:7",
            "relation:8",
            "sync:9",
        ] {
            assert_eq!(value.parse::<LibraryRef>().unwrap().to_string(), value);
        }
    }

    #[test]
    fn storage_rejects_newer_schema_locked_and_invalid_databases() {
        let temp = tempfile::tempdir().unwrap();
        let connection = Connection::open(library_db(&temp)).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);
        let error = Corpus::open_at(temp.path(), Duration::ZERO).unwrap_err();
        assert_eq!(error.code, "MIGRATION_REQUIRED");

        let locked = tempfile::tempdir().unwrap();
        let corpus = Corpus::open_at(locked.path(), Duration::ZERO).unwrap();
        corpus.connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = Corpus::open_at(locked.path(), Duration::ZERO).unwrap_err();
        assert_eq!(error.code, "CORPUS_BUSY");
        assert!(error.retryable);

        let invalid = tempfile::tempdir().unwrap();
        let path = library_db(&invalid);
        fs::write(&path, b"not sqlite").unwrap();
        let error = Corpus::open_at(invalid.path(), Duration::ZERO).unwrap_err();
        assert_eq!(error.code, "CORPUS_CORRUPT");
        assert_eq!(fs::read(path).unwrap(), b"not sqlite");
    }
}
