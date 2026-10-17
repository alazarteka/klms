mod fixture;
use fixture::{
    Env,
    server::{Response, Server},
};

#[test]
fn typed_and_preview_operands_refuse_actions_before_transport() {
    let server = Server::new(|_| Response::html("unexpected request"));
    let env = Env::at(&server);
    for target in [
        "/login/logout.php?sesskey=fixturekey".to_owned(),
        format!("{}/login/logout.php?sesskey=fixturekey", server.url()),
        "/mod/assign/view.php?id=7&sesskey=fixturekey".into(),
        "/mod/assign/view.php?id=7&action=delete".into(),
        "/mod/assign/view.php?id=7&id=8".into(),
        "/mod/assign/index.php?id=7".into(),
    ] {
        for command in ["assignments show", "files download"] {
            let extra = if command == "files download" {
                format!(" --out {}", env.path("file").display())
            } else {
                String::new()
            };
            assert!(
                !env.out(&format!("{command} '{target}'{extra}"))
                    .status
                    .success()
            );
        }
    }
    for command in [
        "quizzes show /login/logout.php?sesskey=fixturekey",
        "videos show /login/logout.php?sesskey=fixturekey",
        "boards posts /login/logout.php?sesskey=fixturekey",
        "boards show /login/logout.php?sesskey=fixturekey",
        "notices show /login/logout.php?sesskey=fixturekey",
        "assignments show /mod/quiz/view.php?id=7",
        "request get /login/logout.php?sesskey=fixturekey",
        "request get /grade/report/evil/delete.php?id=7",
        "request get /mod/evil/view.php?id=7",
        "request get /mod/assign/view.php?id=7&sesskey=fixturekey",
        "request get /mod/assign/view.php?id=7&action=delete",
    ] {
        assert!(!env.out(command).status.success(), "{command}");
    }
    // These are the behavior review's logout probes: neither may reach logout.
    assert!(server.recorded().is_empty(), "{:?}", server.requests());
}

#[test]
fn accepted_detail_urls_are_rebuilt_from_identity() {
    let server = Server::new(|_| {
        Response::html(
            "<body id='page-mod-assign-view' class='path-mod-assign cmid-7'><main><h1>Assignment</h1></main>",
        )
    });
    let env = Env::at(&server);
    env.ok(&format!(
        "assignments show '{}/mod/assign/view.php?%69d=007#ignored'",
        server.url()
    ));
    assert_eq!(
        server.requests(),
        ["GET /mod/assign/view.php?id=007 HTTP/1.1"]
    );
}

#[test]
fn redirects_refuse_actions_and_unrelated_endpoints_without_a_second_request() {
    for destination in [
        "/login/logout.php?sesskey=fixturekey",
        "/mod/assign/view.php?id=7&sesskey=fixturekey",
        "/mod/quiz/view.php?id=7",
    ] {
        for command in [
            "assignments show assign:7",
            "request get /mod/assign/view.php?id=7",
            "files download file:7",
        ] {
            let server = Server::new(move |_| {
                Response::html("")
                    .status("302 Found")
                    .header("Location", destination)
            });
            let env = Env::at(&server);
            let extra = if command.starts_with("files") {
                format!(" --out {}", env.path("file").display())
            } else {
                String::new()
            };
            assert!(!env.out(&format!("{command}{extra}")).status.success());
            assert_eq!(server.recorded().len(), 1);
            assert!(!server.recorded()[0].line.starts_with("GET /login/"));
        }
    }
}

#[test]
fn file_locators_allow_resource_redirects_to_pluginfile() {
    let server = Server::new(|request| {
        if request.target.starts_with("/mod/") {
            Response::html("")
                .status("302 Found")
                .header("Location", "/pluginfile.php/1/notes.html?forcedownload=1")
        } else {
            Response::bytes("text/html", "<html>Attachment</html>")
        }
    });
    let env = Env::at(&server);
    for (i, locator) in [
        "file:7",
        "activity:coursefile:7",
        "/mod/coursefile/view.php?id=7",
        "/pluginfile.php/1/notes.html",
    ]
    .iter()
    .enumerate()
    {
        let path = env.path(&format!("attachment-{i}"));
        env.ok(&format!(
            "files download '{locator}' --out {}",
            path.display()
        ));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "<html>Attachment</html>"
        );
    }
}

#[test]
fn a_redirect_to_the_login_page_is_an_expired_session() {
    let server = Server::new(|_| {
        Response::html("")
            .status("303 See Other")
            .header("Location", "/login/index.php")
    });
    let env = Env::at(&server);
    let (code, error) = env.fail("assignments show assign:7");
    assert_eq!(error["code"], "AUTH_REQUIRED");
    assert_ne!(code, 0);
    assert_eq!(server.recorded().len(), 1);
}

