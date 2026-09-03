use std::{
    fs,
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::TempDir;

mod fixture;
use fixture::server::{Response, Server};

const DASHBOARD: &str = "<a href='/course/view.php?id=42'>Compilers(CS.420_2026_2)</a>";
const MANIFEST: &str = "<main class='course-content'>\
    <li class='activity modtype_resource' id='module-7'>\
    <a href='/mod/resource/view.php?id=7'><span class='instancename'>Lecture</span></a>\
    </li></main>";
const DETAIL: &str = "<main><h1>Lecture</h1><p>compiler material</p>\
    <a href='/pluginfile.php/one'>one.txt</a></main>";

fn command(data: &TempDir, json: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_klms"));
    command.env("XDG_DATA_HOME", data.path());
    if json {
        command.arg("--json");
    }
    command
}

fn local(data: &TempDir, json: bool, arguments: &[&str]) -> Output {
    command(data, json).args(arguments).output().unwrap()
}

fn sync_command(data: &TempDir, server: &Server, json: bool) -> Command {
    let state = data.path().join("state");
    fs::create_dir_all(state.join("klms")).unwrap();
    fs::write(state.join("klms/session.json"), r#"{"version":1,"origin":"http://127.0.0.1:0","created_at":1,"cookies":[{"name":"MoodleSession","value":"synthetic-session"}],"devices":[]}"#).unwrap();
    let mut command = command(data, json);
    command
        .env("XDG_STATE_HOME", state)
        .args(["--base-url", &server.url(), "library", "sync"]);
    command
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn content_error(output: Output) -> Value {
    assert_eq!(output.status.code(), Some(55));
    assert!(output.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["ok"], false);
    envelope["error"].clone()
}

// Both byte operations must fail locally without creating a partial export.
fn byte_errors(data: &TempDir, reference: &str) -> Value {
    let destination = data.path().join("unavailable-export");
    let mut expected = None;
    for operation in ["content", "export"] {
        let mut arguments = vec!["library", operation, reference];
        if operation == "export" {
            arguments.extend(["--out", destination.to_str().unwrap()]);
        }
        let error = content_error(local(data, true, &arguments));
        if let Some(expected) = &expected {
            assert_eq!(&error, expected, "content/export diagnostics diverged");
        }
        let human = local(data, false, &arguments);
        assert_eq!(human.status.code(), Some(55));
        assert!(human.stdout.is_empty());
        let stderr = String::from_utf8(human.stderr).unwrap();
        assert!(
            stderr.contains(error["message"].as_str().unwrap()),
            "{stderr}"
        );
        if let Some(hint) = error["hint"].as_str() {
            assert!(stderr.contains(hint), "{stderr}");
        }
        assert!(!destination.exists());
        expected = Some(error);
    }
    expected.unwrap()
}

fn representation(shown: &Value, url_suffix: &str) -> String {
    shown["data"]["representations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["url"].as_str().unwrap().ends_with(url_suffix))
        .unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_owned()
}

const NOTICE_MANIFEST: &str = "<main class='course-content'>\
    <li class='activity modtype_courseboard' id='module-9'>\
    <a href='/mod/courseboard/view.php?id=9'><span class='instancename'>Notices</span></a>\
    </li></main>";
const NOTICE_LIST: &str = "<table class='generaltable'><tr><td>\
    <a href='/mod/courseboard/article.php?id=9&bwid=10'>Hello</a></td></tr></table>";

#[test]
fn non_file_links_never_recommend_downloads_or_resolve_a_sibling_attachment() {
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(DASHBOARD),
        "/course/view.php?id=42" => Response::html(NOTICE_MANIFEST),
        "/mod/courseboard/view.php?id=9" => Response::html(NOTICE_LIST),
        "/mod/courseboard/article.php?id=9&bwid=10" => Response::html(
            "<div class='courseboard_view'><div class='subject'><h3>Hello</h3></div>\
             <div class='content'>Stored notice text\
             <a href='/mod/courseboard/article.php?id=9&amp;bwid=10'>Permalink</a>\
             <a href='https://example.org/reading'>External reading</a></div>\
             <div class='info'><div class='files'>\
             <a href='/pluginfile.php/one'>one.txt</a></div></div></div>",
        ),
        target => basic_response(target),
    });
    success(
        sync_command(&data, &server, true)
            .arg("--notices")
            .output()
            .unwrap(),
    );
    let shown = success(local(&data, true, &["library", "show", "board-post:9:10"]));
    let links = [
        representation(&shown, "/mod/courseboard/article.php?id=9&bwid=10"),
        representation(&shown, "/reading"),
    ];
    let attachment = representation(&shown, "/pluginfile.php/one");
    let requests_before = server.requests().len();
    let error = byte_errors(&data, &attachment);
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("library sync --course course:42 --notices --download changed")
    );
    for link in &links {
        let error = byte_errors(&data, link);
        assert!(
            error["message"].as_str().unwrap().contains("link"),
            "{error}"
        );
        let hint = error["hint"].as_str().unwrap();
        assert!(hint.contains(&format!("library show {link}")), "{hint}");
        assert!(hint.contains("library show board-post:9:10"), "{hint}");
        assert!(hint.contains("data.source.text"), "{hint}");
        assert!(!error.to_string().contains("library sync"), "{error}");
    }
    assert_eq!(server.requests().len(), requests_before);

    success(
        sync_command(&data, &server, true)
            .args(["--notices", "--download", "changed"])
            .output()
            .unwrap(),
    );
    let bytes = success(local(&data, true, &["library", "content", &attachment]));
    assert_eq!(bytes["data"]["text"], "cached fixture bytes");
    let parent = success(local(
        &data,
        true,
        &["library", "content", "board-post:9:10"],
    ));
    assert_eq!(parent["data"], bytes["data"]);
    let destination = data.path().join("notice-attachment.txt");
    success(local(
        &data,
        true,
        &[
            "library",
            "export",
            "board-post:9:10",
            "--out",
            destination.to_str().unwrap(),
        ],
    ));
    assert_eq!(fs::read(destination).unwrap(), b"cached fixture bytes");
    let requests_before = server.requests().len();
    for link in &links {
        let error = byte_errors(&data, link);
        assert!(
            error["message"].as_str().unwrap().contains("link"),
            "{error}"
        );
        assert!(!error.to_string().contains("library sync"), "{error}");
    }
    assert_eq!(server.requests().len(), requests_before);
}

#[test]
fn resources_without_file_candidates_do_not_invent_a_download_remedy() {
    let data = TempDir::new().unwrap();
    let truncated = Arc::new(AtomicBool::new(false));
    let router_truncated = Arc::clone(&truncated);
    let server = Server::new(move |request| match request.target.as_str() {
        "/my/" => Response::html(DASHBOARD),
        "/course/view.php?id=42" => Response::html(format!(
            "{NOTICE_MANIFEST}<main class='course-content'>\
             <li class='activity modtype_lti' id='module-11'>\
             <a href='/mod/lti/view.php?id=11'><span class='instancename'>Video tool</span></a>\
             </li></main>"
        )),
        "/mod/courseboard/view.php?id=9" => Response::html(NOTICE_LIST),
        "/mod/courseboard/article.php?id=9&bwid=10" => Response::html(format!(
            "<div class='courseboard_view'><div class='subject'><h3>Hello</h3></div>\
             <div class='content'>{}</div></div>",
            if router_truncated.load(Ordering::SeqCst) {
                "x".repeat(100_001)
            } else {
                String::new()
            }
        )),
        target => panic!("unexpected request: {target}"),
    });
    success(
        sync_command(&data, &server, true)
            .arg("--notices")
            .output()
            .unwrap(),
    );
    let lti = success(local(&data, true, &["library", "show", "lti:11"]));
    assert_eq!(lti["data"]["representations"], serde_json::json!([]));
    let empty_notice = success(local(&data, true, &["library", "show", "board-post:9:10"]));
    assert_eq!(empty_notice["data"]["source"]["text"], "");
    let requests_before = server.requests().len();
    for reference in ["lti:11", "board-post:9:10"] {
        let error = byte_errors(&data, reference);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("no downloaded file bytes"),
            "{error}"
        );
        assert!(
            error["hint"]
                .as_str()
                .unwrap()
                .contains(&format!("library show {reference}")),
            "{error}"
        );
        assert!(!error.to_string().contains("library sync"), "{error}");
    }
    let missing_hash = format!("sha256:{}", "0".repeat(64));
    for reference in ["file:999", "representation:999", &missing_hash] {
        let error = byte_errors(&data, reference);
        assert_eq!(error["message"], "no stored content for reference");
        assert!(!error.to_string().contains("library sync"), "{error}");
    }
    assert_eq!(server.requests().len(), requests_before);

    truncated.store(true, Ordering::SeqCst);
    let result = success(
        sync_command(&data, &server, true)
            .arg("--notices")
            .output()
            .unwrap(),
    );
    assert_eq!(result["data"]["truncated"], 1);
    let shown = success(local(&data, true, &["library", "show", "board-post:9:10"]));
    assert_eq!(shown["data"]["source"]["complete"], false);
    let requests_before = server.requests().len();
    let error = byte_errors(&data, "board-post:9:10");
    assert!(
        error["hint"].as_str().unwrap().contains("data.source.text"),
        "{error}"
    );
    assert!(!error.to_string().contains("library sync"), "{error}");
    assert_eq!(server.requests().len(), requests_before);
}

