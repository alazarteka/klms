use scraper::Html;
use url::Url;

use super::shared::{indexed_rows, selector, semantic_table, text, week_number};
use crate::{
    error::AppError,
    models::{Assignment, Course, Quiz, Report},
    reference::ResourceRef,
    safe_url,
};

pub fn assignments(
    html: &str,
    page_url: &Url,
    course: &Course,
) -> Result<Vec<Assignment>, AppError> {
    let document = Html::parse_document(html);
    let table = semantic_table(&document, &["week", "name", "due date"])?;
    let Some(table) = table else {
        if explicit_empty(&document, "assignment") {
            return Ok(Vec::new());
        }
        return Err(AppError::shape(
            "assignment index contained no recognizable assignment table",
        ));
    };
    let parsed = indexed_rows(table)?;
    let mut assignments = Vec::new();
    for row in parsed.rows {
        let title = row.value("name");
        let Some(url) = row.link_for("name", page_url) else {
            if title.is_some() {
                return Err(AppError::shape(
                    "assignment row contained no recognizable detail link",
                ));
            }
            continue;
        };
        let Some(id) = super::shared::query_id(&url, &["id"]) else {
            return Err(AppError::shape(
                "assignment detail link contained no numeric module id",
            ));
        };
        let due_text = row.value("due date");
        assignments.push(Assignment {
            id: id.clone(),
            reference: ResourceRef::Assignment(id).to_string(),
            course_id: course.id.clone(),
            course_ref: course.reference.clone(),
            week: row.value("week").as_deref().and_then(week_number),
            title: title.unwrap_or_else(|| "Untitled assignment".into()),
            due_at: due_text.as_deref().and_then(crate::date::moodle_datetime),
            due_text,
            submission_status: row.value("submit"),
            url: safe_url::display(&url),
        });
    }
    Ok(assignments)
}

pub fn quizzes(html: &str, page_url: &Url, course: &Course) -> Result<Vec<Quiz>, AppError> {
    let document = Html::parse_document(html);
    let table = semantic_table(&document, &["week", "name", "quiz closes"])?;
    let Some(table) = table else {
        if explicit_empty(&document, "quiz") {
            return Ok(Vec::new());
        }
        return Err(AppError::shape(
            "quiz index contained no recognizable quiz table",
        ));
    };
    let parsed = indexed_rows(table)?;
    let mut quizzes = Vec::new();
    for row in parsed.rows {
        let title = row.value("name");
        let Some(url) = row.link_for("name", page_url) else {
            if title.is_some() {
                return Err(AppError::shape(
                    "quiz row contained no recognizable detail link",
                ));
            }
            continue;
        };
        let Some(id) = super::shared::query_id(&url, &["id"]) else {
            return Err(AppError::shape(
                "quiz detail link contained no numeric module id",
            ));
        };
        let closes_text = row.value("quiz closes");
        quizzes.push(Quiz {
            id: id.clone(),
            reference: ResourceRef::Quiz(id).to_string(),
            course_id: course.id.clone(),
            course_ref: course.reference.clone(),
            week: row.value("week").as_deref().and_then(week_number),
            title: title.unwrap_or_else(|| "Untitled quiz".into()),
            closes_at: closes_text
                .as_deref()
                .and_then(crate::date::moodle_datetime),
            closes_text,
            grade: row.value("grade"),
            url: safe_url::display(&url),
        });
    }
    Ok(quizzes)
}

fn explicit_empty(document: &Html, kind: &str) -> bool {
    let page_text = document
        .root_element()
        .text()
        .collect::<String>()
        .to_ascii_lowercase();
    match kind {
        "assignment" => {
            page_text.contains("there are no assignments")
                || page_text.contains("no assignments found")
                || page_text.contains("과제가 없습니다")
        }
        "quiz" => {
            page_text.contains("there are no quizzes")
                || page_text.contains("no quizzes found")
                || page_text.contains("퀴즈가 없습니다")
        }
        _ => false,
    }
}

pub fn grades(html: &str, course_id: String) -> Result<Report, AppError> {
    table_report(
        html,
        course_id,
        &["grade item", "percentage", "contribution"],
    )
}

pub fn attendance(html: &str, course_id: String) -> Result<Report, AppError> {
    table_report(html, course_id, &["date", "attended", "absent"])
}

