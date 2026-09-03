use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
};

use serde_json::Value;
use tempfile::TempDir;

mod fixture;
use fixture::server::{Response, Server};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_klms"))
}

fn storage_state(directory: &TempDir) -> PathBuf {
    let root = directory.path().join("state");
    fs::create_dir_all(root.join("klms")).unwrap();
    fs::write(root.join("klms/session.json"), r#"{"version":1,"origin":"http://127.0.0.1:0","created_at":1,"cookies":[{"name":"MoodleSession","value":"test-session"}],"devices":[]}"#).unwrap();
    root
}

#[test]
fn json_auth_status_is_one_document_without_secrets() {
    let home = TempDir::new().unwrap();
    let output = binary()
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .args(["--json", "auth", "status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], "4");
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["configured"], false);
}

#[test]
fn owned_auth_status_and_logout_never_emit_cookie_values() {
    let state_dir = TempDir::new().unwrap();
    let state_root = storage_state(&state_dir);
    let status = binary()
        .env("XDG_STATE_HOME", &state_root)
        .args([
            "--json",
            "--base-url",
            "http://127.0.0.1:9",
            "auth",
            "status",
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(!String::from_utf8_lossy(&status.stdout).contains("test-session"));
    let value: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["data"]["cookie_count"], 1);

    let logout = binary()
        .env("XDG_STATE_HOME", &state_root)
        .args(["--json", "auth", "logout"])
        .output()
        .unwrap();
    assert!(logout.status.success());
    let value: Value = serde_json::from_slice(&logout.stdout).unwrap();
    assert_eq!(value["data"]["removed"], true);
    assert!(!state_root.join("klms/session.json").exists());
}

#[test]
fn login_help_has_choices_but_no_secret_bearing_options() {
    let output = binary().args(["auth", "login", "--help"]).output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--method"));
    assert!(help.contains("--second-factor"));
    assert!(!help.contains("--password"));
    assert!(!help.contains("--otp"));
}

#[test]
fn json_usage_error_is_structured_and_exits_two() {
    let output = binary()
        .args(["--json", "courses", "show"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "USAGE");
}

#[test]
fn empty_course_queries_fail_before_authentication() {
    for args in [
        ["--json", "courses", "resolve", ""],
        ["--json", "courses", "show", "   "],
    ] {
        let home = TempDir::new().unwrap();
        let output = binary()
            .env("HOME", home.path())
            .env_remove("XDG_CONFIG_HOME")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "USAGE");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("must not be empty")
        );
    }
}

#[test]
fn doctor_fails_with_diagnostics_and_recovery_when_auth_is_missing() {
    let home = TempDir::new().unwrap();
    let output = binary()
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "AUTH_REQUIRED");
    assert_eq!(
        value["error"]["details"]["session_status"],
        "not_configured"
    );
    let hint = value["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("klms auth login"));
    assert!(hint.contains("auth extend"));
}

#[test]
fn doctor_fails_when_server_rejects_the_saved_session() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let length = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET /my/ HTTP/1.1"));
        let body = r#"<html><form><input name="username"><input name="password"></form></html>"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "doctor",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(10));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "AUTH_REQUIRED");
    assert_eq!(value["error"]["details"]["session_status"], "expired");
    assert_eq!(
        value["error"]["details"]["session_error"]["code"],
        "AUTH_REQUIRED"
    );
}

#[test]
fn loopback_dashboard_exercises_cookie_transport_and_parser() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let length = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..length]);
        assert!(request.starts_with("GET /my/ HTTP/1.1"));
        assert!(
            request.contains("cookie: MoodleSession=test-session")
                || request.contains("Cookie: MoodleSession=test-session")
        );
        let body = r#"<select name="year"><option selected>2026</option></select>
          <select name="semester"><option selected>Fall</option></select>
          <a href="/course/view.php?id=42">Compilers(CS.420_2026_2)</a>"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });

    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "dashboard",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["course_count"], 1);
    assert_eq!(value["data"]["courses"][0]["id"], "42");
    assert_eq!(value["data"]["courses"][0]["ref"], "course:42");
}

#[test]
fn course_list_reports_canonical_refs_and_truncation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"<a href="/course/view.php?id=42">Compilers(CS.420_2026_2)</a>
          <a href="/course/view.php?id=43">Databases(CS.430_2026_2)</a>"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "courses",
            "list",
            "--limit",
            "1",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"][0]["ref"], "course:42");
    assert_eq!(value["meta"]["returned"], 1);
    assert_eq!(value["meta"]["total"], 2);
    assert_eq!(value["meta"]["complete"], false);
}