#[test]
fn previously_observed_file_without_bytes_directs_to_parent_not_download_retry() {
    let data = TempDir::new().unwrap();
    let missing = Arc::new(AtomicBool::new(false));
    let router_missing = Arc::clone(&missing);
    let server = Server::new(move |request| {
        if request.target == "/mod/resource/view.php?id=7" && router_missing.load(Ordering::SeqCst)
        {
            Response::html(
                "<main><h1>Lecture</h1><p>The current observation has no file links.</p></main>",
            )
        } else {
            basic_response(&request.target)
        }
    });
    success(sync_command(&data, &server, true).output().unwrap());
    let shown = success(local(&data, true, &["library", "show", "file:7"]));
    let file = representation(&shown, "/pluginfile.php/one");
    missing.store(true, Ordering::SeqCst);
    success(sync_command(&data, &server, true).output().unwrap());
    let shown = success(local(&data, true, &["library", "show", &file]));
    assert_eq!(shown["data"]["remote_state"], "not_observed");
    assert!(shown["data"]["content"].is_null());
    let requests_before = server.requests().len();
    let error = byte_errors(&data, &file);
    assert!(
        error["message"].as_str().unwrap().contains("not observed"),
        "{error}"
    );
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("library show file:7"),
        "{error}"
    );
    assert!(!error.to_string().contains("library sync"), "{error}");
    let resource_error = byte_errors(&data, "file:7");
    assert!(
        !resource_error.to_string().contains("library sync"),
        "{resource_error}"
    );
    assert_eq!(server.requests().len(), requests_before);
}

