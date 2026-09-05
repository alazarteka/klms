use std::{fs, process::Command};

use serde_json::Value;
use tempfile::TempDir;

mod fixture;
use fixture::server::{Response, Server};

fn command(server: &Server, home: &TempDir) -> Command {
    let state = home.path().join("state");
    fs::create_dir_all(state.join("klms")).unwrap();
    fs::write(state.join("klms/session.json"), r#"{"version":1,"origin":"http://127.0.0.1:0","created_at":1,"cookies":[{"name":"MoodleSession","value":"synthetic"}],"devices":[]}"#).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_klms"));
    command
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", state)
        .args(["--json", "--base-url", &server.url()]);
    command
}

#[test]
fn partial_downloads_are_rejected_without_publishing_or_leaving_temporary_files() {
    for range in [
        None,
        Some("bytes 0-2/8"),
        Some("bytes 2-4/5"),
        Some("bytes 0-2/*"),
    ] {
        let server = Server::new(move |_| {
            let response = Response::bytes("application/pdf", b"pdf").status("206 Partial Content");
            match range {
                Some(range) => response.header("Content-Range", range),
                None => response,
            }
        });
        let home = TempDir::new().unwrap();
        let downloads = home.path().join("downloads");
        fs::create_dir(&downloads).unwrap();
        let output = command(&server, &home)
            .args(["files", "download", "/pluginfile.php/notes.pdf", "--out"])
            .arg(downloads.join("notes.pdf"))
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "UPSTREAM_ERROR");
        assert_eq!(fs::read_dir(&downloads).unwrap().count(), 0);
    }
}

#[test]
fn complete_range_download_remains_supported() {
    let server = Server::new(|_| {
        Response::bytes("application/pdf", b"pdf")
            .status("206 Partial Content")
            .header("Content-Range", "bytes 0-2/3")
    });
    let home = TempDir::new().unwrap();
    let destination = home.path().join("notes.pdf");
    let output = command(&server, &home)
        .args(["files", "download", "/pluginfile.php/notes.pdf", "--out"])
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(destination).unwrap(), b"pdf");
    assert!(
        server
            .recorded()
            .iter()
            .all(|request| request.has_header("cookie"))
    );
}

#[test]
fn typed_detail_redirects_cannot_silently_change_identity() {
    for (group, target, requested, redirected) in [
        (
            "assignments",
            "assign:7",
            "/mod/assign/view.php?id=7",
            "/mod/assign/view.php?id=8",
        ),
        (
            "quizzes",
            "7",
            "/mod/quiz/view.php?id=7",
            "/mod/quiz/view.php?id=8",
        ),
        (
            "videos",
            "/mod/vod/view.php?id=7",
            "/mod/vod/view.php?id=7",
            "/mod/vod/view.php?id=8",
        ),
        (
            "videos",
            "vod:7",
            "/mod/vod/view.php?id=7",
            "/mod/lti/view.php?id=7",
        ),
        (
            "boards",
            "board-post:7:9",
            "/mod/courseboard/article.php?id=7&bwid=9",
            "/mod/courseboard/article.php?id=7&bwid=10",
        ),
        (
            "notices",
            "/mod/courseboard/article.php?id=7&bwid=9",
            "/mod/courseboard/article.php?id=7&bwid=9",
            "/mod/courseboard/article.php?id=8&bwid=9",
        ),
    ] {
        let server = Server::new(move |request| {
            if request.target == requested {
                Response::html("")
                    .status("302 Found")
                    .header("Location", redirected)
            } else {
                Response::html("<main><h1>Different resource</h1></main>")
            }
        });
        let home = TempDir::new().unwrap();
        let output = command(&server, &home)
            .args([group, "show", target])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{group}: {target}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            error["error"]["code"], "UPSTREAM_SHAPE_CHANGED",
            "{group}: {error}"
        );
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("identity")
        );
    }
}

#[test]
fn detail_redirects_can_preserve_identity_with_zero_padding_and_extra_parameters() {
    let server = Server::new(|request| {
        if request.target == "/mod/assign/view.php?id=007" {
            Response::html("")
                .status("302 Found")
                .header("Location", "/mod/assign/view.php?id=7&redirect=1")
        } else {
            Response::html("<main><h1>Requested assignment</h1></main>")
        }
    });
    let home = TempDir::new().unwrap();
    let output = command(&server, &home)
        .args(["assignments", "show", "assign:007"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["ref"], "assign:7");
}
