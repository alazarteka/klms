use std::collections::HashSet;

use crate::url::Url;
use scraper::{ElementRef, Html, Selector};

use crate::{error::AppError, models::LinkItem, reference::valid_id, safe_url};

/// One coursework table row as (normalized header, text, first link href).
type Cell = (String, String, Option<String>);
pub(super) struct IndexedRow(Vec<Cell>);

impl IndexedRow {
    // Exact headers win over qualified labels, but neither may be ambiguous.
    // Both accessors resolve the same cell, including when its link is absent.
    fn cell(&self, header: &str) -> Result<Option<&Cell>, AppError> {
        let exact = self.0.iter().any(|cell| cell.0 == header);
        let mut matching = self.0.iter().filter(|cell| {
            if exact {
                cell.0 == header
            } else {
                cell.0.contains(header)
            }
        });
        let cell = matching.next();
        if matching.next().is_some() {
            return Err(AppError::shape(format!(
                "coursework table has ambiguous {header:?} columns"
            )));
        }
        Ok(cell)
    }

    pub(super) fn value(&self, header: &str) -> Result<Option<String>, AppError> {
        Ok(self
            .cell(header)?
            .map(|cell| cell.1.clone())
            .filter(|value| !value.is_empty() && value != "-"))
    }

    pub(super) fn link_for(&self, header: &str, base_url: &Url) -> Result<Option<Url>, AppError> {
        Ok(self
            .cell(header)?
            .and_then(|cell| cell.2.as_deref())
            .and_then(|value| base_url.join(value).ok()))
    }
}

/// Built-in selectors are literals; an invalid one is a bug caught by tests.
pub(super) fn sel(css: &str) -> Selector {
    Selector::parse(css).expect("valid built-in selector")
}

pub(super) fn first<'a>(root: ElementRef<'a>, css: &str) -> Option<ElementRef<'a>> {
    root.select(&sel(css)).next()
}

pub(super) fn all<'a>(root: ElementRef<'a>, css: &str) -> Vec<ElementRef<'a>> {
    root.select(&sel(css)).collect()
}

pub(super) fn has_any(document: &Html, selectors: &[&str]) -> bool {
    selectors
        .iter()
        .any(|css| first(document.root_element(), css).is_some())
}

pub(super) const NEXT_PAGE_SELECTORS: &[&str] = &[
    "a[rel=next]",
    ".pagination .next a",
    "a[data-page-number][aria-label*=Next]",
];

pub(super) fn row_cells(row: ElementRef<'_>) -> Vec<String> {
    all(row, "th, td").into_iter().map(text).collect()
}

/// First table whose normalized header row satisfies `ok`.
pub(super) fn find_table(
    document: &Html,
    ok: impl Fn(&[String]) -> bool,
) -> Option<ElementRef<'_>> {
    all(document.root_element(), "table")
        .into_iter()
        .find(|table| {
            let headers = first(*table, "tr").map(row_cells).unwrap_or_default();
            ok(&headers.iter().map(|h| header_name(h)).collect::<Vec<_>>())
        })
}

pub(super) fn indexed_rows(table: ElementRef<'_>) -> Vec<IndexedRow> {
    let mut rows = all(table, "tr").into_iter();
    let headers: Vec<_> = rows
        .next()
        .map(|row| row_cells(row).iter().map(|h| header_name(h)).collect())
        .unwrap_or_default();
    rows.filter_map(|row| {
        let cells = all(row, "th, td");
        (!cells.is_empty()).then(|| {
            IndexedRow(
                headers
                    .iter()
                    .zip(cells)
                    .map(|(header, cell)| {
                        let href = first(cell, "a[href]").and_then(|a| a.value().attr("href"));
                        (header.clone(), text(cell), href.map(str::to_owned))
                    })
                    .collect(),
            )
        })
    })
    .collect()
}

pub(super) fn href_url(anchor: ElementRef<'_>, base_url: &Url) -> Option<Url> {
    anchor
        .value()
        .attr("href")
        .and_then(|href| base_url.join(href).ok())
}

/// Titled, de-duplicated links from `anchors`, stopping at `limit`.
pub(super) fn link_items<'a>(
    anchors: impl Iterator<Item = ElementRef<'a>>,
    base_url: &Url,
    limit: usize,
) -> Vec<LinkItem> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for anchor in anchors {
        let title = text(anchor);
        let Some(url) = href_url(anchor, base_url) else {
            continue;
        };
        if title.is_empty() || !seen.insert(url.to_string()) {
            continue;
        }
        rows.push(LinkItem {
            title,
            url: safe_url::display(&url),
        });
        if rows.len() == limit {
            break;
        }
    }
    rows
}