#[test]
fn raw_get_is_a_truncated_secret_free_preview() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let length = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..length]);
        assert!(request.starts_with("GET /mod/assign/view.php?id=7 HTTP/1.1"));
        let body = r#"{"sesskey":"bodysecret","payload":"abcdefghijklmnopqrstuvwxyz"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "request",
            "get",
            "/mod/assign/view.php?id=7",
            "--max-bytes",
            "48",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(output.status.success());
    assert!(
        !output
            .stdout
            .windows(10)
            .any(|bytes| bytes == b"bodysecret")
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["truncated"], true);
    assert_eq!(value["data"]["redacted"], true);
    assert!(
        value["data"]["body"]
            .as_str()
            .unwrap()
            .contains("bounded response is incomplete")
    );
}

#[test]
fn transport_errors_do_not_echo_secret_bearing_redirect_urls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: https://example.invalid/continue?sesskey=transportsecret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "request",
            "get",
            "/mod/assign/view.php?id=7",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("transportsecret"));
}

#[test]
fn download_redacts_source_secrets_and_refuses_replacement() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let length = stream.read(&mut request).unwrap();
        assert!(
            String::from_utf8_lossy(&request[..length])
                .starts_with("GET /pluginfile.php/7/notes.pdf?token=downloadsecret HTTP/1.1")
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: 5\r\nConnection: close\r\n\r\nnotes"
        )
        .unwrap();
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let out = state_dir.path().join("notes.pdf");
    let source = format!("http://{address}/pluginfile.php/7/notes.pdf?token=downloadsecret");
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "files",
            "download",
            &source,
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(output.status.success());
    assert_eq!(fs::read(&out).unwrap(), b"notes");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["bytes"], 5);
    assert!(
        !value["data"]["source_url"]
            .as_str()
            .unwrap()
            .contains("downloadsecret")
    );

    let replacement = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "files",
            "download",
            &source,
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!replacement.status.success());
    assert_eq!(fs::read(&out).unwrap(), b"notes");
}

#[test]
fn auth_extend_uses_allowlisted_ajax_and_reports_remaining_time() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let body = match index {
                0 => {
                    assert!(request.starts_with("GET /my/ HTTP/1.1"));
                    r#"<script>var cfg={"sesskey":"abc123"}</script><a href="/course/view.php?id=42">Compilers(CS.420_2026_2)</a>"#
                }
                1 => {
                    assert!(request.contains("info=core_session_touch"));
                    assert!(request.contains("\"methodname\":\"core_session_touch\""));
                    r#"[{"error":false,"data":true}]"#
                }
                _ => {
                    assert!(request.contains("info=core_session_time_remaining"));
                    r#"[{"error":false,"data":{"userid":7,"timeremaining":10800}}]"#
                }
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", if index == 0 { "text/html" } else { "application/json" }, body.len(), body).unwrap();
        }
    });

    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "auth",
            "extend",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "auth.extend");
    assert_eq!(value["data"]["remaining_seconds"], 10800);
    assert_eq!(value["data"]["remaining"], "03:00:00");
}

#[test]
fn auth_time_left_discovers_sesskey_without_persisting_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let (content_type, body) = if index == 0 {
                assert!(request.starts_with("GET /my/ HTTP/1.1"));
                (
                    "text/html",
                    r#"<script>var cfg={"sesskey":"abc123"}</script>"#,
                )
            } else {
                assert!(request.starts_with("POST /lib/ajax/service.php?"));
                assert!(request.contains("info=core_session_time_remaining"));
                (
                    "application/json",
                    r#"[{"error":false,"data":{"timeremaining":7211}}]"#,
                )
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });

    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "auth",
            "time-left",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["remaining_seconds"], 7211);
    assert_eq!(value["data"]["bootstrap_may_have_extended_session"], true);
    let stored = fs::read_to_string(state_path.join("klms/session.json")).unwrap();
    assert!(!stored.contains("abc123"));
}

#[test]
fn explicit_empty_coursework_is_a_complete_success() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let body = match index {
                0 => {
                    assert!(request.starts_with("GET /mod/assign/index.php?id=42 HTTP/1.1"));
                    "<main>There are no assignments in this course.</main>"
                }
                _ => {
                    assert!(request.starts_with("GET /mod/quiz/index.php?id=42 HTTP/1.1"));
                    "<main>No quizzes found.</main>"
                }
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    for resource in ["assignments", "quizzes"] {
        let output = binary()
            .env("XDG_STATE_HOME", &state_path)
            .args([
                "--json",
                "--base-url",
                &format!("http://{address}"),
                resource,
                "list",
                "--course",
                "42",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["data"], serde_json::json!([]));
        assert_eq!(value["meta"]["complete"], true);
        assert_eq!(value["meta"]["total"], 0);
    }
    server.join().unwrap();
}

#[test]
fn schedule_views_accept_an_explicit_empty_calendar() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..length])
                    .starts_with("GET /calendar/view.php?view=upcoming HTTP/1.1")
            );
            let body = "<main class='calendarwrapper'>There are no upcoming events</main>";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    for command in ["today", "upcoming"] {
        let output = binary()
            .env("XDG_STATE_HOME", &state_path)
            .args([
                "--json",
                "--base-url",
                &format!("http://{address}"),
                command,
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["data"], serde_json::json!([]));
        assert_eq!(value["meta"]["complete"], true);
    }
    server.join().unwrap();
}

