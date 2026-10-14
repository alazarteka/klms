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
        "activity:folder:7",
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
