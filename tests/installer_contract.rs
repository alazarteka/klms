//! Run the actual bootstrap script against a local release fixture, without a network.
#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_klms");
const MOCK_CURL: &str = r#"#!/bin/sh
set -eu
out=''
last=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    *) last="$1"; shift ;;
  esac
done
base="https://github.com/alazarteka/klms/releases"
case "$last" in
  "$base/latest") printf '%s/tag/%s' "$base" "$KLMS_FIXTURE_TAG" ;;
  "$base/download/$KLMS_FIXTURE_TAG/$KLMS_FIXTURE_ARCHIVE")
    cp "$KLMS_FIXTURE_ROOT/$KLMS_FIXTURE_ARCHIVE" "$out" ;;
  "$base/download/$KLMS_FIXTURE_TAG/$KLMS_FIXTURE_ARCHIVE.sha256")
    cp "$KLMS_FIXTURE_ROOT/$KLMS_FIXTURE_ARCHIVE.sha256" "$out" ;;
  *) echo "unexpected installer URL: $last" >&2; exit 91 ;;
esac
"#;

struct InstallFixture {
    root: TempDir,
    destination: PathBuf,
    tag: String,
    archive: String,
}

impl InstallFixture {
    fn new() -> Self {
        let target = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("linux", "x86_64") => "x86_64-unknown-linux-musl",
            other => panic!("installer fixture needs a supported platform: {other:?}"),
        };
        let root = TempDir::new().unwrap();
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let package = format!("klms-{tag}-{target}");
        let archive = format!("{package}.tar.gz");
        fs::create_dir(root.path().join(&package)).unwrap();
        fs::copy(BINARY, root.path().join(&package).join("klms")).unwrap();
        let tar = Command::new("tar")
            .args(["-czf", &archive, &package])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(tar.success());
        let digest: String = Sha256::digest(fs::read(root.path().join(&archive)).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let checksum = root.path().join(format!("{archive}.sha256"));
        fs::write(checksum, format!("{digest}  {archive}\n")).unwrap();
        let curl = root.path().join("mock-bin/curl");
        fs::create_dir(curl.parent().unwrap()).unwrap();
        fs::write(&curl, MOCK_CURL).unwrap();
        fs::set_permissions(curl, fs::Permissions::from_mode(0o755)).unwrap();
        let destination = root.path().join("custom binary directory/klms");
        Self {
            root,
            destination,
            tag,
            archive,
        }
    }

    fn run(&self) -> std::process::Output {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mock = self.root.path().join("mock-bin");
        let paths = std::iter::once(mock).chain(std::env::split_paths(&path));
        Command::new("bash")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh"))
            .current_dir(self.root.path())
            .env("PATH", std::env::join_paths(paths).unwrap())
            // These are the test process's application directories, not real user state.
            .env("HOME", self.root.path().join("test-home"))
            .env("XDG_DATA_HOME", self.root.path().join("test-data"))
            .env("XDG_STATE_HOME", self.root.path().join("test-state"))
            .env("KLMS_INSTALL_DIR", self.destination.parent().unwrap())
            .env("KLMS_FIXTURE_ROOT", self.root.path())
            .env("KLMS_FIXTURE_TAG", &self.tag)
            .env("KLMS_FIXTURE_ARCHIVE", &self.archive)
            .output()
            .unwrap()
    }

    fn run_ok(&self) {
        let result = self.run();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn bootstrap_installs_and_replaces_binary() {
    let fixture = InstallFixture::new();
    fixture.run_ok();
    let version = Command::new(&fixture.destination)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    let expected = format!("klms {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(String::from_utf8(version.stdout).unwrap().trim(), expected);
    fs::write(&fixture.destination, b"old executable bytes").unwrap();
    fixture.run_ok();
    assert_eq!(
        fs::read(&fixture.destination).unwrap(),
        fs::read(BINARY).unwrap()
    );
}

#[test]
fn bootstrap_checksum_failure_preserves_existing_install() {
    let fixture = InstallFixture::new();
    fs::create_dir_all(fixture.destination.parent().unwrap()).unwrap();
    fs::write(&fixture.destination, b"old executable").unwrap();
    let checksum = fixture
        .root
        .path()
        .join(format!("{}.sha256", fixture.archive));
    fs::write(
        checksum,
        format!("{}  {}\n", "0".repeat(64), fixture.archive),
    )
    .unwrap();
    let result = fixture.run();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("checksum"));
    assert_eq!(fs::read(&fixture.destination).unwrap(), b"old executable");
}