pub(super) fn first_text(document: &Html, css: &str) -> Option<String> {
    document
        .select(&sel(css))
        .map(text)
        .find(|value| !value.is_empty())
}

pub(super) fn selected_value(document: &Html, css: &str) -> Option<String> {
    let node = document.select(&sel(css)).next()?;
    node.select(&sel("option[selected]"))
        .next()
        .or_else(|| node.select(&sel("option")).next())
        .map(text)
}

fn norm<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn text(element: ElementRef<'_>) -> String {
    norm(element.text())
}

/// Preserve qualified English labels and ambiguity checks while recognizing
/// the Korean labels used by KLMS. Unknown locales still fail visibly.
pub(super) fn header_name(value: &str) -> String {
    let normalized = norm(std::iter::once(value));
    let compact: String = normalized.chars().filter(|c| !c.is_whitespace()).collect();
    let canonical = match compact.as_str() {
        "주차" | "주차(토픽)" => "week",
        "제목" => "name",
        "마감일시" | "마감일" => "due date",
        "제출" | "제출상태" => "submit",
        "퀴즈그만하기" | "퀴즈종료" => "quiz closes",
        "성적" => "grade",
        "성적항목" => "grade item",
        "범위" => "range",
        "피드백" => "feedback",
        "백분율" => "percentage",
        "강의합계에대한기여도" => "contribution",
        "날짜" => "date",
        "출석" => "attended",
        "결석" => "absent",
        _ => return normalized.to_ascii_lowercase(),
    };
    canonical.into()
}

pub(super) fn visible_text(element: ElementRef<'_>) -> String {
    norm(element.descendants().filter_map(|node| {
        let value = node.value().as_text()?;
        let hidden = node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|ancestor| {
                matches!(
                    ancestor.value().name(),
                    "script" | "style" | "noscript" | "template"
                ) || ancestor.value().attr("hidden").is_some()
                    || ancestor.value().attr("aria-hidden") == Some("true")
            });
        (!hidden).then_some(value.as_ref())
    }))
}

pub(super) fn week_number(value: &str) -> Option<u32> {
    let lower = value.to_ascii_lowercase();
    let not_digit = |c: char| !c.is_ascii_digit();
    if let Some(position) = lower.find("week") {
        return lower[position + 4..]
            .split(not_digit)
            .find(|s| !s.is_empty())?
            .parse()
            .ok();
    }
    lower
        .split_once("주차")?
        .0
        .trim_end()
        .rsplit(not_digit)
        .next()?
        .parse()
        .ok()
}

pub(super) fn query_id(url: &Url, names: &[&str]) -> Option<String> {
    url.query_pairs().find_map(|(key, value)| {
        (names.contains(&key.as_ref()) && valid_id(&value)).then(|| value.into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_headers_keep_values_and_links_in_the_same_column() {
        let base = Url::parse("https://klms.kaist.ac.kr").unwrap();
        let link = "<td><a href='/course'>Course</a></td>";
        for (headers, cells, expected) in [
            (
                "<th>Course name</th><th>Name</th>",
                format!("{link}<td><a href='/assignment'>Work</a></td>"),
                Ok(Some("/assignment")),
            ),
            (
                "<th>Assignment name</th><th>Course</th>",
                format!("<td>Work</td>{link}"),
                Ok(None),
            ),
            (
                "<th>Name</th><th>Name</th>",
                format!("<td>Work</td>{link}"),
                Err(()),
            ),
            (
                "<th>Assignment name</th><th>Course name</th>",
                format!("<td>Work</td>{link}"),
                Err(()),
            ),
        ] {
            let document = Html::parse_document(&format!(
                "<table><tr>{headers}</tr><tr>{cells}</tr></table>"
            ));
            let rows = indexed_rows(first(document.root_element(), "table").unwrap());
            let (value, found) = (rows[0].value("name"), rows[0].link_for("name", &base));
            match expected {
                Ok(path) => {
                    assert_eq!(value.unwrap().as_deref(), Some("Work"));
                    assert_eq!(found.unwrap().as_ref().map(Url::path), path);
                }
                Err(()) => {
                    assert_eq!(value.unwrap_err().code, "UPSTREAM_SHAPE_CHANGED");
                    assert_eq!(found.unwrap_err().code, "UPSTREAM_SHAPE_CHANGED");
                }
            }
        }
    }
}
