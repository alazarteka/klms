use super::query::{refresh_subject, row, rows};
use crate::{
    error::AppError,
    models::{Course, LinkItem},
    url::Url,
};
use rusqlite::{Connection, Params, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct PendingResource {
    pub(super) reference: String,
    pub(super) kind: String,
    pub(super) title: String,
    pub(super) url: Option<String>,
    pub(super) week: Option<u32>,
    pub(super) section: Option<String>,
    pub(super) text: Option<String>,
    pub(super) source: Value,
    pub(super) links: Vec<LinkItem>,
    pub(super) observe: bool,
    pub(super) access_lost: bool,
    pub(super) complete: bool,
    pub(super) representations_complete: bool,
}

struct Existing {
    id: i64,
    state: String,
    digest: Option<String>,
}

/// The write context of one sync run: connection, run id and observation time.
pub(super) struct Run<'a> {
    pub(super) c: &'a Connection,
    pub(super) id: i64,
    pub(super) at: i64,
}

impl Run<'_> {
    pub(super) fn change(
        &self,
        kind: &str,
        subject: &str,
        before: Option<&str>,
        after: Option<&str>,
        details: Option<Value>,
    ) -> Result<(), AppError> {
        self.c.execute(
            "INSERT INTO remote_changes(
               sync_run_id,occurred_at,kind,subject_ref,before_ref,after_ref,details_json
             ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                self.id,
                self.at,
                kind,
                subject,
                before,
                after,
                details.map_or_else(|| "{}".to_owned(), |value| value.to_string())
            ],
        )?;
        Ok(())
    }

    /// The row of `table` matching `filter` (alias `t`), with its newest observation digest.
    fn existing(
        &self,
        (table, observations, key): (&str, &str, &str),
        filter: &str,
        p: impl Params,
    ) -> Result<Option<Existing>, AppError> {
        let sql = format!(
            "SELECT t.id,t.remote_state,(SELECT digest FROM {observations}
                WHERE {key}=t.id ORDER BY id DESC LIMIT 1) FROM {table} t WHERE {filter}"
        );
        row(self.c, &sql, p, |r| {
            Ok(Existing {
                id: r.get(0)?,
                state: r.get(1)?,
                digest: r.get(2)?,
            })
        })
    }

    fn insert_id(&self, sql: &str, p: impl Params) -> Result<i64, AppError> {
        self.c.execute(sql, p)?;
        Ok(self.c.last_insert_rowid())
    }

    /// Rows selected as (`id`, `key`, `subject`) whose key `seen` does not claim
    /// are marked missing by `update`, with one recorded change each.
    pub(super) fn mark_missing(
        &self,
        select: &str,
        select_params: impl Params,
        (update, kind, details): (&str, &str, Option<Value>),
        seen: impl Fn(&str) -> bool,
    ) -> Result<(), AppError> {
        let found = rows(self.c, select, select_params, |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for (id, key, subject) in found {
            if !seen(&key) {
                self.c.execute(update, params![self.at, id])?;
                self.change(kind, &subject, None, None, details.clone())?;
            }
        }
        Ok(())
    }

    pub(super) fn upsert_course(&self, course: &Course) -> Result<i64, AppError> {
        let digest = digest_json(course)?;
        let existing = self.existing(
            ("courses", "course_observations", "course_id"),
            "t.ref=?1",
            [&course.reference],
        )?;
        let previous = existing.as_ref().and_then(|e| e.digest.as_deref());
        let changed = previous != Some(digest.as_str());
        let (id, event) = match &existing {
            Some(e) => {
                self.c.execute(
                    "UPDATE courses SET remote_state='listed',last_seen=?1,not_listed_since=NULL
                  WHERE id=?2",
                    params![self.at, e.id],
                )?;
                let event = if e.state != "listed" {
                    Some("course_reappeared")
                } else {
                    changed.then_some("course_source_changed")
                };
                (e.id, event)
            }
            None => {
                let id = self.insert_id(
                    "INSERT INTO courses(ref,first_seen,last_seen) VALUES(?1,?2,?2)",
                    params![course.reference, self.at],
                )?;
                (id, Some("course_appeared"))
            }
        };
        if changed {
            self.c.execute(
                "INSERT INTO course_observations(
               course_id,sync_run_id,observed_at,digest,title,code,term,url
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    self.id,
                    self.at,
                    digest,
                    course.title,
                    course.code,
                    course.term,
                    course.url
                ],
            )?;
        }
        if let Some(kind) = event {
            let details = json!({"title": course.title});
            self.change(
                kind,
                &course.reference,
                previous,
                Some(&digest),
                Some(details),
            )?;
        }
        Ok(id)
    }

    pub(super) fn upsert_resource(
        &self,
        course_id: i64,
        resource: &PendingResource,
    ) -> Result<i64, AppError> {
        let existing = self.existing(
            ("resources", "resource_observations", "resource_id"),
            "t.ref=?1",
            [&resource.reference],
        )?;
        let desired = if resource.access_lost {
            Some("access_lost")
        } else {
            resource.observe.then_some("present")
        };
        let id = match &existing {
            Some(e) => {
                if let Some(state) = desired {
                    self.c.execute(
                        "UPDATE resources SET last_seen=?1,remote_state=?2,not_observed_since=NULL
                      WHERE id=?3",
                        params![self.at, state, e.id],
                    )?;
                }
                e.id
            }
            None => self.insert_id(
                "INSERT INTO resources(ref,course_id,kind,remote_state,first_seen,last_seen)
             VALUES(?1,?2,?3,?4,?5,?5)",
                params![
                    resource.reference,
                    course_id,
                    resource.kind,
                    desired.unwrap_or("present"),
                    self.at
                ],
            )?,
        };
        let digest = digest_json(&resource.source)?;
        let previous = existing.as_ref().and_then(|e| e.digest.as_deref());
        if (resource.observe || previous.is_none()) && previous != Some(digest.as_str()) {
            self.c.execute(
                "INSERT INTO resource_observations(
               resource_id,sync_run_id,observed_at,digest,complete,title,url,
               week,section,text,source_json
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    id,
                    self.id,
                    self.at,
                    digest,
                    resource.complete,
                    resource.title,
                    resource.url,
                    resource.week,
                    resource.section,
                    resource.text,
                    resource.source.to_string()
                ],
            )?;
            let kind = match previous {
                Some(_) => "source_changed",
                None => "resource_appeared",
            };
            let details = json!({"kind": resource.kind});
            self.change(
                kind,
                &resource.reference,
                previous,
                Some(&digest),
                Some(details),
            )?;
        }
        if let (Some(e), Some(after)) = (&existing, desired) {
            if e.state != after {
                let kind = match (e.state.as_str(), after) {
                    (_, "access_lost") => "access_lost",
                    ("access_lost", "present") => "access_restored",
                    _ => "resource_restored",
                };
                self.change(kind, &resource.reference, None, None, None)?;
            }
        }
        Ok(id)
    }

    pub(super) fn upsert_representation(
        &self,
        resource_id: i64,
        url: &Url,
        filename: &str,
    ) -> Result<i64, AppError> {
        let digest = crate::digest::sha256_hex(format!("{}\n{filename}", url.as_str()).as_bytes());
        let existing = self.existing(
            (
                "representations",
                "representation_observations",
                "representation_id",
            ),
            "t.resource_id=?1 AND t.url=?2",
            params![resource_id, url.as_str()],
        )?;
        let (id, restored) = match &existing {
            Some(e) => {
                self.c.execute(
                    "UPDATE representations SET remote_state='present',last_seen=?1,
                        not_observed_since=NULL WHERE id=?2",
                    params![self.at, e.id],
                )?;
                (e.id, e.state != "present")
            }
            None => {
                let id = self.insert_id(
                    "INSERT INTO representations(resource_id,url,kind,first_seen,last_seen)
                 VALUES(?1,?2,?3,?4,?4)",
                    params![resource_id, url.as_str(), representation_kind(url), self.at],
                )?;
                (id, false)
            }
        };
        let subject = format!("representation:{id}");
        let previous = existing.as_ref().and_then(|e| e.digest.as_deref());
        if previous != Some(digest.as_str()) {
            self.c.execute(
                "INSERT INTO representation_observations(
               representation_id,sync_run_id,observed_at,digest,filename
             ) VALUES(?1,?2,?3,?4,?5)",
                params![id, self.id, self.at, digest, filename],
            )?;
            let kind = match previous {
                Some(_) => "representation_source_changed",
                None => "representation_appeared",
            };
            self.change(kind, &subject, previous, Some(&digest), None)?;
        }
        if restored {
            self.change("representation_restored", &subject, None, None, None)?;
        }
        refresh_subject(self.c, &subject)?;
        Ok(id)
    }

    pub(super) fn mark_missing_representations(
        &self,
        resource_id: i64,
        seen: &HashSet<String>,
    ) -> Result<(), AppError> {
        self.mark_missing(
            "SELECT id,url,'representation:'||id FROM representations
          WHERE resource_id=?1 AND remote_state='present'",
            [resource_id],
            (
                "UPDATE representations SET remote_state='not_observed',not_observed_since=?1
              WHERE id=?2",
                "representation_not_observed",
                Some(json!({"collection": "resource_detail"})),
            ),
            |url| seen.contains(url),
        )?;
        // Drop search entries for notice links that are not_observed. This covers
        // links just marked missing above (mark_missing does not call
        // refresh_subject) and any stale entries left by earlier versions. Keep
        // all history and file entries.
        self.c.execute(
            "DELETE FROM search_documents WHERE subject_ref IN (
            SELECT 'representation:'||p.id FROM representations p
              JOIN resources r ON r.id=p.resource_id
             WHERE p.resource_id=?1 AND r.kind='notice' AND p.kind='link'
               AND p.remote_state='not_observed')",
            [resource_id],
        )?;
        Ok(())
    }
}

pub(super) fn representation_kind(url: &Url) -> &'static str {
    let path = url.path();
    if path.contains("pluginfile.php")
        || path.contains("/mod/resource/") && !path.ends_with("/view.php")
    {
        "file"
    } else {
        "link"
    }
}

pub(super) fn digest_json(value: &impl Serialize) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(value).map_err(|error| AppError::internal(error.to_string()))?;
    Ok(crate::digest::sha256_hex(&bytes))
}
