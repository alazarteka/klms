use std::collections::HashSet;

use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{
    error::AppError,
    models::{
        Activity, BoardPost, Course, CourseDetail, Dashboard, LinkItem, Report, ResourceDetail,
    },
};

pub fn dashboard(html: &str, base_url: &Url) -> Result<Dashboard, AppError> {
    let document = Html::parse_document(html);
    let courses = courses_from_document(&document, base_url)?;
    if courses.is_empty() {
        return Err(AppError::shape(
            "authenticated dashboard contained no recognizable course links",
        ));
    }
    let term = selected_value(&document, "select[name=year]")
        .zip(selected_value(&document, "select[name=semester]"))
        .map(|(year, semester)| format!("{year} {semester}"));
    let upcoming = link_items(
        &document,
        base_url,
        ".block_timeline a[href], [data-region=event-list-content] a[href]",
        20,
    )?;
    Ok(Dashboard {
        term,
        course_count: courses.len(),
        courses,
        upcoming,
    })
}

pub fn courses(html: &str, base_url: &Url) -> Result<Vec<Course>, AppError> {
    courses_from_document(&Html::parse_document(html), base_url)
}

pub fn course_detail(
    html: &str,
    base_url: &Url,
    mut course: Course,
) -> Result<CourseDetail, AppError> {
    let document = Html::parse_document(html);
    if let Some(title) = first_text(
        &document,
        ".page-header-headings h1, a.h1[href*='course/view.php'], h1",
    )? {
        course.code = course_code(&title).or(course.code);
        course.term = course
            .code
            .as_deref()
            .and_then(term_from_code)
            .or(course.term);
        course.title = title.split('(').next().unwrap_or(&title).trim().to_owned();
    }
    let professors = professors(&document)?;
    let activity_count = activities_from_document(&document, base_url)?.len();
    Ok(CourseDetail {
        course,
        professors,
        activity_count,
    })
}

