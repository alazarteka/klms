//! Synthetic, loopback-only pagination regressions.
use crate::fixture::{
    self, Env, article,
    server::{Response, Server},
};

fn board(pager: &str) -> String {
    format!("{}{pager}", fixture::NOTICE_LIST)
}

fn server(pager: &'static str) -> Server {
    fixture::lib_server(move |request, _| match request.target.as_str() {
        "/course/view.php?id=42" => Some(Response::html(fixture::notice_manifest())),
        "/mod/courseboard/view.php?id=9" => Some(Response::html(board(pager))),
        "/mod/courseboard/view.php?id=9&page=2" => Some(Response::html(
            fixture::NOTICE_LIST
                .replace("bwid=10", "bwid=11")
                .replace("Hello", "Older notice"),
        )),
        "/mod/courseboard/article.php?id=9&bwid=10" => {
            Some(Response::html(article("Hello", "", "First body")))
        }
        "/mod/courseboard/article.php?id=9&bwid=11" => {
            Some(Response::html(article("Older notice", "", "Second body")))
        }
        _ => None,
    })
}

const NEXT: &str = "<a rel='next' href='/mod/courseboard/view.php?id=9&amp;page=2'>Next</a>";
const NUMERIC: &str = "<div class='paging'><strong>1</strong><a href='/mod/courseboard/view.php?id=9&amp;page=2'>2</a></div>";

#[test]
fn reconstructed_observed_first_pager_and_synthetic_later_pages_are_exhausted() {
    // First-page layout reconstructs the observed DOM with a loopback origin and
    // synthetic board ID. Pages 2–5 are explicitly hypothetical variants.
    let server =
        fixture::lib_server(|request, _| {
            if request.target == "/course/view.php?id=42" {
                return Some(Response::html(fixture::notice_manifest()));
            }
            if request.target.starts_with("/mod/courseboard/view.php?") {
                let page: u32 = request
                    .target
                    .split("page=")
                    .nth(1)
                    .unwrap_or("1")
                    .parse()
                    .unwrap();
                let links = (1..=5).map(|n| if n == page {
                format!("<li class='active'><a href='#'>{n}</a></li>")
            } else {
                format!("<li><a href='/mod/courseboard/view.php?id=9&amp;page={n}'>{n}</a></li>")
            }).collect::<String>();
                let post = fixture::NOTICE_LIST.replace("bwid=10", &format!("bwid={}", 9 + page));
                return Some(Response::html(format!(
                    "{post}<div class='pagination pagination-centered'><ul>{links}</ul></div>"
                )));
            }
            if request.target.starts_with("/mod/courseboard/article.php?") {
                return Some(Response::html(article(
                    "Synthetic notice",
                    "",
                    "Synthetic body",
                )));
            }
            None
        });
    let env = Env::at(&server);
    for command in ["notices list --course course:42", "boards posts board:9"] {
        let result = env.ok(command);
        assert_eq!(result["data"].as_array().unwrap().len(), 5);
        assert_eq!(result["meta"]["complete"], true);
        assert_eq!(result["meta"]["total"], 5);
    }
    let sync = env.sync("--notices");
    assert_eq!(sync["data"]["status"], "complete");
    assert_eq!(sync["data"]["resources"], 6);
    for page in 2..=5 {
        assert_eq!(
            server
                .requests()
                .iter()
                .filter(|line| line.contains(&format!("page={page} ")))
                .count(),
            3
        );
    }
}

#[test]
fn recognized_next_is_followed_by_both_lists() {
    for command in ["notices list --course course:42", "boards posts board:9"] {
        let server = server(NEXT);
        let result = Env::at(&server).ok(command);
        assert_eq!(result["data"].as_array().unwrap().len(), 2);
        assert_eq!(result["meta"]["complete"], true);
        assert_eq!(result["meta"]["source_complete"], true);
        assert!(server.requests().iter().any(|line| line.contains("page=2")));
    }
}

