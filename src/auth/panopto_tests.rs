use super::*;
use crate::{
    fixture_server::{Response as FixtureResponse, Server},
    http::Policy as _,
};

fn url(value: &str) -> Url {
    Url::parse(value).unwrap()
}

fn policy() -> Policy {
    Policy {
        klms: url("https://klms.kaist.ac.kr/"),
        cookie: "MoodleSession=synthetic-klms".into(),
        jar: Jar::default(),
        bearer: Some("synthetic-panopto-token".into()),
        delivery_only: false,
    }
}

#[test]
fn credentials_are_selected_by_exact_origin_and_exchange_phase() {
    let mut policy = policy();
    let pano = url("https://kaist.ap.panopto.com/Panopto/Pages/Viewer/DeliveryInfo.aspx");
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::SET_COOKIE,
        http::HeaderValue::from_static("PanoptoAuth=synthetic-panopto-cookie; Path=/; Secure"),
    );
    policy.jar.capture(&pano, &headers).unwrap();

    let klms = policy.headers(
        &Method::GET,
        &url("https://klms.kaist.ac.kr/mod/lti/auth.php"),
    );
    assert_eq!(
        klms,
        vec![("cookie", "MoodleSession=synthetic-klms".into())]
    );
    let launch = policy.headers(&Method::POST, &pano);
    assert_eq!(
        launch,
        vec![("cookie", "PanoptoAuth=synthetic-panopto-cookie".into())]
    );
    policy.delivery_only = true;
    let delivery = policy.headers(&Method::POST, &pano);
    assert_eq!(
        delivery,
        vec![
            ("cookie", "PanoptoAuth=synthetic-panopto-cookie".into()),
            ("authorization", "Bearer synthetic-panopto-token".into()),
        ]
    );
    for raw in [
        "https://s-cloudfront.cdn.ap.panopto.com/media.mp4",
        "https://unrelated.example/Panopto/Pages/Viewer/DeliveryInfo.aspx",
        "https://kaist.ap.panopto.com:444/Panopto/Pages/Viewer/DeliveryInfo.aspx",
        "http://kaist.ap.panopto.com/Panopto/Pages/Viewer/DeliveryInfo.aspx",
    ] {
        let target = url(raw);
        assert!(policy.headers(&Method::GET, &target).is_empty());
        assert!(policy.allow(&target).is_err());
    }
}

#[test]
fn endpoint_allowlist_excludes_player_telemetry_and_unrelated_hosts() {
    let mut policy = policy();
    for raw in [
        "https://klms.kaist.ac.kr/blocks/panopto/lib/lti/auth.php",
        "https://klms.kaist.ac.kr/mod/lti/auth.php",
        "https://klms.kaist.ac.kr/mod/lti/launch.php",
        "https://klms.kaist.ac.kr/mod/panoptocourseembed/view_content.php",
        "https://kaist.ap.panopto.com/Panopto/lti/adv/platforms/synthetic/login",
        "https://kaist.ap.panopto.com/Panopto/LTI/LTI.aspx",
        "https://kaist.ap.panopto.com/Panopto/Pages/Embed.aspx",
        "https://kaist.ap.panopto.com/Panopto/Pages/EmbeddedList.aspx",
        "https://kaist.ap.panopto.com/Panopto/Pages/Auth/CookieCheck.aspx",
    ] {
        assert!(policy.allow(&url(raw)).is_ok(), "{raw}");
    }
    for raw in [
        "https://klms.kaist.ac.kr/mod/vod/viewer/playtime_ajax.php",
        "https://klms.kaist.ac.kr/lib/ajax/service.php",
        "https://kaist.ap.panopto.com/Panopto/Services/Analytics.svc/AddPlaybackSession",
        "https://kaist.ap.panopto.com.evil.example/Panopto/LTI/LTI.aspx",
        "https://user@kaist.ap.panopto.com/Panopto/LTI/LTI.aspx",
        "https://kaist.ap.panopto.com:444/Panopto/LTI/LTI.aspx",
    ] {
        assert!(policy.allow(&url(raw)).is_err(), "{raw}");
    }
    policy.delivery_only = true;
    assert!(
        policy
            .allow(&url("https://klms.kaist.ac.kr/mod/lti/auth.php"))
            .is_err()
    );
    assert!(
        policy
            .allow(&url("https://kaist.ap.panopto.com/Panopto/LTI/LTI.aspx"))
            .is_err()
    );
    assert!(
        policy
            .allow(&url(
                "https://kaist.ap.panopto.com/Panopto/Services/Data.svc/GetSessions"
            ))
            .is_ok()
    );
}