#[test]
fn typed_show_rejects_a_mismatched_final_resource() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let length = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET /my/ HTTP/1.1"));
        let body = "<main>Dashboard</main>";
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let output = binary()
        .env("XDG_STATE_HOME", &state_path)
        .args([
            "--json",
            "--base-url",
            &format!("http://{address}"),
            "assignments",
            "show",
            "/my/",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "UPSTREAM_SHAPE_CHANGED");
}

#[test]
fn board_post_identity_is_consistent_across_list_and_detail() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let body = if index == 0 {
                assert!(request.starts_with("GET /mod/courseboard/view.php?id=10 HTTP/1.1"));
                "<table class='board-list'><tr><td><a href='/mod/courseboard/article.php?id=10&bwid=11'>Notice</a></td></tr></table>"
            } else {
                assert!(
                    request.starts_with("GET /mod/courseboard/article.php?id=10&bwid=11 HTTP/1.1")
                );
                "<main><h1>Notice</h1><p>Details</p></main>"
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let mut records = Vec::new();
    for args in [
        vec!["boards", "posts", "board:10"],
        vec!["boards", "show", "board-post:10:11"],
        vec!["notices", "show", "board-post:10:11"],
    ] {
        let output = binary()
            .env("XDG_STATE_HOME", &state_path)
            .args(["--json", "--base-url", &format!("http://{address}")])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        records.push(serde_json::from_slice::<Value>(&output.stdout).unwrap());
    }
    server.join().unwrap();
    let listed = &records[0]["data"][0];
    for detail in [&records[1]["data"], &records[2]["data"]] {
        assert_eq!(detail["id"], listed["id"]);
        assert_eq!(detail["board_id"], listed["board_id"]);
        assert_eq!(detail["ref"], listed["ref"]);
    }
}

#[test]
fn top_level_help_exposes_the_agent_resource_surface() {
    let output = binary().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "skill",
        "auth",
        "today",
        "upcoming",
        "courses",
        "activities",
        "assignments",
        "quizzes",
        "calendar",
        "boards",
        "notices",
        "files",
        "videos",
        "grades",
        "attendance",
        "request",
    ] {
        assert!(help.contains(command), "missing {command} in help");
    }
}

