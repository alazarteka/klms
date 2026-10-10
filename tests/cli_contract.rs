use std::fs;

use serde_json::{Value, json};

mod fixture;
use fixture::Env;
use fixture::server::{Response, Server};

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn html_server(body: &'static str) -> Server {
    Server::new(move |_| Response::html(body))
}

#[test]
fn help_and_version_are_json_successes_without_authentication() {
    let env = Env::new();
    for args in [
        "--version",
        "--help",
        "files download --help",
        "update --help",
        "upgrade --help",
    ] {
        let output = env.out(args);
        assert!(output.status.success(), "{args}: {:?}", output.stderr);
        assert!(output.stderr.is_empty());
        let value = parse(&output.stdout);
        assert_eq!(value["ok"], true);
        if args == "--version" {
            assert_eq!(value["command"], "version");
            assert_eq!(value["data"]["version"], env!("CARGO_PKG_VERSION"));
        } else {
            assert_eq!(value["command"], "help");
            assert!(value["data"]["text"].as_str().unwrap().contains("Usage:"));
        }
    }
    assert!(!env.path("state/klms/session.json").exists());
}

#[test]
fn usage_errors_are_structured_exit_two_and_fail_before_authentication() {
    let env = Env::new();
    for args in [
        "update --bogus",
        "courses show",
        "skill install",
        "library edit file:1 --field note --expected-revision 0",
    ] {
        let (code, error) = env.fail(args);
        assert_eq!((code, &error["code"]), (2, &json!("USAGE")), "{args}");
    }
    for args in ["courses resolve ''", "courses show '   '"] {
        let (code, error) = env.fail(args);
        assert_eq!((code, &error["code"]), (2, &json!("USAGE")), "{args}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("must not be empty")
        );
    }
}

#[test]
fn auth_status_and_logout_never_emit_cookie_values() {
    let mut env = Env::new();
    let status = env.data("auth status");
    assert_eq!(status["configured"], false);
    assert_eq!(env.ok("auth status")["schema_version"], "4");
    env.state();
    let output = env.out("--base-url http://127.0.0.1:9 auth status");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test-session"));
    assert_eq!(parse(&output.stdout)["data"]["cookie_count"], 1);
    assert_eq!(env.data("auth logout")["removed"], true);
    assert!(!env.path("state/klms/session.json").exists());
    let help = String::from_utf8(env.run("auth login --help").stdout).unwrap();
    assert!(help.contains("--method") && help.contains("--second-factor"));
    assert!(!help.contains("--password") && !help.contains("--otp"));
}

#[test]
fn doctor_fails_with_diagnostics_when_auth_is_missing_or_rejected() {
    let (code, error) = Env::new().fail("doctor");
    assert_eq!((code, &error["code"]), (10, &json!("AUTH_REQUIRED")));
    assert_eq!(error["details"]["session_status"], "not_configured");
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains("klms auth login") && hint.contains("auth extend"));

    let server = html_server(r#"<form><input name="username"><input name="password"></form>"#);
    let (code, error) = Env::at(&server).fail("doctor");
    assert_eq!(server.requests(), ["GET /my/ HTTP/1.1"]);
    assert_eq!((code, &error["code"]), (10, &json!("AUTH_REQUIRED")));
    assert_eq!(error["details"]["session_status"], "expired");
    assert_eq!(error["details"]["session_error"]["code"], "AUTH_REQUIRED");
}

const COURSES: &str = r#"<select name="year"><option selected>2026</option></select>
    <select name="semester"><option selected>Fall</option></select>
    <a href="/course/view.php?id=42">Compilers(CS.420_2026_2)</a>
    <a href="/course/view.php?id=43">Databases(CS.430_2026_2)</a>"#;
const EMPTY_CALENDAR: &str = "<main class='calendarwrapper'>There are no upcoming events</main>";

