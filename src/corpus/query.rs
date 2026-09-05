use super::{
    ActivityEntry, ChangeEntry, ContentRecord, Corpus, HistoryEntry, LastSync, LibraryRef,
    LibraryStatus, SearchHit, object_store, schema,
};
use crate::error::AppError;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;
pub const ACTIVE_ASSERTION: &str =
    "NOT EXISTS (SELECT 1 FROM retractions x WHERE x.target_ref='assertion:'||a.id)";
pub const ACTIVE_RELATION: &str =
    "NOT EXISTS (SELECT 1 FROM retractions x WHERE x.target_ref='relation:'||r.id)";
#[derive(Clone, Debug)]
pub struct Assertion {
    pub id: i64,
    pub value: String,
    pub actor: String,
    pub revision: i64,
    pub based_on: Option<String>,
}
struct ContentRow {
    sha256: String,
    byte_length: i64,
    mime: Option<String>,
    filename: String,
}

struct CourseRow {
    remote_state: String,
    source: CourseSource,
    digest: String,
}
#[derive(Serialize)]
struct CourseSource {
    title: String,
    code: Option<String>,
    term: Option<String>,
    url: String,
    first_seen: i64,
    last_seen: i64,
    not_listed_since: Option<i64>,
}
#[derive(Serialize)]
struct CourseDetail<'a> {
    #[serde(rename = "ref")]
    reference: &'a str,
    kind: &'static str,
    remote_state: String,
    source: CourseSource,
    effective: Value,
    relations: Vec<String>,
}
struct ResourceRow {
    kind: String,
    course_ref: String,
    remote_state: String,
    source: ResourceSource,
    digest: String,
}
#[derive(Serialize)]
struct ResourceSource {
    title: String,
    url: Option<String>,
    week: Option<i64>,
    section: Option<String>,
    text: Option<String>,
    complete: bool,
    observed_at: i64,
}
#[derive(Serialize)]
struct ResourceDetail<'a> {
    #[serde(rename = "ref")]
    reference: &'a str,
    kind: String,
    course_ref: String,
    remote_state: String,
    source: ResourceSource,
    effective: Value,
    representations: Vec<RepresentationSummary>,
    relations: Vec<String>,
}
#[derive(Serialize)]
struct RepresentationSummary {
    #[serde(rename = "ref")]
    reference: String,
    url: String,
    kind: String,
    filename: Option<String>,
    has_content: bool,
}
struct RepresentationRow {
    resource_ref: String,
    remote_state: String,
    source: RepresentationSource,
}
#[derive(Serialize)]
struct RepresentationSource {
    url: String,
    filename: Option<String>,
    mime: Option<String>,
    observed_at: Option<i64>,
}
#[derive(Serialize)]
struct RepresentationContent {
    sha256_ref: String,
    byte_length: i64,
    mime: Option<String>,
    observed_at: i64,
}
#[derive(Serialize)]
struct RepresentationDetail {
    #[serde(rename = "ref")]
    reference: String,
    resource_ref: String,
    remote_state: String,
    source: RepresentationSource,
    content: Option<RepresentationContent>,
    effective: Value,
    relations: Vec<String>,
}
fn detail_value(detail: impl Serialize) -> Result<Value, AppError> {
    serde_json::to_value(detail)
        .map_err(|error| AppError::internal(format!("failed to encode library detail: {error}")))
}
impl Corpus {
    pub fn status(&self) -> Result<LibraryStatus, AppError> {
        let connection = &self.storage.connection;
        let count = |table: &str| {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .map(|value| value as u64)
                .map_err(AppError::from)
        };
        let stored_bytes = connection.query_row(
            "SELECT COALESCE(SUM(byte_length),0) FROM blobs",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let last_sync = connection
            .query_row(
                "SELECT id,started_at,finished_at,status,source_complete,scope
                   FROM sync_runs
                  ORDER BY id DESC
                  LIMIT 1",
                [],
                |row| {
                    Ok(LastSync {
                        reference: format!("sync:{}", row.get::<_, i64>(0)?),
                        started_at: row.get(1)?,
                        finished_at: row.get(2)?,
                        status: match row.get::<_, String>(3)?.as_str() {
                            "running" => "unfinished".into(),
                            status => status.into(),
                        },
                        source_complete: row.get::<_, i64>(4)? != 0,
                        scope: row.get(5)?,
                    })
                },
            )
            .optional()?;
        let (fresh_through, _) = coverage(connection)?;
        Ok(LibraryStatus {
            database_path: self.storage.paths.database.display().to_string(),
            object_store_path: self.storage.paths.objects.display().to_string(),
            schema_version: schema::VERSION,
            created: self.storage.created,
            courses: count("courses")?,
            resources: count("resources")?,
            representations: count("representations")?,
            blobs: count("blobs")?,
            stored_bytes,
            last_sync,
            fresh_through,
        })
    }
    pub fn coverage(&self) -> Result<(Option<i64>, Option<bool>), AppError> {
        coverage(&self.storage.connection)
    }
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, AppError> {
        if query.trim().is_empty() {
            return Err(AppError::usage("search query must not be empty"));
        }
        let fts_query = format!("\"{}\"*", query.replace('"', "\"\""));
        let mut statement = self.storage.connection.prepare(
            "SELECT subject_ref,kind,NULLIF(course,''),title,
                        snippet(search_documents,4,'[',']',' … ',20), EXISTS( SELECT 1
                            FROM content_observations c
                            JOIN representations p ON p.id=c.representation_id
                            JOIN resources r ON r.id=p.resource_id
                           WHERE 'representation:'||p.id=subject_ref OR r.ref=subject_ref )
                   FROM search_documents WHERE search_documents MATCH ?1
                  LIMIT ?2",
        )?;
        statement
            .query_map(params![fts_query, limit as i64], |row| {
                Ok(SearchHit {
                    reference: row.get(0)?,
                    kind: row.get(1)?,
                    course_ref: row.get(2)?,
                    title: row.get(3)?,
                    snippet: row.get(4)?,
                    has_content: row.get::<_, i64>(5)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(AppError::from)
    }
    pub fn changes(&self, limit: usize) -> Result<Vec<ChangeEntry>, AppError> {
        let mut statement = self.storage.connection.prepare(
            "SELECT id,occurred_at,kind,subject_ref,before_ref,after_ref,details_json
               FROM remote_changes ORDER BY id DESC
              LIMIT ?1",
        )?;
        let mut rows = statement.query([limit as i64])?;
        let mut changes = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let details: String = row.get(6)?;
            changes.push(ChangeEntry {
                id,
                occurred_at: row.get(1)?,
                kind: row.get(2)?,
                subject_ref: row.get(3)?,
                before_ref: row.get(4)?,
                after_ref: row.get(5)?,
                details: stored_json(&details, &format!("remote_changes:{id} details_json"))?,
            });
        }
        Ok(changes)
    }

    pub fn activity(
        &self,
        subject: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ActivityEntry>, AppError> {
        let subject = subject
            .map(str::parse::<LibraryRef>)
            .transpose()?
            .map(|reference| reference.to_string());
        let sql = format!(
            "SELECT 'assertion:'||a.id,a.subject_ref,a.field,a.value,a.actor,
                    a.revision,a.created_at,NOT ({ACTIVE_ASSERTION}) FROM assertions a
              WHERE (?1 IS NULL OR a.subject_ref=?1) UNION ALL
             SELECT 'relation:'||r.id,r.left_ref,'relation:'||r.kind,
                    r.right_ref,r.actor,0,r.created_at,NOT ({ACTIVE_RELATION}) FROM relations r
              WHERE (?1 IS NULL OR r.left_ref=?1 OR r.right_ref=?1) ORDER BY created_at DESC
              LIMIT ?2"
        );
        let mut statement = self.storage.connection.prepare(&sql)?;
        statement
            .query_map(params![subject, limit as i64], |row| {
                Ok(ActivityEntry {
                    reference: row.get(0)?,
                    subject_ref: row.get(1)?,
                    field: row.get(2)?,
                    value: row.get(3)?,
                    actor: row.get(4)?,
                    revision: row.get(5)?,
                    created_at: row.get(6)?,
                    retracted: row.get::<_, i64>(7)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(AppError::from)
    }
    pub fn show(&self, value: &str) -> Result<Value, AppError> {
        match value.parse::<LibraryRef>()? {
            LibraryRef::Course(_) => self.show_course(value),
            LibraryRef::Resource(_) => self.show_resource(value),
            LibraryRef::Representation(id) => self.show_representation(id),
            LibraryRef::Sha256(hash) => self.show_blob(&hash),
            _ => Err(AppError::usage("this reference is not showable")),
        }
    }
    fn show_course(&self, reference: &str) -> Result<Value, AppError> {
        let row = self
            .storage
            .connection
            .query_row(
                "SELECT c.remote_state,c.first_seen,c.last_seen,c.not_listed_since,
                    o.title,o.code,o.term,o.url,o.digest FROM courses c
               JOIN course_observations o ON o.id=( SELECT id FROM course_observations
                  WHERE course_id=c.id ORDER BY id DESC LIMIT 1 )
              WHERE c.ref=?1",
                [reference],
                |row| {
                    Ok(CourseRow {
                        remote_state: row.get(0)?,
                        source: CourseSource {
                            first_seen: row.get(1)?,
                            last_seen: row.get(2)?,
                            not_listed_since: row.get(3)?,
                            title: row.get(4)?,
                            code: row.get(5)?,
                            term: row.get(6)?,
                            url: row.get(7)?,
                        },
                        digest: row.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("library item not found"))?;
        let effective = effective_object(
            &self.storage.connection,
            reference,
            Some(&row.digest),
            Some(&row.source.title),
            None,
        )?;
        detail_value(CourseDetail {
            reference,
            kind: "course",
            remote_state: row.remote_state,
            source: row.source,
            effective,
            relations: relations(&self.storage.connection, reference)?,
        })
    }
    fn show_resource(&self, reference: &str) -> Result<Value, AppError> {
        let row = self
            .storage
            .connection
            .query_row(
                "SELECT r.kind,c.ref,r.remote_state,o.title,o.url,o.week,o.section,
                    o.text,o.complete,o.observed_at,o.digest FROM resources r
               JOIN courses c ON c.id=r.course_id JOIN resource_observations o ON o.id=(
                 SELECT id FROM resource_observations
                  WHERE resource_id=r.id ORDER BY id DESC LIMIT 1 )
              WHERE r.ref=?1",
                [reference],
                |row| {
                    Ok(ResourceRow {
                        kind: row.get(0)?,
                        course_ref: row.get(1)?,
                        remote_state: row.get(2)?,
                        source: ResourceSource {
                            title: row.get(3)?,
                            url: row.get(4)?,
                            week: row.get(5)?,
                            section: row.get(6)?,
                            text: row.get(7)?,
                            complete: row.get::<_, i64>(8)? != 0,
                            observed_at: row.get(9)?,
                        },
                        digest: row.get(10)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("library item not found"))?;
        let mut statement = self.storage.connection.prepare(
            "SELECT p.id,p.url,p.kind,o.filename, EXISTS(SELECT 1 FROM content_observations c
                            WHERE c.representation_id=p.id) FROM representations p
               LEFT JOIN representation_observations o ON o.id=(
                 SELECT id FROM representation_observations
                  WHERE representation_id=p.id ORDER BY id DESC LIMIT 1 )
               JOIN resources r ON r.id=p.resource_id
              WHERE r.ref=?1 ORDER BY p.id",
        )?;
        let representations = statement
            .query_map([reference], |item| {
                Ok(RepresentationSummary {
                    reference: format!("representation:{}", item.get::<_, i64>(0)?),
                    url: item.get(1)?,
                    kind: item.get(2)?,
                    filename: item.get(3)?,
                    has_content: item.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let effective = effective_object(
            &self.storage.connection,
            reference,
            Some(&row.digest),
            Some(&row.source.title),
            None,
        )?;
        detail_value(ResourceDetail {
            reference,
            kind: row.kind,
            course_ref: row.course_ref,
            remote_state: row.remote_state,
            source: row.source,
            effective,
            representations,
            relations: relations(&self.storage.connection, reference)?,
        })
    }
    fn show_representation(&self, id: i64) -> Result<Value, AppError> {
        let row = self
            .storage
            .connection
            .query_row(
                "SELECT r.ref,p.remote_state,p.url,o.filename,p.observed_mime,
                    o.observed_at FROM representations p
               JOIN resources r ON r.id=p.resource_id
               LEFT JOIN representation_observations o ON o.id=(
                 SELECT id FROM representation_observations
                  WHERE representation_id=p.id ORDER BY id DESC LIMIT 1 )
              WHERE p.id=?1",
                [id],
                |row| {
                    Ok(RepresentationRow {
                        resource_ref: row.get(0)?,
                        remote_state: row.get(1)?,
                        source: RepresentationSource {
                            url: row.get(2)?,
                            filename: row.get(3)?,
                            mime: row.get(4)?,
                            observed_at: row.get(5)?,
                        },
                    })
                },
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("representation not found"))?;
        let content = self
            .storage
            .connection
            .query_row(
                "SELECT sha256,byte_length,mime,observed_at FROM content_observations
              WHERE representation_id=?1 ORDER BY id DESC LIMIT 1",
                [id],
                |item| {
                    Ok(RepresentationContent {
                        sha256_ref: format!("sha256:{}", item.get::<_, String>(0)?),
                        byte_length: item.get(1)?,
                        mime: item.get(2)?,
                        observed_at: item.get(3)?,
                    })
                },
            )
            .optional()?;
        let reference = format!("representation:{id}");
        let digest = current_digest(&self.storage.connection, &LibraryRef::Representation(id))?;
        let effective = effective_object(
            &self.storage.connection,
            &reference,
            digest.as_deref(),
            None,
            row.source.filename.as_deref(),
        )?;
        let relations = relations(&self.storage.connection, &reference)?;
        detail_value(RepresentationDetail {
            reference,
            resource_ref: row.resource_ref,
            remote_state: row.remote_state,
            source: row.source,
            content,
            effective,
            relations,
        })
    }
    fn show_blob(&self, sha256: &str) -> Result<Value, AppError> {
        let row = self
            .storage
            .connection
            .query_row(
                "SELECT byte_length,mime,stored_at FROM blobs WHERE sha256=?1",
                [sha256],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("blob not found"))?;
        let mut statement = self.storage.connection.prepare(
            "SELECT DISTINCT 'representation:'||representation_id
               FROM content_observations WHERE sha256=?1 ORDER BY representation_id",
        )?;
        let representations = statement
            .query_map([sha256], |item| item.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(
            json!({"ref": format!("sha256:{sha256}"), "byte_length": row.0,
                  "mime": row.1, "stored_at": row.2,
                  "representations": representations}),
        )
    }
    pub fn history(&self, reference: &str, limit: usize) -> Result<Vec<HistoryEntry>, AppError> {
        let reference = reference.parse::<LibraryRef>()?.to_string();
        let mut statement = self.storage.connection.prepare(
            "SELECT id,observed_at,kind FROM subject_history
              WHERE subject_ref=?1
              ORDER BY observed_at DESC,kind DESC,id DESC
              LIMIT ?2",
        )?;
        let keys = statement
            .query_map(params![reference, limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        keys.into_iter()
            .map(|(id, observed_at, kind)| {
                history_entry(&self.storage.connection, id, observed_at, &kind)
            })
            .collect()
    }
    pub fn content(&self, reference: &str) -> Result<ContentRecord, AppError> {
        let parsed = reference.parse::<LibraryRef>()?;
        let row = match parsed {
            LibraryRef::Sha256(hash) => self
                .storage
                .connection
                .query_row(
                    "SELECT b.sha256,b.byte_length,b.mime,COALESCE(o.filename,b.sha256) FROM blobs b
                   LEFT JOIN content_observations c ON c.sha256=b.sha256
                   LEFT JOIN representation_observations o ON o.id=(
                     SELECT id FROM representation_observations
                      WHERE representation_id=c.representation_id ORDER BY id DESC LIMIT 1 )
                  WHERE b.sha256=?1 LIMIT 1",
                    [hash],
                    content_row,
                )
                .optional()?,
            LibraryRef::Representation(id) => latest_content(&self.storage.connection, id)?,
            LibraryRef::Resource(resource) => {
                let mut statement = self.storage.connection.prepare(
                    "SELECT DISTINCT p.id FROM representations p
                       JOIN resources r ON r.id=p.resource_id WHERE r.ref=?1
                        AND EXISTS(SELECT 1 FROM content_observations c
                                    WHERE c.representation_id=p.id)
                      ORDER BY p.id",
                )?;
                let ids = statement
                    .query_map([&resource], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
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
                match ids.first() {
                    Some(id) => latest_content(&self.storage.connection, *id)?,
                    None => None,
                }
            }
            _ => {
                return Err(AppError::content_unavailable(
                    "reference has no stored content",
                ));
            }
        };
        let Some(row) = row else {
            return Err(missing_content(&self.storage.connection, reference)?);
        };
        let path = object_store::object_path(&self.storage.paths.objects, &row.sha256)?;
        Ok(ContentRecord {
            reference: format!("sha256:{}", row.sha256),
            path,
            byte_length: row.byte_length as u64,
            mime: row.mime,
            filename: row.filename,
        })
    }
    pub fn export(&self, reference: &str, destination: &Path) -> Result<u64, AppError> {
        let content = self.content(reference)?;
        let hash = content.reference.trim_start_matches("sha256:");
        object_store::export(&self.storage.paths.objects, hash, destination)
    }

    pub fn preview(&self, reference: &str, max: usize) -> Result<super::ContentPreview, AppError> {
        use std::io::Read;

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
            Err(error) if truncated && error.error_len().is_none() => Some(
                String::from_utf8(bytes[..error.valid_up_to()].to_vec())
                    .expect("valid UTF-8 prefix"),
            ),
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
struct MissingContentContext {
    kind: String,
    course: String,
    text: String,
    state: String,
    has_present_file: bool,
}

fn missing_content(connection: &Connection, reference: &str) -> Result<AppError, AppError> {
    let (resource, representation) = match reference.parse::<LibraryRef>()? {
        LibraryRef::Resource(resource) => (resource, None),
        LibraryRef::Representation(id) => {
            let row = connection
                .query_row(
                    "SELECT r.ref,p.kind,p.remote_state FROM representations p
                  JOIN resources r ON r.id=p.resource_id
                  WHERE p.id=?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((resource, kind, state)) = row else {
                return Ok(AppError::content_unavailable(
                    "no stored content for reference",
                ));
            };
            (resource, Some((kind, state)))
        }
        _ => {
            return Ok(AppError::content_unavailable(
                "no stored content for reference",
            ));
        }
    };
    let context = connection
        .query_row(
            "SELECT r.kind,c.ref,COALESCE(o.text,''),r.remote_state,
                    EXISTS(SELECT 1 FROM representations p WHERE p.resource_id=r.id
                           AND p.kind='file' AND p.remote_state='present')
              FROM resources r
              JOIN courses c ON c.id=r.course_id
              LEFT JOIN resource_observations o ON o.id=(
                SELECT id FROM resource_observations WHERE resource_id=r.id ORDER BY id DESC LIMIT 1)
              WHERE r.ref=?1",
            [&resource],
            |row| Ok(MissingContentContext {
                kind: row.get(0)?,
                course: row.get(1)?,
                text: row.get(2)?,
                state: row.get(3)?,
                has_present_file: row.get(4)?,
            }),
        )
        .optional()?;
    let Some(context) = context else {
        return Ok(AppError::content_unavailable(
            "no stored content for reference",
        ));
    };
    let has_notice_text = context.kind == "notice" && !context.text.is_empty();
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
        return Ok(AppError::content_unavailable(
            "this representation is stored as a non-file link, not downloaded file content",
        )
        .with_hint(hint));
    }
    if representation.is_none()
        && context.kind == "notice"
        && context.state == "present"
        && context.has_present_file
    {
        // Only offer current file candidates, not links or historical missing
        // attachments. Stored bytes have already taken precedence in content().
        let mut statement = connection.prepare(
            "SELECT p.id,o.filename FROM representations p
              JOIN resources r ON r.id=p.resource_id
              LEFT JOIN representation_observations o ON o.id=(
                SELECT id FROM representation_observations
                 WHERE representation_id=p.id ORDER BY id DESC LIMIT 1)
              WHERE r.ref=?1 AND p.kind='file' AND p.remote_state='present'
              ORDER BY p.id LIMIT 21",
        )?;
        let candidates = statement
            .query_map([&resource], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut references = Vec::new();
        let mut labels = Vec::new();
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
            "To attempt downloading available files, run `klms library sync --course {} --notices --download changed`, then use an attachment reference with content or export. These commands operate on downloaded file bytes.",
            context.course
        ));
        return Ok(
            AppError::content_unavailable("notice attachments have not been downloaded")
                .with_hint(hint)
                .with_details(json!({"representations": references})),
        );
    }
    if representation.is_none() && has_notice_text {
        return Ok(AppError::content_unavailable(
            "notice text is stored as source metadata, not downloaded file bytes",
        )
        .with_hint(format!(
            "Read it with `klms library show {resource}` (JSON: data.source.text). Content and export operate on downloaded file bytes."
        )));
    }
    if representation
        .as_ref()
        .is_some_and(|(_, state)| state == "not_observed")
    {
        return Ok(AppError::content_unavailable(
            "no stored file bytes; this file is recorded as not observed",
        ).with_hint(format!(
            "Inspect its parent resource and observation state with `klms library show {resource}` before deciding whether to refresh it."
        )));
    }
    let has_file_candidate = representation
        .as_ref()
        .map_or(context.has_present_file, |(kind, state)| {
            kind == "file" && state == "present"
        });
    if context.state == "present" && has_file_candidate {
        let course = &context.course;
        let notice_flag = if context.kind == "notice" {
            " --notices"
        } else {
            ""
        };
        return Ok(AppError::content_unavailable(
            "metadata-only: file bytes have not been downloaded",
        ).with_hint(format!(
            "To attempt downloading available files, run `klms library sync --course {course}{notice_flag} --download changed`, then retry. Inspect metadata with `klms library show {reference}`."
        )));
    }
    Ok(AppError::content_unavailable("no downloaded file bytes are stored for this reference")
        .with_hint(format!(
            "Inspect stored metadata and observation state with `klms library show {resource}`. The local record does not establish whether files are available remotely."
        )))
}
pub fn effective_field(
    connection: &Connection,
    subject: &str,
    field: &str,
) -> Result<Option<Assertion>, AppError> {
    let sql = format!(
        "SELECT a.id,a.value,a.actor,a.revision,a.based_on FROM assertions a
          WHERE a.subject_ref=?1 AND a.field=?2 AND {ACTIVE_ASSERTION}
          ORDER BY a.revision DESC LIMIT 1"
    );
    connection
        .query_row(&sql, params![subject, field], |row| {
            Ok(Assertion {
                id: row.get(0)?,
                value: row.get(1)?,
                actor: row.get(2)?,
                revision: row.get(3)?,
                based_on: row.get(4)?,
            })
        })
        .optional()
        .map_err(AppError::from)
}
pub fn current_digest(
    connection: &Connection,
    reference: &LibraryRef,
) -> Result<Option<String>, AppError> {
    let result = match reference {
        LibraryRef::Course(_) => connection
            .query_row(
                "SELECT o.digest FROM course_observations o JOIN courses c ON c.id=o.course_id
              WHERE c.ref=?1 ORDER BY o.id DESC LIMIT 1",
                [reference.to_string()],
                |row| row.get(0),
            )
            .optional(),
        LibraryRef::Resource(_) => connection
            .query_row(
                "SELECT o.digest FROM resource_observations o JOIN resources r ON r.id=o.resource_id
              WHERE r.ref=?1 ORDER BY o.id DESC LIMIT 1",
                [reference.to_string()],
                |row| row.get(0),
            )
            .optional(),
        LibraryRef::Representation(id) => connection
            .query_row(
                "SELECT COALESCE(
                   (SELECT sha256 FROM content_observations
                     WHERE representation_id=?1 ORDER BY id DESC LIMIT 1),
                   (SELECT digest FROM representation_observations
                     WHERE representation_id=?1 ORDER BY id DESC LIMIT 1))",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten),
        LibraryRef::Sha256(hash) => connection
            .query_row("SELECT sha256 FROM blobs WHERE sha256=?1", [hash], |row| {
                row.get(0)
            })
            .optional(),
        _ => Ok(None),
    };
    result.map_err(AppError::from)
}
pub fn refresh_subject(transaction: &Transaction<'_>, subject: &str) -> Result<(), AppError> {
    transaction.execute(
        "DELETE FROM search_documents WHERE subject_ref=?1",
        [subject],
    )?;
    let parsed = subject.parse::<LibraryRef>()?;
    let source = match parsed {
        LibraryRef::Course(_) => transaction
            .query_row(
                "SELECT 'course','',o.title,o.title,COALESCE(o.code,'')||' '||COALESCE(o.term,'')
               FROM course_observations o JOIN courses c ON c.id=o.course_id
              WHERE c.ref=?1 ORDER BY o.id DESC LIMIT 1",
                [subject],
                source_row,
            )
            .optional()?,
        LibraryRef::Resource(_) => transaction
            .query_row(
                "SELECT r.kind,c.ref,o.title,COALESCE(o.text,''),'' FROM resource_observations o
               JOIN resources r ON r.id=o.resource_id JOIN courses c ON c.id=r.course_id
              WHERE r.ref=?1 ORDER BY o.id DESC LIMIT 1",
                [subject],
                source_row,
            )
            .optional()?,
        LibraryRef::Representation(id) => transaction
            .query_row(
                "SELECT p.kind,c.ref,COALESCE(o.filename,p.url),p.url,''
               FROM representations p JOIN resources r ON r.id=p.resource_id
               JOIN courses c ON c.id=r.course_id LEFT JOIN representation_observations o ON o.id=(
                 SELECT id FROM representation_observations
                  WHERE representation_id=p.id ORDER BY id DESC LIMIT 1)
              WHERE p.id=?1 AND NOT (
                r.kind='notice' AND p.kind='link' AND p.remote_state='not_observed')",
                [id],
                source_row,
            )
            .optional()?,
        _ => None,
    };
    let Some((kind, course, mut title, mut body, mut tags)) = source else {
        return Ok(());
    };
    if let Some(value) = effective_field(transaction, subject, "title")? {
        title = value.value;
    }
    for field in ["filename", "summary", "note"] {
        if let Some(value) = effective_field(transaction, subject, field)? {
            body.push(' ');
            body.push_str(&value.value);
        }
    }
    if let Some(value) = effective_field(transaction, subject, "tag")? {
        tags.push(' ');
        tags.push_str(&value.value);
    }
    transaction.execute(
        "INSERT INTO search_documents(subject_ref,kind,course,title,body,tags)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![subject, kind, course, title, body, tags],
    )?;
    Ok(())
}
fn coverage(connection: &Connection) -> Result<(Option<i64>, Option<bool>), AppError> {
    let fresh = connection.query_row(
        "SELECT MAX(finished_at) FROM sync_runs
          WHERE scope='all' AND status='complete' AND source_complete=1",
        [],
        |row| row.get(0),
    )?;
    let complete = connection
        .query_row(
            "SELECT source_complete FROM sync_runs
          WHERE scope='all' AND status!='running' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )
        .optional()?;
    Ok((fresh, complete))
}
fn effective_object(
    connection: &Connection,
    subject: &str,
    digest: Option<&str>,
    source_title: Option<&str>,
    source_filename: Option<&str>,
) -> Result<Value, AppError> {
    let mut object = Map::new();
    let mut provenance = Map::new();
    let mut stale = false;
    for field in ["title", "filename", "summary", "note", "tag"] {
        if let Some(assertion) = effective_field(connection, subject, field)? {
            if field == "summary" {
                stale = assertion.based_on.as_deref() != digest;
            }
            object.insert(field.into(), Value::String(assertion.value));
            provenance.insert(
                field.into(),
                json!({
                    "assertion_ref": format!("assertion:{}", assertion.id),
                    "actor": assertion.actor, "revision": assertion.revision,
                    "based_on": assertion.based_on
                }),
            );
        } else {
            object.insert(field.into(), Value::Null);
        }
    }
    if object["title"].is_null() {
        object.insert(
            "title".into(),
            source_title.map_or(Value::Null, |value| Value::String(value.into())),
        );
    }
    if object["filename"].is_null() {
        object.insert(
            "filename".into(),
            source_filename.map_or(Value::Null, |value| Value::String(value.into())),
        );
    }
    object.insert("summary_stale".into(), Value::Bool(stale));
    object.insert("_provenance".into(), Value::Object(provenance));
    Ok(Value::Object(object))
}
fn relations(connection: &Connection, subject: &str) -> Result<Vec<String>, AppError> {
    let sql = format!(
        "SELECT 'relation:'||r.id FROM relations r
          WHERE (r.left_ref=?1 OR r.right_ref=?1) AND {ACTIVE_RELATION}
          ORDER BY r.id"
    );
    let mut statement = connection.prepare(&sql)?;
    statement
        .query_map([subject], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AppError::from)
}
fn history_entry(
    connection: &Connection,
    id: i64,
    observed_at: i64,
    kind: &str,
) -> Result<HistoryEntry, AppError> {
    let (digest, source) = match kind {
        "course_source" => connection.query_row(
            "SELECT digest,json_object('title',title,'code',code,'term',term,'url',url,
                    'sync_ref','sync:'||sync_run_id) FROM course_observations WHERE id=?1",
            [id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ),
        "resource_source" => connection.query_row(
            "SELECT digest,source_json FROM resource_observations WHERE id=?1",
            [id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ),
        "representation_source" => connection.query_row(
            "SELECT o.digest,json_object('filename',o.filename,'url',p.url,
                    'sync_ref','sync:'||o.sync_run_id) FROM representation_observations o
               JOIN representations p ON p.id=o.representation_id WHERE o.id=?1",
            [id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ),
        "verified_content" => connection.query_row(
            "SELECT c.sha256,json_object('sha256_ref','sha256:'||c.sha256,'url',p.url,
                    'etag',c.etag,'last_modified',c.last_modified,
                    'byte_length',c.byte_length,'mime',c.mime, 'sync_ref','sync:'||c.sync_run_id)
               FROM content_observations c
               JOIN representations p ON p.id=c.representation_id WHERE c.id=?1",
            [id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ),
        _ => return Err(AppError::corpus_corrupt("unknown history entry kind")),
    }?;
    Ok(HistoryEntry {
        id,
        observed_at,
        kind: kind.into(),
        digest,
        source: stored_json(&source, &format!("{kind}:{id} source"))?,
    })
}
fn latest_content(
    connection: &Connection,
    representation_id: i64,
) -> Result<Option<ContentRow>, AppError> {
    connection
        .query_row(
            "SELECT c.sha256,c.byte_length,c.mime,COALESCE(o.filename,c.sha256)
           FROM content_observations c LEFT JOIN representation_observations o ON o.id=(
             SELECT id FROM representation_observations
              WHERE representation_id=c.representation_id ORDER BY id DESC LIMIT 1)
          WHERE c.representation_id=?1 ORDER BY c.id DESC LIMIT 1",
            [representation_id],
            content_row,
        )
        .optional()
        .map_err(AppError::from)
}
fn content_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContentRow> {
    Ok(ContentRow {
        sha256: row.get(0)?,
        byte_length: row.get(1)?,
        mime: row.get(2)?,
        filename: row.get(3)?,
    })
}
fn source_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, String, String)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn stored_json(text: &str, context: &str) -> Result<Value, AppError> {
    serde_json::from_str(text).map_err(|error| {
        AppError::corpus_corrupt(format!("invalid persisted JSON in {context}: {error}"))
    })
}
