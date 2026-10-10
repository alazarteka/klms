//! Private (owner-only) files and their no-overwrite atomic publication.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    path::Path,
};

/// Write-only options that create files with mode 0600 on Unix.
pub fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

#[derive(Debug)]
pub enum PublishError<E> {
    Create(io::Error),
    Fill(E),
    Sync(io::Error),
    Link(io::Error),
}

/// How a publication ended.
pub struct Linked {
    /// `destination` was already there and was left untouched.
    pub existed: bool,
    /// Removing `temporary` failed; the content is published regardless.
    pub leftover: Option<io::Error>,
}

/// Creates `temporary` exclusively with mode 0600, lets `fill` write it,
/// syncs it, hard-links it to `destination` without ever replacing that, and
/// removes `temporary`. On every failure `temporary` is removed again.
pub fn publish_new<T, E>(
    temporary: &Path,
    destination: &Path,
    fill: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<(T, Linked), PublishError<E>> {
    let mut file = private_file_options()
        .create_new(true)
        .open(temporary)
        .map_err(PublishError::Create)?;
    let value = match fill(&mut file) {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(temporary);
            return Err(PublishError::Fill(error));
        }
    };
    if let Err(error) = file.sync_all() {
        let _ = fs::remove_file(temporary);
        return Err(PublishError::Sync(error));
    }
    drop(file);
    let existed = match fs::hard_link(temporary, destination) {
        Ok(()) => false,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => true,
        Err(error) => {
            let _ = fs::remove_file(temporary);
            return Err(PublishError::Link(error));
        }
    };
    let leftover = fs::remove_file(temporary).err();
    Ok((value, Linked { existed, leftover }))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn publication_never_replaces_and_leaves_no_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let (temporary, out) = (directory.path().join("x.part"), directory.path().join("x"));
        let write = |bytes: &'static [u8]| move |file: &mut File| file.write_all(bytes);
        let (_, linked) = publish_new(&temporary, &out, write(b"first")).unwrap();
        assert!(!linked.existed && linked.leftover.is_none());
        let (_, linked) = publish_new(&temporary, &out, write(b"second")).unwrap();
        assert!(linked.existed);
        assert_eq!(fs::read(&out).unwrap(), b"first");
        assert!(!temporary.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&out).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_failed_fill_or_stale_temporary_is_reported_and_cleaned() {
        let directory = tempfile::tempdir().unwrap();
        let (temporary, out) = (directory.path().join("x.part"), directory.path().join("x"));
        let failed = publish_new(&temporary, &out, |_| Err::<(), _>("no"));
        assert!(matches!(failed, Err(PublishError::Fill("no"))));
        assert!(!temporary.exists() && !out.exists());
        fs::write(&temporary, b"stale").unwrap();
        let stale = publish_new(&temporary, &out, |_| Ok::<_, ()>(()));
        assert!(matches!(stale, Err(PublishError::Create(_))));
        assert_eq!(fs::read(&temporary).unwrap(), b"stale");
    }
}
