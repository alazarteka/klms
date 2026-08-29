use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use crate::{
    client::KlmsClient,
    error::AppError,
    models::DownloadResult,
    output::{self, CommandResult},
    safe_url,
};

const MAX_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;

pub(super) fn download(
    client: &KlmsClient,
    source: &str,
    out: &Path,
) -> Result<CommandResult, AppError> {
    if out.exists() {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let parent = out
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        return Err(AppError::config(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }
    let (temp, mut file) = create_temp(parent)?;
    let response = match client.download_to(source, MAX_DOWNLOAD_BYTES, &mut file) {
        Ok(response) => response,
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
    };
    if let Err(error) = file.sync_all() {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::config(format!(
            "failed to write download: {error}"
        )));
    }
    drop(file);
    let cleanup_warning = publish_new(&temp, out)?;
    let final_path = out.canonicalize().unwrap_or_else(|_| out.to_path_buf());
    let model = DownloadResult {
        path: final_path.display().to_string(),
        bytes: response.bytes,
        source_url: safe_url::display(&response.url),
        content_type: response.content_type,
    };
    let mut result = output::result(
        "files.download",
        &model,
        format!("Downloaded {} bytes to {}", model.bytes, model.path),
    )?;
    if let Some(warning) = cleanup_warning {
        result.warnings.push(warning);
    }
    Ok(result)
}

fn create_temp(parent: &Path) -> Result<(PathBuf, File), AppError> {
    for attempt in 0..100 {
        let path = parent.join(format!(
            ".klms-download-{}-{attempt}.part",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::config(format!(
                    "cannot create temporary download: {error}"
                )));
            }
        }
    }
    Err(AppError::config(
        "cannot create a unique temporary download file",
    ))
}

fn publish_new(temp: &Path, out: &Path) -> Result<Option<String>, AppError> {
    std::fs::hard_link(temp, out).map_err(|error| {
        let _ = std::fs::remove_file(temp);
        if out.exists() {
            AppError::config(format!("destination already exists: {}", out.display()))
        } else {
            AppError::config(format!("failed to finalize download: {error}"))
        }
    })?;
    Ok(std::fs::remove_file(temp).err().map(|error| {
        format!(
            "download completed, but temporary link cleanup failed for {}: {error}",
            temp.display()
        )
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{create_temp, publish_new};

    #[test]
    fn publish_new_never_replaces_an_existing_destination() {
        let directory = TempDir::new().unwrap();
        let temp = directory.path().join("download.part");
        let out = directory.path().join("notes.pdf");
        fs::write(&temp, b"new bytes").unwrap();
        fs::write(&out, b"existing bytes").unwrap();

        let error = publish_new(&temp, &out).unwrap_err();

        assert_eq!(error.code, "CONFIG_ERROR");
        assert_eq!(fs::read(&out).unwrap(), b"existing bytes");
        assert!(!temp.exists());
    }

    #[cfg(unix)]
    #[test]
    fn temporary_download_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TempDir::new().unwrap();
        let (path, _) = create_temp(directory.path()).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