fn basic_response(target: &str) -> Response {
    match target {
        "/my/" => Response::html(DASHBOARD),
        "/course/view.php?id=42" => Response::html(MANIFEST),
        "/mod/resource/view.php?id=7" => Response::html(DETAIL),
        "/pluginfile.php/one" => Response::bytes("text/plain", b"cached fixture bytes".to_vec())
            .header("ETag", "\"fixture-v1\""),
        target => panic!("unexpected request: {target}"),
    }
}

#[test]
fn status_distinguishes_never_synced_scoped_and_global_coverage() {
    let data = TempDir::new().unwrap();
    let initial = success(local(&data, true, &["library", "status"]));
    assert_eq!(initial["schema_version"], "4");
    assert_eq!(initial["data"]["schema_version"], 1);
    assert!(initial["data"]["last_sync"].is_null());
    assert!(initial["data"]["fresh_through"].is_null());
    let human = local(&data, false, &["library", "status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Last sync attempt: none"), "{human}");
    assert!(human.contains("Last complete global sync: none"), "{human}");

    let server = Server::new(|request| basic_response(&request.target));
    success(
        sync_command(&data, &server, true)
            .args(["--course", "42"])
            .output()
            .unwrap(),
    );
    let scoped = success(local(&data, true, &["library", "status"]));
    let attempt = &scoped["data"]["last_sync"];
    assert_eq!(attempt["scope"], "course:42");
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["source_complete"], false);
    assert!(attempt["started_at"].as_i64().unwrap() > 0);
    assert!(attempt["finished_at"].as_i64().unwrap() >= attempt["started_at"].as_i64().unwrap());
    assert!(scoped["data"]["fresh_through"].is_null());
    let human = local(&data, false, &["library", "status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    for expected in [
        "ready",
        "complete",
        "course:42",
        "Started",
        "Finished",
        "global",
    ] {
        assert!(human.contains(expected), "missing {expected:?}: {human}");
    }
    success(sync_command(&data, &server, true).output().unwrap());
    let global = success(local(&data, true, &["library", "status"]));
    assert_eq!(global["data"]["last_sync"]["scope"], "all");
    assert_eq!(global["data"]["last_sync"]["source_complete"], true);
    assert!(global["data"]["fresh_through"].as_i64().unwrap() > 0);
    assert!(server.recorded().iter().all(|r| r.has_header("Cookie")));
}

