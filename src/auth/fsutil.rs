//! Private (0700 directory, 0600 file) atomic writes shared by every file
//! `klms auth` owns.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use crate::error::AppError;

/// Atomically replace `path` with `bytes` at mode 0600, creating its parent
/// directory at mode 0700. `what` names the file in error messages.
pub fn write_private(path: &Path, bytes: &[u8], what: &str) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::internal(format!("invalid {what} path")))?;
    fs::create_dir_all(parent).map_err(|error| {
        AppError::config(format!("cannot create {}: {error}", parent.display()))
    })?;
    set_private_dir(parent)?;
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(temp_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let _ = fs::remove_file(&temp);
    let mut file = options
        .open(&temp)
        .map_err(|error| AppError::config(format!("cannot create private {what} file: {error}")))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(AppError::config(format!(
            "cannot write {what} file: {error}"
        )));
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(AppError::config(format!(
            "cannot install {what} file: {error}"
        )));
    }
    set_private_file(path)
}

/// Remove `path`; `Ok(false)` when it was already absent.
pub fn remove_file(path: &Path, what: &str) -> Result<bool, AppError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AppError::config(format!(
            "cannot remove {what} {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(unix)]
pub fn set_private_dir(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| AppError::config(format!("cannot secure {}: {error}", path.display())))
}

#[cfg(not(unix))]
pub fn set_private_dir(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(unix)]
pub fn set_private_file(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| AppError::config(format!("cannot secure {}: {error}", path.display())))
}

#[cfg(not(unix))]
pub fn set_private_file(_path: &Path) -> Result<(), AppError> {
    Ok(())
}
