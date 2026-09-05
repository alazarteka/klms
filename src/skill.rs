use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process,
};

use serde::Serialize;

use crate::{error::AppError, output};

const EMBEDDED_SKILL: &str = include_str!("../skills/klms/SKILL.md");

#[derive(Debug, Serialize)]
struct SkillState {
    version: &'static str,
    payload_path: String,
    link_path: String,
    payload_installed: bool,
    payload_current: bool,
    link_installed: bool,
    link_current: bool,
}

pub fn install() -> Result<output::CommandResult, AppError> {
    let paths = Paths::discover()?;
    preflight(&paths)?;
    install_files(&paths)?;
    let state = inspect(&paths)?;
    let human = format!(
        "Installed klms Agent Skill\nPayload: {}\nDiscovery link: {} -> {}",
        state.payload_path, state.link_path, state.payload_path
    );
    output::result("skill.install", &state, human)
}

// The caller performs preflight once, before any install or rollback work.
fn install_files(paths: &Paths) -> Result<(), AppError> {
    ensure_directory(&paths.payload_dir)?;
    write_atomic(&paths.payload_file, EMBEDDED_SKILL.as_bytes())?;
    ensure_directory(&paths.link_parent)?;
    install_link(&paths.link, &paths.payload_dir)?;

    Ok(())
}

pub fn status() -> Result<output::CommandResult, AppError> {
    let paths = Paths::discover()?;
    let state = inspect(&paths)?;
    let status = if state.payload_current && state.link_current {
        "current"
    } else if state.payload_installed || state.link_installed {
        "needs installation"
    } else {
        "not installed"
    };
    let human = format!(
        "klms Agent Skill: {status}\nPayload: {}\nDiscovery link: {}",
        state.payload_path, state.link_path
    );
    output::result("skill.status", &state, human)
}

struct Paths {
    payload_dir: PathBuf,
    payload_file: PathBuf,
    link_parent: PathBuf,
    link: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self, AppError> {
        let home = nonempty_env("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| AppError::config("HOME is required to install the Agent Skill"))?;
        let data_home = nonempty_env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let payload_dir = data_home.join("klms/skills/klms");
        let link_parent = home.join(".agents/skills");
        Ok(Self {
            payload_file: payload_dir.join("SKILL.md"),
            payload_dir,
            link: link_parent.join("klms"),
            link_parent,
        })
    }
}

fn nonempty_env(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

fn inspect(paths: &Paths) -> Result<SkillState, AppError> {
    let payload_installed = paths.payload_file.is_file();
    let payload_current = payload_installed
        && fs::read(&paths.payload_file)
            .map_err(|error| io_error("read", &paths.payload_file, error))?
            == EMBEDDED_SKILL.as_bytes();
    let link_target = match fs::symlink_metadata(&paths.link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Some(fs::read_link(&paths.link).map_err(|error| io_error("read", &paths.link, error))?)
        }
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error("inspect", &paths.link, error)),
    };
    let link_installed = link_target.is_some();
    let link_current = link_target.as_deref() == Some(paths.payload_dir.as_path());
    Ok(SkillState {
        version: env!("CARGO_PKG_VERSION"),
        payload_path: display(&paths.payload_dir),
        link_path: display(&paths.link),
        payload_installed,
        payload_current,
        link_installed,
        link_current,
    })
}

fn ensure_directory(path: &Path) -> Result<(), AppError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(AppError::config(format!(
                "refusing to replace unexpected path {}",
                display(path)
            )));
        }
        return Ok(());
    }
    fs::create_dir_all(path).map_err(|error| io_error("create", path, error))
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), AppError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_file() {
            return Err(AppError::config(format!(
                "refusing to replace unexpected path {}",
                display(path)
            )));
        }
    }
    let temporary = path.with_extension(format!("tmp-{}", process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| io_error("create", &temporary, error))?;
    if let Err(error) = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path))
    {
        let _ = fs::remove_file(&temporary);
        return Err(io_error("install", path, error));
    }
    Ok(())
}

fn install_link(link: &Path, target: &Path) -> Result<(), AppError> {
    if fs::symlink_metadata(link).is_ok() {
        return Ok(());
    }
    create_directory_symlink(target, link).map_err(|error| io_error("create", link, error))
}

