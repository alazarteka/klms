use super::{
    Corpus, SyncSummary, object_store,
    query::{refresh_subject, row, rows},
};
use crate::date::epoch_now as now;
use crate::url::Url;
use crate::{
    client::{KlmsClient, RemoteMetadata},
    error::AppError,
    models::{Activity, Course, LinkItem},
    parse,
};
use rusqlite::{Connection, Params, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

const MAX_DOWNLOAD: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct SyncOptions {
    pub notices: bool,
    pub files: bool,
    pub download_changed: bool,
}

#[derive(Default)]
struct PendingResource {
    reference: String,
    kind: String,
    title: String,
    url: Option<String>,
    week: Option<u32>,
    section: Option<String>,
    text: Option<String>,
    source: Value,
    links: Vec<LinkItem>,
    observe: bool,
    access_lost: bool,
    complete: bool,
    representations_complete: bool,
}

/// A course with its resources and whether its manifest was fetched completely.
type Collection = (Course, Vec<PendingResource>, bool);
/// Newest content observation of a representation: sha256, etag, last-modified, length.
type Bound = (String, Option<String>, Option<String>, i64);

struct Existing {
    id: i64,
    state: String,
    digest: Option<String>,
}

/// The write context of one sync run: connection, run id and observation time.
struct Run<'a> {
    c: &'a Connection,
    id: i64,
    at: i64,
}

