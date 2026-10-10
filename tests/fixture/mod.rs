// Each integration target uses a different subset of this shared harness.
#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::Value;
use tempfile::TempDir;

pub mod server;
use server::{Request, Response, Server};

/// Creates `<state_root>/klms/session.json` with the given cookie value and returns `state_root`.
pub fn seed_session(state_root: &Path, cookie: &str) -> PathBuf {
    fs::create_dir_all(state_root.join("klms")).unwrap();
    fs::write(
        state_root.join("klms/session.json"),
        format!(
            r#"{{"version":1,"origin":"http://127.0.0.1:0","created_at":1,"cookies":[{{"name":"MoodleSession","value":"{cookie}"}}],"devices":[]}}"#
        ),
    )
    .unwrap();
    state_root.to_path_buf()
}

/// Isolated HOME/XDG directories around the real binary, optionally pointed at a fixture server.
pub struct Env {
    pub dir: TempDir,
    base: Option<String>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            base: None,
        }
    }

    /// An environment holding a saved session that talks to `server`.
    pub fn at(server: &Server) -> Self {
        let mut env = Self::new();
        env.state();
        env.base = Some(server.url());
        env
    }

    /// Seeds the saved session and returns the XDG state root.
    pub fn state(&mut self) -> PathBuf {
        seed_session(&self.dir.path().join("state"), "test-session")
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Whitespace-separated arguments; single quotes group words.
    pub fn cmd(&self, json: bool, args: &str) -> Command {
        let words = args.split('\'').enumerate().flat_map(|(i, part)| {
            let split: Vec<&str> = if i % 2 == 1 {
                vec![part]
            } else {
                part.split_whitespace().collect()
            };
            split
        });

        let mut command = Command::new(env!("CARGO_BIN_EXE_klms"));
        command
            .env("HOME", self.dir.path())
            .env("XDG_DATA_HOME", self.dir.path())
            .env("XDG_STATE_HOME", self.path("state"))
            .env_remove("XDG_CONFIG_HOME");
        if json {
            command.arg("--json");
        }
        if let Some(base) = &self.base {
            command.args(["--base-url", base]);
        }
        command.args(words);
        command
    }

    /// Human-mode output.
    pub fn run(&self, args: &str) -> Output {
        self.cmd(false, args).output().unwrap()
    }

    /// `--json` output, any status.
    pub fn out(&self, args: &str) -> Output {
        self.cmd(true, args).output().unwrap()
    }

    /// The exit code of a `--json` run.
    pub fn code(&self, args: &str) -> i32 {
        self.out(args).status.code().unwrap()
    }

    /// Runs `--json`, requires success with a quiet stderr, and returns the envelope.
    pub fn ok(&self, args: &str) -> Value {
        let output = self.out(args);
        assert!(
            output.status.success(),
            "{args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "{args:?}: stderr not empty");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub fn data(&self, args: &str) -> Value {
        self.ok(args)["data"].clone()
    }

    /// Runs `--json`, requires a failure with empty stdout, and returns (exit code, error object).
    pub fn fail(&self, args: &str) -> (i32, Value) {
        let output = self.out(args);
        assert!(output.stdout.is_empty(), "{args:?}: stdout not empty");
        let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(envelope["ok"], false);
        (output.status.code().unwrap(), envelope["error"].clone())
    }

    pub fn sync(&self, extra: &str) -> Value {
        self.ok(&format!("library sync {extra}"))
    }

    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.path("klms/library.db")).unwrap()
    }
}

/// A shared counter for routers whose answers change between runs.
#[derive(Clone, Default)]
pub struct Counter(Arc<AtomicUsize>);

impl Counter {
    pub fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
    pub fn set(&self, value: usize) {
        self.0.store(value, Ordering::SeqCst);
    }
    /// Increments and returns the previous value.
    pub fn bump(&self) -> usize {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

pub const DASHBOARD: &str = "<a href='/course/view.php?id=42'>Compilers(CS.420_2026_2)</a>";
pub const DETAIL: &str = "<main><h1>Lecture One</h1><p>compiler body</p>\
    <a href='/pluginfile.php/1/lecture.txt'>lecture.txt</a></main>";
pub const FILE_TEXT: &str = "Lecture One compiler body lecture.txt";
pub const BYTES: &str = "fixture bytes";
pub const NOTICE_LIST: &str = "<table class='generaltable'><tr><td>\
    <a href='/mod/courseboard/article.php?id=9&bwid=10'>Hello</a></td></tr></table>";

/// A course page listing `(modtype, id, name)` activities.
pub fn course(modules: &[(&str, u32, &str)]) -> String {
    let items: String = modules
        .iter()
        .map(|(kind, id, name)| {
            format!(
                "<li class='activity modtype_{kind}' id='module-{id}'><a href='/mod/{kind}/view.php?id={id}'>\
                 <span class='instancename'>{name}</span></a></li>"
            )
        })
        .collect();
    format!("<main class='course-content'>{items}</main>")
}

pub fn manifest() -> String {
    course(&[("resource", 7, "Lecture One")])
}

pub fn notice_manifest() -> String {
    course(&[("courseboard", 9, "Notices")])
}

/// A board article with `files` inside `.info .files` and `body` as the content.
pub fn article(title: &str, files: &str, body: &str) -> String {
    format!(
        "<div class='courseboard_view'><div class='subject'><h3>{title}</h3></div>\
         <div class='info'><div class='files'>{files}</div></div>\
         <div class='content'>{body}</div></div>"
    )
}

/// A fixture KLMS with one course, one resource and one downloadable file. `over` receives
/// each request and the 0-based sync number (dashboard hits so far minus one) and may answer first.
pub fn lib_server(
    over: impl Fn(&Request, usize) -> Option<Response> + Send + Sync + 'static,
) -> Server {
    let syncs = Counter::default();
    Server::new(move |request| {
        if request.target == "/my/" {
            syncs.bump();
        }
        let run = syncs.get().saturating_sub(1);
        over(request, run).unwrap_or_else(|| match request.target.as_str() {
            "/my/" => Response::html(DASHBOARD),
            "/course/view.php?id=42" => Response::html(manifest()),
            "/mod/resource/view.php?id=7" => Response::html(DETAIL),
            target if target.starts_with("/pluginfile.php/") => {
                Response::bytes("text/plain", BYTES).header("ETag", "\"fixture-v1\"")
            }
            target => panic!("unexpected request: {} {target}", request.method),
        })
    })
}
