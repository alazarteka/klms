use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
mod fixture;
use fixture::{
    Env,
    server::{Response, Server},
};

const DASHBOARD: &str = r#"<script>M.cfg={"sesskey":"synthetic-key"};</script><a href="/course/view.php?id=99">Current(CS100_2026_3)</a>"#;

fn historical_server() -> Server {
    let page = Arc::new(AtomicUsize::new(0));
    Server::new(move |request| {
        if request.target == "/my/" {
            return Response::html(DASHBOARD);
        }
        if request.target.starts_with("/lib/ajax/service.php?") {
            assert_eq!(request.method, "POST");
            let data = match page.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    json!({"courses":[{"id":42,"fullname":"Compilers","shortname":"CS350_2024_3"},{"id":43,"fullname":"Compilers","shortname":"CS350_2024_2"}],"nextoffset":2})
                }
                1 => {
                    json!({"courses":[{"id":42,"fullname":"Compilers","shortname":"CS350_2024_3"},{"id":44,"fullname":"Unknown","shortname":"special","courseimage":"/2024/fall.png"}],"nextoffset":4})
                }
                _ => json!({"courses":[],"nextoffset":4}),
            };
            return Response::bytes(
                "application/json",
                json!([{"error":false,"data":data}]).to_string(),
            );
        }
        Response::html("<main class='course-content'></main>")
    })
}

#[test]
fn historical_discovery_pages_deduplicates_and_keeps_resource_refs_usable() {
    let server = historical_server();
    let env = Env::at(&server);
    let result = env.ok("courses list --all");
    assert_eq!(result["meta"]["complete"], true);
    assert_eq!(result["meta"]["returned"], 3);
    assert_eq!(result["data"][2]["term"], Value::Null);
    assert_eq!(result["warnings"], json!([]));
    let recorded = server.recorded();
    let calls: Vec<_> = recorded
        .iter()
        .filter(|r| r.line.starts_with("POST "))
        .collect();
    assert_eq!(calls.len(), 3);
    for (call, offset) in calls.iter().zip([0, 2, 4]) {
        let body: Value = serde_json::from_str(&call.body).unwrap();
        assert_eq!(
            body[0]["methodname"],
            "core_course_get_enrolled_courses_by_timeline_classification"
        );
        assert_eq!(
            body[0]["args"],
            json!({"classification":"allincludinghidden","limit":100,"offset":offset,"sort":"fullname"})
        );
    }
    let activities = env.ok("activities list --course course:42");
    assert_eq!(activities["meta"]["complete"], true);
}

#[test]
fn term_filters_match_metadata_and_empty_terms_are_complete() {
    for (args, expected) in [
        ("--year 2024 --semester fall", 1),
        ("--year 2024 --semester summer", 1),
        ("--year 2023 --semester winter", 0),
    ] {
        let server = historical_server();
        let result = Env::at(&server).ok(&format!("courses list {args}"));
        assert_eq!(result["meta"]["returned"], expected);
        assert_eq!(result["meta"]["complete"], true);
        assert_eq!(
            result["warnings"],
            json!([
                "1 course(s) were excluded because their term is unknown; use --all to see them"
            ])
        );
    }
    let server = historical_server();
    let matches = Env::at(&server).ok("courses resolve Compilers --all");
    assert_eq!(matches["meta"]["returned"], 2);
}

#[test]
fn default_scope_stays_dashboard_and_invalid_scopes_fail_before_auth() {
    let server = historical_server();
    let result = Env::at(&server).ok("courses list");
    assert_eq!(result["data"][0]["ref"], "course:99");
    assert_eq!(server.requests(), ["GET /my/ HTTP/1.1"]);
    for args in [
        "courses list --year 2024",
        "courses list --semester fall",
        "courses list --all --year 2024 --semester fall",
        "courses resolve Compilers --year 2024",
    ] {
        assert_eq!(Env::new().code(args), 2);
    }
}

#[test]
fn stalled_pagination_fails_instead_of_claiming_completeness() {
    let server = Server::new(|request| {
        if request.target == "/my/" {
            return Response::html(DASHBOARD);
        }
        Response::bytes("application/json", json!([{"data":{"courses":[{"id":42,"fullname":"Compilers","shortname":"CS350_2024_3"}],"nextoffset":0}}]).to_string())
    });
    let (code, error) = Env::at(&server).fail("courses list --all");
    assert_eq!(code, 30);
    assert_eq!(error["code"], "UPSTREAM_SHAPE_CHANGED");
}

#[test]
fn all_courses_includes_user_hidden_courses() {
    let page = Arc::new(AtomicUsize::new(0));
    let server = Server::new(move |request| {
        if request.target == "/my/" {
            return Response::html(DASHBOARD);
        }
        let courses = if page.fetch_add(1, Ordering::SeqCst) == 0 {
            json!([{"id":45,"fullname":"Hidden course","shortname":"CS100_2024_3","hidden":true}])
        } else {
            json!([])
        };
        Response::bytes(
            "application/json",
            json!([{"data":{"courses":courses,"nextoffset":1}}]).to_string(),
        )
    });
    let result = Env::at(&server).ok("courses list --all");
    assert_eq!(result["data"][0]["ref"], "course:45");
    assert_eq!(result["meta"]["complete"], true);
    for request in server
        .recorded()
        .iter()
        .filter(|r| r.line.starts_with("POST "))
    {
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body[0]["args"]["classification"], "allincludinghidden");
    }
}