#[test]
fn empty_and_truncated_local_collections_keep_feedback_in_the_correct_stream() {
    let data = TempDir::new().unwrap();
    for args in [
        vec!["library", "search", "absent"],
        vec!["library", "changes"],
        vec!["library", "activity"],
    ] {
        let value = success(local(&data, true, &args));
        assert_eq!(value["data"], serde_json::json!([]));
        assert_eq!(value["meta"]["complete"], true);
        let human = local(&data, false, &args);
        assert!(human.status.success());
        assert!(human.stderr.is_empty());
        assert_eq!(
            String::from_utf8(human.stdout).unwrap().trim(),
            "No records found."
        );
    }
    let server = Server::new(|request| basic_response(&request.target));
    success(sync_command(&data, &server, true).output().unwrap());
    for args in [
        vec!["library", "changes", "--limit", "1"],
        vec!["library", "search", "compiler", "--limit", "1"],
    ] {
        let value = success(local(&data, true, &args));
        assert_eq!(value["meta"]["complete"], false);
        assert_eq!(value["data"].as_array().unwrap().len(), 1);
        assert!(
            value["warnings"]
                .to_string()
                .to_lowercase()
                .contains("truncat")
        );
        let human = local(&data, false, &args);
        assert!(human.status.success());
        assert!(!human.stdout.is_empty());
        assert!(
            !String::from_utf8_lossy(&human.stdout)
                .to_lowercase()
                .contains("truncat")
        );
        assert!(
            String::from_utf8_lossy(&human.stderr)
                .to_lowercase()
                .contains("truncat")
        );
    }
}

#[test]
fn partial_sync_succeeds_with_incomplete_summary_and_failure_warnings() {
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| {
        if request.target == "/mod/resource/view.php?id=7" {
            Response::html("temporarily unavailable").status("503 Service Unavailable")
        } else {
            basic_response(&request.target)
        }
    });
    let result = success(sync_command(&data, &server, true).output().unwrap());
    assert_eq!(result["data"]["status"], "incomplete");
    assert_eq!(result["data"]["source_complete"], false);
    let failures = result["data"]["failures"].as_array().unwrap();
    assert!(!failures.is_empty());
    for failure in failures {
        assert!(
            result["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| {
                    warning
                        .as_str()
                        .unwrap()
                        .contains(failure.as_str().unwrap())
                })
        );
    }
    let human = sync_command(&data, &server, false).output().unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("incomplete"));
    assert!(!human.stderr.is_empty());
    let status = local(&data, false, &["library", "status"]);
    assert!(status.status.success());
    let human = String::from_utf8(status.stdout).unwrap();
    assert!(human.contains("ready"));
    assert!(human.contains("incomplete"));
    let status = success(local(&data, true, &["library", "status"]));
    assert_eq!(status["data"]["last_sync"]["status"], "incomplete");
    assert!(status["data"]["fresh_through"].is_null());
}

