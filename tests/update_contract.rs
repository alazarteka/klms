use std::{fs, path::Path, process::Command};

fn install(home: &Path, destination: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_klms"))
        .args(["--json", "__install", "--destination"])
        .arg(destination)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .output()
        .unwrap()
}

#[cfg(unix)]
fn seed_legacy_skill(home: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let payload_dir = home.join("data/klms/skills/klms");
    fs::create_dir_all(&payload_dir).unwrap();
    fs::write(payload_dir.join("SKILL.md"), b"legacy skill").unwrap();
    let link = home.join(".agents/skills/klms");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&payload_dir, &link).unwrap();
    (payload_dir, link)
}

#[test]
fn clean_install_and_replacement_install_the_binary() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("bin/klms");
    let output = install(temp.path(), &destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let version = Command::new(&destination)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("klms {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(!temp.path().join("data").exists());
    assert!(!temp.path().join(".agents").exists());
    fs::write(&destination, b"old executable").unwrap();
    let output = install(temp.path(), &destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&destination).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_klms")).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn install_removes_the_legacy_managed_skill() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("klms");
    let (payload_dir, link) = seed_legacy_skill(temp.path());
    let output = install(temp.path(), &destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::symlink_metadata(&link).is_err());
    assert!(!payload_dir.exists());
}

#[cfg(unix)]
#[test]
fn install_leaves_user_owned_skill_paths_alone() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("klms");
    let mine = temp.path().join(".agents/skills/klms");
    fs::create_dir_all(&mine).unwrap();
    fs::write(mine.join("SKILL.md"), b"user skill").unwrap();
    let output = install(temp.path(), &destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(mine.join("SKILL.md")).unwrap(), b"user skill");

    // A symlink that points anywhere but klms's own payload directory is also kept.
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::remove_dir_all(&mine).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &mine).unwrap();
    assert!(install(temp.path(), &destination).status.success());
    assert_eq!(fs::read_link(&mine).unwrap(), elsewhere);
}

#[test]
fn install_refuses_a_directory_destination_and_leaves_no_staging_directory() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("target");
    fs::create_dir(&destination).unwrap();
    let output = install(temp.path(), &destination);
    assert_eq!(output.status.code(), Some(40));
    assert!(destination.is_dir());
    assert!(!fs::read_dir(temp.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".klms-update-")
    }));
}

#[cfg(unix)]
#[test]
fn replacement_follows_binary_symlink_without_replacing_link() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("real-klms");
    let link = temp.path().join("klms");
    fs::write(&destination, b"old").unwrap();
    std::os::unix::fs::symlink(&destination, &link).unwrap();
    let output = install(temp.path(), &link);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_link(link).unwrap(), destination);
    assert_eq!(
        fs::read(destination).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_klms")).unwrap()
    );
}