#[test]
fn stock_read_parameters_and_pluginfile_query_form_reach_transport() {
    let server = Server::new(|_| Response::html("<main>Fixture content</main>"));
    let env = Env::at(&server);
    for path in [
        "/mod/resource/view.php?id=7&redirect=1&forceview=1",
        "/mod/quiz/view.php?id=7&forceview=1",
        "/course/view.php?id=42&section=1&sectionid=4",
        "/calendar/view.php?view=upcoming&time=1&course=42&lookahead=30&category=2&cal_d=1&cal_m=2&cal_y=2030",
        "/pluginfile.php?file=%2F123%2Fmod_resource%2Fcontent%2F1%2Fnotes.pdf",
    ] {
        env.ok(&format!("request get '{path}'"));
        let before = server.recorded().len();
        for key in ["sesskey", "action", "delete", "redirect"] {
            // redirect is a read key only on module view endpoints.
            if key == "redirect" && path.starts_with("/mod/") {
                continue;
            }
            let (_, error) = env.fail(&format!("request get '{path}&{key}=1'"));
            assert_eq!(error["code"], "USAGE");
        }
        assert_eq!(server.recorded().len(), before);
    }
    // Typed detail operands still accept identity parameters only.
    let before = server.recorded().len();
    assert_eq!(
        env.fail("quizzes show /mod/quiz/view.php?id=7&forceview=1")
            .1["code"],
        "USAGE"
    );
    assert_eq!(server.recorded().len(), before);
}

#[test]
fn operation_redirects_accept_only_documented_read_transitions() {
    for (initial, destination) in [
        (
            "/mod/assign/view.php?id=007",
            "/mod/assign/view.php?id=7&forceview=1",
        ),
        (
            "/mod/resource/view.php?id=7",
            "/mod/resource/view.php?id=7&redirect=1",
        ),
        (
            "/mod/resource/view.php?id=7",
            "/pluginfile.php?file=%2F123%2Fnotes.pdf",
        ),
        (
            "/mod/coursefile/view.php?id=7",
            "/pluginfile.php/123/notes.pdf",
        ),
        (
            "/mod/folder/view.php?id=7",
            "/course/view.php?id=42&section=1",
        ),
        ("/mod/url/view.php?id=7", "/mod/page/view.php?id=9"),
        (
            "/mod/url/view.php?id=7",
            "/calendar/view.php?view=upcoming&lookahead=30",
        ),
    ] {
        let server = Server::new(move |request| {
            if request.target == initial {
                Response::html("")
                    .status("302 Found")
                    .header("Location", destination)
            } else {
                assert_eq!(request.target, destination);
                Response::html("<main>Read destination</main>")
            }
        });
        Env::at(&server).ok(&format!("request get '{initial}'"));
        assert_eq!(server.recorded().len(), 2);
    }
}

#[test]
fn identity_changing_redirects_are_refused_before_second_request_or_publication() {
    for (command, initial, destination) in [
        (
            "assignments show assign:7",
            "/mod/assign/view.php?id=7",
            "/mod/assign/view.php?id=8",
        ),
        (
            "files download file:7",
            "/mod/resource/view.php?id=7",
            "/mod/resource/view.php?id=8",
        ),
        (
            "files download file:7",
            "/mod/resource/view.php?id=7",
            "/mod/coursefile/view.php?id=7",
        ),
    ] {
        let server = Server::new(move |request| {
            assert_eq!(
                request.target, initial,
                "must refuse before requesting the other identity"
            );
            Response::html("")
                .status("302 Found")
                .header("Location", destination)
        });
        let env = Env::at(&server);
        let out = env.path("wrong-file");
        let args = if command.starts_with("files") {
            format!("{command} --out {}", out.display())
        } else {
            command.into()
        };
        assert_eq!(env.fail(&args).1["code"], "UPSTREAM_SHAPE_CHANGED");
        assert_eq!(server.recorded().len(), 1);
        assert!(!out.exists());
    }
}

#[test]
fn policy_refusals_from_server_links_and_redirects_are_upstream_errors() {
    for destination in [
        "/mod/assign/view.php?id=7&sesskey=secret",
        "/mod/folder/download_folder.php?id=7",
        "/mod/url/view.php?id=7&action=delete",
    ] {
        let server = Server::new(move |_| {
            Response::html("")
                .status("302 Found")
                .header("Location", destination)
        });
        assert_eq!(
            Env::at(&server)
                .fail("request get /mod/url/view.php?id=7")
                .1["code"],
            "UPSTREAM_SHAPE_CHANGED"
        );
        assert_eq!(server.recorded().len(), 1);
    }
    let server = fixture::lib_server(|request, _| {
        (request.target == "/course/view.php?id=42")
            .then(|| Response::html(fixture::manifest().replace("id=7'", "id=7&action=delete'")))
    });
    let env = Env::at(&server);
    let result = env.sync("");
    assert_eq!(result["data"]["status"], "incomplete");
    assert!(!server.recorded().iter().any(|r| r.line.contains("action=")));
}

