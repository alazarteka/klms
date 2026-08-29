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
    let text_value = content.map(visible_text).unwrap_or_default();
    let text_truncated = text_value.chars().count() > 100_000;
    let mut links = match content {
        Some(root) => link_items_in(root, base_url, 101)?,
        None => Vec::new(),
    };
    let links_truncated = links.len() > 100;
    links.truncate(100);
    let id = query_id(url, &["id", "bwid"]);
    let reference = if kind == "courseboard-post" {
        query_id(url, &["id"])
            .zip(query_id(url, &["bwid"]))
            .map(|(board, post)| ResourceRef::BoardPost { board, post }.to_string())
    } else {
        ResourceRef::from_activity(kind, id.as_deref(), Some(url.as_str()))
            .map(|reference| reference.to_string())
    };
    Ok(ResourceDetail {
        id,
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
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::sesskey;

    #[test]
    fn extracts_session_key_without_exposing_it_elsewhere() {
        assert_eq!(
            sesskey(r#"<script>var cfg={"sesskey":"abc123"}</script>"#).unwrap(),
            "abc123"
        );
    }
}
