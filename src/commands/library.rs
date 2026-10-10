use clap::ValueEnum;

use crate::url::Url;
use crate::{
    cli::{LibraryCommand, LibraryDownloadArg, LibraryRelationsCommand, LibrarySyncArgs},
    client::KlmsClient,
    corpus::{Corpus, SyncOptions},
    error::AppError,
    output::{self, CommandResult},
};

/// Fetch `limit + 1` rows to detect truncation, then render the page.
fn paged<T: serde::Serialize>(
    command: &'static str,
    limit: usize,
    coverage: (Option<i64>, Option<bool>),
    fetch: impl FnOnce(usize) -> Result<Vec<T>, AppError>,
    format_row: impl Fn(&T) -> String,
) -> Result<CommandResult, AppError> {
    let mut rows = fetch(limit.saturating_add(1))?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let human = rows.iter().map(format_row).collect::<Vec<_>>().join("\n");
    let returned = rows.len();
    output::local_collection(command, &rows, human, returned, limit, !truncated, coverage)
}

pub(super) fn local(command: &LibraryCommand) -> Result<CommandResult, AppError> {
    let mut corpus = Corpus::open()?;
    match command {
        LibraryCommand::Status => library_status(&corpus),
        LibraryCommand::Search { query, list } => paged(
            "library.search",
            list.limit,
            corpus.coverage()?,
            |n| corpus.search(query, n),
            |r| format!("{}\t{}\t{}", r.reference, r.kind, r.title),
        ),
        LibraryCommand::Changes(list) => paged(
            "library.changes",
            list.limit,
            corpus.coverage()?,
            |n| corpus.changes(n),
            |r| format!("{}\t{}\t{}", r.occurred_at, r.kind, r.subject_ref),
        ),
        LibraryCommand::Activity(args) => paged(
            "library.activity",
            args.list.limit,
            (None, None),
            |n| corpus.activity(args.subject.as_deref(), n),
            |r| {
                format!(
                    "{}\t{}\t{}\t{}",
                    r.created_at, r.actor, r.field, r.subject_ref
                )
            },
        ),
        LibraryCommand::Show { reference } => {
            let row = corpus.show(reference)?;
            let human = serde_json::to_string_pretty(&row);
            output::result(
                "library.show",
                &row,
                human.map_err(|e| AppError::internal(e.to_string()))?,
            )
        }
        LibraryCommand::History { reference, list } => paged(
            "library.history",
            list.limit,
            (None, None),
            |n| corpus.history(reference, n),
            |r| format!("{}\t{}\t{}", r.id, r.observed_at, r.digest),
        ),
        LibraryCommand::Content {
            reference,
            max_bytes,
        } => {
            let model = corpus.preview(reference, *max_bytes)?;
            let human = model
                .text
                .clone()
                .unwrap_or_else(|| "Binary content is available through `library export`.".into());
            output::result("library.content", &model, human)
        }
        LibraryCommand::Export { reference, out } => {
            let bytes = corpus.export(reference, out)?;
            let model = serde_json::json!({"ref":reference,"path":out,"byte_length":bytes});
            output::result(
                "library.export",
                &model,
                format!("Exported {} bytes to {}", bytes, out.display()),
            )
        }
        LibraryCommand::Edit(args) => {
            let value = read_library_text(args.value.as_deref(), args.value_file.as_deref())?;
            let field = args.field.to_possible_value().expect("no hidden fields");
            let row = corpus.edit(
                &args.reference,
                field.get_name(),
                &value,
                &args.actor,
                args.expected_revision,
            )?;
            output::result(
                "library.edit",
                &row,
                format!(
                    "{} revision {}: {:?} -> {:?}",
                    row.reference, row.revision, row.before, row.after
                ),
            )
        }
        LibraryCommand::Retract(args) => {
            let row = corpus.retract(&args.reference, &args.actor)?;
            output::result(
                "library.retract",
                &row,
                format!("Retracted {}", row.target_ref),
            )
        }
        LibraryCommand::Relations(args) => {
            let LibraryRelationsCommand::Add {
                left,
                right,
                kind,
                actor,
            } = &args.command;
            let row = corpus.add_relation(left, right, kind, actor)?;
            let human = format!("Recorded {}", row.reference);
            output::result("library.relations.add", &row, human)
        }
        LibraryCommand::Sync(_) => Err(AppError::internal(
            "sync was routed through the local library dispatcher",
        )),
    }
}