#[test]
fn folder_zip_and_folder_download_operands_are_not_supported() {
    let server = Server::new(|_| panic!("folder downloads must be rejected locally"));
    let env = Env::at(&server);
    for source in ["activity:folder:7", "/mod/folder/download_folder.php?id=7"] {
        assert_eq!(
            env.fail(&format!(
                "files download {source} --out {}",
                env.path("folder.zip").display()
            ))
            .1["code"],
            "USAGE"
        );
    }
    assert_eq!(
        env.fail("request get /mod/folder/download_folder.php?id=7")
            .1["code"],
        "USAGE"
    );
    assert!(server.recorded().is_empty());
}

#[test]
fn sync_accepts_server_resource_read_parameters() {
    for query in ["", "&redirect=1&forceview=0", "&redirect=1&forceview=1"] {
        let server = fixture::lib_server(move |request, _| match request.target.as_str() {
            "/course/view.php?id=42" => Some(Response::html(
                fixture::manifest().replace("view.php?id=7", &format!("view.php?id=7{query}")),
            )),
            path if path.starts_with("/mod/resource/") => {
                assert!(path.ends_with("&forceview=1"));
                Some(Response::html(fixture::DETAIL))
            }
            _ => None,
        });
        let env = Env::at(&server);
        assert_eq!(env.sync("")["data"]["status"], "complete");
        assert_eq!(env.data("library show file:7")["remote_state"], "present");
    }
}

#[test]
fn typed_foreign_origins_and_userinfo_remain_usage_errors() {
    let server = Server::new(|_| panic!("invalid typed URL must not be requested"));
    let env = Env::at(&server);
    for (command, endpoint) in [
        ("assignments show", "/mod/assign/view.php?id=7"),
        ("boards posts", "/mod/courseboard/view.php?id=7"),
        ("boards show", "/mod/courseboard/article.php?id=7&bwid=8"),
        ("videos show", "/mod/vod/view.php?id=7"),
        ("request get", "/course/view.php?id=7"),
        ("files download", "/pluginfile.php/1/file"),
    ] {
        for origin in [
            "https://foreign.invalid".to_owned(),
            server.url().replace("http://", "http://user@"),
        ] {
            let extra = if command.starts_with("files") {
                format!(" --out {}", env.path("file").display())
            } else {
                String::new()
            };
            assert_eq!(
                env.fail(&format!("{command} '{origin}{endpoint}'{extra}"))
                    .1["code"],
                "USAGE"
            );
        }
    }
    assert!(server.recorded().is_empty());
}

#[test]
fn sync_redirects_discover_file_bytes_and_isolate_inline_folders() {
    const BYTES: &[u8] = b"%PDF-fixture\0\xff";
    for (kind, destination) in [
        ("resource", "/pluginfile.php/123/notes.pdf"),
        ("resource", "/pluginfile.php?file=%2F123%2Fnotes.pdf"),
        ("coursefile", "/pluginfile.php/123/notes.pdf"),
        ("coursefile", "/pluginfile.php?file=%2F123%2Fnotes.pdf"),
        ("folder", "/course/view.php?id=42&section=1"),
    ] {
        let force = if kind == "resource" {
            "&forceview=1"
        } else {
            ""
        };
        let module = format!("/mod/{kind}/view.php?id=7{force}");
        let server = fixture::lib_server(move |r, _| match r.target.as_str() {
            "/course/view.php?id=42" => {
                Some(Response::html(fixture::course(&[(kind, 7, "Notes")])))
            }
            path if path == module => Some(
                Response::html("")
                    .status("302 Found")
                    .header("Location", destination),
            ),
            "/course/view.php?id=42&section=1" => Some(Response::html(
                "<main><li id='module-8'><a href='/pluginfile.php/other'>Other</a></li><div data-id='7'><span class='activityname'>Notes</span><a href='/pluginfile.php/123/notes.pdf'>notes.pdf</a></div></main>",
            )),
            path if path.starts_with("/pluginfile.php") => {
                assert!(!path.contains("other"));
                Some(Response::bytes("application/pdf", BYTES))
            }
            _ => None,
        });
        let env = Env::at(&server);
        let result = env.sync("--files --download changed");
        assert_eq!(result["data"]["status"], "complete", "{result}");
        assert_eq!(result["data"]["representations"], 1);
        assert_eq!(result["data"]["blobs_added"], 1);
        assert!(
            server
                .requests()
                .iter()
                .any(|line| line.starts_with("HEAD /pluginfile.php"))
        );
        let out = env.path("export.pdf");
        env.ok(&format!(
            "library export representation:1 --out {}",
            out.display()
        ));
        assert_eq!(std::fs::read(out).unwrap(), BYTES);
        if kind != "folder" {
            let locator = if kind == "resource" {
                destination.to_owned()
            } else {
                "activity:coursefile:7".into()
            };
            let out = env.path("download.pdf");
            env.ok(&format!(
                "files download '{locator}' --out {}",
                out.display()
            ));
            assert_eq!(std::fs::read(out).unwrap(), BYTES);
        }
        if kind == "resource" {
            assert_eq!(
                env.fail("request get /mod/resource/view.php?id=7&forceview=1")
                    .1["code"],
                "CONTENT_UNAVAILABLE"
            );
        }
    }
}
