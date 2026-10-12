use super::*;

fn klms() -> Url {
    Url::parse("https://klms.kaist.ac.kr/mod/lti/view.php?id=42").unwrap()
}

#[test]
fn lti13_forms_preserve_encoded_protocol_fields_without_execution() {
    let html = r#"<form method="POST" action="https://kaist.ap.panopto.com/Panopto/lti/adv/platforms/synthetic/login">
    <input type="hidden" name="iss" value="https://klms.kaist.ac.kr">
    <input type="hidden" name="login_hint" value="synthetic&amp;hint">
    <input type="hidden" name="target_link_uri" value="https://kaist.ap.panopto.com/Panopto/LTI/LTI.aspx">
    </form><script>throw new Error('must not execute')</script>"#;
    let form = launch_form(html, &klms()).unwrap().unwrap();
    assert_eq!(form.action.host_str(), Some("kaist.ap.panopto.com"));
    assert!(
        form.fields
            .contains(&("login_hint".into(), "synthetic&hint".into()))
    );
    let response = r#"<form method="post" action="https://kaist.ap.panopto.com/Panopto/LTI/LTI.aspx"><input name="id_token" value="synthetic.jwt"><input name="state" value="synthetic-state"></form>"#;
    assert_eq!(
        launch_form(response, &klms())
            .unwrap()
            .unwrap()
            .fields
            .len(),
        2
    );
}

#[test]
fn signed_lti11_and_local_repost_forms_are_recognized() {
    let legacy = r#"<form method="post" action="https://kaist.ap.panopto.com/Panopto/LTI/LTI.aspx"><input name="oauth_signature" value="synthetic"><input name="lti_message_type" value="basic-lti-launch-request"><input name="oauth_consumer_key" value="synthetic-key"></form>"#;
    assert!(launch_form(legacy, &klms()).unwrap().is_some());
    let repost = r#"<form method="post" action="/mod/lti/auth.php"><input name="repost" value="synthetic"></form>"#;
    assert!(launch_form(repost, &klms()).unwrap().is_some());
}

#[test]
fn non_protocol_forms_and_off_origin_actions_are_not_launches() {
    for action in [
        "https://unrelated.example/Panopto/LTI/LTI.aspx",
        "https://kaist.ap.panopto.com:444/Panopto/LTI/LTI.aspx",
        "http://kaist.ap.panopto.com/Panopto/LTI/LTI.aspx",
        "https://kaist.ap.panopto.com/Panopto/Services/Analytics.svc",
        "/mod/assign/view.php?id=42",
    ] {
        let html = format!(
            r#"<form method="post" action="{action}"><input name="id_token" value="synthetic"><input name="state" value="synthetic"></form>"#
        );
        assert!(launch_form(&html, &klms()).unwrap().is_none(), "{action}");
    }
}

#[test]
fn malformed_recognized_protocol_forms_fail_visibly() {
    let action = "https://kaist.ap.panopto.com/Panopto/LTI/LTI.aspx";
    for body in [
        format!(
            r#"<form method="get" action="{action}"><input name="id_token" value="synthetic"><input name="state" value="synthetic"></form>"#
        ),
        format!(
            r#"<form method="post" action="{action}"><input name="id_token" value="synthetic"></form>"#
        ),
        format!(
            r#"<form method="post" action="{action}"><input name="id_token" value=""><input name="state" value="synthetic"></form>"#
        ),
        format!(
            r#"<form method="post" action="{action}"><input name="unsafe field" value="synthetic"></form>"#
        ),
    ] {
        assert_eq!(
            launch_form(&body, &klms()).err().unwrap().code,
            "UPSTREAM_SHAPE_CHANGED"
        );
    }
    let excessive = format!(
        r#"<form method="post" action="{action}">{}{}</form>"#,
        r#"<input name="id_token" value="synthetic"><input name="state" value="synthetic">"#,
        r#"<input name="extra" value="synthetic">"#.repeat(31)
    );
    assert!(launch_form(&excessive, &klms()).is_err());
}

#[test]
fn embedded_iframe_and_static_launch_wrapper_resolve_narrow_endpoints() {
    let embedded = r#"<iframe src="/mod/panoptocourseembed/view_content.php?course=42&amp;contenturl=synthetic"></iframe>"#;
    let target = embedded_launch(embedded, &klms()).unwrap();
    assert_eq!(target.path(), "/mod/panoptocourseembed/view_content.php");
    assert_eq!(target.query_value("course").as_deref(), Some("42"));
    let wrapper = r#"<script>require(['mod_lti/iframe'], function(m) { m.init('/mod/lti/launch.php?id=42'); });</script>"#;
    assert_eq!(
        embedded_launch(wrapper, &klms())
            .unwrap()
            .query_value("id")
            .as_deref(),
        Some("42")
    );
    for html in [
        r#"<iframe src="https://unrelated.example/mod/panoptocourseembed/view_content.php"></iframe>"#,
        r#"<script>m.init('https://unrelated.example/mod/lti/launch.php?id=42');</script>"#,
        r#"<iframe src="/mod/vod/viewer/playtime_ajax.php?id=42"></iframe>"#,
    ] {
        assert!(embedded_launch(html, &klms()).is_none());
    }
}

#[test]
fn recording_uuid_identity_is_strict() {
    assert!(uuid("11111111-2222-3333-4444-555555555555"));
    for value in [
        "unknown",
        "11111111-2222-3333-4444-55555555555g",
        "11111111222233334444555555555555",
    ] {
        assert!(!uuid(value));
    }
}

#[test]
fn folder_ids_follow_authoritative_url_state_and_decode_json_fragments() {
    let id = "11111111-2222-3333-4444-555555555555";
    for raw in [
        format!("https://kaist.ap.panopto.com/Panopto/Pages/Sessions/List.aspx?folderID={id}"),
        format!(
            "https://kaist.ap.panopto.com/Panopto/Pages/Sessions/List.aspx#folderID=%22{id}%22"
        ),
    ] {
        assert_eq!(folder_id(&Url::parse(&raw).unwrap()).as_deref(), Some(id));
    }
    for raw in [
        "https://kaist.ap.panopto.com/Panopto/Pages/Sessions/List.aspx",
        "https://kaist.ap.panopto.com/Panopto/Pages/Sessions/List.aspx#folderID=null",
        "https://kaist.ap.panopto.com/Panopto/Pages/Sessions/List.aspx#folderID=%22unknown%22",
    ] {
        assert!(folder_id(&Url::parse(raw).unwrap()).is_none());
    }
}
