use url::Url;

use super::read_library_text;
use crate::{
    cli::{
        LibraryCommand, LibraryDownloadArg, LibraryFieldArg, LibraryRelationsCommand,
        LibrarySyncArgs,
    },
    client::KlmsClient,
    error::AppError,
    output::{self, CommandResult},
};

pub(super) fn local(command: &LibraryCommand) -> Result<CommandResult, AppError> {
    let mut corpus = crate::corpus::Corpus::open()?;
    match command {
        LibraryCommand::Status => library_status(&corpus),
        LibraryCommand::Search { query, list } => {
            let (fresh_through, source_complete) = corpus.coverage()?;
            let mut rows = corpus.search(query, list.limit.saturating_add(1))?;
            let truncated = rows.len() > list.limit;
            rows.truncate(list.limit);
            let human = rows
                .iter()
                .map(|row| format!("{}\t{}\t{}", row.reference, row.kind, row.title))
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.search",
                &rows,
                human,
                rows.len(),
                list.limit,
                !truncated,
                fresh_through,
                source_complete,
            )
        }
        LibraryCommand::Changes(list) => {
            let (fresh_through, source_complete) = corpus.coverage()?;
            let mut rows = corpus.changes(list.limit.saturating_add(1))?;
            let truncated = rows.len() > list.limit;
            rows.truncate(list.limit);
            let human = rows
                .iter()
                .map(|r| format!("{}\t{}\t{}", r.occurred_at, r.kind, r.subject_ref))
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.changes",
                &rows,
                human,
                rows.len(),
                list.limit,
                !truncated,
                fresh_through,
                source_complete,
            )
        }
        LibraryCommand::Activity(args) => {
            let mut rows =
                corpus.activity(args.subject.as_deref(), args.list.limit.saturating_add(1))?;
            let truncated = rows.len() > args.list.limit;
            rows.truncate(args.list.limit);
            let human = rows
                .iter()
                .map(|row| {
                    format!(
                        "{}\t{}\t{}\t{}",
                        row.created_at, row.actor, row.field, row.subject_ref
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.activity",
                &rows,
                human,
                rows.len(),
                args.list.limit,
                !truncated,
                None,
                None,
            )
        }
        LibraryCommand::Show { reference } => {
            let row = corpus.show(reference)?;
            let human = serde_json::to_string_pretty(&row)
                .map_err(|error| AppError::internal(error.to_string()))?;
            output::result("library.show", &row, human)
        }
        LibraryCommand::History { reference, list } => {
            let mut rows = corpus.history(reference, list.limit.saturating_add(1))?;
            let truncated = rows.len() > list.limit;
            rows.truncate(list.limit);
            let human = rows
                .iter()
                .map(|row| format!("{}\t{}\t{}", row.id, row.observed_at, row.digest))
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.history",
                &rows,
                human,
                rows.len(),
                list.limit,
                !truncated,
                None,
                None,
            )
        }
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
