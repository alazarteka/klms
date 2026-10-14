mod fixture;
use fixture::{
    Env,
    server::{Response, Server},
};

#[test]
fn surviving_containers_with_unknown_children_are_shape_errors() {
    // The behavior review's drift fixtures, with their outer containers retained.
    for (command, body) in [
        (
            "activities list --course course:42",
            "<body id='page-course-view-topics' class='course-42'><main class='course-content'><div class='module' data-cmid='7'><a href='/mod/assign/view.php?id=7'>Essay</a></div></main>",
        ),
        (
            "calendar list",
            "<main class='calendarwrapper'><div class='new-event'><a href='/mod/assign/view.php?id=7'>Essay</a></div></main>",
        ),
        (
            "boards posts board:10",
            "<table class='generaltable'><tr><td><a href='/mod/courseboard/article.php?id=10&bwid=20'></a></td></tr></table>",
        ),
        ("assignments show assign:7", "<body>Maintenance</body>"),
        ("courses show course:42", "<body>Maintenance</body>"),
    ] {
        let server = Server::new(move |_| Response::html(body));
        let (code, error) = Env::at(&server).fail(command);
        assert_eq!(code, 30, "{command}");
        assert_eq!(error["code"], "UPSTREAM_SHAPE_CHANGED", "{command}");
    }
}

#[test]
fn uncertain_sync_keeps_previously_observed_resources() {
    for drift in [
        "<body id='page-course-view-topics' class='course-42'><main class='course-content'><div class='module' data-cmid='7'><a href='/mod/resource/view.php?id=7'>Notes</a></div></main>",
        "<body>Maintenance</body>",
    ] {
        let server = fixture::lib_server(move |request, run| {
            (run == 1 && request.target == "/course/view.php?id=42").then(|| Response::html(drift))
        });
        let env = Env::at(&server);
        env.sync("");
        let next = env.sync("");
        assert_eq!(next["data"]["status"], "incomplete");
        assert_eq!(next["data"]["source_complete"], false);
        assert_eq!(env.data("library show file:7")["remote_state"], "present");
        assert!(
            !env.data("library changes")
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["kind"] == "resource_not_observed")
        );
    }
}

#[test]
fn failed_details_prevent_absences_in_their_course_scope() {
    let server = fixture::lib_server(|request, run| {
        if run == 1 {
            match request.target.as_str() {
                "/course/view.php?id=42" => {
                    Some(Response::html(fixture::course(&[("resource", 8, "Other")])))
                }
                "/mod/resource/view.php?id=8" => Some(Response::html("<body>Maintenance</body>")),
                _ => None,
            }
        } else {
            None
        }
    });
    let env = Env::at(&server);
    env.sync("");
    assert_eq!(env.sync("")["data"]["status"], "incomplete");
    assert_eq!(env.data("library show file:7")["remote_state"], "present");
}

#[test]
fn recognized_empty_collections_remain_complete() {
    for (command, body) in [
        (
            "activities list --course course:42",
            "<body id='page-course-view-topics' class='course-42'><main class='course-content'></main>",
        ),
        ("calendar list", "<main class='calendarwrapper'></main>"),
        (
            "calendar list",
            "<main class='calendarwrapper'>There are no upcoming events</main>",
        ),
        (
            "boards posts board:10",
            "<table class='generaltable'></table>",
        ),
    ] {
        let server = Server::new(move |_| Response::html(body));
        let result = Env::at(&server).ok(command);
        assert_eq!(result["data"], serde_json::json!([]));
        assert_eq!(result["meta"]["complete"], true);
    }
}

#[test]
fn board_drift_is_visible_to_notice_listing_and_sync() {
    let server = Server::new(|request| match request.target.as_str() {
        "/my/" => Response::html(fixture::DASHBOARD),
        "/course/view.php?id=42" => Response::html(fixture::notice_manifest()),
        "/mod/courseboard/view.php?id=9" => Response::html(
            "<table class='generaltable'><tr><td><a href='/mod/courseboard/article.php?id=9&bwid=10'></a></td></tr></table>",
        ),
        _ => panic!("unexpected request"),
    });
    let env = Env::at(&server);
    assert_eq!(
        env.fail("notices list --course course:42").1["code"],
        "UPSTREAM_SHAPE_CHANGED"
    );
    let sync = env.sync("--notices");
    assert_eq!(sync["data"]["status"], "incomplete");
    assert_eq!(sync["data"]["source_complete"], false);
}