impl Run<'_> {
    fn change(
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
    fn mark_missing(
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
}

impl Corpus {
    pub fn sync(
        &mut self,
        client: &KlmsClient,
        base_url: &Url,
        filter: Option<&str>,
        options: SyncOptions,
    ) -> Result<SyncSummary, AppError> {
        let started_at = now();
        self.connection.execute(
            "INSERT INTO sync_runs(started_at,scope,status) VALUES(?1,?2,'running')",
            params![started_at, filter.unwrap_or("all")],
        )?;
        let run_id = self.connection.last_insert_rowid();
        self.collect_sync(client, base_url, filter, options, run_id, started_at)
            .inspect_err(|_| {
                let _ = self.connection.execute(
                    "UPDATE sync_runs SET finished_at=?1,status='failed' WHERE id=?2",
                    params![now(), run_id],
                );
            })
    }

    fn collect_sync(
        &mut self,
        client: &KlmsClient,
        base_url: &Url,
        filter: Option<&str>,
        options: SyncOptions,
        run_id: i64,
        observed_at: i64,
    ) -> Result<SyncSummary, AppError> {
        let response = client.get("/my/")?;
        let mut courses = parse::dashboard(&response.text, base_url)?.courses;
        if let Some(value) = filter {
            courses = resolve_course(courses, value)?;
            self.connection.execute(
                "UPDATE sync_runs SET scope=?1 WHERE id=?2",
                params![courses[0].reference, run_id],
            )?;
        }
        let mut collections: Vec<Collection> = Vec::new();
        let mut failures = Vec::new();
        for course in courses {
            match collect_course(client, base_url, &course, options) {
                Ok((resources, mut detail_failures)) => {
                    failures.append(&mut detail_failures);
                    collections.push((course, resources, true));
                }
                Err(error) => {
                    failures.push(format!("{}: {}", course.reference, error.message));
                    collections.push((course, Vec::new(), false));
                }
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let x = Run {
            c: &transaction,
            id: run_id,
            at: observed_at,
        };
        let mut listed = HashSet::new();
        let mut frontier = BTreeMap::new();
        let (mut resource_count, mut representation_count, mut truncated_count) = (0_u64, 0_u64, 0);
        for (course, resources, _) in &collections {
            let course_id = upsert_course(&x, course)?;
            listed.insert(course.reference.clone());
            refresh_subject(x.c, &course.reference)?;
            for resource in resources {
                let resource_id = upsert_resource(&x, course_id, resource)?;
                resource_count += 1;
                truncated_count += (resource.observe && !resource.complete) as u64;
                let mut links = resource.links.clone();
                if let Some(url) = (resource.url.as_deref()).filter(|url| !activity_container(url))
                {
                    links.push(LinkItem {
                        title: resource.title.clone(),
                        url: url.into(),
                    });
                }
                let mut seen_urls = HashSet::new();
                for link in links {
                    let Ok(url) = Url::parse(&link.url) else {
                        continue;
                    };
                    seen_urls.insert(url.as_str().to_owned());
                    let id = upsert_representation(&x, resource_id, &url, &link.title)?;
                    if representation_kind(&url) == "file" {
                        frontier.insert(id, url.as_str().to_owned());
                    }
                    representation_count += 1;
                }
                if resource.representations_complete {
                    mark_missing_representations(&x, resource_id, &seen_urls)?;
                }
                refresh_subject(x.c, &resource.reference)?;
            }
        }
        if filter.is_none() {
            x.mark_missing(
                "SELECT id,ref,ref FROM courses WHERE remote_state='listed'",
                [],
                (
                    "UPDATE courses SET remote_state='not_listed',not_listed_since=?1 WHERE id=?2",
                    "course_not_listed",
                    None,
                ),
                |key| listed.contains(key),
            )?;
        }
        for (course, resources, _) in collections.iter().filter(|c| c.2) {
            let seen: HashSet<&str> = resources.iter().map(|r| r.reference.as_str()).collect();
            x.mark_missing(
                "SELECT r.id,r.ref,r.ref FROM resources r JOIN courses c ON c.id=r.course_id
                  WHERE c.ref=?1 AND r.remote_state='present' AND r.kind!='notice'",
                [&course.reference],
                (
                    "UPDATE resources SET remote_state='not_observed',not_observed_since=?1 WHERE id=?2",
                    "resource_not_observed",
                    Some(json!({"collection": "course_manifest"})),
                ),
                |key| seen.contains(key),
            )?;
        }
        transaction.commit()?;
        let (blobs_added, mut validation_failures) = if options.files || options.download_changed {
            self.validate_frontier(client, run_id, options.download_changed, &frontier)?
        } else {
            (0, Vec::new())
        };
        failures.append(&mut validation_failures);
        let status = if failures.is_empty() {
            "complete"
        } else {
            "incomplete"
        };
        let source_complete = failures.is_empty() && filter.is_none();
        let changes = self.connection.query_row(
            "SELECT COUNT(*) FROM remote_changes WHERE sync_run_id=?1",
            [run_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        self.connection.execute(
            "UPDATE sync_runs SET finished_at=?1,status=?2,source_complete=?3 WHERE id=?4",
            params![now(), status, source_complete, run_id],
        )?;
        Ok(SyncSummary {
            reference: format!("sync:{run_id}"),
            status: status.into(),
            source_complete,
            courses: collections.len() as u64,
            resources: resource_count,
            representations: representation_count,
            blobs_added,
            changes,
            truncated: truncated_count,
            failures,
        })
    }

    /// HEAD every observed file representation and, when `download` is set,
    /// fetch the ones whose validators no longer match the stored bytes.
    fn validate_frontier(
        &mut self,
        client: &KlmsClient,
        run_id: i64,
        download: bool,
        frontier: &BTreeMap<i64, String>,
    ) -> Result<(u64, Vec<String>), AppError> {
        let mut blobs_added = 0_u64;
        let mut failures = Vec::new();
        for (&id, url) in frontier {
            let metadata = match client.head(url) {
                Ok(metadata) => metadata,
                Err(error) => {
                    failures.push(format!("representation:{id}: {}", error.message));
                    continue;
                }
            };
            self.update_mime(id, &metadata)?;
            if !download {
                continue;
            }
            let bound = latest_bound_content(&self.connection, id)?;
            if bound
                .as_ref()
                .is_some_and(|bound| validators_match(bound, &metadata))
            {
                continue;
            }
            let conditional = client.get_conditional(
                url,
                bound.as_ref().and_then(|b| b.1.as_deref()),
                bound.as_ref().and_then(|b| b.2.as_deref()),
                MAX_DOWNLOAD,
            );
            let response = match conditional {
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("representation:{id}: {}", error.message));
                    continue;
                }
            };
            self.update_mime(id, &response.metadata)?;
            let Some(bytes) = response.bytes else {
                continue;
            };
            let sha256 = object_store::store(&self.objects, &bytes)?;
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let x = Run {
                c: &transaction,
                id: run_id,
                at: now(),
            };
            let length = bytes.len() as i64;
            blobs_added += transaction.execute(
                "INSERT OR IGNORE INTO blobs(sha256,byte_length,mime,stored_at) VALUES(?1,?2,?3,?4)",
                params![sha256, length, response.metadata.content_type, x.at],
            )? as u64;
            let previous = latest_bound_content(&transaction, id)?;
            // A successful download binds its validators to these bytes even
            // when their digest has not changed. Keep that observation so the
            // next sync does not download the same content again.
            transaction.execute(
                "INSERT INTO content_observations(
                   representation_id,sync_run_id,observed_at,sha256,etag,
                   last_modified,byte_length,mime
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    run_id,
                    x.at,
                    sha256,
                    response.metadata.etag,
                    response.metadata.last_modified,
                    length,
                    response.metadata.content_type
                ],
            )?;
            if let Some((before, ..)) = previous.filter(|bound| bound.0 != sha256) {
                x.change(
                    "verified_content_changed",
                    &format!("representation:{id}"),
                    Some(&format!("sha256:{before}")),
                    Some(&format!("sha256:{sha256}")),
                    None,
                )?;
            }
            transaction.commit()?;
        }
        Ok((blobs_added, failures))
    }

    fn update_mime(&self, id: i64, metadata: &RemoteMetadata) -> Result<(), AppError> {
        self.connection.execute(
            "UPDATE representations SET observed_mime=?1 WHERE id=?2",
            params![metadata.content_type, id],
        )?;
        Ok(())
    }
}

fn resolve_course(courses: Vec<Course>, filter: &str) -> Result<Vec<Course>, AppError> {
    let matched: Vec<_> = courses
        .into_iter()
        .filter(|course| {
            course.id == filter
                || course.reference == filter
                || (course.code.as_deref()).is_some_and(|code| code.eq_ignore_ascii_case(filter))
                || course.title.eq_ignore_ascii_case(filter)
        })
        .collect();
    if matched.len() == 1 {
        Ok(matched)
    } else {
        Err(AppError::not_found(format!(
            "course filter {filter:?} did not resolve uniquely"
        )))
    }
}

fn collect_course(
    client: &KlmsClient,
    base_url: &Url,
    course: &Course,
    options: SyncOptions,
) -> Result<(Vec<PendingResource>, Vec<String>), AppError> {
    let activities = crate::course_pages::activities(client, base_url, &course.id)?;
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for activity in activities {
        let reference = resource_reference(course, &activity)?;
        let mut row = PendingResource {
            reference: reference.clone(),
            kind: activity.kind.clone(),
            title: activity.title.clone(),
            url: activity.url.clone(),
            week: activity.week,
            section: activity.section.clone(),
            source: json!({"activity": activity, "detail": {"state": "not_requested"}}),
            observe: true,
            complete: true,
            representations_complete: (activity.url.as_deref())
                .is_some_and(|url| !activity_container(url)),
            ..Default::default()
        };
        let detail_url = (activity.url.as_deref()).filter(|url| {
            !activity.external
                && activity_container(url)
                && matches!(
                    activity.kind.as_str(),
                    "assign" | "quiz" | "page" | "folder" | "resource" | "coursefile"
                )
        });
        if let Some(url) = detail_url {
            let detail = client.get(url).and_then(|response| {
                parse::resource_detail(&response.text, base_url, &response.url, &activity.kind)
            });
            match detail {
                Ok(detail) => {
                    row.complete = !detail.text_truncated && !detail.links_truncated;
                    row.representations_complete = !detail.links_truncated;
                    row.text = Some(detail.text.clone());
                    row.links = detail.links.clone();
                    row.source = json!({"activity": activity, "detail": detail});
                }
                Err(error) => {
                    row.observe = false;
                    row.complete = false;
                    row.source["detail"]["state"] = json!("incomplete");
                    row.access_lost = error.code == "PERMISSION_DENIED";
                    failures.push(format!("{reference}: {}", error.message));
                }
            }
        }
        if let (true, "courseboard", Some(url)) =
            (options.notices, activity.kind.as_str(), &activity.url)
        {
            let (mut notices, mut notice_failures) = collect_board(client, base_url, course, url);
            rows.append(&mut notices);
            failures.append(&mut notice_failures);
        }
        rows.push(row);
    }
    Ok((rows, failures))
}

fn collect_board(
    client: &KlmsClient,
    base_url: &Url,
    course: &Course,
    start: &str,
) -> (Vec<PendingResource>, Vec<String>) {
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    let fail = |failures: &mut Vec<String>, message: &str| {
        failures.push(format!("{}: {message}", course.reference));
    };
    let mut next = Some(start.to_owned());
    let mut visited = HashSet::new();
    for _ in 0..20 {
        let Some(url) = next.take() else {
            return (rows, failures);
        };
        if !visited.insert(url.clone()) {
            fail(&mut failures, "notice pagination cycle detected");
            break;
        }
        let page = match client.get(&url) {
            Ok(page) => page,
            Err(error) => {
                fail(&mut failures, &error.message);
                break;
            }
        };
        let posts = match parse::board_posts(&page.text, base_url, page.url.query_value("id")) {
            Ok(posts) => posts,
            Err(error) => {
                fail(&mut failures, &error.message);
                break;
            }
        };
        for post in posts {
            let Some(reference) = post.reference else {
                continue;
            };
            let detail = client.get(&post.url).and_then(|response| {
                parse::resource_detail(&response.text, base_url, &response.url, "courseboard-post")
            });
            let detail = match detail {
                Ok(detail) => detail,
                Err(error) => {
                    failures.push(format!("{reference}: {}", error.message));
                    continue;
                }
            };
            rows.push(PendingResource {
                reference,
                kind: "notice".into(),
                title: detail.title.clone(),
                url: Some(detail.url.clone()),
                text: Some(detail.text.clone()),
                source: serde_json::to_value(&detail).unwrap_or(Value::Null),
                observe: true,
                complete: !detail.text_truncated && !detail.links_truncated,
                representations_complete: !detail.links_truncated,
                links: detail.links,
                ..Default::default()
            });
        }
        next = match parse::next_page_url(&page.text, base_url) {
            Ok(next) => next,
            Err(error) => {
                fail(&mut failures, &error.message);
                break;
            }
        };
    }
    if next.is_some() {
        fail(&mut failures, "notice pagination exceeded 20 pages");
    }
    (rows, failures)
}

fn upsert_course(x: &Run, course: &Course) -> Result<i64, AppError> {
    let digest = digest_json(course)?;
    let existing = x.existing(
        ("courses", "course_observations", "course_id"),
        "t.ref=?1",
        [&course.reference],
    )?;
    let previous = existing.as_ref().and_then(|e| e.digest.as_deref());
    let changed = previous != Some(digest.as_str());
    let (id, event) = match &existing {
        Some(e) => {
            x.c.execute(
                "UPDATE courses SET remote_state='listed',last_seen=?1,not_listed_since=NULL
                  WHERE id=?2",
                params![x.at, e.id],
            )?;
            let event = if e.state != "listed" {
                Some("course_reappeared")
            } else {
                changed.then_some("course_source_changed")
            };
            (e.id, event)
        }
        None => {
            let id = x.insert_id(
                "INSERT INTO courses(ref,first_seen,last_seen) VALUES(?1,?2,?2)",
                params![course.reference, x.at],
            )?;
            (id, Some("course_appeared"))
        }
    };
    if changed {
        x.c.execute(
            "INSERT INTO course_observations(
               course_id,sync_run_id,observed_at,digest,title,code,term,url
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                id,
                x.id,
                x.at,
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
        x.change(
            kind,
            &course.reference,
            previous,
            Some(&digest),
            Some(details),
        )?;
    }
    Ok(id)
}

fn upsert_resource(x: &Run, course_id: i64, resource: &PendingResource) -> Result<i64, AppError> {
    let existing = x.existing(
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
                x.c.execute(
                    "UPDATE resources SET last_seen=?1,remote_state=?2,not_observed_since=NULL
                      WHERE id=?3",
                    params![x.at, state, e.id],
                )?;
            }
            e.id
        }
        None => x.insert_id(
            "INSERT INTO resources(ref,course_id,kind,remote_state,first_seen,last_seen)
             VALUES(?1,?2,?3,?4,?5,?5)",
            params![
                resource.reference,
                course_id,
                resource.kind,
                desired.unwrap_or("present"),
                x.at
            ],
        )?,
    };
    let digest = digest_json(&resource.source)?;
    let previous = existing.as_ref().and_then(|e| e.digest.as_deref());
    if (resource.observe || previous.is_none()) && previous != Some(digest.as_str()) {
        x.c.execute(
            "INSERT INTO resource_observations(
               resource_id,sync_run_id,observed_at,digest,complete,title,url,
               week,section,text,source_json
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                id,
                x.id,
                x.at,
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
        x.change(
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
            x.change(kind, &resource.reference, None, None, None)?;
        }
    }
    Ok(id)
}

fn upsert_representation(
    x: &Run,
    resource_id: i64,
    url: &Url,
    filename: &str,
) -> Result<i64, AppError> {
    let digest = object_store::digest(format!("{}\n{filename}", url.as_str()).as_bytes());
    let existing = x.existing(
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
            x.c.execute(
                "UPDATE representations SET remote_state='present',last_seen=?1,
                        not_observed_since=NULL WHERE id=?2",
                params![x.at, e.id],
            )?;
            (e.id, e.state != "present")
        }
        None => {
            let id = x.insert_id(
                "INSERT INTO representations(resource_id,url,kind,first_seen,last_seen)
                 VALUES(?1,?2,?3,?4,?4)",
                params![resource_id, url.as_str(), representation_kind(url), x.at],
            )?;
            (id, false)
        }
    };
    let subject = format!("representation:{id}");
    let previous = existing.as_ref().and_then(|e| e.digest.as_deref());
    if previous != Some(digest.as_str()) {
        x.c.execute(
            "INSERT INTO representation_observations(
               representation_id,sync_run_id,observed_at,digest,filename
             ) VALUES(?1,?2,?3,?4,?5)",
            params![id, x.id, x.at, digest, filename],
        )?;
        let kind = match previous {
            Some(_) => "representation_source_changed",
            None => "representation_appeared",
        };
        x.change(kind, &subject, previous, Some(&digest), None)?;
    }
    if restored {
        x.change("representation_restored", &subject, None, None, None)?;
    }
    refresh_subject(x.c, &subject)?;
    Ok(id)
}

