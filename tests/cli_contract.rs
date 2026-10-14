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
    for command in ["update", "upgrade"] {
        let output = env
            .cmd(false, "")
            .args([command, "--help", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}: {:?}", output.stderr);
        assert!(output.stderr.is_empty());
        let value = parse(&output.stdout);
        assert_eq!(value["ok"], true);
        assert_eq!(value["command"], "help");
        assert!(value["data"]["text"].as_str().unwrap().contains("Usage:"));
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
    assert_eq!(env.ok("auth status")["schema_version"], "5");
    env.state();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let output = env.out(&format!(
        "--base-url http://{} auth status",
        socket.local_addr().unwrap()
    ));
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
fn agenda_kind_filter_selects_rows_and_warns_when_a_kind_matches_nothing() {
    // The assignment card plus a site event (no module link) on the same day.
    let page = include_str!("fixtures/localized/calendar.html")
        .replace("&amp;time=1899989400", "")
        .replace("내일", "오늘");
    let card = &page[page.find("<div class=\"event").unwrap()..page.find("</main>").unwrap()];
    let event = card
        .replace(
            "/mod/assign/view.php?id=7&amp;action=editsubmission",
            "/calendar/event.php?id=99",
        )
        .replace("Reading response is due", "Department seminar");
    let page = page.replace("</main>", &format!("{event}</main>"));
    let server = Server::new(move |_| Response::html(page.as_bytes()));
    let env = Env::at(&server);
    let all = env.ok("today");
    let today = all["data"][0]["starts_at"].as_str().unwrap()[..10].to_owned();
    let kinds: Vec<_> = all["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["kind"].clone())
        .collect();
    assert_eq!(kinds, [json!("assign"), json!("event")]);
    for command in ["today", "upcoming"] {
        let span = match command {
            "today" => format!("for {today}"),
            _ => format!("from {today} through "),
        };
        let run = |kinds: &str| {
            let value = env.ok(&format!("{command}{kinds}"));
            assert_eq!(value["meta"]["complete"], true);
            let titles: Vec<_> = (value["data"].as_array().unwrap().iter())
                .map(|row| row["title"].as_str().unwrap().to_owned())
                .collect();
            let warnings: Vec<_> = (value["warnings"].as_array().unwrap().iter())
                .map(|warning| warning.as_str().unwrap().to_owned())
                .collect();
            (titles, warnings)
        };
        let (assign, seminar) = ("Reading response is due", "Department seminar");
        let quiet: &[String] = &[];
        assert_eq!(
            run(""),
            (vec![assign.into(), seminar.into()], quiet.to_vec())
        );
        for kinds in [" --kind assign", " --kind ASSIGN", " --kind Assign,assign"] {
            assert_eq!(run(kinds), (vec![assign.into()], quiet.to_vec()), "{kinds}");
        }
        assert_eq!(run(" --kind EVENT"), (vec![seminar.into()], quiet.to_vec()));

        // A kind that matches nothing (a typo, or simply none due) says so and
        // lists what was there, whether or not another kind matched.
        for (kinds, titles, missing) in [
            (" --kind quiz,assign", vec![assign.to_owned()], "quiz"),
            (" --kind quiz --kind assignment", vec![], "assignment, quiz"),
            (" --kind zoom", vec![], "zoom"),
        ] {
            let (rows, warnings) = run(kinds);
            assert_eq!(rows, titles, "{kinds}");
            let [warning] = &warnings[..] else {
                panic!("{kinds}: {warnings:?}")
            };
            let head = format!("--kind {missing} matched no calendar row {span}");
            assert!(warning.starts_with(&head), "{warning}");
            assert!(
                warning.ends_with(" (kinds present: assign, event)"),
                "{warning}"
            );
        }
        let (exit, error) = env.fail(&format!("{command} --kind 'a b'"));
        assert_eq!((exit, &error["code"]), (2, &json!("USAGE")));
    }
    let human = env.run("today --kind quiz");
    assert_eq!(
        String::from_utf8(human.stdout).unwrap().trim(),
        format!("Nothing of kind quiz scheduled for {today}.")
    );
    assert_eq!(
        String::from_utf8(human.stderr).unwrap().trim(),
        format!(
            "warning: --kind quiz matched no calendar row for {today} (kinds present: assign, event)"
        )
    );
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
        "{}/pluginfile.php/7/notes.pdf?forcedownload=1",
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
    let (_, error) = env.fail(&args);
    assert_eq!(error["code"], "CONFIG_ERROR");
    assert!(
        (error["message"].as_str().unwrap()).contains("destination already exists"),
        "{error}"
    );
    assert_eq!(fs::read(&out).unwrap(), b"notes");
    let leftovers = fs::read_dir(out.parent().unwrap())
        .unwrap()
        .filter(|entry| (entry.as_ref().unwrap().file_name().to_string_lossy()).ends_with(".part"))
        .count();
    assert_eq!(leftovers, 0, "no temporary file is left behind");
    assert_eq!(
        server.requests(),
        ["GET /pluginfile.php/7/notes.pdf?forcedownload=1 HTTP/1.1"]
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
fn typed_show_rejects_a_mismatched_operand_before_transport() {
    let server = html_server("<main>Dashboard</main>");
    let (_, error) = Env::at(&server).fail("assignments show /my/");
    assert!(server.requests().is_empty());
    assert_eq!(error["code"], "USAGE");
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
    // Zero padding keeps the same identity. Unexpected parameters are refused.
    let server = Server::new(|request| {
        if request.target == "/mod/assign/view.php?id=007" {
            Response::html("")
                .status("302 Found")
                .header("Location", "/mod/assign/view.php?id=7")
        } else {
            Response::html(
                "<body id='page-mod-assign-view' class='path-mod-assign cmid-7'><main><h1>Requested assignment</h1></main>",
            )
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
fn help_is_the_agent_manual() {
    let env = Env::new();
    let help = |args: &str| {
        let output = env.run(args);
        assert!(output.status.success(), "{args:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let top = help("--help");
    let spec = env.data("spec");
    let commands = spec["commands"].as_array().unwrap();
    let examples: Vec<_> = top
        .lines()
        .filter_map(|line| line.trim().strip_prefix("klms "))
        .collect();
    assert!(!examples.is_empty());
    for example in examples {
        let words: Vec<_> = example
            .split_whitespace()
            .filter(|word| *word != "--json")
            .collect();
        assert!(
            commands.iter().any(|command| {
                let path: Vec<_> = command["path"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|part| part.as_str().unwrap())
                    .collect();
                words.starts_with(&path)
            }),
            "manual command is absent from spec: {example}"
        );
    }
    assert!(top.contains("complete") && top.contains("source_complete") && top.contains("--limit"));
    assert!(top.contains("upstream") && top.contains("truncation"));
    let login = help("auth login --help");
    assert!(!login.contains("--password") && !login.contains("--otp"));
    for command in commands {
        for arg in command["args"].as_array().unwrap() {
            assert!(!matches!(
                arg["name"].as_str(),
                Some("--password" | "--otp")
            ));
        }
    }
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

#[test]
fn spec_serializes_argument_requires_and_conflicts() {
    let env = Env::new();
    let spec = env.data("spec");
    let argument = |path: [&str; 2], name: &str| {
        spec["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["path"] == json!(path))
            .unwrap()["args"]
            .as_array()
            .unwrap()
            .iter()
            .find(|argument| argument["name"] == name)
            .unwrap()
    };
    assert_eq!(
        argument(["courses", "list"], "--year")["requires"],
        json!(["--semester"])
    );
    assert_eq!(
        argument(["courses", "list"], "--semester")["requires"],
        json!(["--year"])
    );
    assert_eq!(
        argument(["courses", "list"], "--all")["conflicts"],
        json!(["--year", "--semester"])
    );
    assert!(
        argument(["auth", "login"], "--code")["conflicts"]
            .as_array()
            .unwrap()
            .contains(&json!("--user"))
    );
    assert_eq!(
        argument(["auth", "login"], "--insecure-storage")["requires"],
        json!(["--remember-password"])
    );
}

#[test]
fn native_video_download_is_static_private_complete_and_never_overwrites() {
    let mp4 = b"\x00\x00\x00\x18ftypisomsynthetic-media";
    let server = Server::new(move |request| match request.target.as_str() {
        "/mod/vod/view.php?id=7" => Response::html(
            "<body id='page-mod-vod-view' class='path-mod-vod cmid-7'><main><h1>Synthetic lecture</h1></main>",
        ),
        "/mod/vod/viewer/index.php?id=7" => Response::html(
            r#"<script>flowplayer('#player', {clip: {sources: [{type: "video/mp4", src: "/vod1/synthetic.mp4"}]}});</script>"#,
        ),
        "/vod1/synthetic.mp4" => Response::bytes("video/mp4", mp4.to_vec()),
        _ => panic!("unexpected request: {}", request.target),
    });
    let env = Env::at(&server);
    let out = env.path("lecture.mp4");
    let args = format!("videos download vod:7 --out {}", out.display());
    let result = env.data(&args);
    assert_eq!(result["bytes"], mp4.len());
    assert_eq!(fs::read(&out).unwrap(), mp4);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let calls = server.requests().len();
    assert_eq!(env.fail(&args).1["code"], "CONFIG_ERROR");
    assert_eq!(server.requests().len(), calls);
    assert_eq!(fs::read(&out).unwrap(), mp4);
    assert!(
        server
            .requests()
            .iter()
            .all(|r| !r.contains("playtime") && !r.contains("progress"))
    );
}

#[test]
fn failed_native_video_download_cleans_temporary_and_output() {
    for (response, max_bytes, code) in [
        (
            Response::html("<!doctype html><html>permission denied</html>"),
            1024,
            "UPSTREAM_ERROR",
        ),
        (
            Response::bytes("video/mp4", b"\x00\x00\x00\x18ftypisom-too-large".to_vec()),
            12,
            "LIMIT_EXCEEDED",
        ),
    ] {
        let responses = std::sync::Mutex::new(std::collections::VecDeque::from([
            Response::html(
                "<body id='page-mod-vod-view' class='path-mod-vod cmid-7'><main><h1>Lecture</h1></main>",
            ),
            Response::html(
                r#"<script>flowplayer('#player', {clip: {sources: [{type: "video/mp4", src: "/vod1/synthetic.mp4"}]}});</script>"#,
            ),
            response,
        ]));
        let server = Server::new(move |_| {
            responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("extra request")
        });
        let env = Env::at(&server);
        let out = env.path("failed.mp4");
        let (_, error) = env.fail(&format!(
            "videos download vod:7 --out {} --max-bytes {max_bytes}",
            out.display()
        ));
        assert_eq!(error["code"], code, "{error}");
        assert_eq!(server.requests().len(), 3);
        assert!(!out.exists());
        let parent = out.parent().unwrap();
        assert!(fs::read_dir(parent).unwrap().all(|p| {
            !p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".klms-video-")
        }));
    }
}

#[test]
fn video_grammar_rejects_ambiguous_ids_and_invalid_limits() {
    let env = Env::new();
    for args in [
        "videos download vod:7 --out x --max-seconds 0",
        "videos download vod:7 --out x --max-bytes 0",
        "videos download vod:7",
        "videos download 7 --out x --max-seconds 86401",
        "videos download vod:7 --out x --stall-seconds 0",
    ] {
        assert_eq!(env.fail(args).1["code"], "USAGE", "{args}");
    }
    let server = html_server("<main>unused</main>");
    assert_eq!(Env::at(&server).fail("videos show 7").1["code"], "USAGE");
    assert!(server.requests().is_empty());
}

#[test]
fn video_list_reads_only_the_course_page_and_never_opens_activities() {
    let server = Server::new(|request| match request.target.as_str() {
        "/course/view.php?id=42" => Response::html(
            r#"<body id="page-course-view-topics" class="course-42"><main class="course-content"><li class="activity modtype_vod" id="module-6"><a href="/mod/vod/view.php?id=6"><span class="instancename">Native lecture</span></a></li><li class="activity modtype_lti" id="module-7"><a href="/mod/lti/view.php?id=7"><span class="instancename">Panopto Lecture</span></a></li><li class="activity modtype_panoptocourseembed" id="module-8"><span class="instancename">Embedded collection</span></li><li class="activity modtype_assign" id="module-9"><a href="/mod/assign/view.php?id=9"><span class="instancename">Homework</span></a></li></main>"#,
        ),
        _ => panic!("videos list opened an activity: {}", request.target),
    });
    let result = Env::at(&server).ok("videos list --course course:42");
    assert_eq!(server.requests(), ["GET /course/view.php?id=42 HTTP/1.1"]);
    let rows = result["data"].as_array().unwrap();
    let summary = rows
        .iter()
        .map(|v| {
            (
                v["ref"].as_str().unwrap(),
                v["video_type"].as_str().unwrap(),
                v["downloadable"].is_null(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            ("vod:6", "recording", true),
            ("lti:7", "lti", true),
            ("panoptocourseembed:8", "panopto", true),
        ]
    );
    assert_eq!(result["meta"]["complete"], true);
    assert_eq!(result["warnings"], json!([]));
}

#[test]
fn native_viewer_redirects_cannot_change_the_recording_identity() {
    let server = Server::new(|request| match request.target.as_str() {
        "/mod/vod/view.php?id=7" => Response::html(
            "<body id='page-mod-vod-view' class='path-mod-vod cmid-7'><main><h1>Requested lecture</h1></main>",
        ),
        "/mod/vod/viewer/index.php?id=7" => Response::html("")
            .status("302 Found")
            .header("Location", "/mod/vod/viewer/index.php?id=8"),
        "/mod/vod/viewer/index.php?id=8" => Response::html(
            r#"<script>flowplayer('#player', {clip:{sources:[{type:"video/mp4",src:"/vod1/other.mp4"}]}});</script>"#,
        ),
        _ => panic!("unexpected media request"),
    });
    let env = Env::at(&server);
    let out = env.path("wrong.mp4");
    let error = env
        .fail(&format!("videos download vod:7 --out {}", out.display()))
        .1;
    assert_eq!(error["code"], "UPSTREAM_SHAPE_CHANGED");
    assert!(error["message"].as_str().unwrap().contains("identity"));
    assert!(!out.exists());
}

#[test]
fn course_listings_read_every_week_of_the_paged_week_format() {
    // The default page of KLMS's paged week format shows only the current week; the picker's
    // "All" choice (dayselect 0) is served at section=0. Every course-page command must merge it.
    let section = |week: u32, modules: &str| {
        format!(
            "<li class='section'><h3 class='sectionname'>week {week}</h3><ul>{modules}</ul></li>"
        )
    };
    let module = |kind: &str, id: u32, name: &str| {
        format!(
            "<li class='activity modtype_{kind}' id='module-{id}'><a href='/mod/{kind}/view.php?id={id}'>\
             <span class='instancename'>{name}</span></a></li>"
        )
    };
    let current = format!(
        "<a href='javascript:M.course.format.dayselect(0, 42, 0)'>All</a><body id='page-course-view-topics' class='course-42'><main class='course-content'>{}</main>",
        section(5, &module("resource", 7, "Week 5 slides"))
    );
    let all = format!(
        "<body id='page-course-view-topics' class='course-42'><main class='course-content'>{}{}</main>",
        section(
            1,
            &(module("resource", 3, "Week 1 slides") + &module("vod", 4, "Week 1 lecture"))
        ),
        section(5, &module("resource", 7, "Week 5 slides"))
    );
    let server = Server::new(move |request| match request.target.as_str() {
        "/course/view.php?id=42" => Response::html(current.as_bytes()),
        "/course/view.php?id=42&section=0" => Response::html(all.as_bytes()),
        other => panic!("unexpected request: {other}"),
    });
    let env = Env::at(&server);
    let refs = |args: &str| -> Vec<String> {
        let value = env.ok(args);
        assert_eq!(value["meta"]["complete"], true, "{args}");
        let rows = value["data"].as_array().unwrap();
        rows.iter()
            .map(|row| row["ref"].as_str().unwrap().to_owned())
            .collect()
    };
    // Rows of the default page come first, then the weeks only the "All" view lists, once each.
    assert_eq!(
        refs("activities list --course 42"),
        ["file:7", "file:3", "vod:4"]
    );
    assert_eq!(
        refs("activities list --course 42 --week 1"),
        ["file:3", "vod:4"]
    );
    assert_eq!(refs("activities list --course 42 --week 5"), ["file:7"]);
    assert_eq!(refs("files list --course 42"), ["file:7", "file:3"]);
    assert_eq!(refs("videos list --course 42"), ["vod:4"]);
    assert!(server.requests().chunks(2).all(|pair| pair
        == [
            "GET /course/view.php?id=42 HTTP/1.1",
            "GET /course/view.php?id=42&section=0 HTTP/1.1"
        ]));

    // Other course formats stay a single request.
    let plain = format!(
        "<body id='page-course-view-topics' class='course-42'><main class='course-content'>{}</main>",
        module("resource", 7, "Slides")
    );
    let server = Server::new(move |request| match request.target.as_str() {
        "/course/view.php?id=42" => Response::html(plain.as_bytes()),
        other => panic!("unexpected request: {other}"),
    });
    assert_eq!(
        Env::at(&server).data("files list --course 42")[0]["ref"],
        "file:7"
    );
    assert_eq!(server.requests(), ["GET /course/view.php?id=42 HTTP/1.1"]);
}

#[test]
fn agenda_skips_undated_events_with_a_warning_instead_of_failing() {
    // An event KLMS lists without any time (e.g. a quiz with no close date) is not an error:
    // the dated events are returned, the undated one is counted in a warning, and `calendar list`
    // still shows it.
    let dated = include_str!("fixtures/localized/calendar.html")
        .replace("&amp;time=1899989400", "")
        .replace("내일", "오늘");
    let page = dated.replace(
        "</main>",
        "<div class='event'><a href='/mod/quiz/view.php?id=9'>Attempt quiz now</a></div></main>",
    );
    let server = Server::new(move |_| Response::html(page.as_bytes()));
    let env = Env::at(&server);
    for command in ["today", "upcoming"] {
        let value = env.ok(command);
        let warnings = value["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{command}");
        let warning = warnings[0].as_str().unwrap();
        assert!(
            warning.contains('1') && warning.contains("date") && warning.contains("calendar list"),
            "{warning}"
        );
        assert_eq!(value["meta"]["complete"], true, "{command}");
        assert_eq!(value["data"].as_array().unwrap().len(), 1, "{command}");
        assert_eq!(value["data"][0]["ref"], "assign:7");
    }
    let listed = env.ok("calendar list");
    assert_eq!(listed["data"].as_array().unwrap().len(), 2);
    assert_eq!(listed["data"][1]["ref"], "quiz:9");
    assert_eq!(listed["data"][1]["starts_at"], Value::Null);

    // A time that is present but unreadable still fails the agenda rather than hiding the event.
    let page = dated.replace(
        "</main>",
        "<div class='event'><a href='/mod/quiz/view.php?id=9'>Quiz</a><time>sometime</time></div></main>",
    );
    let server = Server::new(move |_| Response::html(page.as_bytes()));
    let (code, error) = Env::at(&server).fail("today");
    assert_eq!(
        (code, &error["code"]),
        (30, &json!("UPSTREAM_SHAPE_CHANGED"))
    );
}
