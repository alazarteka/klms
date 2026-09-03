use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct Course {
    pub id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    pub code: Option<String>,
    pub term: Option<String>,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct Dashboard {
    pub term: Option<String>,
    pub course_count: usize,
    pub courses: Vec<Course>,
    pub courses_complete: bool,
    pub upcoming_count: usize,
    pub upcoming: Vec<LinkItem>,
    pub upcoming_complete: bool,
}

#[derive(Debug, Serialize)]
pub struct CourseDetail {
    #[serde(flatten)]
    pub course: Course,
    pub professors: Vec<String>,
    pub activity_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkItem {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub id: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub kind: String,
    pub title: String,
    pub week: Option<u32>,
    pub section: Option<String>,
    pub url: Option<String>,
    pub external: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assignment {
    pub id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub course_id: String,
    pub course_ref: String,
    pub week: Option<u32>,
    pub title: String,
    pub due_at: Option<String>,
    pub due_text: Option<String>,
    pub submission_status: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Quiz {
    pub id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub course_id: String,
    pub course_ref: String,
    pub week: Option<u32>,
    pub title: String,
    pub closes_at: Option<String>,
    pub closes_text: Option<String>,
    pub grade: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalendarEvent {
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub kind: String,
    pub title: String,
    pub course_id: Option<String>,
    pub course: Option<String>,
    pub starts_at: Option<String>,
    pub when_text: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Notice {
    #[serde(rename = "ref")]
    pub reference: String,
    pub board_ref: String,
    pub course_id: String,
    pub course_ref: String,
    pub title: String,
    pub posted_at: Option<String>,
    pub posted_text: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileResource {
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub id: Option<String>,
    pub kind: String,
    pub title: String,
    pub course_id: String,
    pub course_ref: String,
    pub week: Option<u32>,
    pub section: Option<String>,
    pub url: Option<String>,
    pub downloadable: bool,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub course_id: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ResourceDetail {
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board_id: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub kind: String,
    pub title: String,
    pub url: String,
    pub text: String,
    pub text_truncated: bool,
    pub links: Vec<LinkItem>,
    pub links_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct BoardPost {
    pub board_id: Option<String>,
    pub id: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub title: String,
    pub posted: Option<String>,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct SessionTime {
    pub remaining_seconds: u64,
    pub remaining: String,
    pub bootstrap_may_have_extended_session: bool,
    pub extended: bool,
}

#[derive(Debug, Serialize)]
pub struct DownloadResult {
    pub path: String,
    pub bytes: usize,
    pub source_url: String,
    pub content_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RawGet {
    pub url: String,
    pub content_type: Option<String>,
    pub bytes: usize,
    pub body: String,
    pub truncated: bool,
    pub redacted: bool,
}

#[derive(Debug, Serialize)]
pub struct LastSync {
    #[serde(rename = "ref")]
    pub reference: String,
    pub scope: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub source_complete: bool,
}

#[derive(Debug, Serialize)]
pub struct LibraryStatus {
    pub database_path: String,
    pub object_store_path: String,
    pub schema_version: u32,
    pub created: bool,
    pub courses: u64,
    pub resources: u64,
    pub representations: u64,
    pub blobs: u64,
    pub stored_bytes: u64,
    pub last_sync: Option<LastSync>,
    pub fresh_through: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SyncSummary {
    #[serde(rename = "ref")]
    pub reference: String,
    pub status: String,
    pub source_complete: bool,
    pub courses: u64,
    pub resources: u64,
    pub representations: u64,
    pub blobs_added: u64,
    pub changes: u64,
    /// Resources whose detail page hit a parser cap; their observation is
    /// stored as incomplete without failing the run.
    pub truncated: u64,
    pub failures: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: String,
    pub course_ref: Option<String>,
    pub title: String,
    pub snippet: String,
    pub has_content: bool,
}

#[derive(Debug, Serialize)]
pub struct ChangeEntry {
    pub id: i64,
    pub occurred_at: i64,
    pub kind: String,
    pub subject_ref: String,
    pub before_ref: Option<String>,
    pub after_ref: Option<String>,
    pub details: Value,
}

#[derive(Debug, Serialize)]
pub struct ActivityEntry {
    #[serde(rename = "ref")]
    pub reference: String,
    pub subject_ref: String,
    pub field: String,
    pub value: String,
    pub actor: String,
    pub revision: i64,
    pub created_at: i64,
    pub retracted: bool,
}

#[derive(Debug, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub observed_at: i64,
    pub kind: String,
    pub digest: String,
    pub source: Value,
}

pub struct ContentRecord {
    pub reference: String,
    pub path: PathBuf,
    pub byte_length: u64,
    pub mime: Option<String>,
    pub filename: String,
}

#[derive(Debug, Serialize)]
pub struct EditResult {
    #[serde(rename = "ref")]
    pub reference: String,
    pub subject_ref: String,
    pub field: String,
    pub before: Option<String>,
    pub after: String,
    pub revision: i64,
    pub actor: String,
}

#[derive(Debug, Serialize)]
pub struct RetractionResult {
    #[serde(rename = "ref")]
    pub reference: String,
    pub target_ref: String,
    pub actor: String,
}

#[derive(Debug, Serialize)]
pub struct RelationResult {
    #[serde(rename = "ref")]
    pub reference: String,
}