#[test]
fn read_commands_parse_fixture_pages_over_the_cookie_transport() {
    // (args, page, request target, expected JSON pointers)
    type Case = (
        &'static str,
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
    );
    let cases: &[Case] = &[
        (
            "dashboard",
            COURSES,
            "/my/",
            &[
                ("/data/course_count", "2"),
                ("/data/courses/0/id", "\"42\""),
                ("/data/courses/0/ref", "\"course:42\""),
            ],
        ),
        (
            "courses list --limit 1",
            COURSES,
            "/my/",
            &[
                ("/data/0/ref", "\"course:42\""),
                ("/meta/returned", "1"),
                ("/meta/total", "2"),
                ("/meta/complete", "false"),
            ],
        ),
        (
            "assignments list --course 42",
            "<main>There are no assignments in this course.</main>",
            "/mod/assign/index.php?id=42",
            &[
                ("/data", "[]"),
                ("/meta/complete", "true"),
                ("/meta/total", "0"),
            ],
        ),
        (
            "quizzes list --course 42",
            "<main>No quizzes found.</main>",
            "/mod/quiz/index.php?id=42",
            &[
                ("/data", "[]"),
                ("/meta/complete", "true"),
                ("/meta/total", "0"),
            ],
        ),
        (
            "today",
            EMPTY_CALENDAR,
            "/calendar/view.php?view=upcoming",
            &[("/data", "[]"), ("/meta/complete", "true")],
        ),
        (
            "upcoming",
            EMPTY_CALENDAR,
            "/calendar/view.php?view=upcoming",
            &[("/data", "[]"), ("/meta/complete", "true")],
        ),
    ];
    for (args, page, target, expected) in cases {
        let page = *page;
        let server = Server::new(move |_| Response::html(page));
        let value = Env::at(&server).ok(args);
        assert_eq!(
            server.requests(),
            [format!("GET {target} HTTP/1.1")],
            "{args}"
        );
        let cookie = server.recorded()[0]
            .header_value("cookie")
            .map(str::to_owned);
        assert_eq!(cookie.as_deref(), Some("MoodleSession=test-session"));
        for (pointer, want) in *expected {
            assert_eq!(
                value.pointer(pointer).unwrap(),
                &parse(want.as_bytes()),
                "{args} {pointer}"
            );
        }
    }
}

#[test]
fn localized_calendar_cards_survive_calendar_and_agenda_commands() {
    let page = include_str!("fixtures/localized/calendar.html")
        .replace("&amp;time=1899989400", "")
        .replace("내일", "오늘");
    let server = Server::new(move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/calendar/view.php?view=upcoming");
        Response::html(page.as_bytes())
    });
    let env = Env::at(&server);
    for args in [
        "calendar list",
        "today",
        "upcoming --through 7d --course 42",
    ] {
        let value = env.ok(args);
        assert_eq!(value["meta"]["complete"], true);
        assert_eq!(value["warnings"], json!([]));
        assert_eq!(value["data"].as_array().unwrap().len(), 1);
        let event = &value["data"][0];
        assert_eq!(event["title"], "Reading response is due");
        assert_eq!(event["ref"], "assign:7");
        assert_eq!(event["course_id"], "42");
        assert_eq!(
            event["url"],
            format!("{}/mod/assign/view.php?id=7", server.url())
        );
        assert!(
            event["starts_at"]
                .as_str()
                .unwrap()
                .ends_with("T23:50:00+09:00")
        );
    }
}

#[test]
fn raw_get_is_a_truncated_secret_free_preview_and_redirect_errors_stay_secret_free() {
    let server = Server::new(|_| {
        Response::bytes(
            "application/json",
            r#"{"sesskey":"bodysecret","payload":"abcdefghijklmnopqrstuvwxyz"}"#,
        )
    });
    let output = Env::at(&server).out("request get /mod/assign/view.php?id=7 --max-bytes 48");
    assert_eq!(
        server.requests(),
        ["GET /mod/assign/view.php?id=7 HTTP/1.1"]
    );
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("bodysecret"));
    let data = &parse(&output.stdout)["data"];
    assert_eq!(
        (&data["truncated"], &data["redacted"]),
        (&json!(true), &json!(true))
    );
    assert!(
        data["body"]
            .as_str()
            .unwrap()
            .contains("bounded response is incomplete")
    );

    let server = Server::new(|_| {
        Response::html("").status("302 Found").header(
            "Location",
            "https://example.invalid/continue?sesskey=transportsecret",
        )
    });
    let output = Env::at(&server).out("request get /mod/assign/view.php?id=7");
    assert_eq!(server.requests().len(), 1);
    assert!(!output.status.success() && output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("transportsecret"));
}

