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
    let path = directory.path().join("state.json");
    fs::write(&path, r#"{"cookies":[{"name":"MoodleSession","value":"test-session","domain":"127.0.0.1","path":"/","secure":false}]}"#).unwrap();
    path
}

#[test]
fn json_auth_status_is_one_document_without_secrets() {
    let home = TempDir::new().unwrap();
    let output = binary()
        .env("HOME", home.path())
        .env_remove("KLMS_STORAGE_STATE")
        .env_remove("XDG_CONFIG_HOME")
        .args(["--json", "auth", "status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], "2");
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["configured"], false);
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
fn auth_time_left_uses_cached_sesskey_without_dashboard_touch() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8192];
        let length = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..length]);
        assert!(request.starts_with("POST /lib/ajax/service.php?"));
        assert!(request.contains("info=core_session_time_remaining"));
        let body = r#"[{"error":false,"data":{"timeremaining":7211}}]"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });

    let state_dir = TempDir::new().unwrap();
    let state_path = storage_state(&state_dir);
    let cache_dir = state_dir.path().join("cache/klms");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(
        cache_dir.join("session.json"),
        format!(
            r#"{{"origin":"http://127.0.0.1:{}","sesskey":"abc123","stored_at":1}}"#,
            address.port()
        ),
    )
    .unwrap();
    let output = binary()
        .env("KLMS_STORAGE_STATE", &state_path)
        .env("XDG_CACHE_HOME", state_dir.path().join("cache"))
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
    assert_eq!(value["data"]["bootstrap_may_have_extended_session"], false);
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
            .env("KLMS_STORAGE_STATE", &state_path)
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
            .env("KLMS_STORAGE_STATE", &state_path)
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
        .env("KLMS_STORAGE_STATE", &state_path)
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
fn top_level_help_exposes_the_agent_resource_surface() {
    let output = binary().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
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
