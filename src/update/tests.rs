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
    let checksum = format!("{}  release.tar.gz\n", sha256_hex(b"archive"));
    assert!(verify_checksum(b"archive", checksum.as_bytes(), "release.tar.gz").is_ok());
    assert!(verify_checksum(b"partial", checksum.as_bytes(), "release.tar.gz").is_err());
    assert!(verify_checksum(b"archive", checksum.as_bytes(), "other.tar.gz").is_err());
}

use crate::fixture_server::{Response, Server};

fn release_server(archive: Vec<u8>, checksum_ok: bool, status: &'static str) -> Server {
    Server::new(move |request| {
        if request.target == "/latest" {
            Response::bytes(
                "application/json",
                format!(
                    r#"{{"tag_name":"v{}","draft":false,"prerelease":false}}"#,
                    env!("CARGO_PKG_VERSION")
                ),
            )
        } else if request.target.ends_with(".sha256") {
            let digest = sha256_hex(if checksum_ok { &archive } else { b"wrong" });
            let name = request
                .target
                .rsplit('/')
                .next()
                .unwrap()
                .trim_end_matches(".sha256");
            Response::bytes("text/plain", format!("{digest}  {name}\n"))
        } else {
            Response::bytes("application/gzip", archive.clone()).status(status)
        }
    })
}

fn test_update(
    server: &Server,
    current: &str,
    check: bool,
    home: &Path,
) -> Result<output::CommandResult, AppError> {
    run_with_source(
        &crate::client::release_client(5).unwrap(),
        check,
        &ReleaseSource {
            metadata_url: format!("{}/latest", server.url()),
            downloads_url: server.url(),
            current_version: current.into(),
            destination: home.join("klms"),
            legacy_skill: legacy_skill_paths_at(home, &home.join("data")),
        },
    )
}

#[test]
fn check_and_no_downgrade_only_fetch_metadata() {
    for (current, check, available) in [
        ("0.2.0", true, true),
        (env!("CARGO_PKG_VERSION"), false, false),
        ("99.0.0", false, false),
    ] {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("klms"), b"old executable").unwrap();
        let server = release_server(vec![], true, "200 OK");
        let result = test_update(&server, current, check, home.path()).unwrap();
        assert_eq!(result.data["update_available"], available);
        assert_eq!(result.data["updated"], false);
        assert_eq!(server.requests(), ["GET /latest HTTP/1.1"]);
        assert_eq!(
            fs::read(home.path().join("klms")).unwrap(),
            b"old executable"
        );
        let entries: Vec<_> = fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["klms"], "nothing is created or installed");
    }
}

