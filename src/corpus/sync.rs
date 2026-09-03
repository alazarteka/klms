use super::{Corpus, SyncSummary, object_store, query::refresh_subject, storage::now};
use crate::{
    client::{KlmsClient, RemoteMetadata},
    error::AppError,
    models::{Activity, Course, LinkItem},
    parse,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use url::Url;
const MAX_DOWNLOAD: usize = 128 * 1024 * 1024;
#[derive(Clone, Copy)]
pub struct SyncOptions {
    pub notices: bool,
    pub files: bool,
    pub download_changed: bool,
}
struct PendingResource {
    reference: String,
    course_ref: String,
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
struct CourseCollection {
    course: Course,
    resources: Vec<PendingResource>,
    manifest_complete: bool,
}
struct ExistingCourse {
    id: i64,
    state: String,
    digest: Option<String>,
}
struct ExistingResource {
    id: i64,
    state: String,
    digest: Option<String>,
}
struct ExistingRepresentation {
    id: i64,
    state: String,
    digest: Option<String>,
}
struct ValidationTarget {
    id: i64,
    url: String,
}
struct BoundContent {
    sha256: String,
    etag: Option<String>,
    last_modified: Option<String>,
    length: i64,
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
        self.storage.connection.execute(
            "INSERT INTO sync_runs(started_at,scope,status) VALUES(?1,?2,'running')",
            params![started_at, filter.unwrap_or("all")],
        )?;
        let run_id = self.storage.connection.last_insert_rowid();
        match self.collect_sync(client, base_url, filter, options, run_id, started_at) {
            Ok(summary) => Ok(summary),
            Err(error) => {
                let failures = serde_json::to_string(&vec![error.message.clone()])
                    .unwrap_or_else(|_| "[]".into());
                let _ = self.storage.connection.execute(
                    "UPDATE sync_runs SET finished_at=?1,status='failed',failures=?2 WHERE id=?3",
                    params![now(), failures, run_id],
                );
                Err(error)
            }
        }
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
        let dashboard = parse::dashboard(&response.text, base_url)?;
        let list_complete = dashboard.courses_complete;
        let mut courses = dashboard.courses;
        if let Some(value) = filter {
            courses = resolve_course(courses, value)?;
            self.storage.connection.execute(
                "UPDATE sync_runs SET scope=?1 WHERE id=?2",
                params![courses[0].reference, run_id],
            )?;
        }
        let mut collections = Vec::new();
        let mut failures = Vec::new();
        for course in courses {
            match collect_course(client, base_url, course.clone(), options) {
                Ok((resources, mut detail_failures)) => {
                    failures.append(&mut detail_failures);
                    collections.push(CourseCollection {
                        course,
                        resources,
                        manifest_complete: true,
                    });
                }
                Err(error) => {
                    failures.push(format!("{}: {}", course.reference, error.message));
                    collections.push(CourseCollection {
                        course,
                        resources: Vec::new(),
                        manifest_complete: false,
                    });
                }
            }
        }
        let transaction = self
            .storage
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut course_ids = HashSet::new();
        let mut resources_seen: HashMap<String, HashSet<String>> = HashMap::new();
        let mut frontier = HashSet::new();
        let mut resource_count = 0_u64;
        let mut representation_count = 0_u64;
        let mut truncated_count = 0_u64;
        for collection in &collections {
            let course_id = upsert_course(&transaction, run_id, observed_at, &collection.course)?;
            course_ids.insert(course_id);
            refresh_subject(&transaction, &collection.course.reference)?;
            for resource in &collection.resources {
                resources_seen
                    .entry(resource.course_ref.clone())
                    .or_default()
                    .insert(resource.reference.clone());
                let resource_id = upsert_resource(&transaction, run_id, observed_at, resource)?;
                resource_count += 1;
                if resource.observe && !resource.complete {
                    truncated_count += 1;
                }
                let mut links = resource.links.clone();
                if let Some(url) = resource
                    .url
                    .as_deref()
                    .filter(|url| !activity_container(url))
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
                    let representation_id = upsert_representation(
                        &transaction,
                        run_id,
                        observed_at,
                        resource_id,
                        &url,
                        &link.title,
                    )?;
                    frontier.insert(representation_id);
                    representation_count += 1;
                }
                if resource.representations_complete {
                    mark_missing_representations(
                        &transaction,
                        run_id,
                        observed_at,
                        resource_id,
                        &seen_urls,
                    )?;
                }
                refresh_subject(&transaction, &resource.reference)?;
            }
        }
        if list_complete && filter.is_none() {
            mark_missing_courses(&transaction, run_id, observed_at, &course_ids)?;
        }
        for collection in &collections {
            if collection.manifest_complete {
                mark_missing_resources(
                    &transaction,
                    run_id,
                    observed_at,
                    &collection.course.reference,
                    resources_seen.get(&collection.course.reference),
                )?;
            }
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
        let source_complete = list_complete && failures.is_empty() && filter.is_none();
        let changes = self.storage.connection.query_row(
            "SELECT COUNT(*) FROM remote_changes WHERE sync_run_id=?1",
            [run_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let encoded_failures = serde_json::to_string(&failures)
            .map_err(|error| AppError::internal(error.to_string()))?;
        self.storage.connection.execute(
            "UPDATE sync_runs
                SET finished_at=?1,status=?2,source_complete=?3,failures=?4
              WHERE id=?5",
            params![
                now(),
                status,
                source_complete as i64,
                encoded_failures,
                run_id
            ],
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
    fn validate_frontier(
        &mut self,
        client: &KlmsClient,
        run_id: i64,
        download: bool,
        frontier: &HashSet<i64>,
    ) -> Result<(u64, Vec<String>), AppError> {
        let targets = {
            let mut statement = self
                .storage
                .connection
                .prepare("SELECT id,url FROM representations WHERE id=?1 AND kind='file'")?;
            let mut values = Vec::new();
            for id in frontier {
                if let Some(target) = statement
                    .query_row([id], |row| {
                        Ok(ValidationTarget {
                            id: row.get(0)?,
                            url: row.get(1)?,
                        })
                    })
                    .optional()?
                {
                    values.push(target);
                }
            }
            values
        };
        let mut blobs_added = 0_u64;
        let mut failures = Vec::new();
        for target in targets {
            let metadata = match client.head(&target.url) {
                Ok(metadata) => metadata,
                Err(error) => {
                    failures.push(format!("representation:{}: {}", target.id, error.message));
                    continue;
                }
            };
            self.update_metadata(target.id, &metadata)?;
            if !download {
                continue;
            }
            let bound = latest_bound_content(&self.storage.connection, target.id)?;
            if bound
                .as_ref()
                .is_some_and(|bound| validators_match(bound, &metadata))
            {
                continue;
            }
            let conditional = client.get_conditional(
                &target.url,
                bound.as_ref().and_then(|row| row.etag.as_deref()),
                bound.as_ref().and_then(|row| row.last_modified.as_deref()),
                MAX_DOWNLOAD,
            );
            let response = match conditional {
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("representation:{}: {}", target.id, error.message));
                    continue;
                }
            };
            self.update_metadata(target.id, &response.metadata)?;
            let Some(bytes) = response.bytes else {
                continue;
            };
            let object = object_store::store(&self.storage.paths.objects, &bytes)?;
            let transaction = self
                .storage
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO blobs(sha256,byte_length,mime,stored_at)
                 VALUES(?1,?2,?3,?4)",
                params![
                    object.sha256,
                    object.bytes as i64,
                    response.metadata.content_type,
                    now()
                ],
            )?;
            blobs_added += inserted as u64;
            let previous = latest_bound_content(&transaction, target.id)?;
            if previous.as_ref().map(|row| row.sha256.as_str()) != Some(&object.sha256) {
                transaction.execute(
                    "INSERT INTO content_observations(
                       representation_id,sync_run_id,observed_at,sha256,etag,
                       last_modified,byte_length,mime
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        target.id,
                        run_id,
                        now(),
                        object.sha256,
                        response.metadata.etag,
                        response.metadata.last_modified,
                        object.bytes as i64,
                        response.metadata.content_type
                    ],
                )?;
                if let Some(previous) = previous {
                    change(
                        &transaction,
                        (run_id, now()),
                        "verified_content_changed",
                        &format!("representation:{}", target.id),
                        Some(&format!("sha256:{}", previous.sha256)),
                        Some(&format!("sha256:{}", object.sha256)),
                        json!({}),
                    )?;
                }
            }
            transaction.commit()?;
        }
        Ok((blobs_added, failures))
    }
    fn update_metadata(&self, id: i64, metadata: &RemoteMetadata) -> Result<(), AppError> {
        self.storage.connection.execute(
            "UPDATE representations
                SET observed_etag=?1,observed_last_modified=?2,
                    observed_length=?3,observed_mime=?4
              WHERE id=?5",
            params![
                metadata.etag,
                metadata.last_modified,
                metadata.content_length.map(|value| value as i64),
                metadata.content_type,
                id
            ],
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
                || course
                    .code
                    .as_deref()
                    .is_some_and(|code| code.eq_ignore_ascii_case(filter))
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
    course: Course,
    options: SyncOptions,
) -> Result<(Vec<PendingResource>, Vec<String>), AppError> {
    let response = client.get(&format!("/course/view.php?id={}", course.id))?;
    let activities = parse::activities(&response.text, base_url, None)?;
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for activity in activities {
        let reference = library_resource_reference(&course, &activity)?;
        let activity_source = serde_json::to_value(&activity)
            .map_err(|error| AppError::internal(error.to_string()))?;
        let mut row = PendingResource {
            reference: reference.clone(),
            course_ref: course.reference.clone(),
            kind: activity.kind.clone(),
            title: activity.title.clone(),
            url: activity.url.clone(),
            week: activity.week,
            section: activity.section.clone(),
            text: None,
            source: json!({"activity": activity_source, "detail": {"state": "not_requested"}}),
            links: Vec::new(),
            observe: true,
            access_lost: false,
            complete: true,
            representations_complete: activity
                .url
                .as_deref()
                .is_some_and(|url| !activity_container(url)),
        };
        let detail_wanted = !activity.external
            && activity.url.as_deref().is_some_and(activity_container)
            && matches!(
                activity.kind.as_str(),
                "assign" | "quiz" | "page" | "folder" | "resource" | "coursefile"
            );
        if detail_wanted {
            if let Some(url) = &activity.url {
                match client.get(url).and_then(|response| {
                    parse::resource_detail(&response.text, base_url, &response.url, &activity.kind)
                }) {
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
        }
        if options.notices && activity.kind == "courseboard" {
            if let Some(url) = &activity.url {
                let (mut notices, mut notice_failures) =
                    collect_board(client, base_url, &course, url);
                rows.append(&mut notices);
                failures.append(&mut notice_failures);
            }
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
    let mut next = Some(start.to_owned());
    let mut visited = HashSet::new();
    for _ in 0..20 {
        let Some(url) = next.take() else {
            return (rows, failures);
        };
        if !visited.insert(url.clone()) {
            failures.push(format!(
                "{}: notice pagination cycle detected",
                course.reference
            ));
            break;
        }
        let page = match client.get(&url) {
            Ok(page) => page,
            Err(error) => {
                failures.push(format!("{}: {}", course.reference, error.message));
                break;
            }
        };
        let board_id = query(&page.url, "id");
        let posts = match parse::board_posts(&page.text, base_url, board_id) {
            Ok(posts) => posts,
            Err(error) => {
                failures.push(format!("{}: {}", course.reference, error.message));
                break;
            }
        };
        for post in posts {
            let Some(reference) = post.reference else {
                continue;
            };
            let detail = match client.get(&post.url).and_then(|response| {
                parse::resource_detail(&response.text, base_url, &response.url, "courseboard-post")
            }) {
                Ok(detail) => detail,
                Err(error) => {
                    failures.push(format!("{reference}: {}", error.message));
                    continue;
                }
            };
            let complete = !detail.text_truncated && !detail.links_truncated;
            rows.push(PendingResource {
                reference,
                course_ref: course.reference.clone(),
                kind: "notice".into(),
                title: detail.title.clone(),
                url: Some(detail.url.clone()),
                week: None,
                section: None,
                text: Some(detail.text.clone()),
                source: serde_json::to_value(&detail).unwrap_or(Value::Null),
                links: detail.links,
                observe: true,
                access_lost: false,
                complete,
                representations_complete: !detail.links_truncated,
            });
        }
        next = match parse::next_page_url(&page.text, base_url) {
            Ok(next) => next,
            Err(error) => {
                failures.push(format!("{}: {}", course.reference, error.message));
                break;
            }
        };
    }
    if next.is_some() {
        failures.push(format!(
            "{}: notice pagination exceeded 20 pages",
            course.reference
        ));
    }
    (rows, failures)
}
fn upsert_course(
    transaction: &Transaction<'_>,
    run_id: i64,
    at: i64,
    course: &Course,
) -> Result<i64, AppError> {
    let digest = digest_json(course)?;
    let existing = transaction
        .query_row(
            "SELECT c.id,c.remote_state,
                (SELECT digest FROM course_observations
                  WHERE course_id=c.id ORDER BY id DESC LIMIT 1)
           FROM courses c WHERE c.ref=?1",
            [&course.reference],
            |row| {
                Ok(ExistingCourse {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    digest: row.get(2)?,
                })
            },
        )
        .optional()?;
    let (id, event) = if let Some(existing) = existing {
        transaction.execute(
            "UPDATE courses SET remote_state='listed',last_seen=?1,not_listed_since=NULL
              WHERE id=?2",
            params![at, existing.id],
        )?;
        let event = if existing.state != "listed" {
            Some("course_reappeared")
        } else if existing.digest.as_deref() != Some(&digest) {
            Some("course_source_changed")
        } else {
            None
        };
        (existing.id, event)
    } else {
        transaction.execute(
            "INSERT INTO courses(ref,first_seen,last_seen) VALUES(?1,?2,?2)",
            params![course.reference, at],
        )?;
        (transaction.last_insert_rowid(), Some("course_appeared"))
    };
    let previous = transaction
        .query_row(
            "SELECT digest FROM course_observations
          WHERE course_id=?1 ORDER BY id DESC LIMIT 1",
            [id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if previous.as_deref() != Some(&digest) {
        transaction.execute(
            "INSERT INTO course_observations(
               course_id,sync_run_id,observed_at,digest,title,code,term,url
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                id,
                run_id,
                at,
                digest,
                course.title,
                course.code,
                course.term,
                course.url
            ],
        )?;
    }
    if let Some(kind) = event {
        change(
            transaction,
            (run_id, at),
            kind,
            &course.reference,
            previous.as_deref(),
            Some(&digest),
            json!({"title": course.title}),
        )?;
    }
    Ok(id)
}
fn upsert_resource(
    transaction: &Transaction<'_>,
    run_id: i64,
    at: i64,
    resource: &PendingResource,
) -> Result<i64, AppError> {
    let course_id: i64 = transaction.query_row(
        "SELECT id FROM courses WHERE ref=?1",
        [&resource.course_ref],
        |row| row.get::<_, i64>(0),
    )?;
    let existing = transaction
        .query_row(
            "SELECT r.id,r.remote_state,
                (SELECT digest FROM resource_observations
                  WHERE resource_id=r.id ORDER BY id DESC LIMIT 1)
           FROM resources r WHERE r.ref=?1",
            [&resource.reference],
            |row| {
                Ok(ExistingResource {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    digest: row.get(2)?,
                })
            },
        )
        .optional()?;
    let desired = if resource.access_lost {
        Some("access_lost")
    } else if resource.observe {
        Some("present")
    } else {
        None
    };
    let previous_state = existing.as_ref().map(|row| row.state.clone());
    let id = if let Some(existing) = &existing {
        if let Some(state) = desired {
            transaction.execute(
                "UPDATE resources SET last_seen=?1,remote_state=?2,
                        not_observed_since=NULL WHERE id=?3",
                params![at, state, existing.id],
            )?;
        }
        existing.id
    } else {
        transaction.execute(
            "INSERT INTO resources(
               ref,course_id,kind,remote_state,first_seen,last_seen
             ) VALUES(?1,?2,?3,?4,?5,?5)",
            params![
                resource.reference,
                course_id,
                resource.kind,
                desired.unwrap_or("present"),
                at
            ],
        )?;
        transaction.last_insert_rowid()
    };
    let digest = digest_json(&resource.source)?;
    let previous = existing.as_ref().and_then(|row| row.digest.as_deref());
    if (resource.observe || previous.is_none()) && previous != Some(&digest) {
        transaction.execute(
            "INSERT INTO resource_observations(
               resource_id,sync_run_id,observed_at,digest,complete,title,url,
               week,section,text,source_json
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                id,
                run_id,
                at,
                digest,
                resource.complete as i64,
                resource.title,
                resource.url,
                resource.week,
                resource.section,
                resource.text,
                resource.source.to_string()
            ],
        )?;
        change(
            transaction,
            (run_id, at),
            if previous.is_some() {
                "source_changed"
            } else {
                "resource_appeared"
            },
            &resource.reference,
            previous,
            Some(&digest),
            json!({"kind": resource.kind}),
        )?;
    }
    if let (Some(before), Some(after)) = (previous_state.as_deref(), desired) {
        if before != after {
            let kind = match (before, after) {
                ("not_observed", "present") => "resource_restored",
                (_, "access_lost") => "access_lost",
                ("access_lost", "present") => "access_restored",
                _ => "resource_restored",
            };
            change(
                transaction,
                (run_id, at),
                kind,
                &resource.reference,
                None,
                None,
                json!({}),
            )?;
        }
    }
    Ok(id)
}
fn upsert_representation(
    transaction: &Transaction<'_>,
    run_id: i64,
    at: i64,
    resource_id: i64,
    url: &Url,
    filename: &str,
) -> Result<i64, AppError> {
    let digest = object_store::digest(format!("{}\n{filename}", url.as_str()).as_bytes());
    let existing = transaction
        .query_row(
            "SELECT p.id,p.remote_state,
                (SELECT digest FROM representation_observations
                  WHERE representation_id=p.id ORDER BY id DESC LIMIT 1)
           FROM representations p WHERE p.resource_id=?1 AND p.url=?2",
            params![resource_id, url.as_str()],
            |row| {
                Ok(ExistingRepresentation {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    digest: row.get(2)?,
                })
            },
        )
        .optional()?;
    let (id, restored) = if let Some(existing) = &existing {
        transaction.execute(
            "UPDATE representations SET remote_state='present',last_seen=?1,
                    not_observed_since=NULL WHERE id=?2",
            params![at, existing.id],
        )?;
        (existing.id, existing.state != "present")
    } else {
        transaction.execute(
            "INSERT INTO representations(resource_id,url,kind,first_seen,last_seen)
             VALUES(?1,?2,?3,?4,?4)",
            params![resource_id, url.as_str(), representation_kind(url), at],
        )?;
        (transaction.last_insert_rowid(), false)
    };
    let previous = existing.as_ref().and_then(|row| row.digest.as_deref());
    if previous != Some(&digest) {
        transaction.execute(
            "INSERT INTO representation_observations(
               representation_id,sync_run_id,observed_at,digest,filename
             ) VALUES(?1,?2,?3,?4,?5)",
            params![id, run_id, at, digest, filename],
        )?;
        change(
            transaction,
            (run_id, at),
            if previous.is_some() {
                "representation_source_changed"
            } else {
                "representation_appeared"
            },
            &format!("representation:{id}"),
            previous,
            Some(&digest),
            json!({}),
        )?;
    }
    if restored {
        change(
            transaction,
            (run_id, at),
            "representation_restored",
            &format!("representation:{id}"),
            None,
            None,
            json!({}),
        )?;
    }
    refresh_subject(transaction, &format!("representation:{id}"))?;
    Ok(id)
}
fn mark_missing_courses(
    transaction: &Transaction<'_>,
    run_id: i64,
    at: i64,
    seen: &HashSet<i64>,
) -> Result<(), AppError> {
    let rows = {
        let mut statement =
            transaction.prepare("SELECT id,ref FROM courses WHERE remote_state='listed'")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, reference) in rows {
        if !seen.contains(&id) {
            transaction.execute(
                "UPDATE courses SET remote_state='not_listed',not_listed_since=?1 WHERE id=?2",
                params![at, id],
            )?;
            change(
                transaction,
                (run_id, at),
                "course_not_listed",
                &reference,
                None,
                None,
                json!({}),
            )?;
        }
    }
    Ok(())
}
fn mark_missing_resources(
    transaction: &Transaction<'_>,
    run_id: i64,
    at: i64,
    course_ref: &str,
    seen: Option<&HashSet<String>>,
) -> Result<(), AppError> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT r.id,r.ref FROM resources r JOIN courses c ON c.id=r.course_id
              WHERE c.ref=?1 AND r.remote_state='present' AND r.kind!='notice'",
        )?;
        statement
            .query_map([course_ref], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, reference) in rows {
        if !seen.is_some_and(|values| values.contains(&reference)) {
            transaction.execute(
                "UPDATE resources SET remote_state='not_observed',
                        not_observed_since=?1 WHERE id=?2",
                params![at, id],
            )?;
            change(
                transaction,
                (run_id, at),
                "resource_not_observed",
                &reference,
                None,
                None,
                json!({"collection": "course_manifest"}),
            )?;
        }
    }
    Ok(())
}
fn mark_missing_representations(
    transaction: &Transaction<'_>,
    run_id: i64,
    at: i64,
    resource_id: i64,
    seen: &HashSet<String>,
) -> Result<(), AppError> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT id,url FROM representations
              WHERE resource_id=?1 AND remote_state='present'",
        )?;
        statement
            .query_map([resource_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, url) in rows {
        if !seen.contains(&url) {
            transaction.execute(
                "UPDATE representations SET remote_state='not_observed',
                        not_observed_since=?1 WHERE id=?2",
                params![at, id],
            )?;
            change(
                transaction,
                (run_id, at),
                "representation_not_observed",
                &format!("representation:{id}"),
                None,
                None,
                json!({"collection": "resource_detail"}),
            )?;
        }
    }
    // Also clear stale index entries left by earlier versions after a link
    // had already become not_observed. Keep all history and file entries.
    transaction.execute(
        "DELETE FROM search_documents WHERE subject_ref IN (
            SELECT 'representation:'||p.id FROM representations p
              JOIN resources r ON r.id=p.resource_id
             WHERE p.resource_id=?1 AND r.kind='notice' AND p.kind='link'
               AND p.remote_state='not_observed')",
        [resource_id],
    )?;
    Ok(())
}
fn latest_bound_content(
    connection: &rusqlite::Connection,
    representation_id: i64,
) -> Result<Option<BoundContent>, AppError> {
    connection
        .query_row(
            "SELECT sha256,etag,last_modified,byte_length
           FROM content_observations
          WHERE representation_id=?1 ORDER BY id DESC LIMIT 1",
            [representation_id],
            |row| {
                Ok(BoundContent {
                    sha256: row.get(0)?,
                    etag: row.get(1)?,
                    last_modified: row.get(2)?,
                    length: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(AppError::from)
}
fn validators_match(bound: &BoundContent, metadata: &RemoteMetadata) -> bool {
    match (bound.etag.as_deref(), metadata.etag.as_deref()) {
        (Some(before), Some(after)) => before == after,
        _ => {
            bound.last_modified.as_deref() == metadata.last_modified.as_deref()
                && bound.last_modified.is_some()
                && metadata.content_length.map(|value| value as i64) == Some(bound.length)
        }
    }
}
fn stable_resource_reference(course: &Course, activity: &Activity) -> Result<String, AppError> {
    let identity = if let Some(id) = activity.id.as_deref() {
        json!({"course_ref": course.reference, "module": id})
    } else if let Some(url) = activity.url.as_deref() {
        json!({"course_ref": course.reference, "url": url})
    } else {
        return Err(AppError::shape("activity has no stable identity"));
    };
    Ok(format!("resource:{}", &digest_json(&identity)?[..24]))
}
fn library_resource_reference(course: &Course, activity: &Activity) -> Result<String, AppError> {
    match activity.reference.as_deref() {
        Some(reference) => Ok(reference.to_owned()),
        None => stable_resource_reference(course, activity),
    }
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
fn query(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find_map(|(name, value)| (name == key).then(|| value.into_owned()))
}
fn digest_json(value: &impl Serialize) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(value).map_err(|error| AppError::internal(error.to_string()))?;
    Ok(object_store::digest(&bytes))
}
fn change(
    transaction: &Transaction<'_>,
    run_and_time: (i64, i64),
    kind: &str,
    subject: &str,
    before: Option<&str>,
    after: Option<&str>,
    details: Value,
) -> Result<(), AppError> {
    transaction.execute(
        "INSERT INTO remote_changes(
           sync_run_id,occurred_at,kind,subject_ref,before_ref,after_ref,details_json
         ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            run_and_time.0,
            run_and_time.1,
            kind,
            subject,
            before,
            after,
            details.to_string()
        ],
    )?;
    Ok(())
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
        let before = stable_resource_reference(&course, &activity("Old", "Week 1")).unwrap();
        let after = stable_resource_reference(&course, &activity("New", "Week 2")).unwrap();
        assert_eq!(before, after);
    }
}