fn table_report(html: &str, course_id: String, expected: &[&str]) -> Result<Report, AppError> {
    let document = Html::parse_document(html);
    let tables = selector("table")?;
    let rows = selector("tr")?;
    let cells = selector("th, td")?;
    for table in document.select(&tables) {
        let headers: Vec<_> = table
            .select(&rows)
            .next()
            .map(|row| row.select(&cells).map(text).collect())
            .unwrap_or_default();
        let joined = headers.join(" ").to_ascii_lowercase();
        if !expected.iter().any(|needle| joined.contains(needle)) {
            continue;
        }
        let mut values = Vec::new();
        for row in table.select(&rows) {
            let row_values: Vec<_> = row.select(&cells).map(text).collect();
            if row_values.is_empty() || row_values == headers {
                continue;
            }
            values.push(row_values);
        }
        return Ok(Report {
            course_id,
            headers,
            rows: values,
        });
    }
    Err(AppError::shape("could not find the expected report table"))
}

#[cfg(test)]
mod tests {
    use super::{assignments, attendance, grades, quizzes};
    use crate::models::Course;
    use url::Url;

    const BASE: &str = "https://klms.kaist.ac.kr";

    #[test]
    fn parses_report_tables_by_semantics() {
        let grade_html = "<table><thead><tr><th>Grade item</th><th>Percentage</th></tr></thead><tbody><tr><td>Quiz</td><td>90%</td></tr></tbody></table>";
        assert_eq!(grades(grade_html, "42".into()).unwrap().rows.len(), 1);
        let attendance_html = "<table><tr><th>Date</th><th>Attended</th><th>Absent</th></tr><tr><td>Aug 1</td><td>Y</td><td></td></tr></table>";
        assert_eq!(
            attendance(attendance_html, "42".into()).unwrap().rows.len(),
            1
        );
    }

    #[test]
    fn parses_typed_assignment_and_quiz_indexes() {
        let base = Url::parse(BASE).unwrap();
        let course = Course {
            id: "42".into(),
            reference: "course:42".into(),
            title: "Compilers".into(),
            code: Some("CS.420".into()),
            term: None,
            url: format!("{BASE}/course/view.php?id=42"),
        };
        let assignment_html = r#"<table><tr><th>No</th><th>Week</th><th>Name</th><th>Due date</th><th>Submit</th></tr>
          <tr><td>1</td><td>week 2</td><td><a href='/mod/assign/view.php?id=7'>Written work</a></td><td>Tuesday, 17 March 2026, 11:59 PM</td><td>Submitted for grading</td></tr></table>"#;
        let rows = assignments(assignment_html, &base, &course).unwrap();
        assert_eq!(rows[0].reference, "assign:7");
        assert_eq!(rows[0].week, Some(2));
        assert_eq!(rows[0].due_at.as_deref(), Some("2026-03-17T23:59:00+09:00"));

        let quiz_html = r#"<table><tr><th>No</th><th>Week</th><th>Name</th><th>Quiz closes</th><th>Grade</th></tr>
          <tr><td>1</td><td>week 3</td><td><a href='view.php?id=8'>Attendance quiz</a></td><td>Saturday, 21 March 2026, 11:59 PM</td><td>-</td></tr></table>"#;
        let quiz_page = base.join("/mod/quiz/index.php?id=42").unwrap();
        let rows = quizzes(quiz_html, &quiz_page, &course).unwrap();
        assert_eq!(rows[0].reference, "quiz:8");
        assert_eq!(rows[0].url, format!("{BASE}/mod/quiz/view.php?id=8"));
        assert_eq!(
            rows[0].closes_at.as_deref(),
            Some("2026-03-21T23:59:00+09:00")
        );
    }

    #[test]
    fn rejects_nonempty_coursework_rows_without_actionable_identity() {
        let base = Url::parse(BASE).unwrap();
        let course = Course {
            id: "42".into(),
            reference: "course:42".into(),
            title: "Compilers".into(),
            code: None,
            term: None,
            url: format!("{BASE}/course/view.php?id=42"),
        };
        let missing_link = "<table><tr><th>Week</th><th>Name</th><th>Due date</th></tr><tr><td>1</td><td>Written work</td><td>soon</td></tr></table>";
        assert!(assignments(missing_link, &base, &course).is_err());

        let missing_id = "<table><tr><th>Week</th><th>Name</th><th>Quiz closes</th></tr><tr><td>1</td><td><a href='/mod/quiz/view.php'>Quiz</a></td><td>soon</td></tr></table>";
        assert!(quizzes(missing_id, &base, &course).is_err());
    }

    #[test]
    fn accepts_explicit_empty_coursework_pages() {
        let base = Url::parse(BASE).unwrap();
        let course = Course {
            id: "42".into(),
            reference: "course:42".into(),
            title: "Compilers".into(),
            code: None,
            term: None,
            url: format!("{BASE}/course/view.php?id=42"),
        };
        assert!(
            assignments(
                "<main>There are no assignments in this course.</main>",
                &base,
                &course
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            quizzes("<main>No quizzes found.</main>", &base, &course)
                .unwrap()
                .is_empty()
        );
    }
}
