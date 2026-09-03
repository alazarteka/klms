use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::storage::{private_dir, private_file_options};
use crate::error::AppError;

#[derive(Debug)]
pub struct StoredObject {
    pub sha256: String,
    pub bytes: u64,
}

pub fn store(root: &Path, bytes: &[u8]) -> Result<StoredObject, AppError> {
    let sha256 = digest(bytes);
    let directory = root.join(&sha256[..2]);
    private_dir(&directory)?;
    let destination = root.join(&sha256[..2]).join(&sha256[2..]);
    if let Ok(metadata) = fs::symlink_metadata(&destination) {
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_file()
            || metadata.len() != bytes.len() as u64
        {
            return Err(AppError::corpus_corrupt(format!(
                "invalid object {}",
                destination.display()
            )));
        }
        return Ok(StoredObject {
            sha256,
            bytes: bytes.len() as u64,
        });
    }
    let temporary = directory.join(format!(".{sha256}.{}.tmp", std::process::id()));
    let mut file = private_file_options()
        .create_new(true)
        .open(&temporary)
        .map_err(|error| AppError::library_io(format!("cannot create object: {error}")))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(AppError::library_io(format!(
            "cannot write object: {error}"
        )));
    }
    match fs::hard_link(&temporary, &destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&destination)
                .map_err(|error| AppError::library_io(error.to_string()))?;
            if metadata.file_type().is_symlink()
                || !metadata.file_type().is_file()
                || metadata.len() != bytes.len() as u64
            {
                let _ = fs::remove_file(&temporary);
                return Err(AppError::corpus_corrupt("object destination collision"));
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(AppError::library_io(format!(
                "cannot publish object: {error}"
            )));
        }
    }
    fs::remove_file(&temporary)
        .map_err(|error| AppError::library_io(format!("cannot remove temporary file: {error}")))?;
    Ok(StoredObject {
        sha256,
        bytes: bytes.len() as u64,
    })
}

pub fn object_path(root: &Path, sha256: &str) -> Result<PathBuf, AppError> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::corpus_corrupt("invalid object digest"));
    }
    let path = root.join(&sha256[..2]).join(&sha256[2..]);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| AppError::content_unavailable("stored content file is missing"))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(AppError::corpus_corrupt(
            "stored content target is not a regular file",
        ));
    }
    Ok(path)
}

pub fn export(root: &Path, sha256: &str, destination: &Path) -> Result<u64, AppError> {
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(AppError::library_io("export destination already exists")),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(AppError::library_io(format!(
                "cannot inspect export destination: {error}"
            )));
        }
        Err(_) => {}
    }
    let source = object_path(root, sha256)?;
    let mut input = fs::File::open(source)
        .map_err(|error| AppError::library_io(format!("cannot open object: {error}")))?;
    let mut output = private_file_options()
        .create_new(true)
        .open(destination)
        .map_err(|error| AppError::library_io(format!("cannot create export: {error}")))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| AppError::library_io(format!("cannot read object: {error}")))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total += count as u64;
        if let Err(error) = output.write_all(&buffer[..count]) {
            let _ = fs::remove_file(destination);
            return Err(AppError::library_io(format!(
                "cannot write export: {error}"
            )));
        }
    }
    if hex(hasher.finalize().as_slice()) != sha256 {
        let _ = fs::remove_file(destination);
        return Err(AppError::corpus_corrupt("stored content digest mismatch"));
    }
    Ok(total)
}

pub fn digest(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_deduplicates_and_rejects_length_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sha256");
        fs::create_dir(&root).unwrap();
        let first = store(&root, b"same").unwrap();
        let second = store(&root, b"same").unwrap();
        assert_eq!(first.sha256, second.sha256);
        fs::write(object_path(&root, &first.sha256).unwrap(), b"longer").unwrap();
        assert_eq!(store(&root, b"same").unwrap_err().code, "CORPUS_CORRUPT");
    }

    #[test]
    fn export_rejects_digest_mismatch_and_symlink_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sha256");
        fs::create_dir(&root).unwrap();
        let object = store(&root, b"same").unwrap();
        fs::write(object_path(&root, &object.sha256).unwrap(), b"evil").unwrap();
        let output = temp.path().join("output");
        assert_eq!(
            export(&root, &object.sha256, &output).unwrap_err().code,
            "CORPUS_CORRUPT"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("missing", &output).unwrap();
            assert_eq!(
                export(&root, &object.sha256, &output).unwrap_err().code,
                "LIBRARY_IO"
            );
        }
    }
}
