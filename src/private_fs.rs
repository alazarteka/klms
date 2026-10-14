//! Private (owner-only) files and their no-overwrite atomic publication.
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn xdg_home(name: &str, home: Option<&Path>, fallback: &str) -> Option<PathBuf> {
    env_path(name).or_else(|| home.map(|home| home.join(fallback)))
}

/// Write-only options that create files with mode 0600 on Unix.
pub(crate) fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

#[derive(Debug)]
pub(crate) enum PublishError<E> {
    Create(io::Error),
    Fill(E),
    Sync(io::Error),
    Link(io::Error),
}

pub(crate) struct Linked {
    /// The temporary this call created (`temporary`, or a suffixed variant
    /// when that name was already taken).
    pub(crate) temporary: PathBuf,
    /// `destination` was already there and was left untouched.
    pub(crate) existed: bool,
    /// A non-atomic exclusive copy was used because hard linking failed.
    pub(crate) copied: bool,
    /// Removing `temporary` failed; the content is published regardless.
    pub(crate) leftover: Option<io::Error>,
}

const TEMPORARY_ATTEMPTS: u32 = 8;

/// Publish atomically without replacing an existing destination. Hard-link
/// failures are errors; partially visible objects are never accepted.
pub(crate) fn publish_atomic<T, E>(
    temporary: &Path,
    destination: &Path,
    fill: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<(T, Linked), PublishError<E>> {
    publish_new_with(
        Publication::Atomic,
        |from, to| fs::hard_link(from, to),
        temporary,
        destination,
        fill,
    )
}

/// Publish a user download, falling back to an exclusive copy when hard links
/// fail. `Linked::copied` reports that the destination was not published atomically.
pub(crate) fn publish_download<T, E>(
    temporary: &Path,
    destination: &Path,
    fill: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<(T, Linked), PublishError<E>> {
    publish_new_with(
        Publication::Download,
        |from, to| fs::hard_link(from, to),
        temporary,
        destination,
        fill,
    )
}

#[derive(Clone, Copy)]
enum Publication {
    Atomic,
    Download,
}

fn publish_new_with<T, E>(
    mode: Publication,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
    temporary: &Path,
    destination: &Path,
    fill: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<(T, Linked), PublishError<E>> {
    let (mut file, temporary) = create_exclusive(temporary)?;
    let temporary = temporary.as_path();
    if matches!(mode, Publication::Download) {
        if let Ok(own) = file.metadata() {
            sweep_stale(out_parent(destination), &own);
        }
    }
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
    let mut copied = false;
    let existed = match link(temporary, destination) {
        Ok(()) => false,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => true,
        Err(_) if matches!(mode, Publication::Download) => match copy_new(temporary, destination) {
            Ok(existed) => {
                copied = !existed;
                existed
            }
            Err(error) => {
                let _ = fs::remove_file(temporary);
                return Err(PublishError::Link(error));
            }
        },
        Err(error) => {
            let _ = fs::remove_file(temporary);
            return Err(PublishError::Link(error));
        }
    };
    let leftover = fs::remove_file(temporary).err();
    let linked = Linked {
        temporary: temporary.to_path_buf(),
        existed,
        copied,
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

const TEMP_PREFIX: &str = ".klms-download-";
const STALE_AFTER: Duration = Duration::from_secs(48 * 60 * 60);

pub(crate) fn temporary_name(suffix: &str) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
        + u128::from(SEQUENCE.fetch_add(1, Ordering::Relaxed));
    format!("{TEMP_PREFIX}{}-{nonce:x}.{suffix}", std::process::id())
}

/// Recognize generated scratch and part names, including exclusive-create retries.
fn temporary_kind(name: &str) -> Option<&str> {
    let (pid, rest) = name.strip_prefix(TEMP_PREFIX)?.split_once('-')?;
    if pid.is_empty() || !pid.bytes().all(|c| c.is_ascii_digit()) || pid.parse::<u32>().is_err() {
        return None;
    }
    let (nonce, suffix) = rest.split_once('.')?;
    if nonce.is_empty() || !nonce.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if let Some(retry) = suffix.strip_prefix("part.") {
        return (!retry.is_empty() && retry.bytes().all(|c| c.is_ascii_hexdigit()))
            .then_some("part");
    }
    matches!(suffix, "scratch" | "part").then_some(suffix)
}

/// Forty-eight hours exceeds the CLI's maximum 24-hour download deadline.
fn sweep_stale(parent: &Path, own: &Metadata) {
    #[cfg(not(unix))]
    let _ = own;
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(kind) = name.to_str().and_then(temporary_kind) else {
            continue;
        };
        let path = entry.path();
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != own.uid() {
                continue;
            }
        }
        if meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_none_or(|age| age <= STALE_AFTER)
        {
            continue;
        }
        if meta.file_type().is_file() && kind == "part" {
            let _ = fs::remove_file(&path);
        } else if meta.file_type().is_dir() && kind == "scratch" {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// A private, exclusively created directory removed when its owner is dropped.
pub(crate) struct ScratchDir(pub(crate) PathBuf);
impl ScratchDir {
    pub(crate) fn create(parent: &Path) -> Result<Self, (PathBuf, io::Error)> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        for attempt in 0..TEMPORARY_ATTEMPTS {
            let path = parent.join(temporary_name("scratch"));
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e)
                    if e.kind() == ErrorKind::AlreadyExists && attempt + 1 < TEMPORARY_ATTEMPTS => {
                }
                Err(e) => return Err((path, e)),
            }
        }
        unreachable!("at least one creation attempt")
    }
}
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn out_parent(out: &Path) -> &Path {
    out.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
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
        let (_, linked) = publish_atomic(&temporary, &out, write(b"first")).unwrap();
        assert!(!linked.existed && linked.leftover.is_none());
        let (_, linked) = publish_atomic(&temporary, &out, write(b"second")).unwrap();
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
        let failed = publish_atomic(&temporary, &out, |_| Err::<(), _>("no"));
        assert!(matches!(failed, Err(PublishError::Fill("no"))));
        assert!(!temporary.exists() && !out.exists());
        fs::write(&temporary, b"stale").unwrap();
        let (_, linked) = publish_atomic(&temporary, &out, |file| file.write_all(b"new")).unwrap();
        assert_ne!(linked.temporary, temporary);
        assert!(!linked.temporary.exists() && !linked.existed);
        assert_eq!(fs::read(&temporary).unwrap(), b"stale");
        assert_eq!(fs::read(&out).unwrap(), b"new");
    }

    fn no_links(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(ErrorKind::Unsupported))
    }

    #[test]
    fn atomic_publication_rejects_unsupported_hard_links() {
        let dir = tempfile::tempdir().unwrap();
        let temporary = dir.path().join("x.part");
        let out = dir.path().join("x");
        let result = publish_new_with(Publication::Atomic, no_links, &temporary, &out, |file| {
            file.write_all(b"object")
        });
        assert!(matches!(result, Err(PublishError::Link(e)) if e.kind() == ErrorKind::Unsupported));
        assert!(!temporary.exists() && !out.exists());
    }

    #[test]
    fn without_hard_links_the_temporary_is_copied_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let (temporary, out) = (directory.path().join("x.part"), directory.path().join("x"));
        let write = |bytes: &'static [u8]| move |file: &mut File| file.write_all(bytes);
        let (_, linked) = publish_new_with(
            Publication::Download,
            no_links,
            &temporary,
            &out,
            write(b"first"),
        )
        .unwrap();
        assert!(!linked.existed && linked.copied && linked.leftover.is_none());
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
        let (_, linked) = publish_new_with(
            Publication::Download,
            no_links,
            &temporary,
            &out,
            write(b"second"),
        )
        .unwrap();
        assert!(linked.existed);
        assert_eq!(fs::read(&out).unwrap(), b"first");
        assert!(!temporary.exists());
    }

    #[test]
    fn a_failed_fallback_leaves_neither_destination_nor_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("x.part");
        let out = directory.path().join("missing/x");
        let result = publish_new_with(Publication::Download, no_links, &temporary, &out, |file| {
            file.write_all(b"data")
        });
        let Err(PublishError::Link(error)) = result else {
            panic!("expected a link error");
        };
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(!temporary.exists() && !out.exists());
    }

    #[test]
    fn temporary_names_match_only_generated_entries() {
        for (name, expected) in [
            (".klms-download-42-ab.part", Some("part")),
            (".klms-download-42-ab.part.1a2", Some("part")),
            (".klms-download-42-ab.scratch", Some("scratch")),
            (".klms-download-project.part-notes", None),
            (".klms-download-42.scratch", None),
            (".klms-download--ab.part", None),
            (".klms-download-4294967296-ab.part", None),
            (".klms-download-42-.part", None),
            (".klms-download-42-xyz.part", None),
            (".klms-download-42-ab.part-notes", None),
            (".klms-download-42-ab.part.", None),
            (".klms-download-42-ab.part.1z", None),
            (".klms-download-42-ab.scratch.1a2", None),
        ] {
            assert_eq!(temporary_kind(name), expected, "{name}");
        }
        for kind in ["scratch", "part"] {
            assert_eq!(temporary_kind(&temporary_name(kind)), Some(kind));
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspaces_are_unique_and_sweep_keeps_notes_and_active_work() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("video.mp4");
        let legacy = dir
            .path()
            .join(format!(".klms-download-{}.scratch", std::process::id()));
        std::fs::create_dir(&legacy).unwrap();
        let notes = dir.path().join(".klms-download-project.part-notes");
        std::fs::write(&notes, b"user notes").unwrap();
        let old = SystemTime::now() - STALE_AFTER - std::time::Duration::from_secs(1);
        std::fs::File::open(&notes)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let first = ScratchDir::create(out_parent(&out)).unwrap();
        let second = ScratchDir::create(out_parent(&out)).unwrap();
        assert_ne!(first.0, second.0);
        assert!(first.0.exists() && second.0.exists() && legacy.exists());
        assert_eq!(std::fs::read(&notes).unwrap(), b"user notes");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&first.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let (first_path, second_path) = (first.0.clone(), second.0.clone());
        drop(first);
        drop(second);
        assert!(!first_path.exists() && !second_path.exists());
    }
}
