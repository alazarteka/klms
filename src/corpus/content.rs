use super::{
    ContentRecord, Corpus, LibraryRef, object_store,
    query::{latest, row, rows},
};
use crate::error::AppError;
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;

type ContentRow = (String, i64, Option<String>, String);

impl Corpus {
    pub(crate) fn content(&self, reference: &str) -> Result<ContentRecord, AppError> {
        let c = &self.connection;
        let found = match reference.parse::<LibraryRef>()? {
            LibraryRef::Sha256(hash) => {
                let sql = format!(
                    "SELECT b.sha256,b.byte_length,b.mime,COALESCE(o.filename,b.sha256) FROM blobs b
                       LEFT JOIN content_observations c ON c.sha256=b.sha256
                       LEFT JOIN representation_observations o ON o.id={}
                      WHERE b.sha256=?1 LIMIT 1",
                    latest("representation_observations", "representation_id", "c.representation_id")
                );
                row(c, &sql, [hash], |r| r.try_into())?
            }
            LibraryRef::Representation(id) => latest_content(c, id)?,
            LibraryRef::Resource(resource) => {
                let ids = rows(
                    c,
                    "SELECT p.id FROM representations p JOIN resources r ON r.id=p.resource_id
                      WHERE r.ref=?1 AND EXISTS(SELECT 1 FROM content_observations c
                                                 WHERE c.representation_id=p.id)
                      ORDER BY p.id",
                    [&resource],
                    |r| r.get::<_, i64>(0),
                )?;
                if ids.len() > 1 {
                    let references: Vec<_> = ids
                        .iter()
                        .map(|id| format!("representation:{id}"))
                        .collect();
                    return Err(AppError::content_unavailable(
                        "multiple downloaded representations are available for this resource",
                    )
                    .with_hint(format!(
                        "Choose one representation reference for `klms library content REF` or `klms library export REF --out PATH`: {}.",
                        references.join(", ")
                    ))
                    .with_details(json!({"representations": references})));
                }
                ids.first()
                    .map(|id| latest_content(c, *id))
                    .transpose()?
                    .flatten()
            }
            _ => {
                return Err(AppError::content_unavailable(
                    "reference has no stored content",
                ));
            }
        };
        let Some((sha256, byte_length, mime, filename)) = found else {
            return Err(missing_content(c, reference)?);
        };
        Ok(ContentRecord {
            path: object_store::object_path(&self.objects, &sha256)?,
            reference: format!("sha256:{sha256}"),
            byte_length: byte_length as u64,
            mime,
            filename,
        })
    }

    pub(crate) fn export(&self, reference: &str, destination: &Path) -> Result<u64, AppError> {
        let content = self.content(reference)?;
        let hash = content.reference.trim_start_matches("sha256:");
        object_store::export(&self.objects, hash, destination)
    }

    pub(crate) fn preview(
        &self,
        reference: &str,
        max: usize,
    ) -> Result<super::ContentPreview, AppError> {
        let record = self.content(reference)?;
        let file = std::fs::File::open(&record.path).map_err(|e| {
            AppError::content_unavailable(format!("cannot read stored content: {e}"))
        })?;
        let mut bytes = crate::http::read_prefix(file, max as u64)
            .map_err(|e| AppError::library_io(format!("cannot read stored content: {e}")))?;
        let truncated = bytes.len() > max;
        bytes.truncate(max);
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => Some(text.to_owned()),
            // A cap that splits a multi-byte character still yields the valid prefix.
            Err(error) if truncated && error.error_len().is_none() => {
                Some(String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned())
            }
            Err(_) => None,
        };
        Ok(super::ContentPreview {
            reference: record.reference,
            byte_length: record.byte_length,
            mime: record.mime,
            filename: record.filename,
            text,
            truncated,
        })
    }
}

