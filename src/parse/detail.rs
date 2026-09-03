use scraper::{Html, Selector};
use std::collections::HashSet;
use url::Url;

use super::shared::{first_text, has_any, link_items_in, query_id, selector, visible_text};
use crate::{
    error::AppError,
    models::{LinkItem, ResourceDetail},
    reference::ResourceRef,
    safe_url,
};

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
    let (title, text_value, mut links) = if kind == "courseboard-post" {
        notice_content(&document, base_url)?
    } else {
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
        let links = match content {
            Some(root) => link_items_in(root, base_url, 101)?,
            None => Vec::new(),
        };
        (title, text_value, links)
    };
    let text_truncated = text_value.chars().count() > 100_000;
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

/// Courseboard chrome contains mutable counters, adjacent post links and a
/// password dialog. Only the post's subject, body and attachments are content.
fn notice_content(
    document: &Html,
    base_url: &Url,
) -> Result<(String, String, Vec<LinkItem>), AppError> {
    let root = document
        .select(&selector(".courseboard_view")?)
        .next()
        .ok_or_else(|| AppError::shape("notice page contained no recognizable post region"))?;
    let title = root
        .select(&selector(".courseboard_view > .subject > h3")?)
        .next()
        .map(visible_text)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| AppError::shape("notice page contained no recognizable post title"))?;
    let body = root
        .select(&selector(".courseboard_view > .content")?)
        .next()
        .ok_or_else(|| AppError::shape("notice page contained no recognizable post body"))?;
    let text = strip_embedded_active_markup(visible_text(body));
    let mut links = Vec::new();
    let mut seen = HashSet::new();
    for part in root.select(&selector(
        ".courseboard_view > .content, .courseboard_view > .info > .files",
    )?) {
        for link in link_items_in(part, base_url, 101)? {
            if seen.insert(link.url.clone()) {
                links.push(link);
                if links.len() == 101 {
                    return Ok((title, text, links));
                }
            }
        }
    }
    Ok((title, text, links))
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

