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
fn clean_install_and_replacement_install_matching_skill() {
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
    let payload = temp.path().join("data/klms/skills/klms/SKILL.md");
    assert_eq!(
        fs::read(&payload).unwrap(),
        include_bytes!("../skills/klms/SKILL.md")
    );
    assert_eq!(
        fs::read_link(temp.path().join(".agents/skills/klms")).unwrap(),
        payload.parent().unwrap()
    );
    fs::write(&destination, b"old executable").unwrap();
    fs::write(&payload, b"old skill").unwrap();
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
    assert_eq!(
        fs::read(payload).unwrap(),
        include_bytes!("../skills/klms/SKILL.md")
    );
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

// Compile the private transport harness only in this integration test. The
// shipped CLI has neither source overrides nor an install-destination override
// on its public update command.
#[allow(dead_code)]
#[path = "../src/client.rs"]
mod client;
#[allow(dead_code)]
#[path = "../src/error.rs"]
mod error;
#[allow(dead_code)]
#[path = "../src/output.rs"]
mod output;
#[allow(dead_code)]
#[path = "../src/skill.rs"]
mod skill;
#[allow(dead_code)]
#[path = "../src/update.rs"]
mod update;

struct ReleaseServer {
    origin: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl ReleaseServer {
    fn new(archive: Vec<u8>, checksum_ok: bool, status: u16) -> Self {
        use sha2::{Digest, Sha256};
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 8192];
                let n = socket.read(&mut request).unwrap();
                let text = String::from_utf8_lossy(&request[..n]);
                let path = text.split_whitespace().nth(1).unwrap().to_owned();
                recorded.lock().unwrap().push(path.clone());
                let body = if path == "/latest" {
                    format!(
                        r#"{{"tag_name":"v{}","draft":false,"prerelease":false}}"#,
                        env!("CARGO_PKG_VERSION")
                    )
                    .into_bytes()
                } else if path.ends_with(".sha256") {
                    let bytes = if checksum_ok {
                        archive.as_slice()
                    } else {
                        b"wrong"
                    };
                    let digest: String = Sha256::digest(bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    format!(
                        "{digest}  {}\n",
                        path.rsplit('/').next().unwrap().trim_end_matches(".sha256")
                    )
                    .into_bytes()
                } else {
                    archive.clone()
                };
                let code = if path.ends_with(".tar.gz") {
                    status
                } else {
                    200
                };
                write!(
                    socket,
                    "HTTP/1.1 {code} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                socket.write_all(&body).unwrap();
                if recorded.lock().unwrap().len() >= 3 {
                    break;
                }
            }
        });
        Self { origin, requests }
    }
}

fn release_archive(root: &Path) -> Vec<u8> {
    let target = if cfg!(target_os = "macos") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-musl"
    };
    let package = format!("klms-v{}-{target}", env!("CARGO_PKG_VERSION"));
    fs::create_dir(root.join(&package)).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_klms"), root.join(&package).join("klms")).unwrap();
    let output = Command::new("tar")
        .arg("-czf")
        .arg("-")
        .arg("-C")
        .arg(root)
        .arg(package)
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

#[test]
fn updater_checks_without_download_and_never_downgrades() {
    for (current, check, available) in [
        ("0.2.0", true, true),
        (env!("CARGO_PKG_VERSION"), false, false),
        ("99.0.0", false, false),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("klms");
        fs::write(&destination, b"old executable").unwrap();
        let server = ReleaseServer::new(vec![], true, 200);
        let result =
            update::test_update(&server.origin, current, check, &destination, temp.path()).unwrap();
        assert_eq!(result.data["update_available"], available);
        assert_eq!(result.data["updated"], false);
        assert_eq!(server.requests.lock().unwrap().as_slice(), ["/latest"]);
        assert_eq!(fs::read(destination).unwrap(), b"old executable");
        assert!(!temp.path().join("data").exists());
    }
}

#[test]
fn updater_installs_verified_release_and_preserves_old_install_on_failures() {
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = release_archive(archive_dir.path());
    for (checksum_ok, status, conflict) in [
        (true, 200, false),
        (false, 200, false),
        (true, 404, false),
        (true, 200, true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("klms");
        fs::write(&destination, b"old executable").unwrap();
        if conflict {
            fs::create_dir_all(temp.path().join(".agents/skills/klms")).unwrap();
        }
        let server = ReleaseServer::new(archive.clone(), checksum_ok, status);
        let result = update::test_update(&server.origin, "0.2.0", false, &destination, temp.path());
        if checksum_ok && status == 200 && !conflict {
            assert_eq!(result.unwrap().data["updated"], true);
            assert_eq!(
                fs::read(&destination).unwrap(),
                fs::read(env!("CARGO_BIN_EXE_klms")).unwrap()
            );
            assert_eq!(
                fs::read(temp.path().join("data/klms/skills/klms/SKILL.md")).unwrap(),
                include_bytes!("../skills/klms/SKILL.md")
            );
        } else {
            let error = result.err().expect("expected update failure");
            if conflict {
                assert!(
                    error.message.contains("refusing to replace"),
                    "{}",
                    error.message
                );
            }
            assert_eq!(fs::read(&destination).unwrap(), b"old executable");
            assert!(!temp.path().join("data/klms/skills/klms/SKILL.md").exists());
        }
    }
}
