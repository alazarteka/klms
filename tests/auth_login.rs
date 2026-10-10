//! CLI-level contract for remembered logins, forgetting, and the
//! non-interactive two-step password login. Passwords live in a plaintext
//! credentials file inside a temp dir; no OS keyring is ever consulted.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use serde_json::Value;
use tempfile::TempDir;

mod fixture;
use fixture::server::{Response, Server};

struct Home(TempDir);

impl Home {
    fn new() -> Self {
        Self(TempDir::new().unwrap())
    }

    fn config(&self) -> PathBuf {
        self.0.path().join("config/klms")
    }

    fn state(&self) -> PathBuf {
        self.0.path().join("state/klms")
    }

    /// Runs klms with no terminal on stdin and no helper programs on PATH.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_klms"))
            .env("HOME", self.0.path())
            .env("XDG_CONFIG_HOME", self.0.path().join("config"))
            .env("XDG_STATE_HOME", self.0.path().join("state"))
            .env("PATH", self.0.path().join("bin"))
            .stdin(Stdio::null())
            .args(args)
            .output()
            .unwrap()
    }

    /// `klms --json --base-url URL auth login EXTRA...`
    fn login(&self, url: &str, extra: &[&str]) -> Output {
        let head = ["--json", "--base-url", url, "auth", "login"];
        self.run(&[&head[..], extra].concat())
    }

    /// A remembered login, with its password in the credentials file if asked.
    fn seed(&self, password: bool) {
        fs::create_dir_all(self.config()).unwrap();
        let backend = if password {
            r#","password_backend":"plaintext-file""#
        } else {
            ""
        };
        let login = format!(
            r#"{{"version":1,"username":"student","method":"password","second_factor":"email"{backend}}}"#
        );
        fs::write(self.config().join("login.json"), login).unwrap();
        if password {
            let credentials = r#"{"version":1,"passwords":{"student":"s3cret"}}"#;
            fs::write(self.config().join("credentials.json"), credentials).unwrap();
        }
    }
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn help_documents_the_login_flags_and_the_parser_rejects_bad_combinations() {
    let home = Home::new();
    let help = String::from_utf8(home.run(&["auth", "login", "--help"]).stdout).unwrap();
    for needle in [
        "--user <ID>",
        "--remember-password",
        "--insecure-storage",
        "--code <CODE>",
        "CODE_REQUIRED",
        "plaintext",
        "Signing in as",
        "12 CODE_REQUIRED",
        "login.json",
    ] {
        assert!(help.contains(needle), "missing {needle:?} in:\n{help}");
    }
    assert!(!help.contains("--password") && !help.contains("--otp"));
    let forget = String::from_utf8(home.run(&["auth", "forget", "--help"]).stdout).unwrap();
    assert!(forget.contains("plaintext credentials file") && forget.contains("keychain"));

    for args in [
        &["--insecure-storage"][..],
        &["--code", "123456", "--method", "password"],
        &["--code", "123456", "--user", "x"],
        &["--code", "123456", "--remember-password"],
    ] {
        let output = home.run(&[&["--json", "auth", "login"][..], args].concat());
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(json(&output.stderr)["error"]["code"], "USAGE");
    }
}