#[cfg(unix)]
#[test]
fn skill_install_materializes_embedded_payload_and_discovery_link() {
    let home = TempDir::new().unwrap();
    let data_home = home.path().join("data");
    for _ in 0..2 {
        let output = binary()
            .env("HOME", home.path())
            .env("XDG_DATA_HOME", &data_home)
            .args(["--json", "skill", "install"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["command"], "skill.install");
        assert_eq!(value["data"]["payload_current"], true);
        assert_eq!(value["data"]["link_current"], true);
    }

    let payload_dir = data_home.join("klms/skills/klms");
    let payload = fs::read_to_string(payload_dir.join("SKILL.md")).unwrap();
    let source =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/skills/klms/SKILL.md")).unwrap();
    assert_eq!(payload, source);
    assert_eq!(
        fs::read_link(home.path().join(".agents/skills/klms")).unwrap(),
        payload_dir
    );

    let status = binary()
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", &data_home)
        .args(["--json", "skill", "status"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let value: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["command"], "skill.status");
    assert_eq!(value["data"]["payload_current"], true);
    assert_eq!(value["data"]["link_current"], true);
}

#[cfg(unix)]
#[test]
fn skill_install_refuses_an_unexpected_discovery_path() {
    let home = TempDir::new().unwrap();
    let link = home.path().join(".agents/skills/klms");
    fs::create_dir_all(&link).unwrap();
    let output = binary()
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .args(["--json", "skill", "install"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(40));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "CONFIG_ERROR");
    assert!(link.is_dir());
    assert!(!home.path().join("data/klms").exists());
}

const LIBRARY_DASHBOARD: &str = "<a href='/course/view.php?id=42'>Compilers(CS.420_2026_2)</a>";
const LIBRARY_MANIFEST: &str = "<main class='course-content'>\
    <li class='activity modtype_resource' id='module-7'>\
    <a href='/mod/resource/view.php?id=7'>\
    <span class='instancename'>Lecture One</span></a></li></main>";
const LIBRARY_DETAIL: &str = "<main><h1>Lecture One</h1><p>compiler body</p>\
    <a href='/pluginfile.php/1/mod_resource/content/1/lecture.txt'>lecture.txt</a></main>";

fn library_sync(
    state: &TempDir,
    data: &TempDir,
    server: &Server,
    extra: &[&str],
) -> std::process::Output {
    let state_root = storage_state(state);
    let mut arguments = vec![
        "--json",
        "--base-url",
        server.url().leak(),
        "library",
        "sync",
    ];
    arguments.extend_from_slice(extra);
    binary()
        .env("XDG_STATE_HOME", state_root)
        .env("XDG_DATA_HOME", data.path())
        .args(arguments)
        .output()
        .unwrap()
}

fn library_local(data: &TempDir, arguments: &[&str]) -> std::process::Output {
    binary()
        .env("XDG_DATA_HOME", data.path())
        .arg("--json")
        .args(arguments)
        .output()
        .unwrap()
}

fn success_json(output: std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn basic_library_server() -> Server {
    Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(LIBRARY_DASHBOARD),
        "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
        "/mod/resource/view.php?id=7" => Response::html(LIBRARY_DETAIL),
        target if target.starts_with("/pluginfile.php/") => {
            Response::bytes("text/plain", b"fixture content".to_vec())
                .header("ETag", "\"fixture-v1\"")
        }
        target => panic!("unexpected request: {} {target}", request.method),
    })
}

#[cfg(unix)]
#[test]
fn library_status_initializes_private_paths_without_auth() {
    use std::os::unix::fs::PermissionsExt;
    let data = TempDir::new().unwrap();
    for created in [true, false] {
        let value = success_json(library_local(&data, &["library", "status"]));
        assert_eq!(value["schema_version"], "4");
        assert_eq!(value["data"]["schema_version"], 1);
        assert_eq!(value["data"]["created"], created);
    }
    let root = data.path().join("klms");
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.join("library.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn sync_records_courses_resources_and_representations() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = basic_library_server();
    let sync = success_json(library_sync(&state, &data, &server, &[]));
    assert_eq!(sync["data"]["ref"], "sync:1");
    assert_eq!(sync["data"]["source_complete"], true);
    assert_eq!(sync["data"]["truncated"], 0);
    let course = success_json(library_local(&data, &["library", "show", "course:42"]));
    assert_eq!(
        course["data"]["source"]["title"],
        "Compilers(CS.420_2026_2)"
    );
    let resource = success_json(library_local(&data, &["library", "show", "file:7"]));
    assert_eq!(
        resource["data"]["source"]["text"],
        "Lecture One compiler body lecture.txt"
    );
    let representation = success_json(library_local(
        &data,
        &["library", "show", "representation:1"],
    ));
    assert_eq!(representation["data"]["source"]["filename"], "lecture.txt");
}

#[test]
fn sync_with_notices_walks_board_pages() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(LIBRARY_DASHBOARD),
        "/course/view.php?id=42" => Response::html(
            "<main class='course-content'><li class='activity modtype_courseboard' \
             id='module-9'><a href='/mod/courseboard/view.php?id=9'>\
             <span class='instancename'>Notices</span></a></li></main>",
        ),
        "/mod/courseboard/view.php?id=9" => Response::html(
            "<table class='generaltable'><tr><td><a \
             href='/mod/courseboard/article.php?id=9&bwid=10'>Hello</a></td></tr></table>\
             <a rel='next' href='/mod/courseboard/view.php?id=9&page=2'>Next</a>",
        ),
        "/mod/courseboard/view.php?id=9&page=2" => {
            Response::html("<table class='generaltable'></table>")
        }
        "/mod/courseboard/article.php?id=9&bwid=10" => {
            Response::html("<main><h1>Hello</h1><p>notice body</p></main>")
        }
        target => panic!("unexpected request: {} {target}", request.method),
    });
    success_json(library_sync(&state, &data, &server, &["--notices"]));
    let notice = success_json(library_local(
        &data,
        &["library", "show", "board-post:9:10"],
    ));
    assert_eq!(notice["data"]["kind"], "notice");
    assert!(server.requests().iter().any(|line| line.contains("page=2")));
}

#[test]
fn sync_without_notices_does_not_request_boards() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(LIBRARY_DASHBOARD),
        "/course/view.php?id=42" => Response::html(
            "<main class='course-content'><li class='activity modtype_courseboard' \
             id='module-9'><a href='/mod/courseboard/view.php?id=9'>\
             <span class='instancename'>Notices</span></a></li></main>",
        ),
        target => panic!("unexpected request: {} {target}", request.method),
    });
    success_json(library_sync(&state, &data, &server, &[]));
    assert!(
        !server
            .requests()
            .iter()
            .any(|line| line.contains("courseboard/view"))
    );
}