#[test]
fn parent_notice_byte_errors_offer_text_and_only_present_file_candidates() {
    for text in ["", ".", "Read the syllabus"] {
        for count in [0, 1, 2, 21] {
            let data = TempDir::new().unwrap();
            let removed = Arc::new(AtomicBool::new(false));
            let fixture_removed = Arc::clone(&removed);
            let server = Server::new(move |request| match request.target.as_str() {
                "/my/" => Response::html(DASHBOARD),
                "/course/view.php?id=42" => Response::html(NOTICE_MANIFEST),
                "/mod/courseboard/view.php?id=9" => Response::html(NOTICE_LIST),
                "/mod/courseboard/article.php?id=9&bwid=10" => {
                    let first = usize::from(fixture_removed.load(Ordering::SeqCst));
                    let files = (first..count).map(|index| format!(
                            "<a href='/pluginfile.php/attachment-{index}'>syllabus-{index}.pdf</a>"
                        )).collect::<String>();
                    Response::html(format!(
                        "<div class='courseboard_view'><div class='subject'><h3>Syllabus</h3></div>\
                         <div class='content'>{text}</div><div class='info'><div class='files'>\
                         {files}<a href='https://example.org/info'>Info link</a>\
                         </div></div></div>"
                    ))
                }
                target => panic!("unexpected request: {target}"),
            });
            success(
                sync_command(&data, &server, true)
                    .arg("--notices")
                    .output()
                    .unwrap(),
            );
            let shown = success(local(&data, true, &["library", "show", "board-post:9:10"]));
            assert_eq!(shown["data"]["source"]["text"], text);
            let link = representation(&shown, "/info");
            let requests_before = server.requests().len();
            let error = byte_errors(&data, "board-post:9:10");
            let hint = error["hint"].as_str().unwrap();
            assert!(hint.contains("klms library show board-post:9:10"), "{hint}");
            assert_eq!(
                hint.contains("data.source.text"),
                !text.is_empty(),
                "{hint}"
            );
            assert!(!hint.contains(&link), "{hint}");
            if count > 0 {
                assert!(
                    hint.contains(
                        "klms library sync --course course:42 --notices --download changed"
                    ),
                    "{hint}"
                );
                let expected: Vec<_> = (0..count.min(20))
                    .map(|index| {
                        let reference = representation(&shown, &format!("/attachment-{index}"));
                        assert!(hint.contains(&reference), "{hint}");
                        assert!(hint.contains(&format!("syllabus-{index}.pdf")), "{hint}");
                        reference
                    })
                    .collect();
                assert_eq!(
                    error["details"]["representations"],
                    serde_json::json!(expected)
                );
                assert_eq!(hint.contains("first 20"), count > 20, "{hint}");
            } else {
                assert!(!hint.contains("library sync"), "{hint}");
            }
            assert_eq!(
                server.requests().len(),
                requests_before,
                "errors must stay local"
            );

            removed.store(true, Ordering::SeqCst);
            success(
                sync_command(&data, &server, true)
                    .arg("--notices")
                    .output()
                    .unwrap(),
            );
            let requests_before = server.requests().len();
            let error = byte_errors(&data, "board-post:9:10");
            let hint = error["hint"].as_str().unwrap();
            if count > 1 {
                let expected: Vec<_> = (1..count)
                    .map(|index| representation(&shown, &format!("/attachment-{index}")))
                    .collect();
                assert_eq!(
                    error["details"]["representations"],
                    serde_json::json!(expected)
                );
                assert!(!hint.contains("syllabus-0.pdf"), "{hint}");
                assert!(!hint.contains("first 20"), "{hint}");
            } else {
                assert!(!hint.contains("library sync"), "{hint}");
                assert!(error["details"]["representations"].is_null());
            }
            assert_eq!(server.requests().len(), requests_before);
        }
    }
}

