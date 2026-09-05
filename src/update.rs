//! Explicit, unauthenticated release updates. Never invoked during startup.
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{client::release_bytes as fetch, error::AppError, output, skill};

const REPO: &str = "alazarteka/klms";
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
const MAX_BINARY: u64 = 128 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

pub fn run(check: bool, timeout: u64) -> Result<output::CommandResult, AppError> {
    let client = crate::client::release_client(timeout)?;
    run_with_source(
        &client,
        check,
        &ReleaseSource {
            metadata_url: format!("https://api.github.com/repos/{REPO}/releases/latest"),
            downloads_url: format!("https://github.com/{REPO}/releases/download"),
            current_version: env!("CARGO_PKG_VERSION").into(),
            destination: env::current_exe().and_then(fs::canonicalize).map_err(io)?,
            #[cfg(test)]
            home: None,
        },
    )
}

struct ReleaseSource {
    metadata_url: String,
    downloads_url: String,
    current_version: String,
    destination: PathBuf,
    #[cfg(test)]
    home: Option<PathBuf>,
}

fn run_with_source(
    client: &Client,
    check: bool,
    source: &ReleaseSource,
) -> Result<output::CommandResult, AppError> {
    let metadata = fetch(client, &source.metadata_url, 1024 * 1024)?;
    let release: Release = serde_json::from_slice(&metadata)
        .map_err(|e| AppError::upstream(format!("invalid release metadata: {e}")))?;
    if release.draft || release.prerelease {
        return Err(AppError::upstream(
            "latest release is not a stable published release",
        ));
    }
    let latest = version(&release.tag_name)?;
    let current = version(&source.current_version)?;
    let available = latest > current;
    if check || !available {
        return output::result(
            "update",
            &json!({
                "current_version": source.current_version,
                "latest_version": release.tag_name.trim_start_matches('v'),
                "update_available": available,
                "updated": false,
                "path": source.destination,
            }),
            if available {
                format!(
                    "{} is available (installed: {}). Run `klms update`.",
                    release.tag_name, source.current_version
                )
            } else {
                format!(
                    "klms {} is current; latest stable release is {}.",
                    source.current_version, release.tag_name
                )
            },
        );
    }
    let target = platform()?;
    let package = format!("klms-{}-{target}", release.tag_name);
    let archive_name = format!("{package}.tar.gz");
    let base = format!("{}/{}", source.downloads_url, release.tag_name);
    let checksum = fetch(client, &format!("{base}/{archive_name}.sha256"), 4096)?;
    let archive = fetch(client, &format!("{base}/{archive_name}"), MAX_ARCHIVE)?;
    verify_checksum(&archive, &checksum, &archive_name)?;
    let work = WorkDir::new(&env::temp_dir())?;
    let archive_path = work.0.join(&archive_name);
    fs::write(&archive_path, archive).map_err(io)?;
    let candidate = work.0.join("klms");
    extract_binary(&archive_path, &format!("{package}/klms"), &candidate)?;
    let actual = Command::new(&candidate)
        .arg("--version")
        .output()
        .map_err(io)?;
    if !actual.status.success()
        || String::from_utf8_lossy(&actual.stdout).trim()
            != format!("klms {}.{}.{}", latest.0, latest.1, latest.2)
    {
        return Err(AppError::upstream(
            "downloaded binary version does not match release",
        ));
    }
    let destination = &source.destination;
    let mut command = Command::new(&candidate);
    command
        .args(["--json", "__install", "--destination"])
        .arg(destination);
    #[cfg(test)]
    if let Some(home) = &source.home {
        command
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join("data"));
    }
    let installed = command.output().map_err(io)?;
    if !installed.status.success() {
        let diagnostic = serde_json::from_slice::<serde_json::Value>(&installed.stdout)
            .or_else(|_| serde_json::from_slice(&installed.stderr))
            .ok();
        let message = diagnostic
            .as_ref()
            .and_then(|v| v["error"]["message"].as_str())
            .unwrap_or("candidate installation failed");
        return Err(AppError::config(message).with_details(json!({"candidate_error": diagnostic})));
    }
    output::result(
        "update",
        &json!({
            "current_version": source.current_version,
            "latest_version": release.tag_name.trim_start_matches('v'),
            "update_available": false,
            "updated": true,
            "path": destination,
        }),
        format!(
            "Updated to {} at {}",
            release.tag_name,
            destination.display()
        ),
    )
}