fn mark_missing_representations(
    x: &Run,
    resource_id: i64,
    seen: &HashSet<String>,
) -> Result<(), AppError> {
    x.mark_missing(
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
    x.c.execute(
        "DELETE FROM search_documents WHERE subject_ref IN (
            SELECT 'representation:'||p.id FROM representations p
              JOIN resources r ON r.id=p.resource_id
             WHERE p.resource_id=?1 AND r.kind='notice' AND p.kind='link'
               AND p.remote_state='not_observed')",
        [resource_id],
    )?;
    Ok(())
}

fn latest_bound_content(c: &Connection, representation_id: i64) -> Result<Option<Bound>, AppError> {
    row(
        c,
        "SELECT sha256,etag,last_modified,byte_length FROM content_observations
          WHERE representation_id=?1 ORDER BY id DESC LIMIT 1",
        [representation_id],
        |r| r.try_into(),
    )
}

fn validators_match((_, etag, last_modified, length): &Bound, metadata: &RemoteMetadata) -> bool {
    match (etag.as_deref(), metadata.etag.as_deref()) {
        (Some(before), Some(after)) => before == after,
        _ => {
            last_modified.is_some()
                && last_modified.as_deref() == metadata.last_modified.as_deref()
                && metadata.content_length.map(|value| value as i64) == Some(*length)
        }
    }
}

