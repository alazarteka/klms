use crate::url::Url;
use scraper::{ElementRef, Html};

use super::shared::{
    NEXT_PAGE_SELECTORS, first_text, has_any, link_items, query_id, sel, visible_text,
};
use crate::{
    error::AppError,
    models::{LinkItem, ResourceDetail},
    reference::ResourceRef,
    safe_url,
};

pub fn sesskey(html: &str) -> Result<String, AppError> {
    let token = ["\"sesskey\":\"", "\"sesskey\": \"", "sesskey="]
        .into_iter()
        .filter_map(|marker| html.split(marker).nth(1))
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect::<String>()
        })
        .find(|value| !value.is_empty());
    if let Some(value) = token {
        return Ok(value);
    }
    Html::parse_document(html)
        .select(&sel("input[name=sesskey]"))
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
    let board_post = kind == "courseboard-post";
    let (title, text_value, mut links) = if board_post {
        notice_content(&document, base_url)?
    } else {
        let title = first_text(
            &document,
            ".page-header-headings h1, #page-header h1, h1, title",
        )
        .unwrap_or_else(|| format!("{kind} detail"));
        let content = content_root(&document);
        let links = content
            .map(|root| link_items(root.select(&sel("a[href]")), base_url, 101))
            .unwrap_or_default();
        (title, preview_from_document(&document), links)
    };
    let text_truncated = text_value.chars().count() > 100_000;
    let links_truncated = links.len() > 100;
    links.truncate(100);
    let (id, board_id, reference) = if board_post {
        let (board, post) = (query_id(url, &["id"]), query_id(url, &["bwid"]));
        let reference = board
            .clone()
            .zip(post.clone())
            .map(|(board, post)| ResourceRef::BoardPost { board, post }.to_string());
        (post, board, reference)
    } else {
        let id = query_id(url, &["id", "bwid"]);
        let reference = ResourceRef::from_activity(kind, id.as_deref(), Some(url))
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
        .select(&sel(".courseboard_view"))
        .next()
        .ok_or_else(|| AppError::shape("notice page contained no recognizable post region"))?;
    let title = root
        .select(&sel(".courseboard_view > .subject > h3"))
        .next()
        .map(visible_text)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| AppError::shape("notice page contained no recognizable post title"))?;
    let body = root
        .select(&sel(".courseboard_view > .content"))
        .next()
        .ok_or_else(|| AppError::shape("notice page contained no recognizable post body"))?;
    let css =
        sel(".courseboard_view > .content a[href], .courseboard_view > .info > .files a[href]");
    let anchors = root.select(&css);
    Ok((
        title,
        strip_embedded_active_markup(visible_text(body)),
        link_items(anchors, base_url, 101),
    ))
}

pub fn has_next_page(html: &str) -> Result<bool, AppError> {
    Ok(has_any(&Html::parse_document(html), NEXT_PAGE_SELECTORS))
}

pub fn next_page_url(html: &str, base_url: &Url) -> Result<Option<String>, AppError> {
    let document = Html::parse_document(html);
    let joined = NEXT_PAGE_SELECTORS
        .iter()
        .map(|css| format!("{css}[href]"))
        .collect::<Vec<_>>()
        .join(", ");
    let Some(href) = document
        .select(&sel(&joined))
        .find_map(|node| node.value().attr("href"))
    else {
        return Ok(None);
    };
    let url = base_url
        .join(href)
        .map_err(|e| AppError::shape(format!("invalid pagination URL: {e}")))?;
    if url.origin() != base_url.origin() {
        return Err(AppError::shape("pagination URL left the KLMS origin"));
    }
    Ok(Some(safe_url::display(&url)))
}

pub fn safe_html_preview(html: &str) -> String {
    preview_from_document(&Html::parse_document(html))
}

pub(super) fn preview_from_document(document: &Html) -> String {
    content_root(document)
        .map(visible_text)
        .map(strip_embedded_active_markup)
        .unwrap_or_default()
}