#[test]
fn download_redacts_source_secrets_and_refuses_replacement() {
    let server = Server::new(|_| Response::bytes("application/pdf", "notes"));
    let env = Env::at(&server);
    let out = env.path("notes.pdf");
    let source = format!(
        "{}/pluginfile.php/7/notes.pdf?token=downloadsecret",
        server.url()
    );
    let args = format!("files download '{source}' --out {}", out.display());
    let data = env.data(&args);
    assert_eq!(fs::read(&out).unwrap(), b"notes");
    assert_eq!(data["bytes"], 5);
    assert!(
        !data["source_url"]
            .as_str()
            .unwrap()
            .contains("downloadsecret")
    );
    assert!(!env.out(&args).status.success());
    assert_eq!(fs::read(&out).unwrap(), b"notes");
    assert_eq!(
        server.requests(),
        ["GET /pluginfile.php/7/notes.pdf?token=downloadsecret HTTP/1.1"]
    );
}

#[test]
fn partial_downloads_are_rejected_without_publishing_but_complete_ranges_work() {
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
        let env = Env::at(&server);
        let downloads = env.path("downloads");
        fs::create_dir(&downloads).unwrap();
        let args = format!(
            "files download /pluginfile.php/notes.pdf --out {}/notes.pdf",
            downloads.display()
        );
        let (_, error) = env.fail(&args);
        assert_eq!(error["code"], "UPSTREAM_ERROR");
        assert_eq!(fs::read_dir(&downloads).unwrap().count(), 0);
    }
    let server = Server::new(|_| {
        Response::bytes("application/pdf", b"pdf")
            .status("206 Partial Content")
            .header("Content-Range", "bytes 0-2/3")
    });
    let env = Env::at(&server);
    let out = env.path("notes.pdf");
    env.ok(&format!(
        "files download /pluginfile.php/notes.pdf --out {}",
        out.display()
    ));
    assert_eq!(fs::read(out).unwrap(), b"pdf");
    assert!(server.recorded().iter().all(|r| r.has_header("cookie")));
}

#[test]
fn auth_extend_and_time_left_use_allowlisted_ajax_without_persisting_the_sesskey() {
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(r#"<script>var cfg={"sesskey":"abc123"}</script>"#),
        target if target.contains("info=core_session_touch") => {
            Response::bytes("application/json", r#"[{"error":false,"data":true}]"#)
        }
        target if target.contains("info=core_session_time_remaining") => {
            assert_eq!(request.method, "POST");
            Response::bytes(
                "application/json",
                r#"[{"error":false,"data":{"userid":7,"timeremaining":10800}}]"#,
            )
        }
        target => panic!("unexpected request {target}"),
    });
    let env = Env::at(&server);
    let value = env.ok("auth extend");
    assert_eq!(value["command"], "auth.extend");
    assert_eq!(value["data"]["remaining_seconds"], 10800);
    assert_eq!(value["data"]["remaining"], "03:00:00");
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 3);
    assert_eq!(recorded[0].line, "GET /my/ HTTP/1.1");
    assert!(recorded[1].line.contains("info=core_session_touch"));
    assert!(
        recorded[1]
            .body
            .contains("\"methodname\":\"core_session_touch\"")
    );
    assert!(
        recorded[2]
            .line
            .contains("info=core_session_time_remaining")
    );

    let value = env.ok("auth time-left");
    assert_eq!(value["data"]["remaining_seconds"], 10800);
    assert_eq!(value["data"]["bootstrap_may_have_extended_session"], true);
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 5);
    assert!(recorded[4].line.starts_with("POST /lib/ajax/service.php?"));
    assert!(
        recorded[4]
            .line
            .contains("info=core_session_time_remaining")
    );
    let stored = fs::read_to_string(env.path("state/klms/session.json")).unwrap();
    assert!(!stored.contains("abc123"));
}

