//! Run the actual bootstrap script against a local release fixture, without a network.
#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

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
        fs::copy(
            env!("CARGO_BIN_EXE_klms"),
            root.path().join(&package).join("klms"),
        )
        .unwrap();
        assert!(
            Command::new("tar")
                .args(["-czf", &archive, &package])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        let digest = Sha256::digest(fs::read(root.path().join(&archive)).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        fs::write(
            root.path().join(format!("{archive}.sha256")),
            format!("{digest}  {archive}\n"),
        )
        .unwrap();
        let mock_bin = root.path().join("mock-bin");
        fs::create_dir(&mock_bin).unwrap();
        let curl = mock_bin.join("curl");
        fs::write(
            &curl,
            r#"#!/bin/sh
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
"#,
        )
        .unwrap();
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
        let path = std::env::join_paths(std::iter::once(self.root.path().join("mock-bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        Command::new("bash")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh"))
            .current_dir(self.root.path())
            .env("PATH", path)
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

    fn payload(&self) -> PathBuf {
        self.root.path().join("test-data/klms/skills/klms/SKILL.md")
    }
}

#[test]
fn bootstrap_installs_and_replaces_binary_with_matching_skill() {
    let fixture = InstallFixture::new();
    let first = fixture.run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let version = Command::new(&fixture.destination)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        format!("klms {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        fs::read(fixture.payload()).unwrap(),
        include_bytes!("../skills/klms/SKILL.md")
    );
    assert_eq!(
        fs::read_link(fixture.root.path().join("test-home/.agents/skills/klms")).unwrap(),
        fixture.payload().parent().unwrap()
    );

    fs::write(&fixture.destination, b"old executable bytes").unwrap();
    fs::write(fixture.payload(), b"old embedded skill").unwrap();
    let second = fixture.run();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        fs::read(&fixture.destination).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_klms")).unwrap()
    );
    assert_eq!(
        fs::read(fixture.payload()).unwrap(),
        include_bytes!("../skills/klms/SKILL.md")
    );
}

#[test]
fn bootstrap_checksum_failure_preserves_existing_install() {
    let fixture = InstallFixture::new();
    fs::create_dir_all(fixture.destination.parent().unwrap()).unwrap();
    fs::write(&fixture.destination, b"old executable").unwrap();
    fs::write(
        fixture
            .root
            .path()
            .join(format!("{}.sha256", fixture.archive)),
        format!("{}  {}\n", "0".repeat(64), fixture.archive),
    )
    .unwrap();
    let result = fixture.run();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("checksum"));
    assert_eq!(fs::read(&fixture.destination).unwrap(), b"old executable");
    assert!(!fixture.payload().exists());
}

#[test]
fn bootstrap_skill_conflict_preserves_binary_and_skill() {
    let fixture = InstallFixture::new();
    fs::create_dir_all(fixture.destination.parent().unwrap()).unwrap();
    fs::write(&fixture.destination, b"old executable").unwrap();
    let conflict = fixture.root.path().join("test-home/.agents/skills/klms");
    fs::create_dir_all(&conflict).unwrap();
    fs::write(conflict.join("SKILL.md"), b"user-managed skill").unwrap();
    let result = fixture.run();
    assert!(!result.status.success());
    assert_eq!(fs::read(&fixture.destination).unwrap(), b"old executable");
    assert_eq!(
        fs::read(conflict.join("SKILL.md")).unwrap(),
        b"user-managed skill"
    );
    assert!(!fixture.payload().exists());
}
