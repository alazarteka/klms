use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const BINARY: &str = env!("CARGO_BIN_EXE_klms");

fn install(home: &Path, destination: &Path) -> Output {
    Command::new(BINARY)
        .args(["--json", "__install", "--destination"])
        .arg(destination)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .output()
        .unwrap()
}

fn assert_installed(home: &Path, destination: &Path) {
    let output = install(home, destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(destination).unwrap(), fs::read(BINARY).unwrap());
}

#[test]
fn clean_install_replaces_and_follows_binary_symlinks_without_replacing_the_link() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("bin/klms");
    assert_installed(temp.path(), &destination);
    let version = Command::new(&destination)
        .arg("--version")
        .output()
        .unwrap();
    let expected = format!("klms {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), expected);
    assert!(!temp.path().join("data").exists() && !temp.path().join(".agents").exists());
    fs::write(&destination, b"old executable").unwrap();
    assert_installed(temp.path(), &destination);

    #[cfg(unix)]
    {
        let (real, link) = (temp.path().join("real-klms"), temp.path().join("link-klms"));
        fs::write(&real, b"old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_installed(temp.path(), &link);
        assert_eq!(fs::read_link(&link).unwrap(), real);
        assert_eq!(fs::read(real).unwrap(), fs::read(BINARY).unwrap());
    }
}

#[cfg(unix)]
#[test]
fn install_removes_only_the_legacy_managed_skill() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("klms");
    let payload_dir = temp.path().join("data/klms/skills/klms");
    let link = temp.path().join(".agents/skills/klms");
    fs::create_dir_all(&payload_dir).unwrap();
    fs::write(payload_dir.join("SKILL.md"), b"legacy skill").unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&payload_dir, &link).unwrap();
    assert_installed(temp.path(), &destination);
    assert!(fs::symlink_metadata(&link).is_err() && !payload_dir.exists());

    // A user-owned directory, or a symlink to anywhere else, is kept.
    fs::create_dir_all(&link).unwrap();
    fs::write(link.join("SKILL.md"), b"user skill").unwrap();
    assert_installed(temp.path(), &destination);
    assert_eq!(fs::read(link.join("SKILL.md")).unwrap(), b"user skill");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::remove_dir_all(&link).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    assert_installed(temp.path(), &destination);
    assert_eq!(fs::read_link(&link).unwrap(), elsewhere);
}

#[test]
fn install_refuses_a_directory_destination_and_leaves_no_staging_directory() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("target");
    fs::create_dir(&destination).unwrap();
    assert_eq!(install(temp.path(), &destination).status.code(), Some(40));
    assert!(destination.is_dir());
    let staged = fs::read_dir(temp.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".klms-update-")
    });
    assert!(!staged);
}