/// Moodle activities keep their own reference; anything else is identified by
/// a digest of its course and module id (or URL), stable across renames and moves.
fn resource_reference(course: &Course, activity: &Activity) -> Result<String, AppError> {
    if let Some(reference) = activity.reference.as_deref() {
        return Ok(reference.to_owned());
    }
    let identity = match (activity.id.as_deref(), activity.url.as_deref()) {
        (Some(id), _) => json!({"course_ref": course.reference, "module": id}),
        (None, Some(url)) => json!({"course_ref": course.reference, "url": url}),
        _ => return Err(AppError::shape("activity has no stable identity")),
    };
    Ok(format!("resource:{}", &digest_json(&identity)?[..24]))
}

fn representation_kind(url: &Url) -> &'static str {
    let path = url.path();
    if path.contains("pluginfile.php")
        || path.contains("/mod/resource/") && !path.ends_with("/view.php")
    {
        "file"
    } else {
        "link"
    }
}

fn activity_container(value: &str) -> bool {
    Url::parse(value)
        .is_ok_and(|url| url.path().starts_with("/mod/") && url.path().ends_with("/view.php"))
}

fn digest_json(value: &impl Serialize) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(value).map_err(|error| AppError::internal(error.to_string()))?;
    Ok(object_store::digest(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_resource_ref_is_stable_under_rename_and_section_move() {
        let course = Course {
            id: "42".into(),
            reference: "course:42".into(),
            title: "Course".into(),
            code: None,
            term: None,
            url: "https://klms.example/course/view.php?id=42".into(),
        };
        let activity = |title: &str, section: &str| Activity {
            id: None,
            reference: None,
            kind: "label".into(),
            title: title.into(),
            week: None,
            section: Some(section.into()),
            url: Some("https://klms.example/local/item?id=9".into()),
            external: false,
        };
        let before = resource_reference(&course, &activity("Old", "Week 1")).unwrap();
        let after = resource_reference(&course, &activity("New", "Week 2")).unwrap();
        assert_eq!(before, after);
    }
}
