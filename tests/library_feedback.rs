use std::{
    fs,
    process::Stdio,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

mod fixture;
use fixture::server::{Response, Server};
use fixture::{
    BYTES, Counter, Env, FILE_TEXT, NOTICE_LIST, article, course, lib_server, notice_manifest,
};

const ARTICLE: &str = "/mod/courseboard/article.php?id=9&bwid=10";
const NOTICE_SYNC_HINT: &str = "library sync --course course:42 --notices --download changed";

fn html(body: impl Into<Vec<u8>>) -> Option<Response> {
    Some(Response::html(body))
}

/// A server whose course holds one notice (and, optionally, the default resource).
fn notice_server(article_html: impl Fn(usize) -> String + Send + Sync + 'static) -> Server {
    lib_server(move |r, run| match r.target.as_str() {
        "/course/view.php?id=42" => html(notice_manifest()),
        "/mod/courseboard/view.php?id=9" => html(NOTICE_LIST),
        ARTICLE => html(article_html(run)),
        _ => None,
    })
}

fn rep(shown: &Value, url_suffix: &str) -> String {
    shown["representations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["url"].as_str().unwrap().ends_with(url_suffix))
        .unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Both byte operations must fail locally (exit 55, same diagnostics in JSON and human
/// mode) without creating a partial export. Returns the shared error.
fn byte_errors(env: &Env, reference: &str) -> Value {
    let destination = env.path("unavailable-export");
    let mut expected: Option<Value> = None;
    for operation in ["content", "export"] {
        let mut args = format!("library {operation} {reference}");
        if operation == "export" {
            args += &format!(" --out {}", destination.display());
        }
        let (code, error) = env.fail(&args);
        assert_eq!(code, 55);
        if let Some(expected) = &expected {
            assert_eq!(&error, expected, "content/export diagnostics diverged");
        }
        let human = env.run(&args);
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

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap().to_owned()
}

fn has(value: &Value, needle: &str) {
    assert!(
        value.as_str().unwrap().contains(needle),
        "{value}: missing {needle}"
    );
}

fn no_sync_remedy(error: &Value) {
    assert!(!error.to_string().contains("library sync"), "{error}");
}

#[test]
fn non_file_links_never_recommend_downloads_or_resolve_a_sibling_attachment() {
    let server = notice_server(|_| {
        article(
            "Hello",
            "<a href='/pluginfile.php/one'>one.txt</a>",
            "Stored notice text<a href='/mod/courseboard/article.php?id=9&amp;bwid=10'>Permalink</a>\
             <a href='https://example.org/reading'>External reading</a>",
        )
    });
    let env = Env::at(&server);
    env.sync("--notices");
    let shown = env.data("library show board-post:9:10");
    let links = [rep(&shown, ARTICLE), rep(&shown, "/reading")];
    let attachment = rep(&shown, "/pluginfile.php/one");
    let before = server.requests().len();
    assert!(
        byte_errors(&env, &attachment)["hint"]
            .as_str()
            .unwrap()
            .contains(NOTICE_SYNC_HINT)
    );
    let link_errors = || {
        for link in &links {
            let error = byte_errors(&env, link);
            has(&error["message"], "link");
            let hint = error["hint"].as_str().unwrap();
            assert!(hint.contains(&format!("library show {link}")), "{hint}");
            assert!(hint.contains("library show board-post:9:10"), "{hint}");
            assert!(hint.contains("data.source.text"), "{hint}");
            no_sync_remedy(&error);
        }
    };
    link_errors();
    assert_eq!(server.requests().len(), before);

    env.sync("--notices --download changed");
    let bytes = env.data(&format!("library content {}", attachment));
    assert_eq!(bytes["text"], BYTES);
    assert_eq!(env.data("library content board-post:9:10"), bytes);
    let destination = env.path("notice-attachment.txt");
    env.ok(&format!(
        "library export board-post:9:10 --out {}",
        destination.to_str().unwrap()
    ));
    assert_eq!(fs::read(destination).unwrap(), BYTES.as_bytes());
    let before = server.requests().len();
    link_errors();
    assert_eq!(server.requests().len(), before);
}

#[test]
fn resources_without_file_candidates_do_not_invent_a_download_remedy() {
    let truncated = Counter::default();
    let flag = truncated.clone();
    let server = lib_server(move |r, _| match r.target.as_str() {
        "/course/view.php?id=42" => html(course(&[
            ("courseboard", 9, "Notices"),
            ("lti", 11, "Video tool"),
        ])),
        "/mod/courseboard/view.php?id=9" => html(NOTICE_LIST),
        ARTICLE => html(article(
            "Hello",
            "",
            &"x".repeat(if flag.get() == 1 { 100_001 } else { 0 }),
        )),
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("--notices");
    let lti = env.data("library show lti:11");
    assert_eq!(lti["representations"], json!([]));
    let notice = env.data("library show board-post:9:10");
    assert_eq!(notice["source"]["text"], "");
    let before = server.requests().len();
    for reference in ["lti:11", "board-post:9:10"] {
        let error = byte_errors(&env, reference);
        has(&error["message"], "no downloaded file bytes");
        has(&error["hint"], &format!("library show {reference}"));
        no_sync_remedy(&error);
    }
    let missing_hash = format!("sha256:{}", "0".repeat(64));
    for reference in ["file:999", "representation:999", &missing_hash] {
        let error = byte_errors(&env, reference);
        assert_eq!(error["message"], "no stored content for reference");
        no_sync_remedy(&error);
    }
    assert_eq!(server.requests().len(), before);

    truncated.set(1);
    assert_eq!(env.sync("--notices")["data"]["truncated"], 1);
    let shown = env.data("library show board-post:9:10");
    assert_eq!(shown["source"]["complete"], false);
    let before = server.requests().len();
    let error = byte_errors(&env, "board-post:9:10");
    has(&error["hint"], "data.source.text");
    no_sync_remedy(&error);
    assert_eq!(server.requests().len(), before);
}

#[test]
fn previously_observed_file_without_bytes_directs_to_parent_not_download_retry() {
    let gone = Counter::default();
    let flag = gone.clone();
    let server = lib_server(move |r, _| {
        (r.target == "/mod/resource/view.php?id=7" && flag.get() == 1).then(|| {
            Response::html(
                "<main><h1>Lecture</h1><p>The current observation has no file links.</p></main>",
            )
        })
    });
    let env = Env::at(&server);
    env.sync("");
    let file = rep(&env.data("library show file:7"), "/lecture.txt");
    for reference in ["file:7", file.as_str()] {
        let error = byte_errors(&env, reference);
        has(&error["message"], "metadata");
        has(
            &error["hint"],
            "library sync --course course:42 --download changed",
        );
    }
    gone.set(1);
    env.sync("");
    let shown = env.data(&format!("library show {}", file));
    assert_eq!(shown["remote_state"], "not_observed");
    assert!(shown["content"].is_null());
    let before = server.requests().len();
    let error = byte_errors(&env, &file);
    has(&error["message"], "not observed");
    has(&error["hint"], "library show file:7");
    no_sync_remedy(&error);
    no_sync_remedy(&byte_errors(&env, "file:7"));
    assert_eq!(server.requests().len(), before);
}

#[test]
fn status_distinguishes_never_synced_scoped_and_global_coverage() {
    let server = lib_server(|_, _| None);
    let env = Env::at(&server);
    // Never synced: initializes private storage without needing the network.
    for created in [true, false] {
        let initial = env.ok("library status");
        assert_eq!(initial["schema_version"], "4");
        assert_eq!(initial["data"]["schema_version"], 1);
        assert_eq!(initial["data"]["created"], created);
        assert!(initial["data"]["last_sync"].is_null());
        assert!(initial["data"]["fresh_through"].is_null());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: std::path::PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(env.path("klms")), 0o700);
        assert_eq!(mode(env.path("klms/library.db")), 0o600);
    }
    let human = String::from_utf8(env.run("library status").stdout).unwrap();
    assert!(human.contains("Last sync attempt: none"), "{human}");
    assert!(human.contains("Last complete global sync: none"), "{human}");
    assert_eq!(server.requests().len(), 0);

    env.sync("--course 42");
    let scoped = env.data("library status");
    let attempt = &scoped["last_sync"];
    assert_eq!(attempt["scope"], "course:42");
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["source_complete"], false);
    assert!(attempt["started_at"].as_i64().unwrap() > 0);
    assert!(attempt["finished_at"].as_i64().unwrap() >= attempt["started_at"].as_i64().unwrap());
    assert!(scoped["fresh_through"].is_null());
    let human = env.run("library status");
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
    env.sync("");
    let global = env.data("library status");
    assert_eq!(global["last_sync"]["scope"], "all");
    assert_eq!(global["last_sync"]["source_complete"], true);
    assert!(global["fresh_through"].as_i64().unwrap() > 0);
    assert!(server.recorded().iter().all(|r| r.has_header("Cookie")));
}

#[test]
fn empty_and_truncated_local_collections_keep_feedback_in_the_correct_stream() {
    let server = lib_server(|_, _| None);
    let env = Env::at(&server);
    for args in [
        "library search absent",
        "library changes",
        "library activity",
    ] {
        let value = env.ok(args);
        assert_eq!(value["data"], json!([]));
        assert_eq!(value["meta"]["complete"], true);
        let human = env.run(args);
        assert!(human.status.success());
        assert!(human.stderr.is_empty());
        assert_eq!(
            String::from_utf8(human.stdout).unwrap().trim(),
            "No records found."
        );
    }
    env.sync("");
    for args in [
        "library changes --limit 1",
        "library search compiler --limit 1",
    ] {
        let value = env.ok(args);
        assert_eq!(value["meta"]["complete"], false);
        assert_eq!(value["data"].as_array().unwrap().len(), 1);
        assert!(
            value["warnings"]
                .to_string()
                .to_lowercase()
                .contains("truncat")
        );
        let human = env.run(args);
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
    let server = lib_server(|r, _| {
        (r.target == "/mod/resource/view.php?id=7")
            .then(|| Response::html("temporarily unavailable").status("503 Service Unavailable"))
    });
    let env = Env::at(&server);
    let result = env.sync("");
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
    let human = env.run("library sync");
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("incomplete"));
    assert!(!human.stderr.is_empty());
    let status = String::from_utf8(env.run("library status").stdout).unwrap();
    assert!(status.contains("ready") && status.contains("incomplete"));
    let status = env.data("library status");
    assert_eq!(status["last_sync"]["status"], "incomplete");
    assert!(status["fresh_through"].is_null());
}

#[test]
fn parent_notice_byte_errors_offer_text_and_only_present_file_candidates() {
    for body in ["", ".", "Read the syllabus"] {
        for count in [0, 1, 2, 21] {
            let removed = Counter::default();
            let flag = removed.clone();
            let server = notice_server(move |_| {
                let files: String = (flag.get()..count)
                    .map(|i| {
                        format!("<a href='/pluginfile.php/attachment-{i}'>syllabus-{i}.pdf</a>")
                    })
                    .collect();
                article(
                    "Syllabus",
                    &format!("{files}<a href='https://example.org/info'>Info link</a>"),
                    body,
                )
            });
            let env = Env::at(&server);
            env.sync("--notices");
            let shown = env.data("library show board-post:9:10");
            assert_eq!(shown["source"]["text"], body);
            let link = rep(&shown, "/info");
            let before = server.requests().len();
            let error = byte_errors(&env, "board-post:9:10");
            let hint = error["hint"].as_str().unwrap();
            assert!(hint.contains("klms library show board-post:9:10"), "{hint}");
            assert_eq!(
                hint.contains("data.source.text"),
                !body.is_empty(),
                "{hint}"
            );
            assert!(!hint.contains(&link), "{hint}");
            if count > 0 {
                assert!(hint.contains(&format!("klms {NOTICE_SYNC_HINT}")), "{hint}");
                let expected: Vec<_> = (0..count.min(20))
                    .map(|i| {
                        let reference = rep(&shown, &format!("/attachment-{i}"));
                        assert!(hint.contains(&reference), "{hint}");
                        assert!(hint.contains(&format!("syllabus-{i}.pdf")), "{hint}");
                        reference
                    })
                    .collect();
                assert_eq!(error["details"]["representations"], json!(expected));
                assert_eq!(hint.contains("first 20"), count > 20, "{hint}");
            } else {
                assert!(!hint.contains("library sync"), "{hint}");
            }
            assert_eq!(server.requests().len(), before, "errors must stay local");

            removed.set(1);
            env.sync("--notices");
            let before = server.requests().len();
            let error = byte_errors(&env, "board-post:9:10");
            let hint = error["hint"].as_str().unwrap();
            if count > 1 {
                let expected: Vec<_> = (1..count)
                    .map(|i| rep(&shown, &format!("/attachment-{i}")))
                    .collect();
                assert_eq!(error["details"]["representations"], json!(expected));
                assert!(
                    !hint.contains("syllabus-0.pdf") && !hint.contains("first 20"),
                    "{hint}"
                );
            } else {
                assert!(!hint.contains("library sync"), "{hint}");
                assert!(error["details"]["representations"].is_null());
            }
            assert_eq!(server.requests().len(), before);
        }
    }
}

#[test]
fn ambiguous_content_lists_candidates_and_downloaded_byte_paths_remain_usable() {
    let server = lib_server(|r, _| match r.target.as_str() {
        "/mod/resource/view.php?id=7" => html(
            "<main><a href='/pluginfile.php/one'>one.txt</a><a href='/pluginfile.php/two'>two.txt</a></main>",
        ),
        "/pluginfile.php/two" => Some(Response::bytes("text/plain", "second fixture")),
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("--download changed");
    let shown = env.data("library show file:7");
    let (first, second) = (
        rep(&shown, "/pluginfile.php/one"),
        rep(&shown, "/pluginfile.php/two"),
    );
    let error = byte_errors(&env, "file:7");
    has(&error["message"], "multiple");
    assert_eq!(error["details"]["representations"], json!([first, second]));
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains(&first) && hint.contains(&second), "{hint}");
    let bytes = env.data(&format!("library content {}", first));
    assert_eq!(bytes["text"], BYTES);
    let hash = text(&bytes, "ref");
    assert!(hash.starts_with("sha256:"));
    assert_eq!(
        env.data(&format!("library content {}", hash))["text"],
        bytes["text"]
    );
    let destination = env.path("existing.txt");
    fs::write(&destination, b"keep me").unwrap();
    let args = format!("library export {first} --out {}", destination.display());
    assert!(!env.out(&args).status.success());
    assert_eq!(fs::read(destination).unwrap(), b"keep me");
}

#[cfg(unix)]
struct HeldChild {
    child: std::process::Child,
    release: Counter,
}

#[cfg(unix)]
impl Drop for HeldChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.release.set(1);
    }
}

#[cfg(unix)]
fn assert_unfinished(env: &Env, before: &Value) {
    let status = env.ok("library status");
    let sync = &status["data"]["last_sync"];
    assert_eq!(sync["status"], "unfinished");
    assert_eq!(sync["scope"], "course:42");
    assert!(sync["finished_at"].is_null());
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
    let human = env.run("library status");
    assert!(String::from_utf8_lossy(&human.stdout).contains("unfinished"));
    assert!(
        String::from_utf8_lossy(&human.stderr)
            .contains("may still be active or may have been interrupted")
    );
    let raw: (String, Option<i64>) = env
        .db()
        .query_row(
            "SELECT status,finished_at FROM sync_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(raw, ("running".into(), None));
    assert_eq!(env.data("library content file:7")["text"], BYTES);
}

#[cfg(unix)]
#[test]
fn active_and_interrupted_attempts_are_unfinished_without_liveness_claims() {
    let scoped = "library sync --course 42 --download changed";
    for signal in ["-INT", "-KILL"] {
        let (hold, release) = (Counter::default(), Counter::default());
        let (router_hold, router_release) = (hold.clone(), release.clone());
        let (entered, observed) = mpsc::channel();
        let server = lib_server(move |r, _| {
            if r.target == "/course/view.php?id=42" && router_hold.get() == 1 {
                router_hold.set(0);
                entered.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while router_release.get() == 0 && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            None
        });
        let env = Env::at(&server);
        env.sync("--download changed");
        let before = env.ok("library status");
        hold.set(1);
        let mut active = HeldChild {
            child: env
                .cmd(true, scoped)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            release: release.clone(),
        };
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(active.child.try_wait().unwrap().is_none());
        assert_unfinished(&env, &before);
        let killed = std::process::Command::new("kill")
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
        assert_unfinished(&env, &before);
        release.set(1);
        env.ok(scoped);
        let recovered = env.data("library status");
        assert_eq!(recovered["last_sync"]["status"], "complete");
        assert_eq!(recovered["blobs"], before["data"]["blobs"]);
        assert_eq!(env.data("library content file:7")["text"], BYTES);
    }
}

#[test]
fn sync_records_courses_resources_and_representations() {
    let server = lib_server(|_, _| None);
    let env = Env::at(&server);
    let sync = env.sync("");
    assert_eq!(sync["data"]["ref"], "sync:1");
    assert_eq!(sync["data"]["source_complete"], true);
    assert_eq!(sync["data"]["truncated"], 0);
    let show = |reference| env.data(&format!("library show {}", reference));
    assert_eq!(
        show("course:42")["source"]["title"],
        "Compilers(CS.420_2026_2)"
    );
    assert_eq!(show("file:7")["source"]["text"], FILE_TEXT);
    assert_eq!(
        show("representation:1")["source"]["filename"],
        "lecture.txt"
    );
    // The assignment activity is stored under its parser ref.
    let server = lib_server(|r, _| match r.target.as_str() {
        "/course/view.php?id=42" => html(course(&[("assign", 5, "Homework One")])),
        "/mod/assign/view.php?id=5" => {
            html("<main><h1>Homework One</h1><p>submit a parser</p></main>")
        }
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("");
    let assignment = env.data("library show assign:5");
    assert_eq!(assignment["ref"], "assign:5");
    assert_eq!(assignment["kind"], "assign");
    assert_eq!(assignment["source"]["text"], "Homework One submit a parser");
    assert_eq!(env.data("library search parser")[0]["ref"], "assign:5");
}

#[test]
fn notices_are_synced_only_on_request_and_walk_board_pages() {
    let server = lib_server(|r, _| match r.target.as_str() {
        "/course/view.php?id=42" => html(notice_manifest()),
        "/mod/courseboard/view.php?id=9" => html(format!(
            "{NOTICE_LIST}<a rel='next' href='/mod/courseboard/view.php?id=9&page=2'>Next</a>"
        )),
        "/mod/courseboard/view.php?id=9&page=2" => html("<table class='generaltable'></table>"),
        ARTICLE => html(article("Hello", "", "notice body")),
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("");
    assert!(
        !server
            .requests()
            .iter()
            .any(|line| line.contains("courseboard/view"))
    );
    env.sync("--notices");
    assert_eq!(env.data("library show board-post:9:10")["kind"], "notice");
    assert!(server.requests().iter().any(|line| line.contains("page=2")));
}

#[test]
fn notice_sync_ignores_chrome_but_records_title_body_and_attachment_changes() {
    let phase = Counter::default();
    let router = phase.clone();
    let server = notice_server(move |_| {
        let phase = router.get();
        if phase == 5 {
            return "<main>Changed upstream layout without a post body</main>".into();
        }
        let title = if phase >= 2 {
            "Revised exam schedule"
        } else {
            "Exam schedule"
        };
        let body = if phase >= 3 { "Wednesday" } else { "Tuesday" };
        let file = if phase >= 4 { "new.pdf" } else { "old.pdf" };
        format!(
            "<h1>Generic heading</h1><div class='courseboard_view'>\
             <div class='subject'><h3>{title}</h3></div>\
             <div class='info'><div class='hit'>Views : {phase}</div>\
             <div class='files'><a href='/pluginfile.php/1/{file}'>{file}</a></div></div>\
             <div class='content'>{body}</div>\
             <div class='pre_next'><a href='/neighbor/{phase}'>Navigation {phase}</a></div>\
             <div id='password_confirm'>Enter password</div></div>"
        )
    });
    let env = Env::at(&server);
    let run = || env.sync("--notices");
    run();
    phase.set(1);
    assert_eq!(run()["data"]["changes"], 0);
    for next in 2..=4 {
        phase.set(next);
        assert!(run()["data"]["changes"].as_u64().unwrap() > 0);
        assert_eq!(run()["data"]["changes"], 0);
    }
    let history = env.data("library history board-post:9:10");
    assert_eq!(history.as_array().unwrap().len(), 4);
    let before = env.data("library show board-post:9:10");
    phase.set(5);
    let failed = run();
    assert_eq!(failed["data"]["status"], "incomplete");
    has(&failed["data"]["failures"][0], "post region");
    assert_eq!(
        env.data("library show board-post:9:10")["source"],
        before["source"]
    );
    for query in ["Navigation", "Enter password"] {
        assert_eq!(env.data(&format!("library search '{query}'")), json!([]));
    }
}

#[test]
fn corrected_notice_keeps_history_curation_and_files_but_unindexes_old_navigation() {
    let corrected = Counter::default();
    let router = corrected.clone();
    let server = lib_server(move |r, _| match r.target.as_str() {
        "/course/view.php?id=42" => html(notice_manifest()),
        "/mod/courseboard/view.php?id=9" => html(NOTICE_LIST),
        // Seed the polluted text/links the old broad parser persisted, then move that
        // chrome outside the semantic body.
        ARTICLE => {
            let chrome = "<a href='/neighbor'>Phantomnavigation</a><p>Views : 1</p>";
            let attachment = "<a href='/pluginfile.php/1/lecture.txt'>lecture.txt</a>";
            let (body, outside) = if router.get() == 1 {
                ("Actual notice".to_owned(), chrome.to_owned())
            } else {
                (
                    format!("Actual notice {chrome} {attachment}"),
                    String::new(),
                )
            };
            html(format!(
                "<div class='courseboard_view'><div class='subject'><h3>Exam schedule</h3></div>\
                 <div class='content'>{body}</div><div class='pre_next'>{outside}</div></div>"
            ))
        }
        "/pluginfile.php/1/lecture.txt" => Some(Response::bytes("text/plain", "preserved bytes")),
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("--notices --download changed");
    let original = env.data("library history board-post:9:10");
    let notice = env.data("library show board-post:9:10");
    let (nav, file) = (rep(&notice, "/neighbor"), rep(&notice, "/lecture.txt"));
    let edit = |subject: &str, value: &str| {
        env.ok(&format!(
            "library edit {subject} --field note --value '{value}' --expected-revision 0"
        ))
    };
    edit("board-post:9:10", "Keep my note");
    corrected.set(1);
    env.sync("--notices");
    let history = env.data("library history board-post:9:10");
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert_eq!(history[1], original[0]);
    let after = env.data("library show board-post:9:10");
    assert_eq!(after["effective"]["note"], "Keep my note");
    assert_eq!(
        env.data(&format!("library show {}", nav))["remote_state"],
        "not_observed"
    );
    // Older versions also retained index entries for already-missing links.
    env.db()
        .execute(
            "INSERT INTO search_documents(subject_ref,kind,course,title,body)
             VALUES(?1,'link','course:42','Phantomnavigation','Old index entry')",
            [&nav],
        )
        .unwrap();
    assert_eq!(env.sync("--notices")["data"]["changes"], 0);
    let search = || env.data("library search Phantomnavigation");
    assert_eq!(search(), json!([]));
    // A later curation refresh must not reintroduce an obsolete notice link.
    edit(&nav, "Phantomnavigation annotation");
    assert_eq!(search(), json!([]));
    assert_eq!(
        env.data(&format!("library content {}", file))["text"],
        "preserved bytes"
    );
    let files = env.data("library search lecture.txt");
    assert!(files.as_array().unwrap().iter().any(|r| r["ref"] == file));
    assert_eq!(env.sync("--notices")["data"]["changes"], 0);
}

#[test]
fn download_changed_stores_bytes_once_and_sends_no_conditional_headers() {
    let server = lib_server(|_, _| None);
    let env = Env::at(&server);
    env.sync("--files");
    let count = |method: &str| {
        server
            .requests()
            .iter()
            .filter(|line| line.starts_with(&format!("{method} /pluginfile.php/")))
            .count()
    };
    assert_eq!(count("HEAD"), 1);
    let download = "--download changed";
    assert_eq!(env.sync(download)["data"]["blobs_added"], 1);
    assert_eq!(env.sync(download)["data"]["blobs_added"], 0);
    assert_eq!(env.data("library status")["blobs"], 1);
    let gets: Vec<_> = server
        .recorded()
        .into_iter()
        .filter(|r| r.line.starts_with("GET /pluginfile.php/"))
        .collect();
    assert_eq!(gets.len(), 1);
    assert!(!gets[0].has_header("If-None-Match") && !gets[0].has_header("If-Modified-Since"));
}

#[test]
fn content_changes_append_history_but_changed_validators_alone_do_not() {
    let server = lib_server(|r, run| {
        r.target.starts_with("/pluginfile.php/").then(|| {
            let value = if run == 1 { "B" } else { "A" };
            Response::bytes("text/plain", value).header("ETag", &format!("\"{value}\""))
        })
    });
    let env = Env::at(&server);
    for _ in 0..4 {
        env.sync("--download changed");
    }
    let history = env.data("library history representation:1");
    let verified = history
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "verified_content")
        .count();
    assert_eq!(verified, 3);
    let changes = env.data("library changes");
    let changed: Vec<_> = changes
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "verified_content_changed")
        .collect();
    assert_eq!(changed.len(), 2);
    assert!(changed.iter().all(|e| {
        e["before_ref"].as_str().unwrap().starts_with("sha256:")
            && e["after_ref"].as_str().unwrap().starts_with("sha256:")
    }));

    // Same bytes under a new validator: no re-download beyond the validation GET, no change.
    let version = Counter::default();
    let etag = version.clone();
    let server = lib_server(move |r, _| {
        r.target.starts_with("/pluginfile.php/").then(|| {
            Response::bytes("text/plain", "unchanged bytes")
                .header("ETag", &format!("\"v{}\"", etag.get()))
        })
    });
    let env = Env::at(&server);
    let gets = || {
        server
            .requests()
            .iter()
            .filter(|r| r.starts_with("GET /pluginfile.php/"))
            .count()
    };
    env.sync("--download changed");
    version.set(2);
    assert_eq!(env.sync("--download changed")["data"]["blobs_added"], 0);
    assert_eq!(gets(), 2);
    assert_eq!(env.sync("--download changed")["data"]["blobs_added"], 0);
    assert_eq!(gets(), 2);
    let changes = env.data("library changes");
    assert!(
        !changes
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "verified_content_changed")
    );
}

#[test]
fn course_and_resource_remote_states_follow_the_source() {
    let state = |env: &Env, reference: &str| {
        env.data(&format!("library show {}", reference))["remote_state"].clone()
    };
    // Dashboard failures never mark courses unlisted; disappearing and reappearing does.
    let server = lib_server(|r, run| match (r.target.as_str(), run) {
        ("/my/", 1) => html("<a href='/course/view.php?id=99'>Databases</a>"),
        ("/my/", 3) => html("<html>incomplete dashboard</html>"),
        ("/course/view.php?id=99", _) => html(course(&[])),
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("");
    env.sync("");
    assert_eq!(state(&env, "course:42"), "not_listed");
    env.sync("");
    assert_eq!(state(&env, "course:42"), "listed");
    assert!(!env.out("library sync").status.success());
    assert_eq!(state(&env, "course:42"), "listed");

    // Forbidden details record access lost then restored; a complete manifest without a
    // resource marks it not observed; failed details keep the stored state.
    let server = lib_server(|r, run| match (r.target.as_str(), run) {
        ("/mod/resource/view.php?id=7", 1) => {
            Some(Response::html("forbidden").status("403 Forbidden"))
        }
        ("/course/view.php?id=42", 3) => html(course(&[])),
        ("/mod/resource/view.php?id=7", 4) => {
            Some(Response::html("failed").status("500 Internal Server Error"))
        }
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("");
    assert_eq!(env.sync("")["data"]["status"], "incomplete");
    assert_eq!(state(&env, "file:7"), "access_lost");
    assert_eq!(env.data("library show file:7")["source"]["text"], FILE_TEXT);
    env.sync("");
    assert_eq!(state(&env, "file:7"), "present");
    let changes = env.data("library changes");
    let kinds: Vec<_> = changes
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["subject_ref"] == "file:7")
        .map(|e| text(e, "kind"))
        .collect();
    for kind in ["access_lost", "access_restored"] {
        assert!(kinds.contains(&kind.to_owned()), "{kinds:?}");
    }
    env.sync("");
    assert_eq!(state(&env, "file:7"), "not_observed");
    assert_eq!(env.sync("")["data"]["status"], "incomplete");
    assert_eq!(env.data("library show file:7")["source"]["text"], FILE_TEXT);
}

#[test]
fn truncated_detail_links_never_mark_representations_not_observed() {
    let many: String = (0..101)
        .map(|i| {
            format!(
                "<a href='/pluginfile.php/1/mod_resource/content/1/extra-{i}.txt'>extra {i}</a>"
            )
        })
        .collect();
    let server = lib_server(move |r, run| {
        (r.target == "/mod/resource/view.php?id=7" && run == 1)
            .then(|| Response::html(format!("<main><h1>Lecture One</h1>{many}</main>")))
    });
    let env = Env::at(&server);
    env.sync("");
    // Parser caps mark the observation incomplete without failing the run.
    let truncated = env.sync("");
    assert_eq!(truncated["data"]["status"], "complete");
    assert_eq!(truncated["data"]["truncated"], 1);
    let lecture = env.data("library show representation:1");
    assert_eq!(lecture["source"]["filename"], "lecture.txt");
    assert_eq!(lecture["remote_state"], "present");
    let changes = env.data("library changes");
    assert!(
        !changes
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "representation_not_observed")
    );
}

#[test]
fn reverted_source_keeps_every_observation_in_history() {
    let server = lib_server(|r, run| match r.target.as_str() {
        "/my/" if run == 1 => html("<a href='/course/view.php?id=42'>Renamed Compilers</a>"),
        "/mod/resource/view.php?id=7" if run == 1 => {
            html("<main><h1>Lecture One</h1><p>revised body</p></main>")
        }
        _ => None,
    });
    let env = Env::at(&server);
    for _ in 0..3 {
        env.sync("");
    }
    let course = env.data("library show course:42");
    assert_eq!(course["source"]["title"], "Compilers(CS.420_2026_2)");
    for (reference, kind) in [
        ("course:42", "course_source"),
        ("file:7", "resource_source"),
    ] {
        let history = env.data(&format!("library history {}", reference));
        let observations = history
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == kind)
            .count();
        assert_eq!(observations, 3, "{reference}");
    }
}

#[test]
fn scoped_sync_validates_only_its_course() {
    let server = lib_server(|r, _| {
        (r.target == "/my/").then(|| {
            Response::html(
                "<a href='/course/view.php?id=42'>Compilers(CS.420)</a>\
                 <a href='/course/view.php?id=99'>Databases(CS.430)</a>",
            )
        })
    });
    let env = Env::at(&server);
    let value = env.sync("--course course:42 --files");
    assert_eq!(value["data"]["source_complete"], false);
    assert!(!server.requests().iter().any(|line| line.contains("id=99")));
}

#[test]
fn curation_edits_conflict_retract_search_and_relations() {
    let server = lib_server(|_, _| None);
    let env = Env::at(&server);
    env.sync("");
    let edit = |value: &str, actor: &str, rev: u8| {
        let args = format!("library edit file:7 --field title --value {value} --actor {actor}");
        format!("{args} --expected-revision {rev}")
    };
    let retract = |value: &Value| env.ok(&format!("library retract {}", text(value, "ref")));
    let first = env.data(&edit("Human", "human", 0));
    let second = env.data(&edit("Agent", "agent", 1));
    assert_eq!(env.code(&edit("Stale", "agent", 1)), 54);
    retract(&second);
    assert_eq!(
        env.data("library show file:7")["effective"]["title"],
        "Human"
    );
    assert_eq!(first["actor"], "human");

    // Search matches source text and active curation only.
    let hits = env.data("library search compiler");
    assert!(
        hits.as_array()
            .unwrap()
            .iter()
            .any(|r| r["ref"] == "file:7")
    );
    let note = env.data("library edit file:7 --field note --value zirconium --expected-revision 0");
    assert_eq!(env.data("library search zirconium")[0]["ref"], "file:7");
    retract(&note);
    assert_eq!(env.data("library search zirconium"), json!([]));

    let add = "library relations add course:42 file:7 --kind related_to --actor agent";
    let relation = env.data(add);
    assert_eq!(env.code(add), 54);
    retract(&relation);
    env.ok(add);
    let activity = env.data("library activity --subject course:42");
    assert_eq!(activity.as_array().unwrap().len(), 2);
}

#[test]
fn summary_goes_stale_after_source_change() {
    let server = lib_server(|r, run| {
        (r.target == "/mod/resource/view.php?id=7")
            .then(|| Response::html(format!("<main><h1>Lecture</h1><p>body {run}</p></main>")))
    });
    let env = Env::at(&server);
    env.sync("");
    env.ok("library edit file:7 --field summary --value Summary --expected-revision 0");
    env.sync("");
    assert_eq!(
        env.data("library show file:7")["effective"]["summary_stale"],
        true
    );
}

#[test]
fn aliases_blobs_and_stored_json_keep_curation_invariants() {
    let server = lib_server(|_, _| None);
    let env = Env::at(&server);
    env.sync("--download changed");
    let reference = rep(&env.data("library show file:7"), "/lecture.txt");
    let padded = |reference: &str| {
        let (kind, id) = reference.split_once(':').unwrap();
        format!("{kind}:0{id}")
    };
    let alias = padded(&reference);
    let edit = |subject: &str, value: &str| {
        format!("library edit {subject} --field note --value {value} --expected-revision 0")
    };
    let relate = |a: &str, b: &str| format!("library relations add {a} {b} --kind related_to");
    // Numeric aliases share revisions, retractions and relations with the canonical ref.
    let edited = env.data(&edit(&alias, "zirconium"));
    assert_eq!(edited["subject_ref"], reference);
    assert_eq!(env.code(&edit(&reference, "lostupdate")), 54);
    assert_eq!(env.data("library search zirconium")[0]["ref"], reference);
    let activity = env.data(&format!("library activity --subject {alias}"));
    assert_eq!(activity.as_array().unwrap().len(), 1);
    let assertion = text(&edited, "ref");
    let retracted = env.data(&format!("library retract {}", padded(&assertion)));
    assert_eq!(retracted["target_ref"], assertion);
    assert_eq!(env.code(&format!("library retract {assertion}")), 54);
    assert_eq!(env.data("library search zirconium"), json!([]));
    let relation = env.data(&relate(&alias, "file:7"));
    assert_eq!(env.code(&relate(&reference, "file:7")), 54);
    env.ok(&format!(
        "library retract {}",
        padded(&text(&relation, "ref"))
    ));
    env.ok(&relate(&reference, "file:7"));

    // Immutable blob refs are showable but cannot become invisible curation subjects.
    let shown = env.data(&format!("library show {}", reference));
    let blob = text(&shown["content"], "sha256_ref");
    assert_eq!(env.data(&format!("library show {}", blob))["ref"], blob);
    let counts = || -> (i64, i64) {
        let db = env.db();
        let count = |table: &str| {
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        (count("assertions"), count("relations"))
    };
    let before = counts();
    assert_eq!(env.code(&edit(&blob, "invisible")), 2);
    for (left, right) in [(blob.as_str(), "file:7"), ("file:7", blob.as_str())] {
        assert_eq!(env.code(&relate(left, right)), 2);
    }
    assert_eq!(counts(), before);
    assert_eq!(env.data("library search invisible"), json!([]));

    // Corrupt stored JSON is reported with its record context.
    let db = env.db();
    for (table, column, command, label) in [
        (
            "remote_changes",
            "details_json",
            "library changes",
            "remote_changes",
        ),
        (
            "resource_observations",
            "source_json",
            "library history file:7",
            "resource_source",
        ),
    ] {
        let id: i64 = db
            .query_row(&format!("SELECT MAX(id) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        db.execute(
            &format!("UPDATE {table} SET {column}='{{' WHERE id=?1"),
            [id],
        )
        .unwrap();
        let (_, error) = env.fail(command);
        assert_eq!(error["code"], "CORPUS_CORRUPT");
        let field = if table == "remote_changes" {
            column
        } else {
            "source"
        };
        has(&error["message"], &format!("{label}:{id} {field}"));
    }
}

#[test]
fn utf8_preview_cuts_at_character_boundaries_and_binary_reports_no_text() {
    let content = |bytes: &'static [u8]| {
        let server = lib_server(move |r, _| {
            r.target
                .starts_with("/pluginfile.php/")
                .then(|| Response::bytes("application/octet-stream", bytes))
        });
        let env = Env::at(&server);
        env.sync("--download changed");
        env
    };
    let env = content("a한글z".as_bytes());
    for (limit, expected) in [("2", "a"), ("4", "a한"), ("5", "a한"), ("8", "a한글z")] {
        let result = env.data(&format!("library content file:7 --max-bytes {}", limit));
        assert_eq!(result["text"], expected);
        assert_eq!(result["truncated"], limit != "8");
    }
    for bytes in [b"a\xffmore".as_slice(), b"a\xe2\x82".as_slice()] {
        let result = content(bytes).data("library content file:7");
        assert!(result["text"].is_null());
        assert_eq!(result["truncated"], false);
    }
}

#[test]
fn detail_response_keys_types_and_curation_remain_stable() {
    let server = lib_server(|_, _| None);
    let env = Env::at(&server);
    env.sync("--download changed");
    let keys = |value: &Value, expected: &str| {
        let actual: std::collections::BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(actual, expected.split(' ').collect());
    };
    let course = env.data("library show course:42");
    keys(&course, "ref kind remote_state source effective relations");
    keys(
        &course["source"],
        "title code term url first_seen last_seen not_listed_since",
    );
    assert_eq!(course["kind"], "course");
    assert!(course["source"]["first_seen"].is_i64() && course["source"]["last_seen"].is_i64());
    assert!(course["source"]["not_listed_since"].is_null());
    assert_eq!(course["effective"]["title"], course["source"]["title"]);

    let resource = env.data("library show file:7");
    keys(
        &resource,
        "ref kind course_ref remote_state source effective representations relations",
    );
    keys(
        &resource["source"],
        "title url week section text complete observed_at",
    );
    assert_eq!(resource["source"]["title"], "Lecture One");
    assert!(
        resource["source"]["complete"].is_boolean() && resource["source"]["observed_at"].is_i64()
    );
    for representation in resource["representations"].as_array().unwrap() {
        keys(representation, "ref url kind filename has_content");
        assert!(representation["has_content"].is_boolean());
    }
    let reference = rep(&resource, "/lecture.txt");
    let shown = env.data(&format!("library show {}", reference));
    keys(
        &shown,
        "ref resource_ref remote_state source content effective relations",
    );
    keys(&shown["source"], "url filename mime observed_at");
    keys(&shown["content"], "sha256_ref byte_length mime observed_at");
    assert_eq!(shown["resource_ref"], "file:7");
    assert_eq!(shown["content"]["byte_length"], BYTES.len());
    assert!(shown["content"]["observed_at"].is_i64());
    env.ok(&format!(
        "library edit {} --field filename --value curated.txt --expected-revision 0",
        reference
    ));
    let edited = env.data(&format!("library show {}", reference));
    assert_eq!(edited["source"], shown["source"]);
    assert_eq!(edited["content"], shown["content"]);
    assert_eq!(edited["effective"]["filename"], "curated.txt");
}

#[test]
fn download_frontier_follows_representation_order_and_sync_reads_all_weeks() {
    let server = lib_server(|r, _| match r.target.as_str() {
        "/mod/resource/view.php?id=7" => html(
            "<main><h1>Lecture</h1><a href='/pluginfile.php/z'>z.txt</a>\
             <a href='/pluginfile.php/a'>a.txt</a><a href='/pluginfile.php/m'>m.txt</a></main>",
        ),
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("--download changed");
    let expected: Vec<String> = env
        .db()
        .prepare("SELECT url FROM representations WHERE kind='file' ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| {
            let url = row.unwrap();
            let target = url.split_once("://").unwrap().1;
            let path = target[target.find('/').unwrap()..]
                .split(['?', '#'])
                .next()
                .unwrap()
                .to_owned();
            format!("HEAD {path} HTTP/1.1")
        })
        .collect();
    assert!(expected.len() >= 3);
    let actual: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.starts_with("HEAD "))
        .collect();
    assert_eq!(actual, expected);

    // KLMS's paged week format renders only current weeks by default; the picker's "All"
    // choice (dayselect 0) is served at section=0.
    let server = lib_server(move |r, _| match r.target.as_str() {
        "/course/view.php?id=42" => html(format!(
            "<div class='week-slider'><a href='javascript:M.course.format.dayselect(0,42,0)'>All</a>\
             <a href='javascript:M.course.format.dayselect(5,42,0)'>week 5</a></div>{}",
            course(&[("resource", 7, "Week 5 slides")])
        )),
        "/course/view.php?id=42&section=0" => html(course(&[
            ("resource", 3, "Week 1 slides"),
            ("resource", 7, "Week 5 slides"),
        ])),
        "/mod/resource/view.php?id=3" | "/mod/resource/view.php?id=7" => {
            html("<main><h1>Slides</h1><p>no files</p></main>")
        }
        _ => None,
    });
    let env = Env::at(&server);
    env.sync("--download changed");
    let refs: Vec<String> = env
        .db()
        .prepare("SELECT ref FROM resources ORDER BY ref")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(refs, ["file:3", "file:7"]);
}