#[test]
fn typed_show_rejects_a_mismatched_final_resource() {
    let server = html_server("<main>Dashboard</main>");
    let (_, error) = Env::at(&server).fail("assignments show /my/");
    assert_eq!(server.requests(), ["GET /my/ HTTP/1.1"]);
    assert_eq!(error["code"], "UPSTREAM_SHAPE_CHANGED");
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
        let (_, error) = Env::at(&server).fail(&format!("{group} show '{target}'"));
        assert_eq!(error["code"], "UPSTREAM_SHAPE_CHANGED", "{group}: {error}");
        assert!(error["message"].as_str().unwrap().contains("identity"));
    }
    // Zero padding and extra parameters keep the same identity.
    let server = Server::new(|request| {
        if request.target == "/mod/assign/view.php?id=007" {
            Response::html("")
                .status("302 Found")
                .header("Location", "/mod/assign/view.php?id=7&redirect=1")
        } else {
            Response::html("<main><h1>Requested assignment</h1></main>")
        }
    });
    assert_eq!(
        Env::at(&server).data("assignments show assign:007")["ref"],
        "assign:7"
    );
}

#[test]
fn board_post_identity_is_consistent_across_list_and_detail() {
    let server = Server::new(|request| match request.target.as_str() {
        "/mod/courseboard/view.php?id=10" => Response::html(
            "<table class='board-list'><tr><td><a href='/mod/courseboard/article.php?id=10&bwid=11'>Notice</a></td></tr></table>",
        ),
        "/mod/courseboard/article.php?id=10&bwid=11" => {
            Response::html(fixture::article("Notice", "", "Details"))
        }
        target => panic!("unexpected request {target}"),
    });
    let env = Env::at(&server);
    let listed = env.data("boards posts board:10")[0].clone();
    for args in [
        "boards show board-post:10:11",
        "notices show board-post:10:11",
    ] {
        let detail = env.data(args);
        for key in ["id", "board_id", "ref"] {
            assert_eq!(detail[key], listed[key], "{args} {key}");
        }
    }
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn spec_and_completions_describe_the_command_surface() {
    let env = Env::new();
    assert!(
        String::from_utf8(env.run("spec").stdout)
            .unwrap()
            .contains("klms library sync")
    );
    let spec = env.ok("spec");
    assert_eq!(spec["command"], "spec");
    assert_eq!(spec["data"]["name"], "klms");
    let find = |path: [&str; 2]| {
        spec["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["path"] == json!(path))
            .unwrap()
    };
    let download = find(["library", "sync"])["args"]
        .as_array()
        .unwrap()
        .iter()
        .find(|arg| arg["name"] == "--download")
        .unwrap();
    assert_eq!(download["kind"], "option");
    assert_eq!(download["choices"], json!(["changed"]));
    assert_eq!(
        find(["library", "edit"])["groups"],
        json!([{"name": "value_source", "args": ["--value", "--value-file"], "required": true, "multiple": false}])
    );
    let globals = spec["data"]["global_args"].as_array().unwrap();
    assert!(
        globals
            .iter()
            .any(|arg| arg["name"] == "--json" && arg["kind"] == "flag")
    );

    let script = String::from_utf8(env.run("completions bash").stdout).unwrap();
    assert!(script.contains("_klms") && script.contains("library"));
    let zsh = env.data("completions zsh");
    assert_eq!(zsh["shell"], "zsh");
    assert!(zsh["script"].as_str().unwrap().contains("#compdef klms"));
    assert!(!env.run("completions tcsh").status.success());
}
