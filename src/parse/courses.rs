use std::collections::HashSet;

use scraper::{ElementRef, Html};
use url::Url;

use super::shared::{first_text, has_any, link_items, selected_value, selector, text, week_number};
use crate::{
    error::AppError,
    models::{Activity, Course, CourseDetail, Dashboard},
    reference::ResourceRef,
    safe_url,
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
        usize::MAX,
    )?;
    Ok(Dashboard {
        term,
        course_count: courses.len(),
        courses,
        courses_complete: true,
        upcoming_count: upcoming.len(),
        upcoming,
        upcoming_complete: true,
    })
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
    if rows.is_empty() && !has_any(&document, &[".course-content"])? {
        return Err(AppError::shape(
            "course page contained no recognizable activity region",
        ));
    }
    if let Some(week) = week {
        rows.retain(|row| row.week == Some(week));
    }
    Ok(rows)
}

pub fn is_video_activity(activity: &Activity) -> bool {
    matches!(
        activity.kind.to_ascii_lowercase().as_str(),
        "vod" | "panopto" | "panoptocourseembed"
    ) || activity.kind.eq_ignore_ascii_case("lti")
        && (activity.title.to_ascii_lowercase().contains("panopto")
            || activity.title.to_ascii_lowercase().contains("vod"))
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
            (key == "id" && crate::reference::valid_id(&value)).then(|| value.into_owned())
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
            reference: ResourceRef::Course(id.clone()).to_string(),
            id,
            term: code.as_deref().and_then(term_from_code),
            code,
            title,
            url: safe_url::display(&url),
        });
    }
    Ok(courses)
}

fn activities_from_document(document: &Html, base_url: &Url) -> Result<Vec<Activity>, AppError> {
    let modules = selector("li.activity, .activity-item[data-id]")?;
    let anchors = selector(".activityinstance a[href], a.aalink[href], a[href]")?;
    let downloads = selector("[onclick*='downloadFile']")?;
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
        let href = anchor
            .and_then(|node| node.value().attr("href").map(str::to_owned))
            .or_else(|| {
                module
                    .select(&downloads)
                    .find_map(|node| node.value().attr("onclick").and_then(download_url))
            });
        let url = href.as_deref().and_then(|value| base_url.join(value).ok());
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
        let reference =
            ResourceRef::from_activity(&kind, id.as_deref(), url.as_ref().map(Url::as_str))
                .map(|reference| reference.to_string());
        rows.push(Activity {
            id,
            reference,
            kind,
            title,
            week,
            section,
            external: url.as_ref().is_some_and(|url| !same_origin(url, base_url)),
            url: url.as_ref().map(safe_url::display),
        });
    }
    Ok(rows)
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

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn download_url(script: &str) -> Option<String> {
    for marker in ["downloadFile('", "downloadFile(\""] {
        let Some(rest) = script.split(marker).nth(1) else {
            continue;
        };
        let quote = marker.chars().last()?;
        let value = rest.split(quote).next()?.trim();
        if value.starts_with('/') || value.starts_with("https://") {
            return Some(value.into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{activities, course_detail, dashboard};
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
    fn extracts_direct_file_url_from_klms_download_handler() {
        let html = r#"<li class="activity modtype_resource" id="module-9">
          <div class="aalink" onclick="M.course.format.downloadFile('https://klms.kaist.ac.kr/pluginfile.php/123/notes.pdf', 'notes.pdf')">
          <span class="instancename">Notes File</span></div></li>"#;
        let rows = activities(html, &Url::parse(BASE).unwrap(), None).unwrap();
        assert_eq!(
            rows[0].url.as_deref(),
            Some("https://klms.kaist.ac.kr/pluginfile.php/123/notes.pdf")
        );
    }

    #[test]
    fn parses_live_shape_course_header_and_professor() {
        let html = r#"<a class="h1 mr-auto" href="/course/view.php?id=42">Programming Language(CS.30200_2026_3)</a>
          <div class="d-flex courseinfo"><div class="border-left py-2">Professors
          <div><a class="dropdown-toggle text-primary">Ryu Seokyoung</a></div></div></div>"#;
        let course = Course {
            id: "42".into(),
            reference: "course:42".into(),
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

    #[test]
    fn empty_activity_pages_require_a_recognizable_container() {
        let base = Url::parse(BASE).unwrap();
        assert!(activities("<html><body>maintenance</body></html>", &base, None).is_err());
        assert!(activities("<main class='course-content'></main>", &base, None).is_ok());
    }
}