#[test]
fn redirect_is_refused_before_credentials_reach_the_target() {
    let target = Server::new(|_| panic!("unexpected credential-bearing request"));
    let destination = format!("{}/mod/lti/auth.php", target.url());
    let source = Server::new(move |_| {
        FixtureResponse::bytes("text/plain", "")
            .status("302 Found")
            .header("Location", &destination)
    });
    let mut policy = policy();
    policy.klms = url(&source.url());
    let error = request(
        &http::agent(Duration::from_secs(2), None),
        &mut policy,
        Method::POST,
        url(&format!("{}/mod/lti/auth.php", source.url())),
        Some(Payload {
            content_type: "application/x-www-form-urlencoded",
            bytes: b"repost=synthetic".to_vec(),
        }),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "CONTENT_UNAVAILABLE");
    assert_eq!(source.recorded().len(), 1);
    assert_eq!(
        source.recorded()[0].header_value("cookie"),
        Some("MoodleSession=synthetic-klms")
    );
    assert!(target.recorded().is_empty());
}

#[test]
fn launch_rejects_redirects_that_would_repost_protocol_secrets() {
    for status in ["307 Temporary Redirect", "308 Permanent Redirect"] {
        let server = Server::new(move |_| {
            FixtureResponse::bytes("text/plain", "")
                .status(status)
                .header("Location", "/mod/lti/auth.php")
        });
        let mut policy = policy();
        policy.klms = url(&server.url());
        let error = request(
            &http::agent(Duration::from_secs(2), None),
            &mut policy,
            Method::POST,
            url(&format!("{}/blocks/panopto/lib/lti/auth.php", server.url())),
            Some(Payload {
                content_type: "application/x-www-form-urlencoded",
                bytes: b"id_token=synthetic&state=synthetic".to_vec(),
            }),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "CONTENT_UNAVAILABLE");
        assert_eq!(server.recorded().len(), 1);
    }
}

#[test]
fn enrollment_and_signin_redirects_are_actionable_failures() {
    let mut policy = policy();
    for path in ["/enrol/index.php", "/login/index.php"] {
        let error = policy.allow(&policy.klms.join(path).unwrap()).unwrap_err();
        assert_eq!(error.code, "CONTENT_UNAVAILABLE");
        assert!(error.message.contains("enrollment") || error.message.contains("sign-in"));
    }
}

#[test]
fn static_embed_metadata_needs_no_player_or_network_execution() {
    let base = url("https://klms.kaist.ac.kr/");
    let embed = url(
        "https://kaist.ap.panopto.com/Panopto/Pages/Embed.aspx?id=11111111-2222-3333-4444-555555555555&access_token=synthetic",
    );
    let session = launch(&base, "MoodleSession=synthetic", 2, "<html></html>", &embed).unwrap();
    assert_eq!(
        session.recording_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
}

#[test]
fn csrf_cookie_is_forwarded_as_header_only_to_authorized_panopto_metadata() {
    let mut p = policy();
    let url = url("https://kaist.ap.panopto.com/Panopto/Services/Data.svc/GetSessions");
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::SET_COOKIE,
        http::HeaderValue::from_static("csrfToken=synthetic-csrf; Path=/; Secure"),
    );
    p.jar.capture(&url, &headers).unwrap();
    assert!(
        !p.headers(&Method::POST, &url)
            .iter()
            .any(|(k, _)| *k == "x-csrf-token")
    );
    p.delivery_only = true;
    assert!(
        p.headers(&Method::POST, &url)
            .contains(&("x-csrf-token", "synthetic-csrf".into()))
    );
    assert!(
        p.headers(
            &Method::GET,
            &super::Url::parse("https://s-cloudfront.cdn.ap.panopto.com/file.mp4").unwrap()
        )
        .is_empty()
    );
}
