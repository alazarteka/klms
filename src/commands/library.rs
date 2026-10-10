use crate::url::Url;

use crate::{
    cli::{
        LibraryCommand, LibraryDownloadArg, LibraryFieldArg, LibraryRelationsCommand,
        LibrarySyncArgs,
    },
    client::KlmsClient,
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
    output::local_collection(
        command,
        &rows,
        human,
        rows.len(),
        limit,
        !truncated,
        coverage,
    )
}

pub(super) fn local(command: &LibraryCommand) -> Result<CommandResult, AppError> {
    let mut corpus = crate::corpus::Corpus::open()?;
    match command {
        LibraryCommand::Status => library_status(&corpus),
        LibraryCommand::Search { query, list } => {
            let coverage = corpus.coverage()?;
            paged(
                "library.search",
                list.limit,
                coverage,
                |n| corpus.search(query, n),
                |r| format!("{}\t{}\t{}", r.reference, r.kind, r.title),
            )
        }
        LibraryCommand::Changes(list) => {
            let coverage = corpus.coverage()?;
            paged(
                "library.changes",
                list.limit,
                coverage,
                |n| corpus.changes(n),
                |r| format!("{}\t{}\t{}", r.occurred_at, r.kind, r.subject_ref),
            )
        }
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
            let human = serde_json::to_string_pretty(&row)
                .map_err(|error| AppError::internal(error.to_string()))?;
            output::result("library.show", &row, human)
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
        } => library_content(&corpus, reference, *max_bytes),
        LibraryCommand::Export { reference, out } => library_export(&corpus, reference, out),
        LibraryCommand::Edit(args) => {
            let value = read_library_text(args.value.as_deref(), args.value_file.as_deref())?;
            let field = match args.field {
                LibraryFieldArg::Title => "title",
                LibraryFieldArg::Filename => "filename",
                LibraryFieldArg::Summary => "summary",
                LibraryFieldArg::Note => "note",
                LibraryFieldArg::Tag => "tag",
            };
            let row = corpus.edit(
                &args.reference,
                field,
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
        LibraryCommand::Relations(args) => match &args.command {
            LibraryRelationsCommand::Add {
                left,
                right,
                kind,
                actor,
            } => {
                let row = corpus.add_relation(left, right, kind, actor)?;
                let reference = row.reference.clone();
                output::result(
                    "library.relations.add",
                    &row,
                    format!("Recorded {reference}"),
                )
            }
        },
        LibraryCommand::Sync(_) => Err(AppError::internal(
            "sync was routed through the local library dispatcher",
        )),
    }
}

fn library_status(corpus: &crate::corpus::Corpus) -> Result<CommandResult, AppError> {
    let model = corpus.status()?;
    let mut human = format!(
        "Library storage: {}\nDatabase: {}\nObjects: {}\nSchema: {}\nCourses: {}\nResources: {}\nRepresentations: {}\nStored content: {} bytes",
        if model.created {
            "initialized"
        } else {
            "ready"
        },
        model.database_path,
        model.object_store_path,
        model.schema_version,
        model.courses,
        model.resources,
        model.representations,
        model.stored_bytes,
    );
    if let Some(sync) = &model.last_sync {
        human.push_str(&format!(
            "\nLast sync attempt: {} — {}\nScope: {}\nStarted: {}\nFinished: {}",
            sync.reference,
            sync.status,
            sync.scope,
            crate::date::epoch_to_seoul(sync.started_at).unwrap_or_else(|| "unknown".into()),
            sync.finished_at
                .and_then(crate::date::epoch_to_seoul)
                .unwrap_or_else(|| "not recorded".into()),
        ));
        if sync.scope != "all" {
            human.push_str("\nCourse-scoped syncs do not establish global coverage.");
        }
    } else {
        human.push_str("\nLast sync attempt: none");
    }
    human.push_str(&format!(
        "\nLast complete global sync: {}",
        model
            .fresh_through
            .and_then(crate::date::epoch_to_seoul)
            .unwrap_or_else(|| "none".into()),
    ));
    let mut result = output::result("library.status", &model, human)?;
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

fn library_content(
    corpus: &crate::corpus::Corpus,
    reference: &str,
    max: usize,
) -> Result<CommandResult, AppError> {
    let model = corpus.preview(reference, max)?;
    let human = model
        .text
        .clone()
        .unwrap_or_else(|| "Binary content is available through `library export`.".into());
    output::result("library.content", &model, human)
}
fn library_export(
    corpus: &crate::corpus::Corpus,
    reference: &str,
    out: &std::path::Path,
) -> Result<CommandResult, AppError> {
    let bytes = corpus.export(reference, out)?;
    let model = serde_json::json!({"ref":reference,"path":out,"byte_length":bytes});
    output::result(
        "library.export",
        &model,
        format!("Exported {} bytes to {}", bytes, out.display()),
    )
}

pub(super) fn sync(
    client: &KlmsClient,
    base_url: &Url,
    args: &LibrarySyncArgs,
) -> Result<CommandResult, AppError> {
    let mut corpus = crate::corpus::Corpus::open()?;
    let model = corpus.sync(
        client,
        base_url,
        args.course.as_deref(),
        crate::corpus::SyncOptions {
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
    let mut text = if let Some(value) = value {
        value.to_owned()
    } else if let Some(path) = path {
        if path == std::path::Path::new("-") {
            read_curation_text(std::io::stdin().lock())?
        } else {
            let file = std::fs::File::open(path)
                .map_err(|e| AppError::config(format!("cannot read {}: {e}", path.display())))?;
            read_curation_text(file)?
        }
    } else {
        unreachable!("clap requires exactly one of --value and --value-file");
    };
    if text.len() > MAX_CURATION_TEXT {
        return Err(AppError::limit("curation text exceeds 1 MiB"));
    }
    while text.ends_with('\n') {
        text.pop();
    }
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
