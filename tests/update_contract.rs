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

#[test]
fn skill_conflict_preserves_previous_executable_and_payload() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("klms");
    fs::write(&destination, b"old executable").unwrap();
    let conflict = temp.path().join(".agents/skills/klms");
    fs::create_dir_all(&conflict).unwrap();
    fs::write(conflict.join("mine"), b"user skill").unwrap();
    let payload = temp.path().join("data/klms/skills/klms/SKILL.md");
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    fs::write(&payload, b"old payload").unwrap();
    let output = install(temp.path(), &destination);
    assert!(!output.status.success());
    assert_eq!(fs::read(&destination).unwrap(), b"old executable");
    assert_eq!(fs::read(payload).unwrap(), b"old payload");
    assert_eq!(fs::read(conflict.join("mine")).unwrap(), b"user skill");
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
