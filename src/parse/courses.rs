use std::collections::HashSet;

use crate::url::Url;
use scraper::{ElementRef, Html};

use super::shared::{
    all, first, first_text, has_any, href_url, link_items, module_view, page_identity, query_id,
    resolve_http_url, sel, selected_value, text, week_number,
};
use crate::{
    date,
    error::AppError,
    models::{Activity, BoardPost, Course, CourseDetail, Dashboard},
    reference::{ResourceRef, module_kind, valid_id},
    safe_url,
};

pub fn dashboard(html: &str, base_url: &Url) -> Result<Dashboard, AppError> {
    let document = Html::parse_document(html);
    let courses = courses_from_document(&document, base_url);
    if courses.is_empty() {
        return Err(AppError::shape(
            "authenticated dashboard contained no recognizable course links",
        ));
    }
    let term = selected_value(&document, "select[name=year]")
        .zip(selected_value(&document, "select[name=semester]"))
        .map(|(year, semester)| format!("{year} {semester}"));
    let upcoming = link_items(
        document.select(&sel(
            ".block_timeline a[href], [data-region=event-list-content] a[href]",
        )),
        base_url,
        usize::MAX,
    );
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

/// Parse only Moodle's documented enrolled-course metadata, never course images/media paths.
pub fn enrolled_courses(
    value: &serde_json::Value,
    base: &Url,
) -> Result<(Vec<Course>, usize), AppError> {
    let records = value
        .get("courses")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AppError::shape("enrolled courses response has no courses array"))?;
    let next = value
        .get("nextoffset")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| AppError::shape("enrolled courses response has no numeric nextoffset"))?;
    let mut rows = Vec::new();
    for record in records {
        let id = record
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .filter(|id| *id > 0)
            .map(|id| id.to_string())
            .ok_or_else(|| AppError::shape("enrolled course has no positive numeric id"))?;
        let title = record
            .get("fullname")
            .and_then(serde_json::Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .ok_or_else(|| AppError::shape("enrolled course has no title"))?;
        let code = record
            .get("shortname")
            .and_then(serde_json::Value::as_str)
            .filter(|code| !code.trim().is_empty())
            .map(str::to_owned);
        rows.push(Course {
            reference: format!("course:{id}"),
            url: resolve_http_url(base, &format!("/course/view.php?id={id}"))
                .expect("built-in course path")
                .into(),
            id,
            title: title.to_owned(),
            term: code.as_deref().and_then(term_from_code),
            code,
        });
    }
    Ok((rows, next))
}

pub fn course_detail(
    html: &str,
    base_url: &Url,
    mut course: Course,
) -> Result<CourseDetail, AppError> {
    let document = Html::parse_document(html);
    page_identity(&document, "course", Some(&course.id))?;
    if let Some(title) = first_text(
        &document,
        ".page-header-headings h1, a.h1[href*='course/view.php'], h1",
    ) {
        course.code = course_code(&title).or(course.code);
        course.term = course
            .code
            .as_deref()
            .and_then(term_from_code)
            .or(course.term);
        course.title = title.split('(').next().unwrap_or(&title).trim().to_owned();
    }
    if !has_any(
        &document,
        &[".course-content", "a.h1[href*='course/view.php']"],
    ) {
        return Err(AppError::shape(
            "course page contained no recognizable course region",
        ));
    }
    let activity_count = if has_any(&document, &[".course-content"]) {
        activities(html, base_url, None)?.len()
    } else {
        0
    };
    Ok(CourseDetail {
        course,
        professors: professors(&document),
        activity_count,
    })
}

pub fn activities(
    html: &str,
    base_url: &Url,
    week: Option<u32>,
) -> Result<Vec<Activity>, AppError> {
    let document = Html::parse_document(html);
    page_identity(&document, "course", query_id(base_url, &["id"]).as_deref())?;
    let mut rows = activities_from_document(&document, base_url);
    // Every module view link in course content must have become an activity.
    let represented: HashSet<_> = rows.iter().filter_map(|row| row.url.as_deref()).collect();
    if !has_any(&document, &[".course-content"])
        || document
            .select(&sel(".course-content a[href]"))
            .filter_map(|a| href_url(a, base_url))
            .any(|url| module_view(&url) && !represented.contains(safe_url::display(&url).as_str()))
    {
        return Err(AppError::shape(
            "course content is missing or has unrecognized activity links",
        ));
    }
    if let Some(week) = week {
        rows.retain(|row| row.week == Some(week));
    }
    Ok(rows)
}