fn validate_link(link: &Path, target: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let existing = fs::read_link(link).map_err(|error| io_error("read", link, error))?;
            if existing == target {
                Ok(())
            } else {
                Err(AppError::config(format!(
                    "refusing to replace symlink {} -> {}",
                    display(link),
                    display(&existing)
                )))
            }
        }
        Ok(_) => Err(AppError::config(format!(
            "refusing to replace unexpected path {}",
            display(link)
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect", link, error)),
    }
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

fn io_error(action: &str, path: &Path, error: std::io::Error) -> AppError {
    AppError::config(format!("failed to {action} {}: {error}", display(path)))
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// Preserve skill content and discovery state if the final binary switch fails.
// Empty directories created during installation may remain after rollback.
pub fn with_install<T>(commit: impl FnOnce() -> Result<T, AppError>) -> Result<T, AppError> {
    with_install_at(Paths::discover()?, commit)
}

fn with_install_at<T>(
    paths: Paths,
    commit: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    preflight(&paths)?;
    let previous = match fs::read(&paths.payload_file) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error("read", &paths.payload_file, error)),
    };
    let link_existed = fs::symlink_metadata(&paths.link).is_ok();
    let result = install_files(&paths).and_then(|_| commit());
    if let Err(error) = result {
        let rollback = (|| {
            if !link_existed && fs::symlink_metadata(&paths.link).is_ok() {
                fs::remove_file(&paths.link).map_err(|e| io_error("restore", &paths.link, e))?;
            }
            match previous {
                Some(bytes) => write_atomic(&paths.payload_file, &bytes)?,
                None if paths.payload_file.exists() => fs::remove_file(&paths.payload_file)
                    .map_err(|e| io_error("restore", &paths.payload_file, e))?,
                None => (),
            }
            Ok::<(), AppError>(())
        })();
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback) => Err(AppError::config(format!(
                "{error}; skill rollback also failed: {rollback}"
            ))),
        };
    }
    result
}

fn preflight(paths: &Paths) -> Result<(), AppError> {
    validate_link(&paths.link, &paths.payload_dir)?;
    for directory in [&paths.payload_dir, &paths.link_parent] {
        // HOME itself may be a platform-managed symlink. Validate the managed
        // destination and reject non-directory ancestors without following files.
        for ancestor in directory.ancestors() {
            match fs::metadata(ancestor) {
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(AppError::config(format!(
                        "expected directory {}",
                        display(ancestor)
                    )));
                }
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(io_error("inspect", ancestor, error)),
            }
        }
        if let Ok(metadata) = fs::symlink_metadata(directory) {
            if metadata.file_type().is_symlink() {
                return Err(AppError::config(format!(
                    "refusing unexpected symlink {}",
                    display(directory)
                )));
            }
        }
    }
    match fs::symlink_metadata(&paths.payload_file) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            Err(AppError::config(format!(
                "refusing unexpected skill payload {}",
                display(&paths.payload_file)
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect", &paths.payload_file, error)),
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;

    fn paths(home: &Path) -> Paths {
        let payload_dir = home.join("data/klms");
        let link_parent = home.join("skills");
        Paths {
            payload_file: payload_dir.join("SKILL.md"),
            payload_dir,
            link: link_parent.join("klms"),
            link_parent,
        }
    }

    #[test]
    fn failed_binary_commit_restores_previous_skill() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths(home.path());
        fs::create_dir_all(&paths.payload_dir).unwrap();
        fs::write(&paths.payload_file, b"old skill").unwrap();
        let result = with_install_at(paths, || {
            Err::<(), _>(AppError::config("binary rename failed"))
        });
        assert!(result.is_err());
        let paths = self::paths(home.path());
        assert_eq!(fs::read(paths.payload_file).unwrap(), b"old skill");
        assert!(fs::symlink_metadata(paths.link).is_err());
    }

    #[test]
    fn failed_binary_commit_removes_fresh_skill() {
        let home = tempfile::tempdir().unwrap();
        let result = with_install_at(paths(home.path()), || {
            Err::<(), _>(AppError::config("binary rename failed"))
        });
        assert!(result.is_err());
        let paths = paths(home.path());
        assert!(!paths.payload_file.exists());
        assert!(fs::symlink_metadata(paths.link).is_err());
    }
}