fn content_root(document: &Html) -> Option<ElementRef<'_>> {
    ["#region-main", "[role=main]", "main", "body"]
        .into_iter()
        .find_map(|css| document.select(&sel(css)).next())
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
                .map_or(text.len(), |offset| start + offset + closing.len());
            text.replace_range(start..end, " [embedded launch data omitted] ");
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{next_page_url, resource_detail, sesskey};
    use crate::models::ResourceDetail;
    use crate::url::Url;

    fn base() -> Url {
        Url::parse("https://klms.example").unwrap()
    }

    fn notice(html: &str) -> Result<ResourceDetail, crate::error::AppError> {
        let url = base()
            .join("/mod/courseboard/article.php?id=10&bwid=11")
            .unwrap();
        resource_detail(html, &base(), &url, "courseboard-post")
    }

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
        let url = base().join("/mod/lti/view.php?id=7").unwrap();
        let detail = resource_detail(html, &base(), &url, "lti").unwrap();
        assert!(!detail.text.contains("private-user-id"));
        assert!(!detail.text.contains("login_hint"));
        assert!(detail.text.contains("Course tool"));
        assert!(detail.text.contains("Ready"));
    }

    #[test]
    fn board_post_detail_uses_post_id_and_preserves_board_id() {
        let detail = notice(
            "<div class='courseboard_view'><div class='subject'><h3>Notice</h3></div>\
             <div class='content'>Body</div></div>",
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
        let first = notice(html).unwrap();
        assert_eq!(first.title, "Exam schedule");
        assert_eq!(
            first.text,
            "Discuss Views : 10, Next, and Enter password in class. Reading Notes"
        );
        assert_eq!(first.links.len(), 2);
        assert!(first.links.iter().all(|l| !l.url.contains("article.php")));
        let changed_chrome = html
            .replace("<div class='hit'>Views : 10", "<div class='hit'>Views : 11")
            .replace("Other post", "Different neighbor");
        let second = notice(&changed_chrome).unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(second).unwrap()
        );
    }

    #[test]
    fn notice_requires_explicit_title_and_body_but_allows_empty_body() {
        for html in [
            "<main><h1>Notice</h1>Fallback is unsafe</main>",
            "<div class='courseboard_view'><div class='content'>Body</div></div>",
            "<div class='courseboard_view'><div class='subject'><h3>Title</h3></div></div>",
        ] {
            assert_eq!(notice(html).unwrap_err().code, "UPSTREAM_SHAPE_CHANGED");
        }
        let empty = notice("<div class='courseboard_view'><div class='subject'><h3>Title</h3></div><div class='content'></div></div>").unwrap();
        assert!(empty.text.is_empty());
    }

    #[test]
    fn notice_preserves_text_and_combined_link_caps() {
        let link = |i: usize| format!("<a href='/pluginfile.php/{i}'>File {i}</a>");
        let attachments: String = (0..60).map(link).collect();
        let body_links: String = (50..102).map(link).collect();
        let html = format!(
            "<div class='courseboard_view'><div class='subject'><h3>Title</h3></div><div class='info'><div class='files'>{attachments}</div></div><div class='content'>{}{body_links}</div></div>",
            "가".repeat(100_001)
        );
        let detail = notice(&html).unwrap();
        assert!(detail.text_truncated);
        assert_eq!(detail.text.chars().count(), 100_000);
        assert!(detail.links_truncated);
        assert_eq!(detail.links.len(), 100);
    }

    #[test]
    fn pagination_stays_on_origin() {
        let base = Url::parse("https://klms.example/").unwrap();
        let next = "<div class='pagination'><span class='next'><a href='/mod/courseboard/view.php?id=8&page=2'>Next</a></span></div>";
        assert_eq!(
            next_page_url(next, &base).unwrap().as_deref(),
            Some("https://klms.example/mod/courseboard/view.php?id=8&page=2")
        );
        let external = "<a rel='next' href='https://external.example/page'>Next</a>";
        assert!(next_page_url(external, &base).is_err());
    }
}