/// Whether a course page is KLMS's paged week format with an "All weeks" choice.
///
/// The `kaistweeks` format renders only the current weeks on the default course
/// page. Its week picker links are `javascript:M.course.format.dayselect(n, course, …)`,
/// where `n = 0` is "All", served at `course/view.php?id=<course>&section=0`.
pub fn has_all_weeks_view(html: &str, course_id: &str) -> bool {
    let wanted = format!("format.dayselect(0,{course_id},");
    Html::parse_document(html)
        .select(&sel("a[href*='dayselect']"))
        .filter_map(|anchor| anchor.value().attr("href"))
        .any(|href| {
            let compact: String = href.chars().filter(|c| !c.is_whitespace()).collect();
            compact.contains(&wanted)
        })
}

fn courses_from_document(document: &Html, base_url: &Url) -> Vec<Course> {
    let mut seen = HashSet::new();
    let mut courses = Vec::new();
    for anchor in document.select(&sel("a[href*='course/view.php']")) {
        let Some(url) = href_url(anchor, base_url) else {
            continue;
        };
        let Some(id) = query_id(&url, &["id"]) else {
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
    courses
}

pub(super) fn activity_title(module: ElementRef<'_>) -> Option<String> {
    [".instancename, .activityname, .activity-title", "a[href]"]
        .into_iter()
        .find_map(|css| {
            first(module, css)
                .map(text)
                .filter(|value| !value.is_empty())
        })
}

fn activities_from_document(document: &Html, base_url: &Url) -> Vec<Activity> {
    let (anchors, downloads) = (sel("a[href]"), sel("[onclick*='downloadFile']"));
    let headings = sel(".sectionname, .section-title, h3");
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for module in document.select(&sel(
        ".course-content li.activity, .course-content .activity-item[data-id]",
    )) {
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
        let url = href
            .as_deref()
            .and_then(|value| resolve_http_url(base_url, value));
        let title = activity_title(module).unwrap_or_else(|| "Untitled activity".into());
        let kind = module
            .value()
            .classes()
            .find_map(|class| class.strip_prefix("modtype_").map(str::to_owned))
            .or_else(|| module_kind(url.as_ref()?).map(str::to_owned))
            .unwrap_or_else(|| "activity".into());
        let section = module
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find_map(|ancestor| ancestor.select(&headings).next().map(text))
            .filter(|value| !value.is_empty());
        let key = id
            .clone()
            .or_else(|| url.as_ref().map(ToString::to_string))
            .unwrap_or_else(|| title.clone());
        if !seen.insert(key) {
            continue;
        }
        let reference = ResourceRef::from_activity(&kind, id.as_deref(), url.as_ref())
            .map(|reference| reference.to_string());
        rows.push(Activity {
            id,
            reference,
            kind,
            title,
            week: section.as_deref().and_then(week_number),
            section,
            external: url
                .as_ref()
                .is_some_and(|url| url.origin() != base_url.origin()),
            url: url.as_ref().map(safe_url::display),
        });
    }
    rows
}

fn professors(document: &Html) -> Vec<String> {
    let root = document.root_element();
    let listed = all(root, ".courseinfo .border-left")
        .into_iter()
        .filter(|node| text(*node).to_ascii_lowercase().starts_with("professors"))
        .flat_map(|node| all(node, "a.dropdown-toggle.text-primary"));
    let generic = all(
        root,
        ".teachers a, .teacher a, [class*=professor] a, [class*=instructor] a",
    );
    let mut names = Vec::new();
    for value in generic.into_iter().chain(listed).map(text) {
        if !value.is_empty() && !names.contains(&value) {
            names.push(value);
        }
    }
    names
}

fn course_code(title: &str) -> Option<String> {
    let parenthesized = title
        .rsplit_once('(')
        .and_then(|(_, tail)| tail.split_once(')'))
        .map(|(value, _)| value.trim())
        .filter(|value| value.contains('_') && value.chars().any(|c| c.is_ascii_digit()));
    if let Some(value) = parenthesized {
        return Some(value.to_owned());
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
    const NOISE: [&str; 8] = [
        "exam bank",
        "기출문제은행",
        "micro learning",
        "teaching skills",
        "learning skills",
        "how to use panopto",
        "guide to klms",
        "how to use klms",
    ];
    let normalized = title.trim().to_ascii_lowercase();
    NOISE.contains(&normalized.as_str()) || normalized.contains("panopto guide")
}

fn term_from_code(code: &str) -> Option<String> {
    let mut parts = code.rsplit('_');
    let semester = parts.next()?;
    let year = parts.next()?;
    (year.len() == 4
        && year.chars().all(|c| c.is_ascii_digit())
        && matches!(semester, "1" | "2" | "3" | "4"))
    .then(|| format!("{year}-{semester}"))
}

fn download_url(script: &str) -> Option<String> {
    for quote in ['\'', '"'] {
        let Some(rest) = script.split(&format!("downloadFile({quote}")).nth(1) else {
            continue;
        };
        let value = rest.split(quote).next()?.trim();
        if value.starts_with('/') || value.starts_with("https://") {
            return Some(value.into());
        }
    }
    None
}

pub fn is_notice_board(activity: &Activity) -> bool {
    activity.kind.eq_ignore_ascii_case("courseboard")
        && (activity.title.to_ascii_lowercase().contains("notice")
            || activity.title.contains("공지"))
}

pub fn board_posts(
    html: &str,
    base_url: &Url,
    board_id: Option<String>,
) -> Result<Vec<BoardPost>, AppError> {
    if board_id.as_deref().is_some_and(|id| !valid_id(id)) {
        return Err(AppError::shape("board page had no numeric board id"));
    }
    let document = Html::parse_document(html);
    let cells = sel("td");
    let mut seen = HashSet::new();
    let mut posts = Vec::new();
    let regions = ".courseboard, table.generaltable, table.board-list";
    // Any courseboard endpoint with bwid looks like a post, even after endpoint drift.
    let links: Vec<_> = document
        .select(&sel(
            ".courseboard a[href], table.generaltable a[href], table.board-list a[href]",
        ))
        .filter_map(|a| Some((a, href_url(a, base_url)?)))
        .filter(|(_, url)| {
            url.path().starts_with("/mod/courseboard/")
                && url.query_pairs().any(|(key, _)| key == "bwid")
        })
        .collect();
    for (anchor, url) in &links {
        if url.path() != "/mod/courseboard/article.php" {
            continue;
        }
        let id = query_id(url, &["bwid"]);
        if id.is_none() || query_id(url, &["id"]).is_none() {
            continue;
        }
        let title = text(*anchor);
        if !seen.insert(id.clone().unwrap_or_else(|| url.to_string())) || title.is_empty() {
            continue;
        }
        let posted = anchor
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|node| node.value().name() == "tr")
            .and_then(|row| {
                row.select(&cells)
                    .map(text)
                    .find(|value| date::normalize_datetime(value).is_some())
            });
        let reference = board_id
            .clone()
            .zip(id.clone())
            .map(|(board, post)| ResourceRef::BoardPost { board, post }.to_string());
        posts.push(BoardPost {
            board_id: board_id.clone(),
            id,
            reference,
            title,
            posted,
            url: safe_url::display(url),
        });
    }
    if !has_any(&document, &[regions])
        || links
            .iter()
            .any(|(_, url)| !posts.iter().any(|post| post.url == safe_url::display(url)))
    {
        return Err(AppError::shape(
            "board region is missing or has unrecognized post links",
        ));
    }
    Ok(posts)
}

#[cfg(test)]
mod tests {
    use super::{activities, board_posts, course_detail, dashboard, has_all_weeks_view};
    use crate::models::Course;
    use crate::url::Url;

    const BASE: &str = "https://klms.kaist.ac.kr";

    fn base() -> Url {
        Url::parse(BASE).unwrap()
    }

    #[test]
    fn historical_terms_use_shortnames_and_preserve_unknowns() {
        let data = serde_json::json!({"courses": [
            {"id":42,"fullname":"Compilers","shortname":"CS350_2024_3","courseimage":"/2026/fall/image.png"},
            {"id":43,"fullname":"Summer","shortname":"CS350_2024_2"},
            {"id":44,"fullname":"Unknown","shortname":"training_2024_20"},
            {"id":45,"fullname":"Unknown","courseimage":"/2024/1/image.png"}
        ],"nextoffset":4});
        let (rows, next) = super::enrolled_courses(&data, &base()).unwrap();
        assert_eq!(next, 4);
        assert_eq!(rows[0].term.as_deref(), Some("2024-3"));
        assert_eq!(rows[1].term.as_deref(), Some("2024-2"));
        assert!(rows[2].term.is_none() && rows[3].term.is_none());
        assert_eq!(rows[0].reference, "course:42");
        let empty = super::enrolled_courses(&serde_json::json!({"courses":[]}), &base());
        assert_eq!(
            empty.unwrap_err().message,
            "enrolled courses response has no numeric nextoffset"
        );
        let bad_id = super::enrolled_courses(
            &serde_json::json!({"courses":[{"id":"oops"}],"nextoffset":1}),
            &base(),
        );
        assert_eq!(
            bad_id.unwrap_err().message,
            "enrolled course has no positive numeric id"
        );
    }

    #[test]
    fn parses_dashboard_deduplicates_courses_and_filters_training_cards() {
        let html = r#"<select name="year"><option selected>2026</option></select>
          <select name="semester"><option selected>Fall</option></select>
          <a href="/course/view.php?id=42">Compilers(CS.420_2026_2)</a>
          <a href="/course/view.php?id=42">duplicate</a>
          <a href="/course/view.php?id=1">Exam Bank</a>"#;
        let model = dashboard(html, &base()).unwrap();
        assert_eq!(model.course_count, 1);
        assert_eq!(model.term.as_deref(), Some("2026 Fall"));
        assert_eq!(model.courses[0].code.as_deref(), Some("CS.420_2026_2"));
        let html = r#"<a href="/course/view.php?id=1">Exam Bank</a>
          <a href="/course/view.php?id=2">Machine Learning</a>"#;
        let model = dashboard(html, &base()).unwrap();
        assert_eq!((model.course_count, model.courses[0].id.as_str()), (1, "2"));
    }

    #[test]
    fn parses_activities_with_sections_external_links_and_download_handlers() {
        let html = r#"<body id="page-course-view-topics" class="course-42"><main class="course-content"><li class="section"><h3>Week 3</h3><ul>
          <li class="activity modtype_quiz" id="module-7"><a class="aalink" href="/mod/quiz/view.php?id=7"><span class="instancename">Quiz</span></a></li>
          <li class="activity modtype_lti" id="module-8"><a href="https://tools.example/launch"><span class="instancename">Lab</span></a></li>
          </ul></li>"#;
        let rows = activities(html, &base(), Some(3)).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].external);
        assert!(rows[1].external);
        let html = r#"<body id="page-course-view-topics" class="course-42"><main class="course-content"><li class="activity modtype_resource" id="module-9">
          <div class="aalink" onclick="M.course.format.downloadFile('https://klms.kaist.ac.kr/pluginfile.php/123/notes.pdf', 'notes.pdf')">
          <span class="instancename">Notes File</span></div></li>"#;
        let rows = activities(html, &base(), None).unwrap();
        let direct = "https://klms.kaist.ac.kr/pluginfile.php/123/notes.pdf";
        assert_eq!(rows[0].url.as_deref(), Some(direct));
    }

    #[test]
    fn parses_live_shape_course_header_and_professor() {
        let html = r#"<body id="page-course-view-kaistweeks" class="course-42"><a class="h1 mr-auto" href="/course/view.php?id=42">Programming Language(CS.30200_2026_3)</a>
          <div class="d-flex courseinfo"><div class="border-left py-2">Professors
          <div><a class="dropdown-toggle text-primary">Synthetic Instructor</a></div></div></div>"#;
        let course = Course {
            id: "42".into(),
            reference: "course:42".into(),
            title: "Course 42".into(),
            code: None,
            term: None,
            url: format!("{BASE}/course/view.php?id=42"),
        };
        let detail = course_detail(html, &base(), course).unwrap();
        assert_eq!(detail.course.title, "Programming Language");
        assert_eq!(detail.course.code.as_deref(), Some("CS.30200_2026_3"));
        assert_eq!(detail.professors, ["Synthetic Instructor"]);
    }

    #[test]
    fn empty_activity_pages_require_a_recognizable_container() {
        assert!(activities("<html><body>maintenance</body></html>", &base(), None).is_err());
        assert!(activities("<body id='page-course-view-topics' class='course-42'><main class='course-content'></main>", &base(), None).is_ok());
    }

    #[test]
    fn recognizes_the_all_weeks_choice_for_this_course_only() {
        let picker = r#"<div class="week-slider"><a href="javascript:M.course.format.dayselect(0,42,0)">All</a>
          <a href="javascript:M.course.format.dayselect(6,42,0)">week 6</a></div>"#;
        let single_week = r#"<a href="javascript:M.course.format.dayselect(6,42,0)">week 6</a>"#;
        for (html, course, expected) in [
            (picker, "42", true),
            (picker, "4", false),
            (single_week, "42", false),
            (
                "<body id='page-course-view-topics' class='course-42'><main class='course-content'></main>",
                "42",
                false,
            ),
        ] {
            assert_eq!(has_all_weeks_view(html, course), expected);
        }
    }

    #[test]
    fn parses_posts_and_refuses_malformed_ids_for_typed_references() {
        let base = Url::parse("https://klms.kaist.ac.kr").unwrap();
        let html = "<table class='generaltable'><tr><td><a href='/mod/courseboard/article.php?id=8&bwid=9'>Notice</a></td><td>2026-08-29</td></tr></table>";
        for id in ["", "oops", "8:9"] {
            assert!(board_posts(html, &base, Some(id.into())).is_err());
        }
        let rows = board_posts(html, &base, Some("8".into())).unwrap();
        assert_eq!(rows[0].id.as_deref(), Some("9"));
        assert_eq!(rows[0].posted.as_deref(), Some("2026-08-29"));
        let html = html.replace("bwid=9", "bwid=oops");
        assert_eq!(
            board_posts(&html, &base, Some("8".into()))
                .unwrap_err()
                .code,
            "UPSTREAM_SHAPE_CHANGED"
        );
    }
}
