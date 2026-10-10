use serde::Serialize;
use serde_json::Value;

use crate::{error::AppError, models::ResourceDetail};

pub const SCHEMA_VERSION: &str = "4";

#[derive(Debug, Clone, Serialize)]
pub struct ListMeta {
    pub returned: usize,
    pub limit: usize,
    pub complete: bool,
    pub total: Option<usize>,
    pub next_cursor: Option<String>,
    pub fresh_through: Option<i64>,
    pub source_complete: Option<bool>,
}

pub struct CommandResult {
    pub command: &'static str,
    pub data: Value,
    pub human: String,
    pub warnings: Vec<String>,
    pub meta: Option<ListMeta>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Envelope<'a> {
    Success {
        schema_version: &'static str,
        ok: bool,
        command: &'a str,
        data: &'a Value,
        warnings: &'a [String],
        meta: &'a Option<ListMeta>,
    },
    Failure {
        schema_version: &'static str,
        ok: bool,
        error: &'a AppError,
    },
}

pub fn result<T: Serialize>(
    command: &'static str,
    data: &T,
    human: String,
) -> Result<CommandResult, AppError> {
    Ok(CommandResult {
        command,
        data: serde_json::to_value(data)
            .map_err(|error| AppError::internal(format!("failed to encode output: {error}")))?,
        human,
        warnings: Vec::new(),
        meta: None,
    })
}

/// Truncate `rows` to `limit`, render them with `render(rows, available)`, and
/// attach list metadata. `source_complete` says whether KLMS showed every row.
pub fn listing<T: Serialize>(
    command: &'static str,
    mut rows: Vec<T>,
    limit: usize,
    source_complete: bool,
    render: impl FnOnce(&[T], usize) -> String,
) -> Result<CommandResult, AppError> {
    let available = rows.len();
    rows.truncate(limit);
    let mut result = result(command, &rows, render(&rows, available))?;
    result.meta = Some(ListMeta {
        returned: rows.len(),
        limit,
        complete: source_complete && rows.len() == available,
        total: source_complete.then_some(available),
        next_cursor: None,
        fresh_through: None,
        source_complete: Some(source_complete),
    });
    Ok(result)
}

/// Local-library freshness: (`fresh_through`, `source_complete`).
pub type Coverage = (Option<i64>, Option<bool>);

pub fn local_collection<T: Serialize>(
    command: &'static str,
    data: &T,
    human: String,
    returned: usize,
    limit: usize,
    query_complete: bool,
    (fresh_through, source_complete): Coverage,
) -> Result<CommandResult, AppError> {
    let human = if returned == 0 {
        "No records found.".into()
    } else {
        human
    };
    let mut result = result(command, data, human)?;
    if !query_complete {
        result.warnings.push(format!(
            "Results truncated at {limit} records; increase --limit to see more."
        ));
    }
    result.meta = Some(ListMeta {
        returned,
        limit,
        complete: query_complete,
        total: query_complete.then_some(returned),
        next_cursor: None,
        fresh_through,
        source_complete,
    });
    Ok(result)
}

/// Tab-separated table headed "TITLE — showing N of AVAILABLE{unit}".
pub fn body<T>(
    rows: &[T],
    available: usize,
    (title, unit, header): (&str, &str, &str),
    format_row: impl Fn(&T) -> String,
) -> String {
    let heading = format!("{title} — showing {} of {available}{unit}", rows.len());
    let lines = rows.iter().map(format_row);
    [heading, header.into()]
        .into_iter()
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `body`, or "No <title> found." when there are no rows.
pub fn table<T>(
    rows: &[T],
    avail: usize,
    title: &str,
    header: &str,
    f: impl Fn(&T) -> String,
) -> String {
    match rows.is_empty() {
        true => format!("No {} found.", title.to_lowercase()),
        false => body(rows, avail, (title, "", header), f),
    }
}

/// One tab-separated row.
pub fn row(cells: &[&str]) -> String {
    cells.join("\t")
}

/// An optional cell, with `fallback` when absent.
pub fn cell<'a>(value: &'a Option<String>, fallback: &'a str) -> &'a str {
    value.as_deref().unwrap_or(fallback)
}

pub fn detail(detail: &ResourceDetail) -> String {
    let mut output = format!(
        "{}\nType: {}\nRef: {}\nURL: {}",
        detail.title,
        detail.kind,
        cell(&detail.reference, "-"),
        detail.url
    );
    if !detail.text.is_empty() {
        output.push_str(&format!("\n\n{}", detail.text));
    }
    if detail.text_truncated {
        output.push_str("\n\n[Detail text truncated]");
    }
    if !detail.links.is_empty() {
        output.push_str("\n\nLinks:");
        for link in &detail.links {
            output.push_str(&format!("\n{}\t{}", link.title, link.url));
        }
    }
    if detail.links_truncated {
        output.push_str("\n[Link list truncated]");
    }
    output
}

pub fn print_success(result: &CommandResult, json: bool) {
    if json {
        let envelope = Envelope::Success {
            schema_version: SCHEMA_VERSION,
            ok: true,
            command: result.command,
            data: &result.data,
            warnings: &result.warnings,
            meta: &result.meta,
        };
        println!(
            "{}",
            serde_json::to_string(&envelope).expect("serializable envelope")
        );
    } else {
        println!("{}", sanitize_terminal(&result.human));
        for warning in &result.warnings {
            eprintln!("warning: {}", sanitize_terminal(warning));
        }
    }
}

pub fn print_error(error: &AppError, json: bool) {
    if json {
        let envelope = Envelope::Failure {
            schema_version: SCHEMA_VERSION,
            ok: false,
            error,
        };
        eprintln!(
            "{}",
            serde_json::to_string(&envelope).expect("serializable envelope")
        );
    } else {
        eprintln!(
            "error [{}]: {}",
            error.code,
            sanitize_terminal(&error.message)
        );
        if let Some(hint) = &error.hint {
            eprintln!("hint: {}", sanitize_terminal(hint));
        }
    }
}

pub fn sanitize_terminal(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            (!character.is_control() || matches!(character, '\n' | '\t'))
                && !matches!(
                    character,
                    '\u{061c}'
                        | '\u{200e}'
                        | '\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        })
        .collect()
}