pub fn next_page_url(html: &str, base_url: &Url) -> Result<Option<String>, AppError> {
    let document = Html::parse_document(html);
    let selector = selector(
        "a[rel=next][href], .pagination .next a[href], a[data-page-number][aria-label*=Next][href]",
    )?;
    let Some(href) = document
        .select(&selector)
        .find_map(|node| node.value().attr("href"))
    else {
        return Ok(None);
    };
    let url = base_url
        .join(href)
        .map_err(|e| AppError::shape(format!("invalid pagination URL: {e}")))?;
    if url.scheme() != base_url.scheme()
        || url.host_str() != base_url.host_str()
        || url.port_or_known_default() != base_url.port_or_known_default()
    {
        return Err(AppError::shape("pagination URL left the KLMS origin"));
    }
    Ok(Some(crate::safe_url::display(&url)))
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
    use super::{next_page_url, resource_detail, sesskey};
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
        let detail = resource_detail(
            "<div class='courseboard_view'><div class='subject'><h3>Notice</h3></div>\
             <div class='content'>Body</div></div>",
            &base,
            &url,
            "courseboard-post",
        )
        .unwrap();
        assert_eq!(detail.id.as_deref(), Some("11"));
        assert_eq!(detail.board_id.as_deref(), Some("10"));
        assert_eq!(detail.reference.as_deref(), Some("board-post:10:11"));
    }

    #[test]
    fn notice_extracts_only_subject_body_and_attachments() {
        // Fictional content with the structural containers observed on KLMS.
        let html = "<h1>Generic board heading</h1><div class='courseboard_view'>\
            <div class='subject'><h3>Exam schedule</h3></div>\
            <div class='info'><div class='writer'>Author</div>\
              <div class='hit'>Views : 10</div></div>\
            <div class='info'><div class='file'>Attachments</div><div class='files'>\
              <ul class='files'><li><a href='/pluginfile.php/1/notes.pdf'>Notes</a></li></ul>\
            </div></div>\
            <div class='content'><p>Discuss Views : 10, Next, and Enter password in class.</p>\
              <a href='https://example.org/reading'>Reading</a>\
              <a href='/pluginfile.php/1/notes.pdf'>Notes</a></div>\
            <div class='pre_next'><a href='/mod/courseboard/article.php?id=10&bwid=12'>Other post</a></div>\
            <div class='button_area'>List</div><div id='password_confirm'>Dialog wording</div></div>";
        let base = Url::parse("https://klms.example").unwrap();
        let url = base
            .join("/mod/courseboard/article.php?id=10&bwid=11")
            .unwrap();
        let first = resource_detail(html, &base, &url, "courseboard-post").unwrap();
        assert_eq!(first.title, "Exam schedule");
        assert_eq!(
            first.text,
            "Discuss Views : 10, Next, and Enter password in class. Reading Notes"
        );
        assert_eq!(first.links.len(), 2);
        assert!(
            first
                .links
                .iter()
                .all(|link| !link.url.contains("article.php"))
        );
        let changed_chrome = html
            .replace("<div class='hit'>Views : 10", "<div class='hit'>Views : 11")
            .replace("Other post", "Different neighbor");
        let second = resource_detail(&changed_chrome, &base, &url, "courseboard-post").unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(second).unwrap()
        );
    }

    #[test]
    fn notice_requires_explicit_title_and_body_but_allows_empty_body() {
        let base = Url::parse("https://klms.example").unwrap();
        let url = base
            .join("/mod/courseboard/article.php?id=10&bwid=11")
            .unwrap();
        for html in [
            "<main><h1>Notice</h1>Fallback is unsafe</main>",
            "<div class='courseboard_view'><div class='content'>Body</div></div>",
            "<div class='courseboard_view'><div class='subject'><h3>Title</h3></div></div>",
        ] {
            assert_eq!(
                resource_detail(html, &base, &url, "courseboard-post")
                    .unwrap_err()
                    .code,
                "UPSTREAM_SHAPE_CHANGED"
            );
        }
        let empty = resource_detail("<div class='courseboard_view'><div class='subject'><h3>Title</h3></div><div class='content'></div></div>", &base, &url, "courseboard-post").unwrap();
        assert!(empty.text.is_empty());
    }

    #[test]
    fn notice_preserves_text_and_combined_link_caps() {
        let base = Url::parse("https://klms.example").unwrap();
        let url = base
            .join("/mod/courseboard/article.php?id=10&bwid=11")
            .unwrap();
        let attachments: String = (0..60)
            .map(|i| format!("<a href='/pluginfile.php/{i}'>File {i}</a>"))
            .collect();
        let body_links: String = (50..102)
            .map(|i| format!("<a href='/pluginfile.php/{i}'>File {i}</a>"))
            .collect();
        let html = format!(
            "<div class='courseboard_view'><div class='subject'><h3>Title</h3></div><div class='info'><div class='files'>{attachments}</div></div><div class='content'>{}{body_links}</div></div>",
            "가".repeat(100_001)
        );
        let detail = resource_detail(&html, &base, &url, "courseboard-post").unwrap();
        assert!(detail.text_truncated);
        assert_eq!(detail.text.chars().count(), 100_000);
        assert!(detail.links_truncated);
        assert_eq!(detail.links.len(), 100);
    }

    #[test]
    fn pagination_stays_on_origin() {
        let base = Url::parse("https://klms.example/").unwrap();
        assert_eq!(next_page_url("<div class='pagination'><span class='next'><a href='/mod/courseboard/view.php?id=8&page=2'>Next</a></span></div>",&base).unwrap().as_deref(),Some("https://klms.example/mod/courseboard/view.php?id=8&page=2"));
        assert!(
            next_page_url(
                "<a rel='next' href='https://external.example/page'>Next</a>",
                &base
            )
            .is_err()
        );
    }
}
