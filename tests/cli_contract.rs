use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    thread,
};

use serde_json::Value;
use tempfile::TempDir;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_klms"))
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
    assert_eq!(value["schema_version"], "1");
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
    let state_path = state_dir.path().join("state.json");
    fs::write(
        &state_path,
        r#"{"cookies":[{"name":"MoodleSession","value":"test-session","domain":"127.0.0.1","path":"/","secure":false}]}"#,
    )
    .unwrap();
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
}