#[test]
fn files_validates_and_download_changed_stores_bytes_once() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = basic_library_server();
    let first = success_json(library_sync(
        &state,
        &data,
        &server,
        &["--download", "changed"],
    ));
    let second = success_json(library_sync(
        &state,
        &data,
        &server,
        &["--download", "changed"],
    ));
    assert_eq!(first["data"]["blobs_added"], 1);
    assert_eq!(second["data"]["blobs_added"], 0);
    let status = success_json(library_local(&data, &["library", "status"]));
    assert_eq!(status["data"]["blobs"], 1);
    let gets = server
        .requests()
        .iter()
        .filter(|line| line.starts_with("GET /pluginfile.php/"))
        .count();
    assert_eq!(gets, 1);
}

#[test]
fn content_change_appends_history_and_before_after_refs() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let server = Server::new(move |request| {
        let current = router_phase.load(Ordering::Relaxed);
        match request.target.as_str() {
            "/my/" => {
                router_phase.fetch_add(1, Ordering::Relaxed);
                Response::html(LIBRARY_DASHBOARD)
            }
            "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
            "/mod/resource/view.php?id=7" => Response::html(LIBRARY_DETAIL),
            target if target.starts_with("/pluginfile.php/") => {
                let value = if current == 2 { "B" } else { "A" };
                Response::bytes("text/plain", value.as_bytes().to_vec())
                    .header("ETag", &format!("\"{value}\""))
            }
            target => panic!("unexpected request: {} {target}", request.method),
        }
    });
    for _ in 0..4 {
        success_json(library_sync(
            &state,
            &data,
            &server,
            &["--download", "changed"],
        ));
    }
    let history = success_json(library_local(
        &data,
        &["library", "history", "representation:1"],
    ));
    let verified: Vec<_> = history["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "verified_content")
        .collect();
    assert_eq!(verified.len(), 3);
    let changes = success_json(library_local(&data, &["library", "changes"]));
    let content_changes: Vec<_> = changes["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "verified_content_changed")
        .collect();
    assert_eq!(content_changes.len(), 2);
    assert!(content_changes.iter().all(|entry| {
        entry["before_ref"].as_str().unwrap().starts_with("sha256:")
            && entry["after_ref"].as_str().unwrap().starts_with("sha256:")
    }));
}

#[test]
fn failed_dashboard_sync_never_marks_courses_not_listed() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let server = Server::new(move |request| match request.target.as_str() {
        "/my/" if router_phase.fetch_add(1, Ordering::Relaxed) == 0 => {
            Response::html(LIBRARY_DASHBOARD)
        }
        "/my/" => Response::html("<html>incomplete dashboard</html>"),
        "/course/view.php?id=42" => Response::html("<main class='course-content'></main>"),
        target => panic!("unexpected request: {} {target}", request.method),
    });
    success_json(library_sync(&state, &data, &server, &[]));
    assert!(!library_sync(&state, &data, &server, &[]).status.success());
    let course = success_json(library_local(&data, &["library", "show", "course:42"]));
    assert_eq!(course["data"]["remote_state"], "listed");
}

#[test]
fn course_disappears_and_reappears() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let server = Server::new(move |request| match request.target.as_str() {
        "/my/" => {
            let run = router_phase.fetch_add(1, Ordering::Relaxed);
            if run == 1 {
                Response::html("<a href='/course/view.php?id=99'>Databases</a>")
            } else {
                Response::html(LIBRARY_DASHBOARD)
            }
        }
        "/course/view.php?id=42" | "/course/view.php?id=99" => {
            Response::html("<main class='course-content'></main>")
        }
        target => panic!("unexpected request: {} {target}", request.method),
    });
    success_json(library_sync(&state, &data, &server, &[]));
    success_json(library_sync(&state, &data, &server, &[]));
    let hidden = success_json(library_local(&data, &["library", "show", "course:42"]));
    assert_eq!(hidden["data"]["remote_state"], "not_listed");
    success_json(library_sync(&state, &data, &server, &[]));
    let restored = success_json(library_local(&data, &["library", "show", "course:42"]));
    assert_eq!(restored["data"]["remote_state"], "listed");
}

