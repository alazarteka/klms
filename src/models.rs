use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Course {
    pub id: String,
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
    pub upcoming: Vec<LinkItem>,
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
    pub kind: String,
    pub title: String,
    pub week: Option<u32>,
    pub section: Option<String>,
    pub url: Option<String>,
    pub external: bool,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub course_id: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}