/// Explain why `reference` has no downloaded bytes, with actionable hints.
fn missing_content(c: &Connection, reference: &str) -> Result<AppError, AppError> {
    let none = || AppError::content_unavailable("no stored content for reference");
    let (resource, representation) = match reference.parse::<LibraryRef>()? {
        LibraryRef::Resource(resource) => (resource, None),
        LibraryRef::Representation(id) => {
            let found: Option<(String, String, String)> = row(
                c,
                "SELECT r.ref,p.kind,p.remote_state FROM representations p
                   JOIN resources r ON r.id=p.resource_id WHERE p.id=?1",
                [id],
                |r| r.try_into(),
            )?;
            match found {
                Some((resource, kind, state)) => (resource, Some((kind, state))),
                None => return Ok(none()),
            }
        }
        _ => return Ok(none()),
    };
    let sql = format!(
        "SELECT r.kind,c.ref,COALESCE(o.text,''),r.remote_state,
                EXISTS(SELECT 1 FROM representations p WHERE p.resource_id=r.id
                       AND p.kind='file' AND p.remote_state='present')
           FROM resources r JOIN courses c ON c.id=r.course_id
           LEFT JOIN resource_observations o ON o.id={}
          WHERE r.ref=?1",
        latest("resource_observations", "resource_id", "r.id")
    );
    let context: Option<(String, String, String, String, bool)> =
        row(c, &sql, [&resource], |r| r.try_into())?;
    let Some((kind, course, text, state, has_present_file)) = context else {
        return Ok(none());
    };
    let has_notice_text = kind == "notice" && !text.is_empty();
    let unavailable =
        |message: &str, hint: String| AppError::content_unavailable(message).with_hint(hint);
    // A representation is its own subject: never substitute its parent's text
    // or a sibling attachment for its bytes, even when those are available.
    if representation
        .as_ref()
        .is_some_and(|(kind, _)| kind == "link")
    {
        let mut hint = format!(
            "Inspect its recorded URL with `klms library show {reference}` (JSON: data.source.url). Content and export do not follow links."
        );
        if has_notice_text {
            hint.push_str(&format!(
                " Read the parent notice's stored text with `klms library show {resource}` (JSON: data.source.text)."
            ));
        }
        return Ok(unavailable(
            "this representation is stored as a non-file link, not downloaded file content",
            hint,
        ));
    }
    if representation.is_none() && kind == "notice" && state == "present" && has_present_file {
        // Only offer current file candidates, not links or historical missing
        // attachments. Stored bytes have already taken precedence in content().
        let candidates = rows(
            c,
            &format!(
                "SELECT p.id,o.filename FROM representations p
                   JOIN resources r ON r.id=p.resource_id
                   LEFT JOIN representation_observations o ON o.id={}
                  WHERE r.ref=?1 AND p.kind='file' AND p.remote_state='present'
                  ORDER BY p.id LIMIT 21",
                latest("representation_observations", "representation_id", "p.id")
            ),
            [&resource],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )?;
        let (mut references, mut labels) = (Vec::new(), Vec::new());
        for (id, filename) in candidates.iter().take(20) {
            let reference = format!("representation:{id}");
            labels.push(match filename {
                Some(filename) => format!("{reference} ({filename:?})"),
                None => reference.clone(),
            });
            references.push(reference);
        }
        let mut hint = if has_notice_text {
            format!(
                "Read stored notice text with `klms library show {resource}` (JSON: data.source.text). "
            )
        } else {
            format!("Inspect notice metadata with `klms library show {resource}`. ")
        };
        hint.push_str(&format!(
            "Recorded file attachments without downloaded bytes: {}. ",
            labels.join(", ")
        ));
        if candidates.len() > 20 {
            hint.push_str("Only the first 20 candidates are listed; show the notice for the remaining metadata. ");
        }
        hint.push_str(&format!(
            "To attempt downloading available files, run `klms library sync --course {course} --notices --download changed`, then use an attachment reference with content or export. These commands operate on downloaded file bytes."
        ));
        return Ok(
            unavailable("notice attachments have not been downloaded", hint)
                .with_details(json!({"representations": references})),
        );
    }
    if representation.is_none() && has_notice_text {
        return Ok(unavailable(
            "notice text is stored as source metadata, not downloaded file bytes",
            format!(
                "Read it with `klms library show {resource}` (JSON: data.source.text). Content and export operate on downloaded file bytes."
            ),
        ));
    }
    if representation
        .as_ref()
        .is_some_and(|(_, state)| state == "not_observed")
    {
        return Ok(unavailable(
            "no stored file bytes; this file is recorded as not observed",
            format!(
                "Inspect its parent resource and observation state with `klms library show {resource}` before deciding whether to refresh it."
            ),
        ));
    }
    let has_file_candidate = representation
        .as_ref()
        .map_or(has_present_file, |(kind, state)| {
            kind == "file" && state == "present"
        });
    if state == "present" && has_file_candidate {
        let notice_flag = if kind == "notice" { " --notices" } else { "" };
        return Ok(unavailable(
            "metadata-only: file bytes have not been downloaded",
            format!(
                "To attempt downloading available files, run `klms library sync --course {course}{notice_flag} --download changed`, then retry. Inspect metadata with `klms library show {reference}`."
            ),
        ));
    }
    Ok(unavailable(
        "no downloaded file bytes are stored for this reference",
        format!(
            "Inspect stored metadata and observation state with `klms library show {resource}`. The local record does not establish whether files are available remotely."
        ),
    ))
}

fn latest_content(c: &Connection, representation_id: i64) -> Result<Option<ContentRow>, AppError> {
    let sql = format!(
        "SELECT c.sha256,c.byte_length,c.mime,COALESCE(o.filename,c.sha256)
           FROM content_observations c LEFT JOIN representation_observations o ON o.id={}
          WHERE c.representation_id=?1 ORDER BY c.id DESC LIMIT 1",
        latest(
            "representation_observations",
            "representation_id",
            "c.representation_id"
        )
    );
    row(c, &sql, [representation_id], |r| r.try_into())
}