/// Called by the verified candidate itself, so its embedded skill matches it.
/// Stage a copy on the destination filesystem, install the skill with rollback,
/// then atomically rename the binary. No fallible operation follows the switch.
pub fn install(destination: &Path) -> Result<output::CommandResult, AppError> {
    if !destination.is_absolute() {
        return Err(AppError::usage("installation destination must be absolute"));
    }
    let destination = match fs::symlink_metadata(destination) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(destination).map_err(io)?,
        Ok(m) if m.is_file() => destination.to_path_buf(),
        Ok(_) => {
            return Err(AppError::config(
                "installation destination is not a regular file",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => destination.to_path_buf(),
        Err(e) => return Err(io(e)),
    };
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::usage("missing install directory"))?;
    fs::create_dir_all(parent).map_err(io)?;
    let work = WorkDir::new(parent)?;
    let staged = work.0.join("klms");
    fs::copy(env::current_exe().map_err(io)?, &staged).map_err(io)?;
    executable(&staged)?;
    fs::File::open(&staged)
        .and_then(|f| f.sync_all())
        .map_err(io)?;
    // Serialize the result before the commit, too: reporting is not part of
    // rollback and should not turn a successful switch into a reported failure.
    let result = output::result(
        "update.install",
        &json!({
            "version": env!("CARGO_PKG_VERSION"), "path": destination,
        }),
        format!(
            "Installed klms {} at {}",
            env!("CARGO_PKG_VERSION"),
            destination.display()
        ),
    )?;
    skill::with_install(|| fs::rename(&staged, &destination).map_err(io))?;
    Ok(result)
}

fn version(value: &str) -> Result<(u64, u64, u64), AppError> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
        })
    {
        return Err(AppError::upstream(
            "release version must be stable major.minor.patch",
        ));
    }
    let parse = |p: &str| {
        p.parse()
            .map_err(|_| AppError::upstream("release version is too large"))
    };
    Ok((parse(parts[0])?, parse(parts[1])?, parse(parts[2])?))
}

fn platform() -> Result<&'static str, AppError> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        _ => Err(AppError::config("no prebuilt release for this platform")),
    }
}

fn verify_checksum(bytes: &[u8], checksum: &[u8], name: &str) -> Result<(), AppError> {
    let text =
        std::str::from_utf8(checksum).map_err(|_| AppError::upstream("invalid checksum file"))?;
    let fields: Vec<_> = text.split_whitespace().collect();
    let digest = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if fields.len() != 2 || fields[0] != digest || fields[1].trim_start_matches('*') != name {
        return Err(AppError::upstream(
            "release archive checksum verification failed",
        ));
    }
    Ok(())
}

fn extract_binary(archive: &Path, member: &str, candidate: &Path) -> Result<(), AppError> {
    // Extract only the fixed executable to stdout, never materialize archive paths.
    let mut child = Command::new("tar")
        .arg("-xzOf")
        .arg(archive)
        .arg(member)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io)?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped stdout")
        .take(MAX_BINARY + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > MAX_BINARY {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AppError::upstream(
            "could not extract bounded release binary",
        ));
    }
    if !child.wait().map_err(io)?.success() || bytes.is_empty() {
        return Err(AppError::upstream(
            "release archive is missing its executable",
        ));
    }
    fs::write(candidate, bytes).map_err(io)?;
    executable(candidate)
}

fn executable(path: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(io)?;
    }
    Ok(())
}

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(parent: &Path) -> Result<Self, AppError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| AppError::internal(e.to_string()))?
            .as_nanos();
        let path = parent.join(format!(".klms-update-{}-{nonce}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(io)?;
        Ok(Self(path))
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn io(error: std::io::Error) -> AppError {
    AppError::config(format!("release update failed: {error}"))
}

#[cfg(test)]
#[allow(dead_code)] // Called by the integration harness that includes this module.
pub(crate) fn test_update(
    origin: &str,
    current: &str,
    check: bool,
    destination: &Path,
    home: &Path,
) -> Result<output::CommandResult, AppError> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    run_with_source(
        &client,
        check,
        &ReleaseSource {
            metadata_url: format!("{origin}/latest"),
            downloads_url: origin.to_owned(),
            current_version: current.to_owned(),
            destination: destination.to_owned(),
            home: Some(home.to_owned()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_versions_and_no_downgrade_comparison() {
        assert!(version("v0.2.1").unwrap() > version("0.2.0").unwrap());
        assert!(version("0.10.0").unwrap() > version("0.9.99").unwrap());
        for bad in ["v1", "1.2.3-rc1", "01.2.3", "1.2.3/evil", "1.2.3.4"] {
            assert!(version(bad).is_err());
        }
    }
    #[test]
    fn checksum_requires_exact_archive_and_bytes() {
        let checksum = format!(
            "{}  release.tar.gz\n",
            Sha256::digest(b"archive")
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert!(verify_checksum(b"archive", checksum.as_bytes(), "release.tar.gz").is_ok());
        assert!(verify_checksum(b"partial", checksum.as_bytes(), "release.tar.gz").is_err());
        assert!(verify_checksum(b"archive", checksum.as_bytes(), "other.tar.gz").is_err());
    }
}
