use std::{
    fs,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::{private_dir, private_file_options};
use crate::{
    error::AppError,
    private_fs::{PublishError, publish_new},
};

fn io(context: &str) -> impl Fn(std::io::Error) -> AppError + '_ {
    move |error| AppError::library_io(format!("cannot {context}: {error}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Store `bytes` under their SHA-256, deduplicating; returns the digest.
pub fn store(root: &Path, bytes: &[u8]) -> Result<String, AppError> {
    let sha256 = digest(bytes);
    let directory = root.join(&sha256[..2]);
    private_dir(&directory)?;
    let destination = directory.join(&sha256[2..]);
    let intact =
        || fs::symlink_metadata(&destination).map(|m| m.is_file() && m.len() == bytes.len() as u64);
    match intact() {
        Ok(true) => return Ok(sha256),
        Ok(false) => {
            return Err(AppError::corpus_corrupt(format!(
                "invalid object {}",
                destination.display()
            )));
        }
        Err(_) => {}
    }
    let temporary = directory.join(format!(".{sha256}.{}.tmp", std::process::id()));
    let (_, linked) =
        publish_new(&temporary, &destination, |file| file.write_all(bytes)).map_err(|error| {
            match error {
                PublishError::Create(error) => io("create object")(error),
                PublishError::Fill(error) | PublishError::Sync(error) => io("write object")(error),
                PublishError::Link(error) => io("publish object")(error),
            }
        })?;
    if linked.existed && !matches!(intact(), Ok(true)) {
        return Err(AppError::corpus_corrupt("object destination collision"));
    }
    linked
        .leftover
        .map_or(Ok(sha256), |error| Err(io("remove temporary file")(error)))
}

pub fn object_path(root: &Path, sha256: &str) -> Result<PathBuf, AppError> {
    if !super::valid_hash(sha256, 64) {
        return Err(AppError::corpus_corrupt("invalid object digest"));
    }
    let path = root.join(&sha256[..2]).join(&sha256[2..]);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| AppError::content_unavailable("stored content file is missing"))?;
    if !metadata.is_file() {
        return Err(AppError::corpus_corrupt(
            "stored content target is not a regular file",
        ));
    }
    Ok(path)
}

pub fn export(root: &Path, sha256: &str, destination: &Path) -> Result<u64, AppError> {
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(AppError::library_io("export destination already exists")),
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(io("inspect export destination")(error));
        }
        Err(_) => {}
    }
    let mut input = fs::File::open(object_path(root, sha256)?).map_err(io("open object"))?;
    let mut output = private_file_options()
        .create_new(true)
        .open(destination)
        .map_err(io("create export"))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(io("read object"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total += count as u64;
        if let Err(error) = output.write_all(&buffer[..count]) {
            let _ = fs::remove_file(destination);
            return Err(io("write export")(error));
        }
    }
    if hex(&hasher.finalize()) != sha256 {
        let _ = fs::remove_file(destination);
        return Err(AppError::corpus_corrupt("stored content digest mismatch"));
    }
    Ok(total)
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
        assert_eq!(first, store(&root, b"same").unwrap());
        fs::write(object_path(&root, &first).unwrap(), b"longer").unwrap();
        assert_eq!(store(&root, b"same").unwrap_err().code, "CORPUS_CORRUPT");
    }

    #[test]
    fn export_rejects_digest_mismatch_and_symlink_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sha256");
        fs::create_dir(&root).unwrap();
        let sha = store(&root, b"same").unwrap();
        fs::write(object_path(&root, &sha).unwrap(), b"evil").unwrap();
        let output = temp.path().join("output");
        assert_eq!(
            export(&root, &sha, &output).unwrap_err().code,
            "CORPUS_CORRUPT"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("missing", &output).unwrap();
            assert_eq!(export(&root, &sha, &output).unwrap_err().code, "LIBRARY_IO");
        }
    }
}