pub fn activities(
    html: &str,
    base_url: &Url,
    week: Option<u32>,
) -> Result<Vec<Activity>, AppError> {
    let document = Html::parse_document(html);
    let mut rows = activities_from_document(&document, base_url)?;
    if let Some(week) = week {
        rows.retain(|row| row.week == Some(week));
    }
    Ok(rows)
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

pub fn sesskey(html: &str) -> Result<String, AppError> {
    for marker in ["\"sesskey\":\"", "\"sesskey\": \"", "sesskey="] {
        if let Some(rest) = html.split(marker).nth(1) {
            let value: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    let document = Html::parse_document(html);
    let input = selector("input[name=sesskey]")?;
    document
        .select(&input)
        .find_map(|node| node.value().attr("value"))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AppError::shape("authenticated page did not expose a Moodle sesskey"))
}

pub fn resource_detail(
    html: &str,
    base_url: &Url,
    url: &Url,
    kind: &str,
) -> Result<ResourceDetail, AppError> {
    let document = Html::parse_document(html);
    let title = first_text(
        &document,
        ".page-header-headings h1, #page-header h1, h1, title",
    )?
    .unwrap_or_else(|| format!("{kind} detail"));
    let main = selector("main, #region-main, [role=main], body")?;
    let text_value = document.select(&main).next().map(text).unwrap_or_default();
    let links = link_items(
        &document,
        base_url,
        "main a[href], #region-main a[href], [role=main] a[href]",
        100,
    )?;
    Ok(ResourceDetail {
        id: query_id(url, &["id", "bwid"]),
        kind: kind.into(),
        title,
        url: url.to_string(),
        text: text_value.chars().take(100_000).collect(),
        links,
    })
}

pub fn board_posts(
    html: &str,
    base_url: &Url,
    board_id: Option<String>,
) -> Result<Vec<BoardPost>, AppError> {
    let document = Html::parse_document(html);
    let anchors = selector("a[href*='/mod/courseboard/article.php']")?;
    let cells = selector("td")?;
    let mut seen = HashSet::new();
    let mut posts = Vec::new();
    for anchor in document.select(&anchors) {
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let Ok(url) = base_url.join(href) else {
            continue;
        };
        let id = query_id(&url, &["bwid"]);
        let key = id.clone().unwrap_or_else(|| url.to_string());
        if !seen.insert(key) {
            continue;
        }
        let title = text(anchor);
        if title.is_empty() {
            continue;
        }
        let posted = anchor
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|node| node.value().name() == "tr")
            .and_then(|row| {
                row.select(&cells)
                    .map(text)
                    .find(|value| looks_like_date(value))
            });
        posts.push(BoardPost {
            board_id: board_id.clone(),
            id,
            title,
            posted,
            url: url.into(),
        });
    }
    Ok(posts)
}

pub fn calendar(html: &str, base_url: &Url, limit: usize) -> Result<Vec<LinkItem>, AppError> {
    let document = Html::parse_document(html);
    link_items(
        &document,
        base_url,
        ".event a[href], [data-region=event-list-content] a[href], .calendarwrapper a[href]",
        limit,
    )
}

fn courses_from_document(document: &Html, base_url: &Url) -> Result<Vec<Course>, AppError> {
    let selector = selector("a[href*='course/view.php']")?;
    let mut seen = HashSet::new();
    let mut courses = Vec::new();
    for anchor in document.select(&selector) {
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let Ok(url) = base_url.join(href) else {
            continue;
        };
        let Some(id) = url.query_pairs().find_map(|(key, value)| {
            (key == "id" && value.chars().all(|c| c.is_ascii_digit())).then(|| value.into_owned())
        }) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let title = text(anchor);
        if title.is_empty() || is_noise_course(&title) {
            continue;
        }
        let code = course_code(&title).or_else(|| {
            anchor
                .ancestors()
                .filter_map(ElementRef::wrap)
                .take(5)
                .map(text)
                .find_map(|value| course_code(&value))
        });
        courses.push(Course {
            id,
            term: code.as_deref().and_then(term_from_code),
            code,
            title,
            url: url.into(),
        });
    }
    Ok(courses)
}

fn activities_from_document(document: &Html, base_url: &Url) -> Result<Vec<Activity>, AppError> {
    let modules = selector("li.activity, .activity-item[data-id]")?;
    let anchors = selector(".activityinstance a[href], a.aalink[href], a[href]")?;
    let names = selector(".instancename, .activityname, .activity-title")?;
    let headings = selector(".sectionname, .section-title, h3")?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for module in document.select(&modules) {
        let id = module
            .value()
            .attr("id")
            .and_then(|value| value.strip_prefix("module-"))
            .or_else(|| module.value().attr("data-id"))
            .map(str::to_owned);
        let anchor = module.select(&anchors).next();
        let href = anchor.and_then(|node| node.value().attr("href"));
        let url = href.and_then(|value| base_url.join(value).ok());
        let title = module
            .select(&names)
            .next()
            .map(text)
            .filter(|value| !value.is_empty())
            .or_else(|| anchor.map(text).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "Untitled activity".into());
        let kind = module
            .value()
            .classes()
            .find_map(|class| class.strip_prefix("modtype_").map(str::to_owned))
            .or_else(|| {
                url.as_ref().and_then(|url| {
                    let parts: Vec<_> = url.path_segments()?.collect();
                    parts
                        .windows(2)
                        .find_map(|pair| (pair[0] == "mod").then(|| pair[1].to_owned()))
                })
            })
            .unwrap_or_else(|| "activity".into());
        let section = module
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find_map(|ancestor| ancestor.select(&headings).next().map(text))
            .filter(|value| !value.is_empty());
        let week = section.as_deref().and_then(week_number);
        let key = id
            .clone()
            .or_else(|| url.as_ref().map(ToString::to_string))
            .unwrap_or_else(|| title.clone());
        if !seen.insert(key) {
            continue;
        }
        rows.push(Activity {
            id,
            kind,
            title,
            week,
            section,
            external: url.as_ref().is_some_and(|url| !same_origin(url, base_url)),
            url: url.map(Into::into),
        });
    }
    Ok(rows)
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

fn professors(document: &Html) -> Result<Vec<String>, AppError> {
    let generic_selector =
        selector(".teachers a, .teacher a, [class*=professor] a, [class*=instructor] a")?;
    let mut names = Vec::new();
    for node in document.select(&generic_selector) {
        let value = text(node);
        if !value.is_empty() && !names.contains(&value) {
            names.push(value);
        }
    }
    let course_info = selector(".courseinfo .border-left")?;
    let anchors = selector("a.dropdown-toggle.text-primary")?;
    for container in document.select(&course_info) {
        if !text(container)
            .to_ascii_lowercase()
            .starts_with("professors")
        {
            continue;
        }
        for node in container.select(&anchors) {
            let value = text(node);
            if !value.is_empty() && !names.contains(&value) {
                names.push(value);
            }
        }
    }
    Ok(names)
}

fn link_items(
    document: &Html,
    base_url: &Url,
    css: &str,
    limit: usize,
) -> Result<Vec<LinkItem>, AppError> {
    let selector = selector(css)?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for anchor in document.select(&selector) {
        let title = text(anchor);
        let Some(url) = anchor
            .value()
            .attr("href")
            .and_then(|href| base_url.join(href).ok())
        else {
            continue;
        };
        if title.is_empty() || !seen.insert(url.to_string()) {
            continue;
        }
        rows.push(LinkItem {
            title,
            url: url.into(),
        });
        if rows.len() == limit {
            break;
        }
    }
    Ok(rows)
}

fn first_text(document: &Html, css: &str) -> Result<Option<String>, AppError> {
    Ok(document
        .select(&selector(css)?)
        .map(text)
        .find(|value| !value.is_empty()))
}

fn selected_value(document: &Html, css: &str) -> Option<String> {
    let select = Selector::parse(css).ok()?;
    let selected = Selector::parse("option[selected]").ok()?;
    let fallback = Selector::parse("option").ok()?;
    let node = document.select(&select).next()?;
    node.select(&selected)
        .next()
        .or_else(|| node.select(&fallback).next())
        .map(text)
}

fn selector(value: &str) -> Result<Selector, AppError> {
    Selector::parse(value)
        .map_err(|_| AppError::internal(format!("invalid built-in selector: {value}")))
}

fn text(element: ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn course_code(title: &str) -> Option<String> {
    if let Some(start) = title.rfind('(').map(|index| index + 1) {
        if let Some(relative_end) = title[start..].find(')') {
            let value = title[start..start + relative_end].trim();
            if value.contains('_') && value.chars().any(|c| c.is_ascii_digit()) {
                return Some(value.to_owned());
            }
        }
    }
    title
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | ':')))
        .find(|token| {
            token.contains('.')
                && token.chars().any(|c| c.is_ascii_alphabetic())
                && token.chars().any(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
}

fn is_noise_course(title: &str) -> bool {
    let normalized = title.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "exam bank"
            | "기출문제은행"
            | "micro learning"
            | "teaching skills"
            | "learning skills"
            | "how to use panopto"
            | "guide to klms"
            | "how to use klms"
    ) || normalized.contains("panopto guide")
}

fn term_from_code(code: &str) -> Option<String> {
    let mut parts = code.rsplit('_');
    let semester = parts.next()?;
    let year = parts.next()?;
    (year.len() == 4
        && year.chars().all(|c| c.is_ascii_digit())
        && semester.chars().all(|c| c.is_ascii_digit()))
    .then(|| format!("{year}-{semester}"))
}

fn week_number(value: &str) -> Option<u32> {
    let lower = value.to_ascii_lowercase();
    let position = lower.find("week")? + 4;
    lower[position..]
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn query_id(url: &Url, names: &[&str]) -> Option<String> {
    url.query_pairs()
        .find_map(|(key, value)| names.contains(&key.as_ref()).then(|| value.into_owned()))
}

fn looks_like_date(value: &str) -> bool {
    let digits = value.chars().filter(|c| c.is_ascii_digit()).count();
    digits >= 6 && (value.contains('-') || value.contains('.') || value.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::{activities, attendance, board_posts, course_detail, dashboard, grades, sesskey};
    use crate::models::Course;
    use url::Url;

    const BASE: &str = "https://klms.kaist.ac.kr";

    #[test]
    fn parses_dashboard_and_deduplicates_courses() {
        let html = r#"<select name="year"><option selected>2026</option></select>
          <select name="semester"><option selected>Fall</option></select>
          <a href="/course/view.php?id=42">Compilers(CS.420_2026_2)</a>
          <a href="/course/view.php?id=42">duplicate</a>"#;
        let model = dashboard(html, &Url::parse(BASE).unwrap()).unwrap();
        assert_eq!(model.course_count, 1);
        assert_eq!(model.term.as_deref(), Some("2026 Fall"));
        assert_eq!(model.courses[0].code.as_deref(), Some("CS.420_2026_2"));
    }

    #[test]
    fn filters_global_training_cards() {
        let html = r#"<a href="/course/view.php?id=1">Exam Bank</a>
          <a href="/course/view.php?id=2">Machine Learning</a>"#;
        let model = dashboard(html, &Url::parse(BASE).unwrap()).unwrap();
        assert_eq!(model.course_count, 1);
        assert_eq!(model.courses[0].id, "2");
    }

    #[test]
    fn parses_activities_with_sections_and_external_links() {
        let html = r#"<li class="section"><h3>Week 3</h3><ul>
          <li class="activity modtype_quiz" id="module-7"><a class="aalink" href="/mod/quiz/view.php?id=7"><span class="instancename">Quiz</span></a></li>
          <li class="activity modtype_lti" id="module-8"><a href="https://tools.example/launch"><span class="instancename">Lab</span></a></li>
          </ul></li>"#;
        let rows = activities(html, &Url::parse(BASE).unwrap(), Some(3)).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].external);
        assert!(rows[1].external);
    }

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
    fn extracts_session_key_without_exposing_it_elsewhere() {
        assert_eq!(
            sesskey(r#"<script>var cfg={"sesskey":"abc123"}</script>"#).unwrap(),
            "abc123"
        );
    }

    #[test]
    fn parses_courseboard_posts() {
        let html = r#"<table><tr><td><a href="/mod/courseboard/article.php?id=8&bwid=9">Exam notice</a></td><td>2026-08-29</td></tr></table>"#;
        let rows = board_posts(html, &Url::parse(BASE).unwrap(), Some("8".into())).unwrap();
        assert_eq!(rows[0].id.as_deref(), Some("9"));
        assert_eq!(rows[0].posted.as_deref(), Some("2026-08-29"));
    }

    #[test]
    fn parses_live_shape_course_header_and_professor() {
        let html = r#"<a class="h1 mr-auto" href="/course/view.php?id=42">Programming Language(CS.30200_2026_3)</a>
          <div class="d-flex courseinfo"><div class="border-left py-2">Professors
          <div><a class="dropdown-toggle text-primary">Ryu Seokyoung</a></div></div></div>"#;
        let course = Course {
            id: "42".into(),
            title: "Course 42".into(),
            code: None,
            term: None,
            url: format!("{BASE}/course/view.php?id=42"),
        };
        let detail = course_detail(html, &Url::parse(BASE).unwrap(), course).unwrap();
        assert_eq!(detail.course.title, "Programming Language");
        assert_eq!(detail.course.code.as_deref(), Some("CS.30200_2026_3"));
        assert_eq!(detail.professors, ["Ryu Seokyoung"]);
    }
}
