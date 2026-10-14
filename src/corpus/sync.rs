use super::{
    Corpus, SyncSummary, object_store,
    observe::{PendingResource, Run, digest_json, representation_kind},
    query::{refresh_subject, row},
};
use crate::date::epoch_now as now;
use crate::url::{Url, query_value};
use crate::{
    client::{KlmsClient, RemoteMetadata},
    error::AppError,
    models::{Activity, Course, LinkItem},
    parse,
};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

const MAX_DOWNLOAD: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct SyncOptions {
    pub notices: bool,
    pub files: bool,
    pub download_changed: bool,
}

/// A course with its resources and whether its manifest was fetched completely.
type Collection = (Course, Vec<PendingResource>, bool);
/// Newest content observation of a representation: sha256, etag, last-modified, length.
type Bound = (String, Option<String>, Option<String>, i64);

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
                    let complete =
                        detail_failures.is_empty() && resources.iter().all(|r| r.complete);
                    failures.append(&mut detail_failures);
                    collections.push((course, resources, complete));
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
            let course_id = x.upsert_course(course)?;
            listed.insert(course.reference.clone());
            refresh_subject(x.c, &course.reference)?;
            for resource in resources {
                let resource_id = x.upsert_resource(course_id, resource)?;
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
                    let id = x.upsert_representation(resource_id, &url, &link.title)?;
                    if representation_kind(&url) == "file" {
                        frontier.insert(id, url.as_str().to_owned());
                    }
                    representation_count += 1;
                }
                if resource.representations_complete {
                    x.mark_missing_representations(resource_id, &seen_urls)?;
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
        let status = if failures.is_empty() && collections.iter().all(|c| c.2) {
            "complete"
        } else {
            "incomplete"
        };
        let source_complete = status == "complete" && filter.is_none();
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
        let posts = match parse::board_posts(&page.text, base_url, query_value(&page.url, "id")) {
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

fn activity_container(value: &str) -> bool {
    Url::parse(value)
        .is_ok_and(|url| url.path().starts_with("/mod/") && url.path().ends_with("/view.php"))
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