// This candidate records the subprocess protocol; actual binary installation
// is exercised by tests/update_contract.rs using Cargo's real CLI executable.
fn protocol_archive(root: &Path, args_file: &Path, candidate_version: &str, fail: bool) -> Vec<u8> {
    let package = format!(
        "klms-v{}-{}",
        env!("CARGO_PKG_VERSION"),
        platform().unwrap()
    );
    fs::create_dir(root.join(&package)).unwrap();
    let candidate = format!(
        r#"#!/bin/sh
if [ "$1" = '--version' ]; then
    printf 'klms {candidate_version}\n'
    exit 0
fi
printf '%s\n' "$@" > '{args_file}'
if [ '{fail}' = 'true' ]; then
    printf '%s\n' '{{"error":{{"message":"fixture install failure"}}}}' >&2
    exit 1
fi
exit 0
"#,
        args_file = args_file.display()
    );
    crate::fixture_executable::write_executable(&root.join(&package).join("klms"), &candidate);
    let output = Command::new("tar")
        .args(["-czf", "-", "-C"])
        .arg(root)
        .arg(package)
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

#[cfg(unix)]
fn install_legacy_skill(home: &Path) -> (PathBuf, PathBuf) {
    let payload_dir = home.join("data/klms/skills/klms");
    let link = home.join(".agents/skills/klms");
    fs::create_dir_all(&payload_dir).unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    fs::write(payload_dir.join("SKILL.md"), b"legacy").unwrap();
    std::os::unix::fs::symlink(&payload_dir, &link).unwrap();
    (payload_dir, link)
}

#[cfg(unix)]
#[test]
fn legacy_skill_is_reported_but_never_removed() {
    let root = tempfile::tempdir().unwrap();
    let (home, data) = (root.path().join("home"), root.path().join("home/data"));
    assert!(legacy_skill_warning(&legacy_skill_paths_at(&home, &data)).is_none());
    let (payload_dir, link) = install_legacy_skill(&home);
    let paths = legacy_skill_paths_at(&home, &data);
    assert_eq!(paths, [link.clone(), payload_dir.clone()]);
    let warning = legacy_skill_warning(&paths).unwrap();
    assert!(warning.contains("deprecated") && warning.contains("`klms --help`"));
    assert!(warning.contains(&link.display().to_string()));
    assert!(warning.contains(&payload_dir.display().to_string()));
    assert_eq!(fs::read_link(&link).unwrap(), payload_dir);
    assert_eq!(fs::read(payload_dir.join("SKILL.md")).unwrap(), b"legacy");

    // A symlink elsewhere or a real directory at the link path is the user's.
    fs::remove_file(&link).unwrap();
    fs::remove_dir_all(data.join("klms")).unwrap();
    fs::create_dir(&link).unwrap();
    assert!(legacy_skill_paths_at(&home, &data).is_empty());
    fs::remove_dir(&link).unwrap();
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    assert!(legacy_skill_paths_at(&home, &data).is_empty());
}

#[cfg(unix)]
#[test]
fn update_check_warns_about_the_legacy_skill_and_removes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let server = release_server(vec![], true, "200 OK");
    let quiet = test_update(&server, "0.2.0", true, home.path()).unwrap();
    assert!(quiet.warnings.is_empty());
    let (payload_dir, link) = install_legacy_skill(home.path());
    let result = test_update(&server, "0.2.0", true, home.path()).unwrap();
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("deprecated"));
    assert_eq!(fs::read_link(&link).unwrap(), payload_dir);
    assert!(payload_dir.join("SKILL.md").is_file());
}

#[test]
fn verified_candidate_receives_install_destination_and_failure_is_propagated() {
    for fail in [false, true] {
        let archive_dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let args_file = home.path().join("candidate-args");
        let archive = protocol_archive(
            archive_dir.path(),
            &args_file,
            env!("CARGO_PKG_VERSION"),
            fail,
        );
        let server = release_server(archive, true, "200 OK");
        let result = test_update(&server, "0.2.0", false, home.path());
        if fail {
            assert_eq!(
                result.err().expect("installation should fail").message,
                "fixture install failure"
            );
        } else {
            let result = result.unwrap();
            assert_eq!(result.data["updated"], true);
            assert!(result.warnings.is_empty());
        }
        // Validate the candidate's CLI contract, not the sender's flag order.
        use clap::Parser;
        let arguments = fs::read_to_string(&args_file).unwrap();
        let cli = crate::cli::Cli::try_parse_from(std::iter::once("klms").chain(arguments.lines()))
            .unwrap();
        assert!(cli.json);
        let crate::cli::Command::Install { destination } = cli.command else {
            panic!("candidate was not asked to install");
        };
        assert_eq!(destination, home.path().join("klms"));
    }
}

#[test]
fn failed_download_checksum_or_version_never_invokes_install() {
    for (checksum_ok, status, candidate_version) in [
        (false, "200 OK", env!("CARGO_PKG_VERSION")),
        (true, "404 Not Found", env!("CARGO_PKG_VERSION")),
        (true, "200 OK", "99.0.0"),
    ] {
        let archive_dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let args_file = home.path().join("candidate-args");
        let archive = protocol_archive(archive_dir.path(), &args_file, candidate_version, false);
        let server = release_server(archive, checksum_ok, status);
        fs::write(home.path().join("klms"), b"old executable").unwrap();
        assert!(test_update(&server, "0.2.0", false, home.path()).is_err());
        assert!(!args_file.exists());
        assert_eq!(
            fs::read(home.path().join("klms")).unwrap(),
            b"old executable"
        );
    }
}
