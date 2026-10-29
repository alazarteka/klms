//! One bounded, identity-checked board traversal for live lists and library sync.
use std::collections::HashSet;

use crate::{
    client::{KlmsClient, policy},
    error::AppError,
    models::BoardPost,
    parse,
    reference::valid_id,
    url::{Url, query_value},
};

pub const PAGE_BUDGET: usize = 20;

/// Visits each distinct post once, with its canonical `board-post:` reference.
/// Returns whether pagination was exhausted: `false` means the page budget ran
/// out while another page was still linked. Integrity failures (cycles,
/// identity changes, refused or malformed pagers) are errors.
pub fn walk(
    client: &KlmsClient,
    start: &str,
    mut visit: impl FnMut(String, BoardPost),
) -> Result<bool, AppError> {
    let base = Url::parse("https://klms.invalid/").expect("fixed URL");
    let initial = base
        .join(start)
        .map_err(|_| AppError::shape("invalid board URL"))?;
    let board = query_value(&initial, "id")
        .filter(|id| valid_id(id))
        .ok_or_else(|| AppError::shape("board URL had no numeric board id"))?;
    let same_id = |url: &Url| {
        query_value(url, "id")
            .is_some_and(|id| id.trim_start_matches('0') == board.trim_start_matches('0'))
    };
    let same_board = |url: &Url| url.path() == "/mod/courseboard/view.php" && same_id(url);
    if !same_board(&initial) {
        return Err(AppError::shape("expected a board listing URL"));
    }
    let mut next = Some(start.to_owned());
    let mut visited = HashSet::new();
    let mut posts = HashSet::new();
    for _ in 0..PAGE_BUDGET {
        let Some(url) = next.take() else {
            return Ok(true);
        };
        let page = client.get(&url)?;
        if !same_board(&page.url) {
            return Err(AppError::shape("board pagination changed board identity"));
        }
        if !visited.insert(page.url.to_string()) {
            return Err(AppError::shape("board pagination cycle detected"));
        }
        for post in parse::board_posts(&page.text, &page.url, Some(board.clone()))? {
            let url =
                Url::parse(&post.url).map_err(|_| AppError::shape("invalid board post URL"))?;
            if url.origin() != page.url.origin() || !same_id(&url) {
                return Err(AppError::shape("board post changed board identity"));
            }
            let reference = post.reference.clone().ok_or_else(|| {
                AppError::shape("board post contained no canonical post reference")
            })?;
            if posts.insert(reference.clone()) {
                visit(reference, post);
            }
        }
        next = parse::next_page_url(&page.text, &page.url)?;
        if let Some(url) = &next {
            let url =
                Url::parse(url).map_err(|_| AppError::shape("invalid board pagination URL"))?;
            if !same_board(&url) {
                return Err(AppError::shape("board pagination changed board identity"));
            }
            // Check query policy before another page can be requested.
            policy::read_url(&url)?;
        }
    }
    Ok(next.is_none())
}