#[test]
fn complete_manifest_marks_missing_resource_and_failed_detail_keeps_state() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let server = Server::new(move |request| {
        let run = router_phase.load(Ordering::Relaxed);
        match request.target.as_str() {
            "/my/" => {
                router_phase.fetch_add(1, Ordering::Relaxed);
                Response::html(LIBRARY_DASHBOARD)
            }
            "/course/view.php?id=42" if run == 2 => {
                Response::html("<main class='course-content'></main>")
            }
            "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
            "/mod/resource/view.php?id=7" if run >= 3 => {
                Response::html("failed").status("500 Internal Server Error")
            }
            "/mod/resource/view.php?id=7" => Response::html(LIBRARY_DETAIL),
            target => panic!("unexpected request: {} {target}", request.method),
        }
    });
    success_json(library_sync(&state, &data, &server, &[]));
    success_json(library_sync(&state, &data, &server, &[]));
    let missing = success_json(library_local(&data, &["library", "show", "file:7"]));
    assert_eq!(missing["data"]["remote_state"], "not_observed");
    let incomplete = success_json(library_sync(&state, &data, &server, &[]));
    assert_eq!(incomplete["data"]["status"], "incomplete");
    let retained = success_json(library_local(&data, &["library", "show", "file:7"]));
    assert_eq!(
        retained["data"]["source"]["text"],
        "Lecture One compiler body lecture.txt"
    );
}

#[test]
fn scoped_sync_validates_only_its_course() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(
            "<a href='/course/view.php?id=42'>Compilers(CS.420)</a>\
             <a href='/course/view.php?id=99'>Databases(CS.430)</a>",
        ),
        "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
        "/mod/resource/view.php?id=7" => Response::html(LIBRARY_DETAIL),
        target if target.starts_with("/pluginfile.php/") => {
            Response::bytes("text/plain", b"fixture".to_vec())
        }
        target => panic!("unexpected request: {} {target}", request.method),
    });
    let value = success_json(library_sync(
        &state,
        &data,
        &server,
        &["--course", "course:42", "--files"],
    ));
    assert_eq!(value["data"]["source_complete"], false);
    assert!(!server.requests().iter().any(|line| line.contains("id=99")));
}

#[test]
fn edit_supersede_conflict_and_retract() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = basic_library_server();
    success_json(library_sync(&state, &data, &server, &[]));
    let first = success_json(library_local(
        &data,
        &[
            "library",
            "edit",
            "file:7",
            "--field",
            "title",
            "--value",
            "Human title",
            "--actor",
            "human",
            "--expected-revision",
            "0",
        ],
    ));
    let second = success_json(library_local(
        &data,
        &[
            "library",
            "edit",
            "file:7",
            "--field",
            "title",
            "--value",
            "Agent title",
            "--actor",
            "agent",
            "--expected-revision",
            "1",
        ],
    ));
    let conflict = library_local(
        &data,
        &[
            "library",
            "edit",
            "file:7",
            "--field",
            "title",
            "--value",
            "Stale",
            "--expected-revision",
            "1",
        ],
    );
    assert_eq!(conflict.status.code(), Some(54));
    success_json(library_local(
        &data,
        &[
            "library",
            "retract",
            second["data"]["ref"].as_str().unwrap(),
        ],
    ));
    let show = success_json(library_local(&data, &["library", "show", "file:7"]));
    assert_eq!(show["data"]["effective"]["title"], "Human title");
    assert_eq!(first["data"]["actor"], "human");
}

#[test]
fn summary_stale_after_source_change() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let server = Server::new(move |request| match request.target.as_str() {
        "/my/" => {
            router_phase.fetch_add(1, Ordering::Relaxed);
            Response::html(LIBRARY_DASHBOARD)
        }
        "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
        "/mod/resource/view.php?id=7" => Response::html(format!(
            "<main><h1>Lecture</h1><p>body {}</p></main>",
            router_phase.load(Ordering::Relaxed)
        )),
        target => panic!("unexpected request: {} {target}", request.method),
    });
    success_json(library_sync(&state, &data, &server, &[]));
    success_json(library_local(
        &data,
        &[
            "library",
            "edit",
            "file:7",
            "--field",
            "summary",
            "--value",
            "Summary",
            "--expected-revision",
            "0",
        ],
    ));
    success_json(library_sync(&state, &data, &server, &[]));
    let show = success_json(library_local(&data, &["library", "show", "file:7"]));
    assert_eq!(show["data"]["effective"]["summary_stale"], true);
}

