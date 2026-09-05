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

#[allow(dead_code)]
#[path = "../../tests/fixture/server.rs"]
mod fixture;
use fixture::{Response, Server};

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
            let digest = Sha256::digest(if checksum_ok {
                archive.as_slice()
            } else {
                b"wrong"
            })
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
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
        &Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap(),
        check,
        &ReleaseSource {
            metadata_url: format!("{}/latest", server.url()),
            downloads_url: server.url(),
            current_version: current.into(),
            destination: home.join("klms"),
            home: Some(home.to_owned()),
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
        assert!(!home.path().join("data").exists());
    }
}

// This candidate records the subprocess protocol; actual binary/skill installation
// is exercised by tests/update_contract.rs using Cargo's real CLI executable.
fn protocol_archive(root: &Path, candidate_version: &str, fail: bool) -> Vec<u8> {
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
printf '%s\n' "$@" > "$HOME/candidate-args"
if [ '{fail}' = 'true' ]; then
    printf '%s\n' '{{"error":{{"message":"fixture install failure"}}}}' >&2
    exit 1
fi
exit 0
"#
    );
    fs::write(root.join(&package).join("klms"), candidate).unwrap();
    let output = Command::new("tar")
        .args(["-czf", "-", "-C"])
        .arg(root)
        .arg(package)
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

#[test]
fn verified_candidate_receives_install_destination_and_failure_is_propagated() {
    for fail in [false, true] {
        let archive_dir = tempfile::tempdir().unwrap();
        let archive = protocol_archive(archive_dir.path(), env!("CARGO_PKG_VERSION"), fail);
        let server = release_server(archive, true, "200 OK");
        let home = tempfile::tempdir().unwrap();
        let result = test_update(&server, "0.2.0", false, home.path());
        if fail {
            assert_eq!(
                result.err().expect("installation should fail").message,
                "fixture install failure"
            );
        } else {
            assert_eq!(result.unwrap().data["updated"], true);
        }
        // Validate the candidate's CLI contract, not the sender's flag order.
        use clap::Parser;
        let arguments = fs::read_to_string(home.path().join("candidate-args")).unwrap();
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
        let archive = protocol_archive(archive_dir.path(), candidate_version, false);
        let server = release_server(archive, checksum_ok, status);
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("klms"), b"old executable").unwrap();
        assert!(test_update(&server, "0.2.0", false, home.path()).is_err());
        assert!(!home.path().join("candidate-args").exists());
        assert_eq!(
            fs::read(home.path().join("klms")).unwrap(),
            b"old executable"
        );
    }
}
