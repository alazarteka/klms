//! Static Panopto launch markup; no script execution or player telemetry.
#[cfg(test)]
#[path = "video_tests.rs"]
mod protocol_tests;
use super::shared::resolve_http_url;
use crate::{
    error::AppError,
    url::{Origin, Url, query_value},
};
use scraper::{ElementRef, Html, Selector};

pub struct LaunchForm {
    pub action: Url,
    pub fields: Vec<(String, String)>,
}
/// Whether `url` is on the trusted Panopto `origin`, without userinfo.
fn on_panopto(url: &Url, panopto: &Origin) -> bool {
    url.origin() == *panopto && url.username().is_empty() && url.password().is_none()
}
pub fn launch_form(
    html: &str,
    base: &Url,
    panopto: &Origin,
) -> Result<Option<LaunchForm>, AppError> {
    let doc = Html::parse_document(html);
    let region = activity_region(&doc);
    let mut candidate = None;
    for f in region
        .select(&Selector::parse("form[action]").unwrap())
        .filter(|f| !in_description(*f))
    {
        let Some(action) = f
            .value()
            .attr("action")
            .and_then(|a| resolve_http_url(base, a))
        else {
            continue;
        };
        let repost = action.origin() == base.origin()
            && matches!(
                action.path(),
                "/blocks/panopto/lib/lti/auth.php" | "/mod/lti/auth.php"
            );
        if !on_panopto(&action, panopto) && !repost {
            continue;
        }
        let path = action.path().to_ascii_lowercase();
        if !(repost
            || path.starts_with("/panopto/lti/adv/platforms/") && path.ends_with("/login")
            || path == "/panopto/lti/lti.aspx")
        {
            continue;
        }
        if !f
            .value()
            .attr("method")
            .unwrap_or("get")
            .eq_ignore_ascii_case("post")
        {
            return Err(AppError::shape("Panopto launch form is not POST"));
        }
        let mut fields = Vec::new();
        for input in f.select(&Selector::parse("input[name]").unwrap()) {
            let name = input.value().attr("name").unwrap();
            let value = input.value().attr("value").unwrap_or("");
            if fields.len() >= 32
                || value.len() > 65536
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            {
                return Err(AppError::shape("Panopto launch form exceeds safe bounds"));
            }
            fields.push((name.to_owned(), value.to_owned()));
        }
        let has = |n: &str| fields.iter().any(|(k, v)| k == n && !v.is_empty());
        if !(repost && has("repost")
            || has("id_token") && has("state")
            || has("iss") && has("login_hint") && has("target_link_uri")
            || has("oauth_signature") && has("lti_message_type"))
        {
            return Err(AppError::shape(
                "Panopto launch form omitted required protocol fields",
            ));
        }
        if candidate.is_some() {
            return Err(AppError::shape("ambiguous Panopto launch forms"));
        }
        candidate = Some(LaunchForm { action, fields });
    }
    Ok(candidate)
}
fn activity_region(doc: &Html) -> ElementRef<'_> {
    ["#region-main", "[role=main]", "main"]
        .into_iter()
        .find_map(|css| doc.select(&Selector::parse(css).ok()?).next())
        .unwrap_or_else(|| doc.root_element())
}
/// Whether `element` sits inside the activity description, which teachers
/// write and which may link unrelated recordings.
fn in_description(element: ElementRef<'_>) -> bool {
    element.ancestors().filter_map(ElementRef::wrap).any(|a| {
        a.value().id() == Some("intro") || a.value().classes().any(|c| c == "activity-description")
    })
}
/// The activity's own launch target: the main region's launch iframe, else
/// its first allowed link. Description blocks are skipped.
pub fn embedded_launch(html: &str, base: &Url, panopto: &Origin) -> Option<Url> {
    let doc = Html::parse_document(html);
    let region = activity_region(&doc);
    let allowed = |u: &Url| {
        u.origin() == base.origin()
            && (u.path() == "/mod/panoptocourseembed/view_content.php"
                || u.path() == "/mod/lti/launch.php"
                    && query_value(u, "id") == query_value(base, "id"))
            || on_panopto(u, panopto)
                && matches!(
                    u.path(),
                    "/Panopto/Pages/Embed.aspx" | "/Panopto/Pages/Sessions/List.aspx"
                )
    };
    for (css, attr) in [("iframe[src]", "src"), ("a[href]", "href")] {
        let selector = Selector::parse(css).ok()?;
        if let Some(u) = region
            .select(&selector)
            .filter(|f| !in_description(*f))
            .filter_map(|f| resolve_http_url(base, f.value().attr(attr)?))
            .find(allowed)
        {
            return Some(u);
        }
    }
    // Moodle's static iframe setup stores its launch URL in a quoted JS literal.
    // Read only a complete literal for this exact endpoint; never evaluate script.
    for script in doc.select(&Selector::parse("script:not([src])").unwrap()) {
        let script = script.text().collect::<String>();
        for quote in ['\"', '\''] {
            for literal in script.split(quote).skip(1).step_by(2) {
                if literal.len() < 2048 && !literal.contains(['\\', '\n', '\r']) {
                    if let Some(u) = resolve_http_url(base, literal) {
                        if u.origin() == base.origin()
                            && u.path() == "/mod/lti/launch.php"
                            && query_value(&u, "id") == query_value(base, "id")
                        {
                            return Some(u);
                        }
                    }
                }
            }
        }
    }
    None
}
pub fn non_panopto_lti_form(html: &str, base: &Url, panopto: &Origin) -> bool {
    let doc = Html::parse_document(html);
    doc.select(&Selector::parse("form[action]").unwrap())
        .any(|form| {
            let Some(u) = form
                .value()
                .attr("action")
                .and_then(|v| resolve_http_url(base, v))
            else {
                return false;
            };
            if u.origin() == base.origin() || on_panopto(&u, panopto) {
                return false;
            }
            let names = form
                .select(&Selector::parse("input[name]").unwrap())
                .filter_map(|i| i.value().attr("name"))
                .collect::<Vec<_>>();
            names.contains(&"lti_message_type") && names.contains(&"oauth_signature")
                || names.contains(&"iss")
                    && names.contains(&"login_hint")
                    && names.contains(&"target_link_uri")
        })
}
/// Panopto list fragments encode state values as JSON strings; query values may be plain UUIDs.
pub fn folder_id(url: &Url) -> Option<String> {
    let direct = query_value(url, "folderID").or_else(|| query_value(url, "folderId"));
    let fragment = || {
        url.fragment()
            .and_then(|fragment| resolve_http_url(url, &format!("?{fragment}")))
            .and_then(|u| query_value(&u, "folderID"))
    };
    let value = direct.or_else(fragment)?;
    let decoded = serde_json::from_str::<String>(&value).unwrap_or(value);
    uuid(&decoded).then(|| decoded.to_ascii_lowercase())
}

pub fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognition_uses_protocol_not_title() {
        let u = Url::parse("https://klms.kaist.ac.kr/mod/lti/view.php?id=1").unwrap();
        let pano = Url::parse("https://kaist.ap.panopto.com/")
            .unwrap()
            .origin();
        assert!(
            launch_form("<h1>Panopto Lecture</h1>", &u, &pano)
                .unwrap()
                .is_none()
        );
        assert!(embedded_launch("<h1>Panopto Lecture</h1>", &u, &pano).is_none());
        assert!(launch_form(
            r#"<form method="post" action="https://kaist.ap.panopto.com/Panopto/lti/adv/platforms/test/login"><input name="iss" value="x"><input name="login_hint" value="x"><input name="target_link_uri" value="x"></form>"#,
            &u, &pano
        ).unwrap().is_some());
        assert!(launch_form(
            r#"<form method="post" action="https://evil.test/Panopto/LTI/LTI.aspx"><input name="id_token" value="secret"><input name="state" value="secret"></form>"#,
            &u, &pano
        ).unwrap().is_none());
    }
}
