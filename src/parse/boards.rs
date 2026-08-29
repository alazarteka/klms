use std::collections::HashSet;

use scraper::{ElementRef, Html};
use url::Url;

use super::shared::{has_any, query_id, selector, text};
use crate::{
    date,
    error::AppError,
    models::{Activity, BoardPost},
    reference::ResourceRef,
    safe_url,
};

pub fn is_notice_board(activity: &Activity) -> bool {
    activity.kind.eq_ignore_ascii_case("courseboard")
        && (activity.title.to_ascii_lowercase().contains("notice")
            || activity.title.contains("공지"))
}

pub fn board_posts(
    html: &str,
    base_url: &Url,
    board_id: Option<String>,
) -> Result<Vec<BoardPost>, AppError> {
    let document = Html::parse_document(html);
    let anchors = selector("a[href*='/mod/courseboard/article.php']")?;
    let cells = selector("td")?;
    let mut seen = HashSet::new();
    let mut posts = Vec::new();
    for anchor in document.select(&anchors) {
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let Ok(url) = base_url.join(href) else {
            continue;
        };
        let id = query_id(&url, &["bwid"]);
        let key = id.clone().unwrap_or_else(|| url.to_string());
        if !seen.insert(key) {
            continue;
        }
        let title = text(anchor);
        if title.is_empty() {
            continue;
        }
        let posted = anchor
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|node| node.value().name() == "tr")
            .and_then(|row| {
                row.select(&cells)
                    .map(text)
                    .find(|value| date::normalize_datetime(value).is_some())
            });
        let reference = board_id.as_ref().zip(id.as_ref()).map(|(board, post)| {
            ResourceRef::BoardPost {
                board: board.clone(),
                post: post.clone(),
            }
            .to_string()
        });
        posts.push(BoardPost {
            board_id: board_id.clone(),
            id,
            reference,
            title,
            posted,
            url: safe_url::display(&url),
        });
    }
    if posts.is_empty()
        && !has_any(
            &document,
            &[".courseboard", "table.generaltable", "table.board-list"],
        )?
    {
        return Err(AppError::shape(
            "board page contained no recognizable post region",
        ));
    }
    Ok(posts)
}

#[cfg(test)]
mod tests {
    use super::board_posts;
    use url::Url;

    #[test]
    fn parses_courseboard_posts() {
        let html = r#"<table><tr><td><a href="/mod/courseboard/article.php?id=8&bwid=9">Exam notice</a></td><td>2026-08-29</td></tr></table>"#;
        let base = Url::parse("https://klms.kaist.ac.kr").unwrap();
        let rows = board_posts(html, &base, Some("8".into())).unwrap();
        assert_eq!(rows[0].id.as_deref(), Some("9"));
        assert_eq!(rows[0].posted.as_deref(), Some("2026-08-29"));
    }
}
