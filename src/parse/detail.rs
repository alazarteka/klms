use scraper::{Html, Selector};
use url::Url;

use super::shared::{first_text, has_any, link_items_in, query_id, selector, visible_text};
use crate::{error::AppError, models::ResourceDetail, reference::ResourceRef, safe_url};

pub fn sesskey(html: &str) -> Result<String, AppError> {
    for marker in ["\"sesskey\":\"", "\"sesskey\": \"", "sesskey="] {
        if let Some(rest) = html.split(marker).nth(1) {
            let value: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    let document = Html::parse_document(html);
    let input = selector("input[name=sesskey]")?;
    document
        .select(&input)
        .find_map(|node| node.value().attr("value"))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AppError::shape("authenticated page did not expose a Moodle sesskey"))
}

pub fn resource_detail(
    html: &str,
    base_url: &Url,
    url: &Url,
    kind: &str,
) -> Result<ResourceDetail, AppError> {
    let document = Html::parse_document(html);
    let title = first_text(
        &document,
        ".page-header-headings h1, #page-header h1, h1, title",
    )?
    .unwrap_or_else(|| format!("{kind} detail"));
    let content = ["#region-main", "[role=main]", "main", "body"]
        .into_iter()
        .find_map(|css| {
            Selector::parse(css)
                .ok()
                .and_then(|selector| document.select(&selector).next())
        });
    let text_value = content
        .map(visible_text)
        .map(strip_embedded_active_markup)
        .unwrap_or_default();
    let text_truncated = text_value.chars().count() > 100_000;
    let mut links = match content {
        Some(root) => link_items_in(root, base_url, 101)?,
        None => Vec::new(),
    };
    let links_truncated = links.len() > 100;
    links.truncate(100);
    let (id, board_id, reference) = if kind == "courseboard-post" {
        let board = query_id(url, &["id"]);
        let post = query_id(url, &["bwid"]);
        let reference = board.as_ref().zip(post.as_ref()).map(|(board, post)| {
            ResourceRef::BoardPost {
                board: board.clone(),
                post: post.clone(),
            }
            .to_string()
        });
        (post, board, reference)
    } else {
        let id = query_id(url, &["id", "bwid"]);
        let reference = ResourceRef::from_activity(kind, id.as_deref(), Some(url.as_str()))
            .map(|reference| reference.to_string());
        (id, None, reference)
    };
    Ok(ResourceDetail {
        id,
        board_id,
        reference,
        kind: kind.into(),
        title,
        url: safe_url::display(url),
        text: text_value.chars().take(100_000).collect(),
        text_truncated,
        links,
        links_truncated,
    })
}

pub fn has_next_page(html: &str) -> Result<bool, AppError> {
    let document = Html::parse_document(html);
    has_any(
        &document,
        &[
            "a[rel=next]",
            ".pagination .next a",
            "a[data-page-number][aria-label*=Next]",
        ],
    )
}

pub fn safe_html_preview(html: &str) -> String {
    let document = Html::parse_document(html);
    ["#region-main", "[role=main]", "main", "body"]
        .into_iter()
        .find_map(|css| {
            Selector::parse(css)
                .ok()
                .and_then(|selector| document.select(&selector).next())
        })
        .map(visible_text)
        .map(strip_embedded_active_markup)
        .unwrap_or_default()
}

fn strip_embedded_active_markup(mut text: String) -> String {
    for tag in ["form", "script"] {
        let opening = format!("<{tag}");
        let closing = format!("</{tag}>");
        loop {
            let lower = text.to_ascii_lowercase();
            let Some(start) = lower.find(&opening) else {
                break;
            };
            let end = lower[start..]
                .find(&closing)
                .map(|offset| start + offset + closing.len())
                .unwrap_or(text.len());
            text.replace_range(start..end, " [embedded launch data omitted] ");
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{resource_detail, sesskey};
    use url::Url;

    #[test]
    fn extracts_session_key_without_exposing_it_elsewhere() {
        assert_eq!(
            sesskey(r#"<script>var cfg={"sesskey":"abc123"}</script>"#).unwrap(),
            "abc123"
        );
    }

    #[test]
    fn omits_escaped_lti_launch_forms_from_detail_text() {
        let html = r#"<main>Course tool &lt;form method=&quot;post&quot;&gt;&lt;input name=&quot;login_hint&quot; value=&quot;private-user-id&quot;/&gt;&lt;/form&gt; Ready</main>"#;
        let base = Url::parse("https://klms.kaist.ac.kr").unwrap();
        let url = base.join("/mod/lti/view.php?id=7").unwrap();
        let detail = resource_detail(html, &base, &url, "lti").unwrap();
        assert!(!detail.text.contains("private-user-id"));
        assert!(!detail.text.contains("login_hint"));
        assert!(detail.text.contains("Course tool"));
        assert!(detail.text.contains("Ready"));
    }

    #[test]
    fn board_post_detail_uses_post_id_and_preserves_board_id() {
        let base = Url::parse("https://klms.kaist.ac.kr").unwrap();
        let url = base
            .join("/mod/courseboard/article.php?id=10&bwid=11")
            .unwrap();
        let detail =
            resource_detail("<main>Notice</main>", &base, &url, "courseboard-post").unwrap();
        assert_eq!(detail.id.as_deref(), Some("11"));
        assert_eq!(detail.board_id.as_deref(), Some("10"));
        assert_eq!(detail.reference.as_deref(), Some("board-post:10:11"));
    }
}