// Synthetic final-review probes from /tmp/klms-review/final/focused.py.
#[test]
fn empty_course_sections_and_board_empty_state_rows_are_complete() {
    for (command, body) in [
        (
            "activities list --course course:42",
            "<body id='page-course-view-topics' class='course-42'><main class='course-content'><h1>Empty course</h1><ul class='topics'><li class='section'><h3>Week 1</h3><ul class='section'></ul></li></ul></main></body>",
        ),
        (
            "boards posts board:10",
            "<table class='generaltable'><tr><th>Subject</th></tr><tr><td colspan='4'>No posts found</td></tr></table>",
        ),
        (
            "calendar list",
            "<main class='calendarwrapper'><h2>Upcoming events</h2><ul></ul><p>No events</p></main>",
        ),
    ] {
        let server = Server::new(move |_| Response::html(body));
        let result = Env::at(&server).ok(command);
        assert_eq!(result["data"], serde_json::json!([]));
        assert_eq!(result["meta"]["complete"], true);
    }
}

#[test]
fn mixed_relative_drift_preserves_file_eight_across_two_syncs() {
    let server = fixture::lib_server(|request, run| match request.target.as_str() {
        "/course/view.php?id=42" => Some(Response::html(if run == 0 {
            fixture::course(&[("resource", 7, "First"), ("resource", 8, "Second")])
        } else {
            fixture::course(&[("resource", 7, "First")]).replace("</main>", "<div class='module' data-cmid='8'><a href='mod/resource/view.php?id=8'>Second</a></div></main>")
        })),
        "/mod/resource/view.php?id=8" => {
            Some(Response::html(fixture::DETAIL.replace("cmid-7", "cmid-8")))
        }
        _ => None,
    });
    let env = Env::at(&server);
    assert_eq!(env.sync("")["data"]["status"], "complete");
    assert_eq!(env.sync("")["data"]["status"], "incomplete");
    assert_eq!(env.data("library show file:8")["remote_state"], "present");
    assert_eq!(
        env.fail("activities list --course course:42").1["code"],
        "UPSTREAM_SHAPE_CHANGED"
    );
    assert!(
        !env.data("library changes")
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["kind"] == "resource_not_observed")
    );
}

#[test]
fn detail_body_must_identify_the_expected_kind_and_id() {
    for (command, body) in [
        (
            "assignments show assign:7",
            "<main><h1>Maintenance</h1></main>",
        ),
        (
            "assignments show assign:7",
            "<body id='page-mod-quiz-view' class='path-mod-quiz cmid-7'><main><h1>Quiz</h1></main></body>",
        ),
        (
            "assignments show assign:7",
            "<body id='page-mod-assign-view' class='path-mod-assign cmid-8'><main><h1>Other assignment</h1></main></body>",
        ),
        (
            "assignments show assign:7",
            "<body id='page-mod-assign-view' class='path-mod-assign'><main><h1>Missing identity</h1></main></body>",
        ),
        (
            "courses show course:42",
            "<body id='page-course-view-topics' class='course-43'><main class='course-content'></main></body>",
        ),
        (
            "activities list --course course:42",
            "<body id='page-course-view-topics' class='course-43'><main class='course-content'></main></body>",
        ),
    ] {
        let server = Server::new(move |_| Response::html(body));
        assert_eq!(
            Env::at(&server).fail(command).1["code"],
            "UPSTREAM_SHAPE_CHANGED",
            "{command}"
        );
    }
}

#[test]
fn collection_links_are_resolved_before_drift_checks() {
    for body in [
        "<main class='calendarwrapper'><div class='event'><a href='/mod/quiz/view.php?id=7'>Quiz opens</a></div><div class='new-event'><a href='/mod/quiz/view.php?id=7'>Quiz closes</a></div></main>",
        "<main class='calendarwrapper'><div class='event'><a href='/mod/assign/view.php?id=7'>Known</a></div><div class='new-event'><a href='mod/quiz/view.php?id=8'>Unknown</a></div></main>",
        "<main class='calendarwrapper'><div class='new-event'><a href='calendar/event.php?id=8'>Unknown calendar event</a></div></main>",
        "<main class='calendarwrapper'><div class='event'><a href='/mod/assign/view.php'>No identity</a></div></main>",
    ] {
        let server = Server::new(move |_| Response::html(body));
        assert_eq!(
            Env::at(&server).fail("calendar list").1["code"],
            "UPSTREAM_SHAPE_CHANGED"
        );
    }
    let server = Server::new(|_| {
        Response::html(
            "<table class='generaltable'><tr><td><a href='article.php?id=10&bwid=20'>Notice</a></td></tr></table>",
        )
    });
    assert_eq!(
        Env::at(&server).data("boards posts board:10")[0]["ref"],
        "board-post:10:20"
    );
}
