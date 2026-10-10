use super::{
    ActivityEntry, ChangeEntry, ContentRecord, Corpus, HistoryEntry, LastSync, LibraryRef,
    LibraryStatus, SearchHit, object_store, schema,
};
use crate::error::AppError;
use rusqlite::{Connection, OptionalExtension, Params, Row, params, types::Value as Sql};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, io::Read, path::Path};

pub const ACTIVE_ASSERTION: &str =
    "NOT EXISTS (SELECT 1 FROM retractions x WHERE x.target_ref='assertion:'||a.id)";
pub const ACTIVE_RELATION: &str =
    "NOT EXISTS (SELECT 1 FROM retractions x WHERE x.target_ref='relation:'||r.id)";
/// The bytes digest of a representation's newest download, else its source digest (`?1` = id).
const REPRESENTATION_DIGEST: &str = "COALESCE(
  (SELECT sha256 FROM content_observations
    WHERE representation_id=?1 ORDER BY id DESC LIMIT 1),
  (SELECT digest FROM representation_observations
    WHERE representation_id=?1 ORDER BY id DESC LIMIT 1))";
pub const FIELDS: [&str; 5] = ["title", "filename", "summary", "note", "tag"];
/// Newest observation row of `table` for the outer row `outer` (via `column`).
fn latest(table: &str, column: &str, outer: &str) -> String {
    format!("(SELECT id FROM {table} WHERE {column}={outer} ORDER BY id DESC LIMIT 1)")
}

type ContentRow = (String, i64, Option<String>, String);