#[test]
fn content_is_ambiguous_with_two_stored_representations_and_export_refuses_overwrite() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(LIBRARY_DASHBOARD),
        "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
        "/mod/resource/view.php?id=7" => Response::html(
            "<main><a href='/pluginfile.php/one'>one.txt</a>\
             <a href='/pluginfile.php/two'>two.txt</a></main>",
        ),
        "/pluginfile.php/one" => Response::bytes("text/plain", b"one".to_vec()),
        "/pluginfile.php/two" => Response::bytes("text/plain", b"two".to_vec()),
        target => panic!("unexpected request: {} {target}", request.method),
    });
    success_json(library_sync(
        &state,
        &data,
        &server,
        &["--download", "changed"],
    ));
    let ambiguous = library_local(&data, &["library", "content", "file:7"]);
    assert_eq!(ambiguous.status.code(), Some(55));
    let destination = data.path().join("out.txt");
    fs::write(&destination, b"keep").unwrap();
    let refusal = library_local(
        &data,
        &[
            "library",
            "export",
            "representation:1",
            "--out",
            destination.to_str().unwrap(),
        ],
    );
    assert!(!refusal.status.success());
    assert_eq!(fs::read(destination).unwrap(), b"keep");
}

#[test]
fn search_matches_source_text_and_active_curation_only() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = basic_library_server();
    success_json(library_sync(&state, &data, &server, &[]));
    let source = success_json(library_local(&data, &["library", "search", "compiler"]));
    assert!(
        source["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["ref"] == "file:7")
    );
    let edit = success_json(library_local(
        &data,
        &[
            "library",
            "edit",
            "file:7",
            "--field",
            "note",
            "--value",
            "zirconium",
            "--expected-revision",
            "0",
        ],
    ));
    let curated = success_json(library_local(&data, &["library", "search", "zirconium"]));
    assert_eq!(curated["data"][0]["ref"], "file:7");
    success_json(library_local(
        &data,
        &["library", "retract", edit["data"]["ref"].as_str().unwrap()],
    ));
    let absent = success_json(library_local(&data, &["library", "search", "zirconium"]));
    assert!(absent["data"].as_array().unwrap().is_empty());
}

#[test]
fn relations_add_conflict_and_retract() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = basic_library_server();
    success_json(library_sync(&state, &data, &server, &[]));
    let args = [
        "library",
        "relations",
        "add",
        "course:42",
        "file:7",
        "--kind",
        "related_to",
        "--actor",
        "agent",
    ];
    let relation = success_json(library_local(&data, &args));
    let conflict = library_local(&data, &args);
    assert_eq!(conflict.status.code(), Some(54));
    success_json(library_local(
        &data,
        &[
            "library",
            "retract",
            relation["data"]["ref"].as_str().unwrap(),
        ],
    ));
    success_json(library_local(&data, &args));
    let activity = success_json(library_local(
        &data,
        &["library", "activity", "--subject", "course:42"],
    ));
    assert_eq!(activity["data"].as_array().unwrap().len(), 2);
}

#[test]
fn truncated_detail_links_never_mark_representations_not_observed() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let many_links: String = (0..101)
        .map(|index| {
            format!(
                "<a href='/pluginfile.php/1/mod_resource/content/1/extra-{index}.txt'>\
                 extra {index}</a>"
            )
        })
        .collect();
    let server = Server::new(move |request| {
        let run = router_phase.load(Ordering::Relaxed);
        match request.target.as_str() {
            "/my/" => {
                router_phase.fetch_add(1, Ordering::Relaxed);
                Response::html(LIBRARY_DASHBOARD)
            }
            "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
            "/mod/resource/view.php?id=7" if run == 2 => {
                Response::html(format!("<main><h1>Lecture One</h1>{many_links}</main>"))
            }
            "/mod/resource/view.php?id=7" => Response::html(LIBRARY_DETAIL),
            target => panic!("unexpected request: {} {target}", request.method),
        }
    });
    success_json(library_sync(&state, &data, &server, &[]));
    // Parser caps mark the observation incomplete without failing the run.
    let truncated = success_json(library_sync(&state, &data, &server, &[]));
    assert_eq!(truncated["data"]["status"], "complete");
    assert_eq!(truncated["data"]["truncated"], 1);
    let lecture = success_json(library_local(
        &data,
        &["library", "show", "representation:1"],
    ));
    assert_eq!(lecture["data"]["source"]["filename"], "lecture.txt");
    assert_eq!(lecture["data"]["remote_state"], "present");
    let changes = success_json(library_local(&data, &["library", "changes"]));
    assert!(
        !changes["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "representation_not_observed")
    );
}

#[test]
fn head_validated_download_sends_no_conditional_headers() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = basic_library_server();
    success_json(library_sync(&state, &data, &server, &["--files"]));
    let heads = server
        .requests()
        .iter()
        .filter(|line| line.starts_with("HEAD /pluginfile.php/"))
        .count();
    assert_eq!(heads, 1);
    let download = success_json(library_sync(
        &state,
        &data,
        &server,
        &["--download", "changed"],
    ));
    assert_eq!(download["data"]["blobs_added"], 1);
    let gets: Vec<_> = server
        .recorded()
        .into_iter()
        .filter(|request| request.line.starts_with("GET /pluginfile.php/"))
        .collect();
    assert_eq!(gets.len(), 1);
    assert!(!gets[0].has_header("If-None-Match"));
    assert!(!gets[0].has_header("If-Modified-Since"));
}

