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
    assert_eq!(value["schema_version"], "3");
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