/// All rows of a query, mapped by `f`.
pub fn rows<T>(
    c: &Connection,
    sql: &str,
    p: impl Params,
    f: impl FnMut(&Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, AppError> {
    Ok(c.prepare(sql)?.query_map(p, f)?.collect::<Result<_, _>>()?)
}

/// The first row of a query, if any, mapped by `f`.
pub fn row<T>(
    c: &Connection,
    sql: &str,
    p: impl Params,
    f: impl FnOnce(&Row) -> rusqlite::Result<T>,
) -> Result<Option<T>, AppError> {
    Ok(c.query_row(sql, p, f).optional()?)
}

fn stored_json(text: &str, context: &str) -> Result<Value, AppError> {
    serde_json::from_str(text).map_err(|error| {
        AppError::corpus_corrupt(format!("invalid persisted JSON in {context}: {error}"))
    })
}

impl Corpus {
    pub fn status(&self) -> Result<LibraryStatus, AppError> {
        let c = &self.connection;
        let count =
            |sql: &str| Ok::<u64, AppError>(c.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64);
        let last_sync = row(
            c,
            "SELECT id,started_at,finished_at,status,source_complete,scope
               FROM sync_runs ORDER BY id DESC LIMIT 1",
            [],
            |r| {
                Ok(LastSync {
                    reference: format!("sync:{}", r.get::<_, i64>(0)?),
                    started_at: r.get(1)?,
                    finished_at: r.get(2)?,
                    status: match r.get::<_, String>(3)?.as_str() {
                        "running" => "unfinished".into(),
                        status => status.into(),
                    },
                    source_complete: r.get(4)?,
                    scope: r.get(5)?,
                })
            },
        )?;
        Ok(LibraryStatus {
            database_path: self.database.display().to_string(),
            object_store_path: self.objects.display().to_string(),
            schema_version: schema::VERSION,
            created: self.created,
            courses: count("SELECT COUNT(*) FROM courses")?,
            resources: count("SELECT COUNT(*) FROM resources")?,
            representations: count("SELECT COUNT(*) FROM representations")?,
            blobs: count("SELECT COUNT(*) FROM blobs")?,
            stored_bytes: count("SELECT COALESCE(SUM(byte_length),0) FROM blobs")?,
            last_sync,
            fresh_through: self.coverage()?.0,
        })
    }

    /// (newest complete global sync, whether the latest finished global sync saw the full source)
    pub fn coverage(&self) -> Result<(Option<i64>, Option<bool>), AppError> {
        let c = &self.connection;
        let fresh = c.query_row(
            "SELECT MAX(finished_at) FROM sync_runs
              WHERE scope='all' AND status='complete' AND source_complete=1",
            [],
            |r| r.get(0),
        )?;
        let complete = row(
            c,
            "SELECT source_complete FROM sync_runs
              WHERE scope='all' AND status!='running' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        Ok((fresh, complete))
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, AppError> {
        if query.trim().is_empty() {
            return Err(AppError::usage("search query must not be empty"));
        }
        let fts_query = format!("\"{}\"*", query.replace('"', "\"\""));
        rows(
            &self.connection,
            "SELECT subject_ref,kind,NULLIF(course,''),title,
                    snippet(search_documents,4,'[',']',' … ',20),
                    EXISTS(SELECT 1 FROM content_observations c
                             JOIN representations p ON p.id=c.representation_id
                             JOIN resources r ON r.id=p.resource_id
                            WHERE 'representation:'||p.id=subject_ref OR r.ref=subject_ref)
               FROM search_documents WHERE search_documents MATCH ?1 LIMIT ?2",
            params![fts_query, limit as i64],
            |r| {
                Ok(SearchHit {
                    reference: r.get(0)?,
                    kind: r.get(1)?,
                    course_ref: r.get(2)?,
                    title: r.get(3)?,
                    snippet: r.get(4)?,
                    has_content: r.get(5)?,
                })
            },
        )
    }

    pub fn changes(&self, limit: usize) -> Result<Vec<ChangeEntry>, AppError> {
        let found = rows(
            &self.connection,
            "SELECT id,occurred_at,kind,subject_ref,before_ref,after_ref,details_json
               FROM remote_changes ORDER BY id DESC LIMIT ?1",
            [limit as i64],
            |r| r.try_into(),
        )?;
        found
            .into_iter()
            .map(
                |(id, occurred_at, kind, subject_ref, before_ref, after_ref, details)| {
                    let details: String = details;
                    Ok(ChangeEntry {
                        id,
                        occurred_at,
                        kind,
                        subject_ref,
                        before_ref,
                        after_ref,
                        details: stored_json(
                            &details,
                            &format!("remote_changes:{id} details_json"),
                        )?,
                    })
                },
            )
            .collect()
    }

    pub fn activity(
        &self,
        subject: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ActivityEntry>, AppError> {
        let subject = subject
            .map(|s| s.parse::<LibraryRef>().map(|r| r.to_string()))
            .transpose()?;
        let sql = format!(
            "SELECT 'assertion:'||a.id,a.subject_ref,a.field,a.value,a.actor,
                    a.revision,a.created_at,NOT ({ACTIVE_ASSERTION}) FROM assertions a
              WHERE (?1 IS NULL OR a.subject_ref=?1) UNION ALL
             SELECT 'relation:'||r.id,r.left_ref,'relation:'||r.kind,
                    r.right_ref,r.actor,0,r.created_at,NOT ({ACTIVE_RELATION}) FROM relations r
              WHERE (?1 IS NULL OR r.left_ref=?1 OR r.right_ref=?1) ORDER BY created_at DESC
              LIMIT ?2"
        );
        rows(
            &self.connection,
            &sql,
            params![subject, limit as i64],
            |r| {
                Ok(ActivityEntry {
                    reference: r.get(0)?,
                    subject_ref: r.get(1)?,
                    field: r.get(2)?,
                    value: r.get(3)?,
                    actor: r.get(4)?,
                    revision: r.get(5)?,
                    created_at: r.get(6)?,
                    retracted: r.get(7)?,
                })
            },
        )
    }

    pub fn show(&self, value: &str) -> Result<Value, AppError> {
        let c = &self.connection;
        match value.parse::<LibraryRef>()? {
            LibraryRef::Course(_) => detail(
                c,
                &format!(
                    "SELECT json_object('kind','course','remote_state',c.remote_state,'source',
                        json_object('title',o.title,'code',o.code,'term',o.term,'url',o.url,
                          'first_seen',c.first_seen,'last_seen',c.last_seen,
                          'not_listed_since',c.not_listed_since)), o.digest
                       FROM courses c JOIN course_observations o ON o.id={}
                      WHERE c.ref=?1",
                    latest("course_observations", "course_id", "c.id")
                ),
                value,
                value,
            ),
            LibraryRef::Resource(_) => {
                let mut shown = detail(
                    c,
                    &format!(
                        "SELECT json_object('kind',r.kind,'course_ref',c.ref,'remote_state',r.remote_state,
                            'source',json_object('title',o.title,'url',o.url,'week',o.week,
                              'section',o.section,'text',o.text,'observed_at',o.observed_at,
                              'complete',json(CASE o.complete WHEN 0 THEN 'false' ELSE 'true' END))),
                            o.digest
                           FROM resources r JOIN courses c ON c.id=r.course_id
                           JOIN resource_observations o ON o.id={}
                          WHERE r.ref=?1",
                        latest("resource_observations", "resource_id", "r.id")
                    ),
                    value,
                    value,
                )?;
                let sql = format!(
                    "SELECT p.id,p.url,p.kind,o.filename,
                            EXISTS(SELECT 1 FROM content_observations c WHERE c.representation_id=p.id)
                       FROM representations p
                       LEFT JOIN representation_observations o ON o.id={}
                       JOIN resources r ON r.id=p.resource_id
                      WHERE r.ref=?1 ORDER BY p.id",
                    latest("representation_observations", "representation_id", "p.id")
                );
                shown["representations"] = json!(rows(c, &sql, [value], |r| {
                    Ok(json!({
                        "ref": format!("representation:{}", r.get::<_, i64>(0)?),
                        "url": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                        "filename": r.get::<_, Option<String>>(3)?,
                        "has_content": r.get::<_, bool>(4)?
                    }))
                })?);
                Ok(shown)
            }
            LibraryRef::Representation(id) => detail(
                c,
                &format!(
                    "SELECT json_object('resource_ref',r.ref,'remote_state',p.remote_state,
                        'source',json_object('url',p.url,'filename',o.filename,
                          'mime',p.observed_mime,'observed_at',o.observed_at),
                        'content',(SELECT json_object('sha256_ref','sha256:'||sha256,
                            'byte_length',byte_length,'mime',mime,'observed_at',observed_at)
                           FROM content_observations
                          WHERE representation_id=p.id ORDER BY id DESC LIMIT 1)),
                        {REPRESENTATION_DIGEST}
                       FROM representations p JOIN resources r ON r.id=p.resource_id
                       LEFT JOIN representation_observations o ON o.id={}
                      WHERE p.id=?1",
                    latest("representation_observations", "representation_id", "p.id")
                ),
                id,
                &format!("representation:{id}"),
            ),
            LibraryRef::Sha256(hash) => {
                let (byte_length, mime, stored_at): (i64, Option<String>, i64) = row(
                    c,
                    "SELECT byte_length,mime,stored_at FROM blobs WHERE sha256=?1",
                    [&hash],
                    |r| r.try_into(),
                )?
                .ok_or_else(|| AppError::not_found("blob not found"))?;
                let representations = rows(
                    c,
                    "SELECT DISTINCT 'representation:'||representation_id
                       FROM content_observations WHERE sha256=?1 ORDER BY representation_id",
                    [&hash],
                    |r| r.get::<_, String>(0),
                )?;
                Ok(json!({
                    "ref": format!("sha256:{hash}"), "byte_length": byte_length, "mime": mime,
                    "stored_at": stored_at, "representations": representations
                }))
            }
            _ => Err(AppError::usage("this reference is not showable")),
        }
    }

    pub fn history(&self, reference: &str, limit: usize) -> Result<Vec<HistoryEntry>, AppError> {
        let c = &self.connection;
        let reference = reference.parse::<LibraryRef>()?.to_string();
        let keys = rows(
            c,
            "SELECT id,observed_at,kind FROM subject_history
              WHERE subject_ref=?1 ORDER BY observed_at DESC,kind DESC,id DESC LIMIT ?2",
            params![reference, limit as i64],
            |r| r.try_into(),
        )?;
        keys.into_iter()
            .map(|(id, observed_at, kind): (i64, i64, String)| {
                let sql = match kind.as_str() {
                    "course_source" => {
                        "SELECT digest,json_object('title',title,'code',code,'term',term,'url',url,
                                'sync_ref','sync:'||sync_run_id) FROM course_observations WHERE id=?1"
                    }
                    "resource_source" => {
                        "SELECT digest,source_json FROM resource_observations WHERE id=?1"
                    }
                    "representation_source" => {
                        "SELECT o.digest,json_object('filename',o.filename,'url',p.url,
                                'sync_ref','sync:'||o.sync_run_id) FROM representation_observations o
                           JOIN representations p ON p.id=o.representation_id WHERE o.id=?1"
                    }
                    "verified_content" => {
                        "SELECT c.sha256,json_object('sha256_ref','sha256:'||c.sha256,'url',p.url,
                                'etag',c.etag,'last_modified',c.last_modified,
                                'byte_length',c.byte_length,'mime',c.mime,'sync_ref','sync:'||c.sync_run_id)
                           FROM content_observations c
                           JOIN representations p ON p.id=c.representation_id WHERE c.id=?1"
                    }
                    _ => return Err(AppError::corpus_corrupt("unknown history entry kind")),
                };
                let (digest, source): (String, String) = c.query_row(sql, [id], |r| r.try_into())?;
                Ok(HistoryEntry {
                    id,
                    observed_at,
                    source: stored_json(&source, &format!("{kind}:{id} source"))?,
                    kind,
                    digest,
                })
            })
            .collect()
    }

    pub fn content(&self, reference: &str) -> Result<ContentRecord, AppError> {
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

    pub fn export(&self, reference: &str, destination: &Path) -> Result<u64, AppError> {
        let content = self.content(reference)?;
        let hash = content.reference.trim_start_matches("sha256:");
        object_store::export(&self.objects, hash, destination)
    }

    pub fn preview(&self, reference: &str, max: usize) -> Result<super::ContentPreview, AppError> {
        let record = self.content(reference)?;
        let file = std::fs::File::open(&record.path).map_err(|e| {
            AppError::content_unavailable(format!("cannot read stored content: {e}"))
        })?;
        let mut bytes = Vec::new();
        file.take((max as u64).saturating_add(1))
            .read_to_end(&mut bytes)
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

pub struct Assertion {
    pub id: i64,
    pub value: String,
    pub actor: String,
    pub revision: i64,
    pub based_on: Option<String>,
}

/// Active assertion per field for a subject; the highest revision wins.
pub fn effective_fields(
    c: &Connection,
    subject: &str,
) -> Result<HashMap<String, Assertion>, AppError> {
    let sql = format!(
        "SELECT a.field,a.id,a.value,a.actor,a.revision,a.based_on FROM assertions a
          WHERE a.subject_ref=?1 AND {ACTIVE_ASSERTION} ORDER BY a.revision"
    );
    let found = rows(c, &sql, [subject], |r| {
        let assertion = Assertion {
            id: r.get(1)?,
            value: r.get(2)?,
            actor: r.get(3)?,
            revision: r.get(4)?,
            based_on: r.get(5)?,
        };
        Ok((r.get::<_, String>(0)?, assertion))
    })?;
    Ok(found.into_iter().collect())
}

pub fn current_digest(c: &Connection, reference: &LibraryRef) -> Result<Option<String>, AppError> {
    let (sql, key) = match reference {
        LibraryRef::Course(_) => (
            "SELECT o.digest FROM course_observations o JOIN courses c ON c.id=o.course_id
              WHERE c.ref=?1 ORDER BY o.id DESC LIMIT 1",
            Sql::Text(reference.to_string()),
        ),
        LibraryRef::Resource(_) => (
            "SELECT o.digest FROM resource_observations o JOIN resources r ON r.id=o.resource_id
              WHERE r.ref=?1 ORDER BY o.id DESC LIMIT 1",
            Sql::Text(reference.to_string()),
        ),
        LibraryRef::Representation(id) => {
            let sql = format!("SELECT {REPRESENTATION_DIGEST}");
            return Ok(row(c, &sql, [id], |r| r.get(0))?.flatten());
        }
        LibraryRef::Sha256(hash) => (
            "SELECT sha256 FROM blobs WHERE sha256=?1",
            Sql::Text(hash.clone()),
        ),
        _ => return Ok(None),
    };
    Ok(row(c, sql, [key], |r| r.get::<_, Option<String>>(0))?.flatten())
}

/// Rebuild the full-text search document for a course, resource or representation.
pub fn refresh_subject(c: &Connection, subject: &str) -> Result<(), AppError> {
    c.execute(
        "DELETE FROM search_documents WHERE subject_ref=?1",
        [subject],
    )?;
    let (sql, key) = match subject.parse::<LibraryRef>()? {
        LibraryRef::Course(_) => (
            "SELECT 'course','',o.title,o.title,COALESCE(o.code,'')||' '||COALESCE(o.term,'')
               FROM course_observations o JOIN courses c ON c.id=o.course_id
              WHERE c.ref=?1 ORDER BY o.id DESC LIMIT 1",
            Sql::Text(subject.into()),
        ),
        LibraryRef::Resource(_) => (
            "SELECT r.kind,c.ref,o.title,COALESCE(o.text,''),'' FROM resource_observations o
               JOIN resources r ON r.id=o.resource_id JOIN courses c ON c.id=r.course_id
              WHERE r.ref=?1 ORDER BY o.id DESC LIMIT 1",
            Sql::Text(subject.into()),
        ),
        LibraryRef::Representation(id) => (
            "SELECT p.kind,c.ref,COALESCE(o.filename,p.url),p.url,''
               FROM representations p JOIN resources r ON r.id=p.resource_id
               JOIN courses c ON c.id=r.course_id
               LEFT JOIN representation_observations o ON o.id=(
                 SELECT id FROM representation_observations
                  WHERE representation_id=p.id ORDER BY id DESC LIMIT 1)
              WHERE p.id=?1 AND NOT (
                r.kind='notice' AND p.kind='link' AND p.remote_state='not_observed')",
            Sql::Integer(id),
        ),
        _ => return Ok(()),
    };
    let source: Option<(String, String, String, String, String)> =
        row(c, sql, [key], |r| r.try_into())?;
    let Some((kind, course, mut title, mut body, mut tags)) = source else {
        return Ok(());
    };
    let mut effective = effective_fields(c, subject)?;
    if let Some(a) = effective.remove("title") {
        title = a.value;
    }
    for field in ["filename", "summary", "note"] {
        if let Some(a) = effective.remove(field) {
            body = format!("{body} {}", a.value);
        }
    }
    if let Some(a) = effective.remove("tag") {
        tags = format!("{tags} {}", a.value);
    }
    c.execute(
        "INSERT INTO search_documents(subject_ref,kind,course,title,body,tags)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![subject, kind, course, title, body, tags],
    )?;
    Ok(())
}

/// A detail object read from `sql` (columns: detail JSON, source digest), completed
/// with its ref, effective curation and relations.
fn detail(
    c: &Connection,
    sql: &str,
    key: impl rusqlite::ToSql,
    reference: &str,
) -> Result<Value, AppError> {
    let found: Option<(String, Option<String>)> = row(c, sql, [key], |r| r.try_into())?;
    let (json, digest) = found.ok_or_else(|| {
        AppError::not_found(if reference.starts_with("representation:") {
            "representation not found"
        } else {
            "library item not found"
        })
    })?;
    let mut shown = stored_json(&json, "library detail")?;
    let source = &shown["source"];
    let effective = effective_object(
        c,
        reference,
        digest.as_deref(),
        source["title"].as_str(),
        source["filename"].as_str(),
    )?;
    shown["ref"] = reference.into();
    shown["effective"] = effective;
    shown["relations"] = json!(relations(c, reference)?);
    Ok(shown)
}

/// Curated fields over the source values, with provenance and summary staleness.
fn effective_object(
    c: &Connection,
    subject: &str,
    digest: Option<&str>,
    source_title: Option<&str>,
    source_filename: Option<&str>,
) -> Result<Value, AppError> {
    let mut assertions = effective_fields(c, subject)?;
    let stale = assertions
        .get("summary")
        .is_some_and(|a| a.based_on.as_deref() != digest);
    let (mut object, mut provenance) = (Map::new(), Map::new());
    for field in FIELDS {
        let value = match assertions.remove(field) {
            Some(a) => {
                provenance.insert(
                    field.into(),
                    json!({
                        "assertion_ref": format!("assertion:{}", a.id),
                        "actor": a.actor, "revision": a.revision, "based_on": a.based_on
                    }),
                );
                Some(a.value)
            }
            None => match field {
                "title" => source_title.map(str::to_owned),
                "filename" => source_filename.map(str::to_owned),
                _ => None,
            },
        };
        object.insert(field.into(), json!(value));
    }
    object.insert("summary_stale".into(), stale.into());
    object.insert("_provenance".into(), Value::Object(provenance));
    Ok(Value::Object(object))
}

fn relations(c: &Connection, subject: &str) -> Result<Vec<String>, AppError> {
    rows(
        c,
        &format!(
            "SELECT 'relation:'||r.id FROM relations r
              WHERE (r.left_ref=?1 OR r.right_ref=?1) AND {ACTIVE_RELATION} ORDER BY r.id"
        ),
        [subject],
        |r| r.get(0),
    )
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