#[test]
fn notice_text_and_metadata_only_files_have_actionable_content_errors() {
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/course/view.php?id=42" => Response::html(format!(
            "{MANIFEST}<main class='course-content'><li class='activity modtype_courseboard' \
             id='module-9'><a href='/mod/courseboard/view.php?id=9'>\
             <span class='instancename'>Notices</span></a></li></main>"
        )),
        "/mod/courseboard/view.php?id=9" => Response::html(
            "<table class='generaltable'><tr><td><a \
             href='/mod/courseboard/article.php?id=9&bwid=10'>Hello</a></td></tr></table>",
        ),
        "/mod/courseboard/article.php?id=9&bwid=10" => Response::html(
            "<div class='courseboard_view'><div class='subject'><h3>Hello</h3></div>\
             <div class='content'>Stored notice text</div>\
             <div class='info'><div class='files'>\
             <a href='/pluginfile.php/notice'>Notice attachment</a></div></div></div>",
        ),
        target => basic_response(target),
    });
    success(
        sync_command(&data, &server, true)
            .arg("--notices")
            .output()
            .unwrap(),
    );
    let shown = success(local(&data, true, &["library", "show", "board-post:9:10"]));
    assert_eq!(shown["data"]["source"]["text"], "Stored notice text");
    let attachment_ref = representation(&shown, "/pluginfile.php/notice");
    let file = success(local(&data, true, &["library", "show", "file:7"]));
    let file_ref = representation(&file, "/pluginfile.php/one");
    let requests_before = server.requests().len();
    let notice = content_error(local(
        &data,
        true,
        &["library", "content", "board-post:9:10"],
    ));
    let hint = notice["hint"].as_str().unwrap();
    assert!(hint.contains("klms library show board-post:9:10"), "{hint}");
    assert!(hint.contains("data.source.text"), "{hint}");
    for reference in ["file:7", &file_ref] {
        let error = content_error(local(&data, true, &["library", "content", reference]));
        assert!(error["message"].as_str().unwrap().contains("metadata"));
        let hint = error["hint"].as_str().unwrap();
        assert!(
            hint.contains("library sync --course course:42 --download changed"),
            "{hint}"
        );
    }
    let attachment = content_error(local(&data, true, &["library", "content", &attachment_ref]));
    assert!(
        attachment["hint"]
            .as_str()
            .unwrap()
            .contains("library sync --course course:42 --notices --download changed")
    );
    assert_eq!(
        server.requests().len(),
        requests_before,
        "hints must not download"
    );
}

#[test]
fn ambiguous_content_lists_candidates_and_downloaded_byte_paths_remain_usable() {
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/mod/resource/view.php?id=7" => Response::html(
            "<main><a href='/pluginfile.php/one'>one.txt</a>\
             <a href='/pluginfile.php/two'>two.txt</a></main>",
        ),
        "/pluginfile.php/two" => Response::bytes("text/plain", b"second fixture".to_vec()),
        target => basic_response(target),
    });
    success(
        sync_command(&data, &server, true)
            .args(["--download", "changed"])
            .output()
            .unwrap(),
    );
    let shown = success(local(&data, true, &["library", "show", "file:7"]));
    let first = representation(&shown, "/pluginfile.php/one");
    let second = representation(&shown, "/pluginfile.php/two");
    let error = byte_errors(&data, "file:7");
    assert!(error["message"].as_str().unwrap().contains("multiple"));
    assert_eq!(
        error["details"]["representations"],
        serde_json::json!([first, second])
    );
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains(&first) && hint.contains(&second), "{hint}");
    let bytes = success(local(&data, true, &["library", "content", &first]));
    assert_eq!(bytes["data"]["text"], "cached fixture bytes");
    let hash = bytes["data"]["ref"].as_str().unwrap();
    assert!(hash.starts_with("sha256:"));
    assert_eq!(
        success(local(&data, true, &["library", "content", hash]))["data"]["text"],
        bytes["data"]["text"]
    );
    let destination = data.path().join("existing.txt");
    fs::write(&destination, b"keep me").unwrap();
    let refusal = local(
        &data,
        true,
        &[
            "library",
            "export",
            &first,
            "--out",
            destination.to_str().unwrap(),
        ],
    );
    assert!(!refusal.status.success());
    assert_eq!(fs::read(destination).unwrap(), b"keep me");
}

