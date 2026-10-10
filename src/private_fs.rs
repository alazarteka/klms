//! Private (owner-only) files and their no-overwrite atomic publication.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
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
    /// The temporary this call created (`temporary`, or a suffixed variant
    /// when that name was already taken).
    pub temporary: PathBuf,
    /// `destination` was already there and was left untouched.
    pub existed: bool,
    /// Removing `temporary` failed; the content is published regardless.
    pub leftover: Option<io::Error>,
}

const TEMPORARY_ATTEMPTS: u32 = 8;

/// Creates a temporary exclusively with mode 0600, lets `fill` write it,
/// syncs it, hard-links it to `destination` without ever replacing that, and
/// removes the temporary. The name is `temporary`; if something already has
/// that name (say, a leftover of a crashed run with a recycled pid), up to
/// seven other unique names are tried, and nothing this call did not create
/// is ever removed. On every failure the temporary it created is removed.
pub fn publish_new<T, E>(
    temporary: &Path,
    destination: &Path,
    fill: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<(T, Linked), PublishError<E>> {
    let (mut file, temporary) = create_exclusive(temporary)?;
    let temporary = temporary.as_path();
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
    let linked = Linked {
        temporary: temporary.to_path_buf(),
        existed,
        leftover,
    };
    Ok((value, linked))
}

fn create_exclusive<E>(base: &Path) -> Result<(File, PathBuf), PublishError<E>> {
    let nanos = (SystemTime::now().duration_since(UNIX_EPOCH)).map_or(0, |d| d.subsec_nanos());
    let mut last = None;
    for attempt in 0..TEMPORARY_ATTEMPTS {
        let candidate = if attempt == 0 {
            base.to_path_buf()
        } else {
            let mut name = base.as_os_str().to_owned();
            name.push(format!(".{nanos:x}{attempt}"));
            PathBuf::from(name)
        };
        match private_file_options().create_new(true).open(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(PublishError::Create(error)),
        }
    }
    Err(PublishError::Create(last.expect("at least one attempt")))
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
    fn a_failed_fill_is_cleaned_and_a_stale_temporary_is_kept_but_bypassed() {
        let directory = tempfile::tempdir().unwrap();
        let (temporary, out) = (directory.path().join("x.part"), directory.path().join("x"));
        let failed = publish_new(&temporary, &out, |_| Err::<(), _>("no"));
        assert!(matches!(failed, Err(PublishError::Fill("no"))));
        assert!(!temporary.exists() && !out.exists());
        fs::write(&temporary, b"stale").unwrap();
        let (_, linked) = publish_new(&temporary, &out, |file| file.write_all(b"new")).unwrap();
        assert_ne!(linked.temporary, temporary);
        assert!(!linked.temporary.exists() && !linked.existed);
        assert_eq!(fs::read(&temporary).unwrap(), b"stale");
        assert_eq!(fs::read(&out).unwrap(), b"new");
    }
}
