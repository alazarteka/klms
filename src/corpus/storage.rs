use std::{
    env, fs,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use super::schema;
use crate::error::AppError;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct LibraryPaths {
    pub database: PathBuf,
    pub objects: PathBuf,
}

impl LibraryPaths {
    fn discover() -> Result<Self, AppError> {
        let data_home = env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|value| PathBuf::from(value).join(".local/share"))
            })
            .ok_or_else(|| {
                AppError::config("HOME or XDG_DATA_HOME is required for the local library")
            })?;
        Ok(Self::at_data_home(data_home))
    }

    fn at_data_home(data_home: PathBuf) -> Self {
        let root = data_home.join("klms");
        Self {
            database: root.join("library.db"),
            objects: root.join("objects/sha256"),
        }
    }

    fn root(&self) -> Result<&Path, AppError> {
        self.database
            .parent()
            .ok_or_else(|| AppError::internal("invalid local library path"))
    }
}

#[derive(Debug)]
pub struct CorpusStorage {
    pub paths: LibraryPaths,
    pub created: bool,
    pub(super) connection: Connection,
}

impl CorpusStorage {
    pub fn initialize() -> Result<Self, AppError> {
        Self::initialize_at(LibraryPaths::discover()?, BUSY_TIMEOUT)
    }

    fn initialize_at(paths: LibraryPaths, timeout: Duration) -> Result<Self, AppError> {
        let data_home = paths
            .root()?
            .parent()
            .ok_or_else(|| AppError::internal("invalid local data path"))?;
        fs::create_dir_all(data_home).map_err(|error| io_error("create", data_home, error))?;
        private_dir(paths.root()?)?;
        let object_parent = paths
            .objects
            .parent()
            .ok_or_else(|| AppError::internal("invalid object-store path"))?;
        private_dir(object_parent)?;
        private_dir(&paths.objects)?;
        let created = create_database(&paths.database)?;
        let mut connection = Connection::open_with_flags(
            &paths.database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| sqlite_error(&paths.database, error))?;
        connection
            .busy_timeout(timeout)
            .map_err(|error| sqlite_error(&paths.database, error))?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY;")
            .map_err(|error| sqlite_error(&paths.database, error))?;
        migrate(&mut connection, &paths.database)?;
        set_private_file(&paths.database)?;
        Ok(Self {
            paths,
            created,
            connection,
        })
    }
}

fn create_database(path: &Path) -> Result<bool, AppError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.file_type().is_file() => {
            return Err(AppError::library_io(format!(
                "refusing unexpected database target {}",
                path.display()
            )));
        }
        Ok(_) => {
            set_private_file(path)?;
            return Ok(false);
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(io_error("inspect", path, error));
        }
        Err(_) => {}
    }
    private_file_options()
        .read(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error("create", path, error))?
        .sync_all()
        .map_err(|error| io_error("create", path, error))?;
    Ok(true)
}

fn migrate(connection: &mut Connection, path: &Path) -> Result<(), AppError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error(path, error))?;
    let version = transaction
        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
        .map_err(|error| sqlite_error(path, error))?;
    if version > schema::VERSION {
        return Err(AppError::migration_required(format!(
            "library schema {version} is newer than supported schema {}",
            schema::VERSION
        )));
    }
    if version == 0 {
        transaction
            .execute_batch(schema::SCHEMA)
            .map_err(|error| sqlite_error(path, error))?;
        transaction
            .pragma_update(None, "user_version", schema::VERSION)
            .map_err(|error| sqlite_error(path, error))?;
    }
    transaction
        .commit()
        .map_err(|error| sqlite_error(path, error))
}

pub fn private_dir(path: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.file_type().is_dir() => {
            return Err(AppError::library_io(format!(
                "refusing unexpected directory target {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| io_error("create", path, error))?;
        }
        Err(error) => return Err(io_error("inspect", path, error)),
    }
    set_private_dir(path)
}

pub fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

#[cfg(unix)]
fn set_private_dir(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("secure", path, error))
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error("secure", path, error))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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

    #[test]
    fn newer_schema_requires_migration() {
        let temp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::at_data_home(temp.path().into());
        fs::create_dir_all(paths.database.parent().unwrap()).unwrap();
        let connection = Connection::open(&paths.database).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);
        let error = CorpusStorage::initialize_at(paths, Duration::ZERO).unwrap_err();
        assert_eq!(error.code, "MIGRATION_REQUIRED");
    }

    #[test]
    fn locked_database_is_retryable() {
        let temp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::at_data_home(temp.path().into());
        let storage = CorpusStorage::initialize_at(paths.clone(), Duration::ZERO).unwrap();
        storage.connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = CorpusStorage::initialize_at(paths, Duration::ZERO).unwrap_err();
        assert_eq!(error.code, "CORPUS_BUSY");
        assert!(error.retryable);
    }

    #[test]
    fn invalid_database_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::at_data_home(temp.path().into());
        fs::create_dir_all(paths.database.parent().unwrap()).unwrap();
        fs::write(&paths.database, b"not sqlite").unwrap();
        let error = CorpusStorage::initialize_at(paths.clone(), Duration::ZERO).unwrap_err();
        assert_eq!(error.code, "CORPUS_CORRUPT");
        assert_eq!(fs::read(paths.database).unwrap(), b"not sqlite");
    }
}