#[test]
fn forbidden_detail_records_access_lost_then_restored() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let server = Server::new(move |request| {
        let run = router_phase.load(Ordering::Relaxed);
        match request.target.as_str() {
            "/my/" => {
                router_phase.fetch_add(1, Ordering::Relaxed);
                Response::html(LIBRARY_DASHBOARD)
            }
            "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
            "/mod/resource/view.php?id=7" if run == 2 => {
                Response::html("forbidden").status("403 Forbidden")
            }
            "/mod/resource/view.php?id=7" => Response::html(LIBRARY_DETAIL),
            target => panic!("unexpected request: {} {target}", request.method),
        }
    });
    success_json(library_sync(&state, &data, &server, &[]));
    let lost = success_json(library_sync(&state, &data, &server, &[]));
    assert_eq!(lost["data"]["status"], "incomplete");
    let hidden = success_json(library_local(&data, &["library", "show", "file:7"]));
    assert_eq!(hidden["data"]["remote_state"], "access_lost");
    assert_eq!(
        hidden["data"]["source"]["text"],
        "Lecture One compiler body lecture.txt"
    );
    success_json(library_sync(&state, &data, &server, &[]));
    let restored = success_json(library_local(&data, &["library", "show", "file:7"]));
    assert_eq!(restored["data"]["remote_state"], "present");
    let changes = success_json(library_local(&data, &["library", "changes"]));
    let kinds: Vec<_> = changes["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["subject_ref"] == "file:7")
        .map(|entry| entry["kind"].as_str().unwrap().to_owned())
        .collect();
    assert!(kinds.contains(&"access_lost".to_owned()), "{kinds:?}");
    assert!(kinds.contains(&"access_restored".to_owned()), "{kinds:?}");
}

#[test]
fn reverted_source_keeps_every_observation_in_history() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let phase = std::sync::Arc::new(AtomicUsize::new(0));
    let router_phase = std::sync::Arc::clone(&phase);
    let server = Server::new(move |request| {
        let run = router_phase.load(Ordering::Relaxed);
        match request.target.as_str() {
            "/my/" => {
                let run = router_phase.fetch_add(1, Ordering::Relaxed);
                if run == 1 {
                    Response::html("<a href='/course/view.php?id=42'>Renamed Compilers</a>")
                } else {
                    Response::html(LIBRARY_DASHBOARD)
                }
            }
            "/course/view.php?id=42" => Response::html(LIBRARY_MANIFEST),
            "/mod/resource/view.php?id=7" if run == 2 => {
                Response::html("<main><h1>Lecture One</h1><p>revised body</p></main>")
            }
            "/mod/resource/view.php?id=7" => Response::html(LIBRARY_DETAIL),
            target => panic!("unexpected request: {} {target}", request.method),
        }
    });
    for _ in 0..3 {
        success_json(library_sync(&state, &data, &server, &[]));
    }
    let course = success_json(library_local(&data, &["library", "show", "course:42"]));
    assert_eq!(
        course["data"]["source"]["title"],
        "Compilers(CS.420_2026_2)"
    );
    for (reference, kind) in [
        ("course:42", "course_source"),
        ("file:7", "resource_source"),
    ] {
        let history = success_json(library_local(&data, &["library", "history", reference]));
        let observations = history["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"] == kind)
            .count();
        assert_eq!(observations, 3, "{reference}");
    }
}

#[test]
fn assignment_activity_is_stored_under_its_parser_ref() {
    let state = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(LIBRARY_DASHBOARD),
        "/course/view.php?id=42" => Response::html(
            "<main class='course-content'><li class='activity modtype_assign' \
             id='module-5'><a href='/mod/assign/view.php?id=5'>\
             <span class='instancename'>Homework One</span></a></li></main>",
        ),
        "/mod/assign/view.php?id=5" => {
            Response::html("<main><h1>Homework One</h1><p>submit a parser</p></main>")
        }
        target => panic!("unexpected request: {} {target}", request.method),
    });
    success_json(library_sync(&state, &data, &server, &[]));
    let assignment = success_json(library_local(&data, &["library", "show", "assign:5"]));
    assert_eq!(assignment["data"]["ref"], "assign:5");
    assert_eq!(assignment["data"]["kind"], "assign");
    assert_eq!(
        assignment["data"]["source"]["text"],
        "Homework One submit a parser"
    );
    let search = success_json(library_local(&data, &["library", "search", "parser"]));
    assert_eq!(search["data"][0]["ref"], "assign:5");
}