#[test]
fn status_reports_remembered_login_and_backend_without_secrets() {
    let home = Home::new();
    let none = home.run(&["--json", "auth", "status"]);
    assert!(json(&none.stdout)["data"]["remembered"].is_null());

    home.seed(true);
    let output = home.run(&["--json", "auth", "status"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("s3cret"));
    let remembered = &json(&output.stdout)["data"]["remembered"];
    assert_eq!(remembered["username"], "student");
    assert_eq!(remembered["method"], "password");
    assert_eq!(remembered["second_factor"], "email");
    assert_eq!(remembered["password_backend"], "plaintext-file");

    home.seed(false);
    let bare = json(&home.run(&["--json", "auth", "status"]).stdout);
    assert_eq!(bare["data"]["remembered"]["password_backend"], "none");
    let human = String::from_utf8(home.run(&["auth", "status"]).stdout).unwrap();
    assert!(human.contains("Remembered login: student (password, email code)"));
    assert!(human.contains("Stored password: none"));
}

#[test]
fn logout_keeps_the_remembered_login_and_forget_removes_it() {
    let home = Home::new();
    home.seed(true);
    let state_root = home.0.path().join("state");
    fixture::seed_session(&state_root, "cookie");
    fs::write(home.state().join("pending-login.json"), "{}").unwrap();

    assert!(home.run(&["--json", "auth", "logout"]).status.success());
    assert!(!home.state().join("session.json").exists());
    assert!(!home.state().join("pending-login.json").exists());
    assert!(home.config().join("login.json").is_file());
    assert!(home.config().join("credentials.json").is_file());

    fixture::seed_session(&state_root, "cookie");
    let forget = home.run(&["--json", "auth", "forget"]);
    assert!(forget.status.success(), "{:?}", forget.stderr);
    let value = json(&forget.stdout);
    assert_eq!(value["command"], "auth.forget");
    assert_eq!(value["data"]["login_removed"], true);
    assert_eq!(value["data"]["password_removed"], true);
    assert_eq!(value["data"]["password_backend"], "plaintext-file");
    assert!(!home.config().join("login.json").exists());
    assert!(!home.config().join("credentials.json").exists());
    assert!(
        home.state().join("session.json").is_file(),
        "session untouched"
    );

    let again = json(&home.run(&["--json", "auth", "forget"]).stdout);
    assert_eq!(again["data"]["login_removed"], false);
    assert_eq!(again["data"]["password_removed"], false);
}

#[test]
fn non_interactive_login_fails_clearly_before_touching_the_network() {
    let home = Home::new();
    // (extra args, exit code, message part, hint part)
    for (extra, exit, message, hint) in [
        (
            &["--user", "student"][..],
            10,
            "approve",
            "--method password",
        ),
        (
            &["--user", "student", "--method", "password"],
            10,
            "no stored password",
            "--remember-password",
        ),
        (&["--method", "password"], 2, "--user", ""),
        (
            &[
                "--method",
                "password",
                "--remember-password",
                "--insecure-storage",
            ],
            2,
            "terminal",
            "",
        ),
    ] {
        let output = home.login("http://127.0.0.1:9", extra);
        assert_eq!(output.status.code(), Some(exit), "{extra:?}");
        let error = &json(&output.stderr)["error"];
        assert!(
            error["message"].as_str().unwrap().contains(message),
            "{extra:?}"
        );
        assert!(
            error["hint"].as_str().unwrap_or_default().contains(hint),
            "{extra:?}"
        );
    }
    assert!(!home.config().join("credentials.json").exists());
}

fn sso_router(target: &str) -> Response {
    let json = |body: &str| Response::bytes("application/json", body.as_bytes().to_vec());
    match target.split('?').next().unwrap() {
        "/auth/kaist/user/login/view" => {
            Response::html("login").header("Set-Cookie", "sso-session=one; Path=/")
        }
        "/auth/user/login/init" => json(&format!(r#"{{"result_data":"{}"}}"#, "00".repeat(48))),
        "/auth/user/login/auth" => json(r#"{"result_code":"SS0098"}"#),
        "/auth/kaist/user/login/second/view" => Response::html("second"),
        "/auth/kaist/user/login/second/ajaxSendMail" => json(r#"{"errorCode":"SS0001"}"#),
        "/auth/kaist/user/login/second/ajaxValidCrtfcNo" => json(r#"{"result_code":"SS0001"}"#),
        "/auth/user/login/link" => Response::html("")
            .status("302 Found")
            .header("Location", "/"),
        "/" => Response::html("ok").header("Set-Cookie", "MoodleSession=owned; Path=/; HttpOnly"),
        other => panic!("unexpected request {other}"),
    }
}

#[test]
fn two_step_password_login_end_to_end() {
    let home = Home::new();
    home.seed(true);
    let server = Server::new(|request| sso_router(&request.target));
    let url = server.url();
    let pending = home.state().join("pending-login.json");

    let first = home.login(&url, &[]);
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert_eq!(first.status.code(), Some(12), "{stderr}");
    assert!(first.stdout.is_empty() && !stderr.contains("s3cret"));
    let error = &json(&first.stderr)["error"];
    assert_eq!(error["code"], "CODE_REQUIRED");
    assert_eq!(error["retryable"], false);
    assert_eq!(error["details"]["channel"], "email");
    assert_eq!(error["details"]["resume"], "klms auth login --code CODE");
    assert!(error["details"]["expires_at"].as_u64().unwrap() > 1_700_000_000);
    assert_eq!(mode(&pending), 0o600);
    assert!(!fs::read_to_string(&pending).unwrap().contains("s3cret"));
    assert!(!home.state().join("session.json").exists());

    // A malformed code is a usage error and does not consume the pending login.
    let bad = home.login(&url, &["--code", "12x"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(pending.is_file());

    let second = home.login(&url, &["--code", "123456"]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let value = json(&second.stdout);
    assert_eq!(value["command"], "auth.login");
    assert_eq!(value["data"]["user"], "student");
    assert_eq!(value["data"]["method"], "password");
    assert!(!String::from_utf8_lossy(&second.stdout).contains("owned"));
    assert!(!pending.exists());
    assert_eq!(mode(&home.state().join("session.json")), 0o600);
    let bodies: Vec<_> = server
        .recorded()
        .into_iter()
        .filter(|seen| seen.line.contains("ajaxValidCrtfcNo"))
        .map(|seen| seen.body)
        .collect();
    assert_eq!(bodies, ["crtfc_no=123456"]);

    // Resuming again has nothing to resume.
    let third = home.login(&url, &["--code", "123456"]);
    assert_eq!(third.status.code(), Some(10));
    let message = &json(&third.stderr)["error"]["message"];
    assert!(message.as_str().unwrap().contains("no login is waiting"));
}

#[test]
fn bare_login_announces_the_remembered_identity_on_stderr() {
    let home = Home::new();
    home.seed(true);
    let server = Server::new(|request| sso_router(&request.target));
    let output = home.run(&["--base-url", &server.url(), "auth", "login"]);
    assert_eq!(output.status.code(), Some(12));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Signing in as student (password)"),
        "{stderr}"
    );
    assert!(stderr.contains("error [CODE_REQUIRED]"));
    assert!(stderr.contains("klms auth login --code CODE"));
    assert!(!stderr.contains("s3cret"));
}