#[test]
fn numeric_paging_is_exhausted_by_both_lists() {
    for command in ["notices list --course course:42", "boards posts board:9"] {
        let server = server(NUMERIC);
        let result = Env::at(&server).ok(command);
        assert_eq!(result["data"].as_array().unwrap().len(), 2);
        assert_eq!(result["meta"]["complete"], true);
        assert_eq!(result["meta"]["total"], 2);
        assert!(server.requests().iter().any(|line| line.contains("page=2")));
    }
}

#[test]
fn numeric_paging_is_exhausted_by_library_sync() {
    let server = server(NUMERIC);
    let result = Env::at(&server).sync("--notices");
    assert_eq!(result["data"]["status"], "complete");
    assert_eq!(result["data"]["source_complete"], true);
    assert!(server.requests().iter().any(|line| line.contains("page=2")));
    assert_eq!(result["data"]["resources"], 3);
}

#[test]
fn relative_next_is_resolved_against_current_board_by_sync() {
    let server = server("<a rel='next' href='?id=9&amp;page=2'>Next</a>");
    let result = Env::at(&server).sync("--notices");
    assert_eq!(result["data"]["status"], "complete");
    assert!(server.requests().iter().any(|line| line.contains("page=2")));
}

#[test]
fn output_limit_does_not_claim_source_is_incomplete_or_hide_total() {
    let server = server(NUMERIC);
    let result = Env::at(&server).ok("notices list --course course:42 --limit 1");
    assert_eq!(result["data"].as_array().unwrap().len(), 1);
    assert_eq!(result["meta"]["complete"], false);
    assert_eq!(result["meta"]["source_complete"], true);
    assert_eq!(result["meta"]["total"], 2);
    assert!(server.requests().iter().any(|line| line.contains("page=2")));
}

#[test]
fn malformed_external_cross_board_and_action_pagers_never_report_complete() {
    for pager in [
        "<div class='paging'><a href='javascript:go(2)'>2</a></div>",
        "<a rel='next' href='https://outside.invalid/mod/courseboard/view.php?id=9&amp;page=2'>Next</a>",
        "<a rel='next' href='/mod/courseboard/view.php?id=99&amp;page=2'>Next</a>",
        "<a rel='next' href='/mod/quiz/view.php?id=9'>Next</a>",
        "<a rel='next' href='/mod/courseboard/view.php?id=9&amp;page=2&amp;action=delete'>Next</a>",
        "<a rel='next'>Next</a>",
    ] {
        let server = server(pager);
        let env = Env::at(&server);
        assert_eq!(
            env.fail("boards posts board:9").1["code"],
            "UPSTREAM_SHAPE_CHANGED"
        );
        assert_eq!(env.sync("--notices")["data"]["status"], "incomplete");
        assert!(
            !server
                .requests()
                .iter()
                .any(|line| line.contains("page=2") || line.contains("quiz"))
        );
    }
}

#[test]
fn paging_failure_preserves_prior_library_observations_without_complete_status() {
    let server = fixture::lib_server(|request, _| match request.target.as_str() {
        "/course/view.php?id=42" => Some(Response::html(fixture::notice_manifest())),
        "/mod/courseboard/view.php?id=9" => Some(Response::html(board(NEXT))),
        "/mod/courseboard/view.php?id=9&page=2" => {
            Some(Response::html("unavailable").status("500 Internal Server Error"))
        }
        "/mod/courseboard/article.php?id=9&bwid=10" => {
            Some(Response::html(article("Hello", "", "First body")))
        }
        _ => None,
    });
    let env = Env::at(&server);
    assert_eq!(env.sync("--notices")["data"]["status"], "incomplete");
    assert_eq!(env.data("library show board-post:9:10")["kind"], "notice");
    assert!(!env.out("boards posts board:9").status.success());
}

