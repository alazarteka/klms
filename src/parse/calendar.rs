use std::collections::HashSet;

use scraper::Html;
use url::Url;

use super::shared::{has_any, query_id, selector, text};
use crate::{date, error::AppError, models::CalendarEvent, reference::ResourceRef, safe_url};

pub struct CalendarPage {
    pub events: Vec<CalendarEvent>,
    pub complete: bool,
    pub unparsed_times: usize,
    pub missing_course_ids: usize,
}

pub fn calendar_page(html: &str, base_url: &Url) -> Result<CalendarPage, AppError> {
    let document = Html::parse_document(html);
    let events = selector(".event, [data-region=event-list-item]")?;
    let links = selector("a[href]")?;
    let times = selector("time")?;
    let course_links = selector("a[href*='course/view.php']")?;
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped = 0;
    for event in document.select(&events) {
        let event_link = event.select(&links).find_map(|anchor| {
            let href = anchor.value().attr("href")?;
            let url = base_url.join(href).ok()?;
            let path = url.path();
            (path.contains("/mod/") || path.contains("/calendar/event.php"))
                .then_some((anchor, url))
        });
        let Some((anchor, url)) = event_link else {
            skipped += 1;
            continue;
        };
        let kind = event
            .value()
            .attr("data-event-component")
            .and_then(|value| value.strip_prefix("mod_"))
            .map(str::to_owned)
            .or_else(|| module_kind(&url))
            .unwrap_or_else(|| "event".into());
        let reference = if kind == "event" {
            None
        } else {
            ResourceRef::from_activity(&kind, None, Some(url.as_str()))
                .map(|reference| reference.to_string())
        };
        let time = event.select(&times).next();
        let when_text = time.map(text).filter(|value| !value.is_empty());
        let starts_at = time
            .and_then(|node| node.value().attr("datetime"))
            .and_then(date::normalize_datetime)
            .or_else(|| when_text.as_deref().and_then(date::moodle_datetime))
            .or_else(|| {
                event
                    .value()
                    .attr("data-event-timestart")
                    .or_else(|| event.value().attr("data-timestart"))
                    .and_then(|value| value.parse::<i64>().ok())
                    .and_then(date::epoch_to_seoul)
            });
        let course_link = event.select(&course_links).next();
        let course_url = course_link
            .and_then(|link| link.value().attr("href"))
            .and_then(|href| base_url.join(href).ok());
        let title = text(anchor);
        let identity = event
            .value()
            .attr("data-event-id")
            .or_else(|| event.value().attr("data-eventid"))
            .map(|id| format!("event:{id}"))
            .unwrap_or_else(|| format!("{}|{}|{}", url, starts_at.as_deref().unwrap_or(""), title));
        if !seen.insert(identity) {
            continue;
        }
        rows.push(CalendarEvent {
            reference,
            kind,
            title,
            course_id: course_url
                .as_ref()
                .and_then(|url| query_id(url, &["id"]))
                .or_else(|| event.value().attr("data-course-id").map(str::to_owned)),
            course: course_link.map(text).filter(|value| !value.is_empty()),
            starts_at,
            when_text,
            url: safe_url::display(&url),
        });
    }
    let explicit_empty = document
        .root_element()
        .text()
        .collect::<String>()
        .to_ascii_lowercase()
        .contains("there are no upcoming events");
    if rows.is_empty()
        && !explicit_empty
        && !has_any(
            &document,
            &[".calendarwrapper", "[data-region=event-list-content]"],
        )?
    {
        return Err(AppError::shape(
            "calendar page contained no recognizable event region",
        ));
    }
    let unparsed_times = rows
        .iter()
        .filter(|event| event.starts_at.is_none())
        .count();
    let missing_course_ids = rows
        .iter()
        .filter(|event| event.course_id.is_none())
        .count();
    let has_next = has_any(
        &document,
        &[
            "a[rel=next]",
            ".pagination .next a",
            "a[data-page-number][aria-label*=Next]",
        ],
    )?;
    Ok(CalendarPage {
        events: rows,
        complete: skipped == 0 && !has_next,
        unparsed_times,
        missing_course_ids,
    })
}

fn module_kind(url: &Url) -> Option<String> {
    let parts: Vec<_> = url.path_segments()?.collect();
    parts
        .windows(2)
        .find_map(|pair| (pair[0] == "mod").then(|| pair[1].to_owned()))
}

#[cfg(test)]
mod tests {
    use super::calendar_page;
    use url::Url;

    const BASE: &str = "https://klms.kaist.ac.kr";

    #[test]
    fn empty_pages_require_a_recognizable_container() {
        let base = Url::parse(BASE).unwrap();
        assert!(calendar_page("<html><body>maintenance</body></html>", &base).is_err());
        assert!(calendar_page("<main class='calendarwrapper'></main>", &base).is_ok());
    }

    #[test]
    fn parses_typed_event_with_course_and_time() {
        let html = r#"<main id='region-main'><div class='event'>
          <a href='/mod/assign/view.php?id=7'>Written work is due</a>
          <a href='/course/view.php?id=42'>Compilers</a>
          <time datetime='2026-09-01T23:59:00+09:00'>Tuesday, 1 September 2026, 11:59 PM</time>
          </div></main>"#;
        let rows = calendar_page(html, &Url::parse(BASE).unwrap())
            .unwrap()
            .events;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reference.as_deref(), Some("assign:7"));
        assert_eq!(rows[0].course_id.as_deref(), Some("42"));
        assert_eq!(
            rows[0].starts_at.as_deref(),
            Some("2026-09-01T23:59:00+09:00")
        );
    }

    #[test]
    fn reports_incomplete_or_unusable_event_pages() {
        let base = Url::parse(BASE).unwrap();
        let skipped = calendar_page(
            "<main class='calendarwrapper'><div class='event'>No detail link</div></main>",
            &base,
        )
        .unwrap();
        assert!(!skipped.complete);

        let unparsed = calendar_page(
            "<main class='calendarwrapper'><div class='event'><a href='/mod/assign/view.php?id=7'>Work</a><time>sometime</time></div></main>",
            &base,
        )
        .unwrap();
        assert_eq!(unparsed.unparsed_times, 1);
        assert_eq!(unparsed.missing_course_ids, 1);
    }

    #[test]
    fn preserves_distinct_events_that_share_a_module_url() {
        let html = r#"<main class='calendarwrapper'>
          <div class='event'><a href='/mod/quiz/view.php?id=8'>Quiz opens</a><time datetime='2026-09-01T09:00:00+09:00'>open</time></div>
          <div class='event'><a href='/mod/quiz/view.php?id=8'>Quiz closes</a><time datetime='2026-09-02T18:00:00+09:00'>close</time></div>
          </main>"#;
        let page = calendar_page(html, &Url::parse(BASE).unwrap()).unwrap();
        assert_eq!(page.events.len(), 2);
        assert!(page.complete);
    }
}
