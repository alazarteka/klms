use std::collections::HashSet;

use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{error::AppError, models::LinkItem, reference::valid_id, safe_url};

struct IndexedCell {
    header: String,
    value: String,
    link: Option<String>,
}

pub(super) struct IndexedRow {
    cells: Vec<IndexedCell>,
}

impl IndexedRow {
    // Exact headers win over qualified labels, but neither may be ambiguous.
    // Both accessors resolve the same cell, including when its link is absent.
    fn cell(&self, header: &str) -> Result<Option<&IndexedCell>, AppError> {
        let exact = self.cells.iter().any(|cell| cell.header == header);
        let mut matching = self.cells.iter().filter(|cell| {
            if exact {
                cell.header == header
            } else {
                cell.header.contains(header)
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
            .map(|cell| cell.value.clone())
            .filter(|value| !value.is_empty() && value != "-"))
    }

    pub(super) fn link_for(&self, header: &str, base_url: &Url) -> Result<Option<Url>, AppError> {
        Ok(self
            .cell(header)?
            .and_then(|cell| cell.link.as_deref())
            .and_then(|value| base_url.join(value).ok()))
    }
}

pub(super) fn has_any(document: &Html, selectors: &[&str]) -> Result<bool, AppError> {
    for css in selectors {
        if document.select(&selector(css)?).next().is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn semantic_table<'a>(
    document: &'a Html,
    expected: &[&str],
) -> Result<Option<ElementRef<'a>>, AppError> {
    let tables = selector("table")?;
    let rows = selector("tr")?;
    let cells = selector("th, td")?;
    Ok(document.select(&tables).find(|table| {
        let headers: Vec<_> = table
            .select(&rows)
            .next()
            .map(|row| {
                row.select(&cells)
                    .map(|cell| text(cell).to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        expected
            .iter()
            .all(|needle| headers.iter().any(|header| header.contains(needle)))
    }))
}

pub(super) fn indexed_rows(table: ElementRef<'_>) -> Result<Vec<IndexedRow>, AppError> {
    let rows = selector("tr")?;
    let cells = selector("th, td")?;
    let links = selector("a[href]")?;
    let mut table_rows = table.select(&rows);
    let headers: Vec<_> = table_rows
        .next()
        .map(|row| {
            row.select(&cells)
                .map(|cell| text(cell).to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let mut parsed = Vec::new();
    for row in table_rows {
        let row_cells: Vec<_> = row.select(&cells).collect();
        if row_cells.is_empty() {
            continue;
        }
        let cells = headers
            .iter()
            .zip(row_cells)
            .map(|(header, cell)| IndexedCell {
                header: header.clone(),
                value: text(cell),
                link: cell
                    .select(&links)
                    .find_map(|anchor| anchor.value().attr("href"))
                    .map(str::to_owned),
            })
            .collect();
        parsed.push(IndexedRow { cells });
    }
    Ok(parsed)
}

pub(super) fn link_items(
    document: &Html,
    base_url: &Url,
    css: &str,
    limit: usize,
) -> Result<Vec<LinkItem>, AppError> {
    let selector = selector(css)?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for anchor in document.select(&selector) {
        let title = text(anchor);
        let Some(url) = anchor
            .value()
            .attr("href")
            .and_then(|href| base_url.join(href).ok())
        else {
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
    Ok(rows)
}

pub(super) fn link_items_in(
    root: ElementRef<'_>,
    base_url: &Url,
    limit: usize,
) -> Result<Vec<LinkItem>, AppError> {
    let anchors = selector("a[href]")?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for anchor in root.select(&anchors) {
        let title = text(anchor);
        let Some(url) = anchor
            .value()
            .attr("href")
            .and_then(|href| base_url.join(href).ok())
        else {
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
    Ok(rows)
}

pub(super) fn first_text(document: &Html, css: &str) -> Result<Option<String>, AppError> {
    Ok(document
        .select(&selector(css)?)
        .map(text)
        .find(|value| !value.is_empty()))
}

pub(super) fn selected_value(document: &Html, css: &str) -> Option<String> {
    let select = Selector::parse(css).ok()?;
    let selected = Selector::parse("option[selected]").ok()?;
    let fallback = Selector::parse("option").ok()?;
    let node = document.select(&select).next()?;
    node.select(&selected)
        .next()
        .or_else(|| node.select(&fallback).next())
        .map(text)
}

pub(super) fn selector(value: &str) -> Result<Selector, AppError> {
    Selector::parse(value)
        .map_err(|_| AppError::internal(format!("invalid built-in selector: {value}")))
}

pub(super) fn text(element: ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn visible_text(element: ElementRef<'_>) -> String {
    element
        .descendants()
        .filter_map(|node| {
            let value = node.value().as_text()?;
            let hidden = node
                .ancestors()
                .filter_map(ElementRef::wrap)
                .any(|ancestor| {
                    matches!(
                        ancestor.value().name(),
                        "script" | "style" | "noscript" | "template"
                    )
                });
            (!hidden).then_some(value.as_ref())
        })
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn week_number(value: &str) -> Option<u32> {
    let lower = value.to_ascii_lowercase();
    let position = lower.find("week")? + 4;
    lower[position..]
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

pub(super) fn query_id(url: &Url, names: &[&str]) -> Option<String> {
    url.query_pairs()
        .find_map(|(key, value)| names.contains(&key.as_ref()).then(|| value.into_owned()))
        .filter(|value| valid_id(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_headers_keep_values_and_links_in_the_same_column() {
        let base = Url::parse("https://klms.kaist.ac.kr").unwrap();
        for (headers, cells, expected, link) in [
            (
                "<th>Course name</th><th>Name</th>",
                "<td><a href='/course'>Course</a></td><td><a href='/assignment'>Work</a></td>",
                "Work",
                Some("/assignment"),
            ),
            (
                "<th>Assignment name</th><th>Course</th>",
                "<td>Work</td><td><a href='/course'>Course</a></td>",
                "Work",
                None,
            ),
        ] {
            let document = Html::parse_document(&format!(
                "<table><tr>{headers}</tr><tr>{cells}</tr></table>"
            ));
            let table = document.select(&selector("table").unwrap()).next().unwrap();
            let rows = indexed_rows(table).unwrap();
            assert_eq!(rows[0].value("name").unwrap().as_deref(), Some(expected));
            assert_eq!(
                rows[0]
                    .link_for("name", &base)
                    .unwrap()
                    .as_ref()
                    .map(Url::path),
                link
            );
        }
    }

    #[test]
    fn ambiguous_headers_fail_for_both_value_and_link_access() {
        let base = Url::parse("https://klms.kaist.ac.kr").unwrap();
        for headers in [
            "<th>Name</th><th>Name</th>",
            "<th>Assignment name</th><th>Course name</th>",
        ] {
            let document = Html::parse_document(&format!(
                "<table><tr>{headers}</tr><tr><td>Work</td><td><a href='/course'>Course</a></td></tr></table>"
            ));
            let table = document.select(&selector("table").unwrap()).next().unwrap();
            let rows = indexed_rows(table).unwrap();
            assert_eq!(
                rows[0].value("name").unwrap_err().code,
                "UPSTREAM_SHAPE_CHANGED"
            );
            assert_eq!(
                rows[0].link_for("name", &base).unwrap_err().code,
                "UPSTREAM_SHAPE_CHANGED"
            );
        }
    }
}