#[test]
fn numeric_previous_links_and_pinned_repeated_posts_are_handled_once() {
    let server = fixture::lib_server(|request, _| match request.target.as_str() {
        "/course/view.php?id=42" => Some(Response::html(fixture::notice_manifest())),
        "/mod/courseboard/view.php?id=9" => Some(Response::html(board(
            "<div class='paging'><a class='next' href='?id=9&amp;page=2'>Next</a></div>",
        ))),
        "/mod/courseboard/view.php?id=9&page=2" => Some(Response::html(format!(
            "{}{}<div class='paging'><a href='?id=9&amp;page=1'>1</a><strong>2</strong></div>",
            fixture::NOTICE_LIST,
            fixture::NOTICE_LIST.replace("bwid=10", "bwid=11")
        ))),
        "/mod/courseboard/article.php?id=9&bwid=10"
        | "/mod/courseboard/article.php?id=9&bwid=11" => {
            Some(Response::html(article("Hello", "", "Body")))
        }
        _ => None,
    });
    let env = Env::at(&server);
    let result = env.ok("boards posts board:9");
    assert_eq!(result["data"].as_array().unwrap().len(), 2);
    assert_eq!(result["meta"]["complete"], true);
    assert_eq!(env.sync("--notices")["data"]["resources"], 3);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|line| line.contains("bwid=10"))
            .count(),
        1
    );
}

/// Every listing page links to the next; with `cycle` every page links to `page=1`.
/// Page N lists post 100+N so the budget case accumulates distinct rows.
fn endless_board(cycle: bool) -> Server {
    fixture::lib_server(move |request, _| {
        if request.target == "/course/view.php?id=42" {
            return Some(Response::html(fixture::notice_manifest()));
        }
        if request.target.starts_with("/mod/courseboard/view.php?") {
            let page: usize = request
                .target
                .split("page=")
                .nth(1)
                .unwrap_or("1")
                .parse()
                .unwrap();
            let next = if cycle { 1 } else { page + 1 };
            let posts = fixture::NOTICE_LIST.replace("bwid=10", &format!("bwid={}", 100 + page));
            return Some(Response::html(format!(
                "{posts}<a rel='next' href='?id=9&amp;page={next}'>Next</a>"
            )));
        }
        if request.target.contains("article.php") {
            return Some(Response::html(article("Hello", "", "Body")));
        }
        None
    })
}

fn board_page_requests(server: &Server) -> usize {
    server
        .requests()
        .iter()
        .filter(|line| line.contains("/courseboard/view.php"))
        .count()
}

#[test]
fn pagination_cycles_fail_lists_and_leave_sync_incomplete() {
    for command in ["notices list --course course:42", "boards posts board:9"] {
        let server = endless_board(true);
        let (_, error) = Env::at(&server).fail(command);
        assert_eq!(error["code"], "UPSTREAM_SHAPE_CHANGED");
        assert_eq!(error["message"], "board pagination cycle detected");
        assert_eq!(board_page_requests(&server), 3);
    }
    let server = endless_board(true);
    let sync = Env::at(&server).sync("--notices");
    assert_eq!(sync["data"]["status"], "incomplete");
    let failures = sync["data"]["failures"].to_string();
    assert!(
        failures.contains("board pagination cycle detected"),
        "{failures}"
    );
    assert_eq!(board_page_requests(&server), 3);
    // The start page and `page=1` list the same post; it is read once before
    // the repeated `page=1` is detected.
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|line| line.contains("article.php"))
            .count(),
        1
    );
}

#[test]
fn page_budget_returns_partial_live_lists_and_leaves_sync_incomplete() {
    for command in ["notices list --course course:42", "boards posts board:9"] {
        let server = endless_board(false);
        let result = Env::at(&server).ok(command);
        assert_eq!(result["data"].as_array().unwrap().len(), 20);
        assert_eq!(result["meta"]["complete"], false);
        assert_eq!(result["meta"]["source_complete"], false);
        assert_eq!(result["meta"]["total"], serde_json::Value::Null);
        assert_eq!(board_page_requests(&server), 20);
    }
    let server = endless_board(false);
    let sync = Env::at(&server).sync("--notices");
    assert_eq!(sync["data"]["status"], "incomplete");
    assert_eq!(sync["data"]["source_complete"], false);
    let failures = sync["data"]["failures"].to_string();
    assert!(
        failures.contains("board pagination exceeded 20 pages"),
        "{failures}"
    );
    assert_eq!(board_page_requests(&server), 20);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|line| line.contains("article.php"))
            .count(),
        20
    );
}