fn library_status(corpus: &Corpus) -> Result<CommandResult, AppError> {
    let model = corpus.status()?;
    let seoul = |at: Option<i64>, none: &str| {
        at.and_then(crate::date::epoch_to_seoul)
            .unwrap_or_else(|| none.into())
    };
    let mut lines = vec![
        format!(
            "Library storage: {}",
            if model.created {
                "initialized"
            } else {
                "ready"
            }
        ),
        format!("Database: {}", model.database_path),
        format!("Objects: {}", model.object_store_path),
        format!("Schema: {}", model.schema_version),
        format!("Courses: {}", model.courses),
        format!("Resources: {}", model.resources),
        format!("Representations: {}", model.representations),
        format!("Stored content: {} bytes", model.stored_bytes),
    ];
    match &model.last_sync {
        Some(sync) => {
            lines.push(format!(
                "Last sync attempt: {} — {}",
                sync.reference, sync.status
            ));
            lines.push(format!("Scope: {}", sync.scope));
            lines.push(format!(
                "Started: {}",
                seoul(Some(sync.started_at), "unknown")
            ));
            lines.push(format!(
                "Finished: {}",
                seoul(sync.finished_at, "not recorded")
            ));
            if sync.scope != "all" {
                lines.push("Course-scoped syncs do not establish global coverage.".into());
            }
        }
        None => lines.push("Last sync attempt: none".into()),
    }
    lines.push(format!(
        "Last complete global sync: {}",
        seoul(model.fresh_through, "none")
    ));
    let mut result = output::result("library.status", &model, lines.join("\n"))?;
    if model
        .last_sync
        .as_ref()
        .is_some_and(|sync| sync.status == "unfinished")
    {
        result.warnings.push(
            "This attempt did not record completion; it may still be active or may have been interrupted. Check the original process and retry the same command once it has stopped.".into(),
        );
    }
    Ok(result)
}

pub(super) fn sync(
    client: &KlmsClient,
    base_url: &Url,
    args: &LibrarySyncArgs,
) -> Result<CommandResult, AppError> {
    let mut corpus = Corpus::open()?;
    let model = corpus.sync(
        client,
        base_url,
        args.course.as_deref(),
        SyncOptions {
            notices: args.notices,
            files: args.files || args.download.is_some(),
            download_changed: matches!(args.download, Some(LibraryDownloadArg::Changed)),
        },
    )?;
    let human = format!(
        "{} — {}: {} courses, {} resources, {} representations, {} blobs, {} changes, {} truncated, {} failures",
        model.reference,
        model.status,
        model.courses,
        model.resources,
        model.representations,
        model.blobs_added,
        model.changes,
        model.truncated,
        model.failures.len()
    );
    let mut result = output::result("library.sync", &model, human)?;
    result.warnings.extend(model.failures);
    Ok(result)
}

const MAX_CURATION_TEXT: usize = 1024 * 1024;

fn read_library_text(
    value: Option<&str>,
    path: Option<&std::path::Path>,
) -> Result<String, AppError> {
    let mut text = match (value, path) {
        (Some(value), _) => read_curation_text(value.as_bytes())?,
        (None, Some(path)) if path == std::path::Path::new("-") => {
            read_curation_text(std::io::stdin().lock())?
        }
        (None, Some(path)) => {
            let file = std::fs::File::open(path)
                .map_err(|e| AppError::config(format!("cannot read {}: {e}", path.display())))?;
            read_curation_text(file)?
        }
        (None, None) => unreachable!("clap requires exactly one of --value and --value-file"),
    };
    text.truncate(text.trim_end_matches('\n').len());
    Ok(text)
}

fn read_curation_text(reader: impl std::io::Read) -> Result<String, AppError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader
        .take(MAX_CURATION_TEXT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::config(format!("cannot read curation text: {e}")))?;
    if bytes.len() > MAX_CURATION_TEXT {
        return Err(AppError::limit("curation text exceeds 1 MiB"));
    }
    String::from_utf8(bytes).map_err(|_| AppError::config("curation text must be UTF-8"))
}

#[cfg(test)]
mod input_tests {
    use super::{read_curation_text, read_library_text};
    use std::io::{Cursor, Read};

    #[test]
    fn curation_input_stops_at_limit_and_classifies_oversized_utf8() {
        let bytes = "é".repeat(524_289).into_bytes();
        let mut source = Cursor::new(bytes);
        let error = read_curation_text(&mut source).unwrap_err();
        assert_eq!(error.code, "LIMIT_EXCEEDED");
        assert_eq!(source.position(), 1_048_577);
        assert_eq!(
            read_curation_text(Cursor::new("é".repeat(524_288)))
                .unwrap()
                .len(),
            1_048_576
        );
        assert!(read_curation_text(Cursor::new([0xff])).is_err());
        // The file route applies the same byte bound and trailing-newline rule.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("value.txt");
        std::fs::write(&path, "value\n\n").unwrap();
        assert_eq!(read_library_text(None, Some(&path)).unwrap(), "value");
        let mut file = std::fs::File::create(&path).unwrap();
        std::io::copy(&mut std::io::repeat(b'x').take(1_048_577), &mut file).unwrap();
        assert_eq!(
            read_library_text(None, Some(&path)).unwrap_err().code,
            "LIMIT_EXCEEDED"
        );
    }
}