struct HeldChild {
    child: Child,
    release: Arc<AtomicBool>,
}

impl Drop for HeldChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.release.store(true, Ordering::SeqCst);
    }
}

#[cfg(unix)]
#[test]
fn active_and_interrupted_attempts_are_unfinished_without_liveness_claims() {
    for signal in ["-INT", "-KILL"] {
        let data = TempDir::new().unwrap();
        let hold = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (entered, observed) = mpsc::channel();
        let router_hold = Arc::clone(&hold);
        let router_release = Arc::clone(&release);
        let server = Server::new(move |request| {
            if request.target == "/course/view.php?id=42"
                && router_hold.swap(false, Ordering::SeqCst)
            {
                entered.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while !router_release.load(Ordering::SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            basic_response(&request.target)
        });
        success(
            sync_command(&data, &server, true)
                .args(["--download", "changed"])
                .output()
                .unwrap(),
        );
        let before = success(local(&data, true, &["library", "status"]));
        hold.store(true, Ordering::SeqCst);
        let mut active = HeldChild {
            child: sync_command(&data, &server, true)
                .args(["--course", "42", "--download", "changed"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            release: Arc::clone(&release),
        };
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(active.child.try_wait().unwrap().is_none());
        assert_unfinished(&data, &before);
        let killed = Command::new("kill")
            .args([signal, &active.child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = active.child.try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "child did not stop after {signal}"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_unfinished(&data, &before);
        release.store(true, Ordering::SeqCst);
        success(
            sync_command(&data, &server, true)
                .args(["--course", "42", "--download", "changed"])
                .output()
                .unwrap(),
        );
        let recovered = success(local(&data, true, &["library", "status"]));
        assert_eq!(recovered["data"]["last_sync"]["status"], "complete");
        assert_eq!(recovered["data"]["blobs"], before["data"]["blobs"]);
        let content = success(local(&data, true, &["library", "content", "file:7"]));
        assert_eq!(content["data"]["text"], "cached fixture bytes");
    }
}

#[cfg(unix)]
fn assert_unfinished(data: &TempDir, before: &Value) {
    let status = success(local(data, true, &["library", "status"]));
    assert_eq!(status["data"]["last_sync"]["status"], "unfinished");
    assert_eq!(status["data"]["last_sync"]["scope"], "course:42");
    assert!(status["data"]["last_sync"]["finished_at"].is_null());
    assert_eq!(
        status["data"]["fresh_through"],
        before["data"]["fresh_through"]
    );
    let warning = status["warnings"].to_string();
    assert!(
        warning.contains("may still be active or may have been interrupted"),
        "{warning}"
    );
    assert!(warning.contains("original process"), "{warning}");
    let human = local(data, false, &["library", "status"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("unfinished"));
    assert!(
        String::from_utf8_lossy(&human.stderr)
            .contains("may still be active or may have been interrupted")
    );
    let database = rusqlite::Connection::open(data.path().join("klms/library.db")).unwrap();
    let raw: (String, Option<i64>) = database
        .query_row(
            "SELECT status,finished_at FROM sync_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(raw, ("running".into(), None));
    let content = success(local(data, true, &["library", "content", "file:7"]));
    assert_eq!(content["data"]["text"], "cached fixture bytes");
}
