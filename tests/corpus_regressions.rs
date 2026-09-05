use serde_json::Value;
use std::{
    fs,
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
// The shared HTTP fixture also exposes helpers used by other integration suites.
#[allow(dead_code)]
mod fixture;
use fixture::server::{Response, Server};

fn local(data: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_klms"))
        .env("XDG_DATA_HOME", data.path())
        .arg("--json")
        .args(args)
        .output()
        .unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
}
fn sync(data: &TempDir, server: &Server) -> Value {
    let state = data.path().join("state");
    fs::create_dir_all(state.join("klms")).unwrap();
    fs::write(state.join("klms/session.json"), r#"{"version":1,"origin":"http://127.0.0.1:0","created_at":1,"cookies":[{"name":"MoodleSession","value":"synthetic-session"}],"devices":[]}"#).unwrap();
    success(
        Command::new(env!("CARGO_BIN_EXE_klms"))
            .env("XDG_DATA_HOME", data.path())
            .env("XDG_STATE_HOME", state)
            .args([
                "--json",
                "--base-url",
                &server.url(),
                "library",
                "sync",
                "--download",
                "changed",
            ])
            .output()
            .unwrap(),
    )
}
fn server(bytes: &'static [u8], version: Arc<AtomicUsize>) -> Server {
    Server::new(move |request| match request.target.as_str() {
        "/my/" => Response::html("<a href='/course/view.php?id=42'>Compilers(CS.420_2026_2)</a>"),
        "/course/view.php?id=42" => Response::html(
            "<main class='course-content'><li class='activity modtype_resource' id='module-7'><a href='/mod/resource/view.php?id=7'><span class='instancename'>Lecture</span></a></li></main>",
        ),
        "/mod/resource/view.php?id=7" => Response::html(
            "<main><h1>Lecture</h1><p>compiler material</p><a href='/pluginfile.php/one'>one.txt</a></main>",
        ),
        "/pluginfile.php/one" => Response::bytes("application/octet-stream", bytes.to_vec())
            .header("ETag", &format!("\"v{}\"", version.load(Ordering::Relaxed))),
        other => panic!("unexpected request: {other}"),
    })
}
fn representation(data: &TempDir) -> String {
    let shown = success(local(data, &["library", "show", "file:7"]));
    shown["representations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| {
            item["url"]
                .as_str()
                .unwrap()
                .ends_with("/pluginfile.php/one")
        })
        .unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn padded(reference: &str) -> String {
    let (kind, id) = reference.split_once(':').unwrap();
    format!("{kind}:0{id}")
}

#[test]
fn numeric_aliases_share_curation_revisions_and_retractions() {
    let data = TempDir::new().unwrap();
    let server = server(b"fixture bytes", Arc::new(AtomicUsize::new(1)));
    sync(&data, &server);
    let reference = representation(&data);
    let alias = padded(&reference);
    let edit = success(local(
        &data,
        &[
            "library",
            "edit",
            &alias,
            "--field",
            "note",
            "--value",
            "zirconium",
            "--expected-revision",
            "0",
        ],
    ));
    assert_eq!(edit["subject_ref"], reference);
    let conflict = local(
        &data,
        &[
            "library",
            "edit",
            &reference,
            "--field",
            "note",
            "--value",
            "lost update",
            "--expected-revision",
            "0",
        ],
    );
    assert_eq!(conflict.status.code(), Some(54));
    let search = success(local(&data, &["library", "search", "zirconium"]));
    assert_eq!(search[0]["ref"], reference);
    let activity = success(local(&data, &["library", "activity", "--subject", &alias]));
    assert_eq!(activity.as_array().unwrap().len(), 1);
    let assertion = edit["ref"].as_str().unwrap();
    let retracted = success(local(&data, &["library", "retract", &padded(assertion)]));
    assert_eq!(retracted["target_ref"], assertion);
    assert_eq!(
        local(&data, &["library", "retract", assertion])
            .status
            .code(),
        Some(54)
    );
    assert!(
        success(local(&data, &["library", "search", "zirconium"]))
            .as_array()
            .unwrap()
            .is_empty()
    );

    let relation = success(local(
        &data,
        &[
            "library",
            "relations",
            "add",
            &alias,
            "file:7",
            "--kind",
            "related_to",
        ],
    ));
    assert_eq!(
        local(
            &data,
            &[
                "library",
                "relations",
                "add",
                &reference,
                "file:7",
                "--kind",
                "related_to"
            ]
        )
        .status
        .code(),
        Some(54)
    );
    success(local(
        &data,
        &[
            "library",
            "retract",
            &padded(relation["ref"].as_str().unwrap()),
        ],
    ));
    success(local(
        &data,
        &[
            "library",
            "relations",
            "add",
            &reference,
            "file:7",
            "--kind",
            "related_to",
        ],
    ));
}

#[test]
fn changed_validators_for_identical_bytes_do_not_redownload_or_report_byte_changes() {
    let data = TempDir::new().unwrap();
    let version = Arc::new(AtomicUsize::new(1));
    let server = server(b"unchanged bytes", Arc::clone(&version));
    sync(&data, &server);
    version.store(2, Ordering::Relaxed);
    let second = sync(&data, &server);
    assert_eq!(second["blobs_added"], 0);
    let count_gets = || {
        server
            .requests()
            .iter()
            .filter(|request| request.starts_with("GET /pluginfile.php/one "))
            .count()
    };
    assert_eq!(count_gets(), 2);
    let third = sync(&data, &server);
    assert_eq!(third["blobs_added"], 0);
    assert_eq!(count_gets(), 2);
    let changes = success(local(&data, &["library", "changes"]));
    assert!(
        !changes
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["kind"] == "verified_content_changed")
    );
}

#[test]
fn utf8_preview_preserves_complete_prefix_at_byte_boundary() {
    let data = TempDir::new().unwrap();
    let server = server("a한글z".as_bytes(), Arc::new(AtomicUsize::new(1)));
    sync(&data, &server);
    for (limit, expected) in [("2", "a"), ("4", "a한"), ("5", "a한"), ("8", "a한글z")] {
        let result = success(local(
            &data,
            &["library", "content", "file:7", "--max-bytes", limit],
        ));
        assert_eq!(result["text"], expected);
        assert_eq!(result["truncated"], limit != "8");
    }
}

#[test]
fn invalid_utf8_still_reports_binary_content() {
    for bytes in [b"a\xffmore".as_slice(), b"a\xe2\x82".as_slice()] {
        let data = TempDir::new().unwrap();
        let server = server(bytes, Arc::new(AtomicUsize::new(1)));
        sync(&data, &server);
        let result = success(local(&data, &["library", "content", "file:7"]));
        assert!(result["text"].is_null());
        assert_eq!(result["truncated"], false);
    }
}
