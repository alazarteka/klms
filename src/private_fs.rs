//! Private (owner-only) files and their no-overwrite atomic publication.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
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
/// removes the temporary. Where hard links are unsupported (exFAT/FAT, some
/// network and FUSE mounts) the temporary is copied into a destination created
/// with O_EXCL instead: still no overwrite, but not atomic, so a crash can
/// leave a partial destination. A temporary named `.klms-download-*.part` also
/// sweeps stale ones of that pattern from its directory. The name is `temporary`; if something already has
/// that name (say, a leftover of a crashed run with a recycled pid), up to
/// seven other unique names are tried, and nothing this call did not create
/// is ever removed. On every failure the temporary it created is removed.
pub fn publish_new<T, E>(
    temporary: &Path,
    destination: &Path,
    fill: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<(T, Linked), PublishError<E>> {
    publish_new_with(
        |from, to| fs::hard_link(from, to),
        temporary,
        destination,
        fill,
    )
}

fn publish_new_with<T, E>(
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
    temporary: &Path,
    destination: &Path,
    fill: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<(T, Linked), PublishError<E>> {
    let (mut file, temporary) = create_exclusive(temporary)?;
    let temporary = temporary.as_path();
    remove_stale_downloads(temporary, &file);
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
    let existed = match link(temporary, destination) {
        Ok(()) => false,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => true,
        Err(_) => match copy_new(temporary, destination) {
            Ok(existed) => existed,
            Err(error) => {
                let _ = fs::remove_file(temporary);
                return Err(PublishError::Link(error));
            }
        },
    };
    let leftover = fs::remove_file(temporary).err();
    let linked = Linked {
        temporary: temporary.to_path_buf(),
        existed,
        leftover,
    };
    Ok((value, linked))
}

/// Link-less publication: `Ok(true)` when `destination` already exists. What
/// this call created is removed again on any failure.
fn copy_new(temporary: &Path, destination: &Path) -> io::Result<bool> {
    let mut copy = match private_file_options().create_new(true).open(destination) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(true),
        Err(error) => return Err(error),
    };
    let written = File::open(temporary)
        .and_then(|mut source| io::copy(&mut source, &mut copy))
        .and_then(|_| copy.sync_all());
    if let Err(error) = written {
        drop(copy);
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(false)
}

const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Best effort: a SIGKILL during a download leaves its `.klms-download-<pid>.part`
/// behind. Remove such regular files, owned by whoever owns `own`, that have
/// not been written for a day. Symlinks and anything else are left alone.
#[cfg(unix)]
fn remove_stale_downloads(temporary: &Path, own: &File) {
    use std::os::unix::fs::MetadataExt;
    let is_download = |name: &std::ffi::OsStr| {
        name.to_str()
            .is_some_and(|n| n.starts_with(".klms-download-") && n.ends_with(".part"))
    };
    if !temporary.file_name().is_some_and(is_download) {
        return;
    }
    let directory = temporary
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (Ok(owner), Ok(entries)) = (own.metadata().map(|m| m.uid()), fs::read_dir(directory))
    else {
        return;
    };
    for entry in entries.flatten() {
        let stale = is_download(&entry.file_name())
            && fs::symlink_metadata(entry.path()).is_ok_and(|meta| {
                meta.file_type().is_file()
                    && meta.uid() == owner
                    && meta
                        .modified()
                        .ok()
                        .and_then(|time| SystemTime::now().duration_since(time).ok())
                        .is_some_and(|age| age > STALE_AFTER)
            });
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(not(unix))]
fn remove_stale_downloads(_temporary: &Path, _own: &File) {}

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

    fn no_links(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(ErrorKind::Unsupported))
    }

    #[test]
    fn without_hard_links_the_temporary_is_copied_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let (temporary, out) = (directory.path().join("x.part"), directory.path().join("x"));
        let write = |bytes: &'static [u8]| move |file: &mut File| file.write_all(bytes);
        let (_, linked) = publish_new_with(no_links, &temporary, &out, write(b"first")).unwrap();
        assert!(!linked.existed && linked.leftover.is_none());
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
        let (_, linked) = publish_new_with(no_links, &temporary, &out, write(b"second")).unwrap();
        assert!(linked.existed);
        assert_eq!(fs::read(&out).unwrap(), b"first");
        assert!(!temporary.exists());
    }

    #[test]
    fn a_failed_fallback_leaves_neither_destination_nor_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("x.part");
        let out = directory.path().join("missing/x");
        let result = publish_new_with(no_links, &temporary, &out, |file| file.write_all(b"data"));
        let Err(PublishError::Link(error)) = result else {
            panic!("expected a link error");
        };
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(!temporary.exists() && !out.exists());
    }

    #[cfg(unix)]
    #[test]
    fn publishing_sweeps_only_stale_regular_download_temporaries() {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path();
        let old = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        let age = |path: &Path| {
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        let stale = dir.join(".klms-download-111.part");
        let fresh = dir.join(".klms-download-222.part");
        let unrelated = dir.join("notes.part");
        let wrong_suffix = dir.join(".klms-download-333.txt");
        let target = dir.join("target");
        for path in [&stale, &fresh, &unrelated, &wrong_suffix, &target] {
            fs::write(path, b"x").unwrap();
        }
        for path in [&stale, &unrelated, &wrong_suffix, &target] {
            age(path);
        }
        let symlink = dir.join(".klms-download-444.part");
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        let folder = dir.join(".klms-download-555.part");
        fs::create_dir(&folder).unwrap();

        let (temporary, out) = (dir.join(".klms-download-999.part"), dir.join("out"));
        publish_new(&temporary, &out, |file| file.write_all(b"new")).unwrap();
        assert!(!stale.exists());
        for kept in [
            &fresh,
            &unrelated,
            &wrong_suffix,
            &target,
            &symlink,
            &folder,
        ] {
            assert!(fs::symlink_metadata(kept).is_ok(), "{}", kept.display());
        }
        assert_eq!(fs::read(&out).unwrap(), b"new");

        // Other kinds of temporaries never trigger a sweep.
        fs::write(&stale, b"x").unwrap();
        age(&stale);
        publish_new(&dir.join("y.part"), &dir.join("y"), |file| {
            file.write_all(b"y")
        })
        .unwrap();
        assert!(stale.exists());
    }
}
