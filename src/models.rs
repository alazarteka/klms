use serde::Serialize;

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

#[derive(Debug, Serialize)]
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
