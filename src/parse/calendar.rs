use std::collections::HashSet;

use scraper::{ElementRef, Html};
use url::Url;

use super::shared::{has_any, query_id, selector, text, visible_text};
use crate::{date, error::AppError, models::CalendarEvent, reference::ResourceRef, safe_url};

pub struct CalendarPage {
    pub events: Vec<CalendarEvent>,
    pub complete: bool,
    pub unparsed_times: usize,
    pub undated_events: usize,
    pub missing_course_ids: usize,
}

pub fn calendar_page(html: &str, base_url: &Url) -> Result<CalendarPage, AppError> {
    calendar_page_on(html, base_url, &date::seoul_today())
}

fn calendar_page_on(html: &str, base_url: &Url, today: &str) -> Result<CalendarPage, AppError> {
    let document = Html::parse_document(html);
    let events = selector(".event, [data-region=event-list-item]")?;
    let titles = selector(".card-header .name, h3.name, [data-region=event-name]")?;
    let course_links = selector("a[href*='course/view.php']")?;
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped = 0;
    for event in document.select(&events) {
        let Some((anchor, url)) = event_link(event, base_url)? else {
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
        let (starts_at, when_text) = event_time(event, base_url, today)?;
        let course_link = event.select(&course_links).next();
        let course_url = course_link
            .and_then(|link| link.value().attr("href"))
            .and_then(|href| base_url.join(href).ok());
        let title = event
            .select(&titles)
            .next()
            .map(visible_text)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| text(anchor));
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
    let explicit_empty = super::detail::safe_html_preview(html)
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
    // A time that is present but unreadable suggests changed markup; an event
    // with no time at all (e.g. a quiz without a close date) is simply undated.
    let unparsed_times = rows
        .iter()
        .filter(|event| event.starts_at.is_none() && event.when_text.is_some())
        .count();
    let undated_events = rows
        .iter()
        .filter(|event| event.starts_at.is_none() && event.when_text.is_none())
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
        undated_events,
        missing_course_ids,
    })
}

fn event_link<'a>(
    event: ElementRef<'a>,
    base_url: &Url,
) -> Result<Option<(ElementRef<'a>, Url)>, AppError> {
    let links = selector("a[href]")?;
    let footer = selector(".card-footer a[href]")?;
    let footer_links: Vec<_> = event.select(&footer).collect();
    let candidates: Vec<_> = if footer_links.is_empty() {
        event.select(&links).collect()
    } else {
        footer_links
    };
    let mut selected: Option<(ElementRef<'a>, Url)> = None;
    for anchor in candidates {
        let Some(mut url) = anchor
            .value()
            .attr("href")
            .and_then(|href| base_url.join(href).ok())
        else {
            continue;
        };
        if url.origin() != base_url.origin()
            || !((url.path().starts_with("/mod/") && url.path().ends_with("/view.php"))
                || url.path() == "/calendar/event.php")
        {
            continue;
        }
        // A footer action identifies the module, but is never a read target.
        // Retain only its numeric identity, dropping action/token parameters.
        if let Some(id) = query_id(&url, &["id"]) {
            url.set_query(None);
            url.set_fragment(None);
            url.query_pairs_mut().append_pair("id", &id);
        } else {
            continue;
        }
        if selected
            .as_ref()
            .is_some_and(|(_, previous)| previous != &url)
        {
            return Err(AppError::shape(
                "calendar event has ambiguous resource links",
            ));
        }
        selected = Some((anchor, url));
    }
    Ok(selected)
}

fn event_time(
    event: ElementRef<'_>,
    base_url: &Url,
    today: &str,
) -> Result<(Option<String>, Option<String>), AppError> {
    let times = selector("time")?;
    let regions =
        selector(".description > .row:first-child, [data-region=event-date], .eventdate")?;
    let links = selector("a[href]")?;
    let time = event.select(&times).next();
    let region = event.select(&regions).next();
    let timestamp = event
        .value()
        .attr("data-event-timestart")
        .or_else(|| event.value().attr("data-timestart"));
    let linked_timestamp = region.and_then(|region| {
        region.select(&links).find_map(|anchor| {
            let url = base_url.join(anchor.value().attr("href")?).ok()?;
            (url.origin() == base_url.origin() && url.path() == "/calendar/view.php")
                .then(|| {
                    url.query_pairs()
                        .find(|(key, _)| key == "time")
                        .map(|(_, value)| value.into_owned())
                })
                .flatten()
        })
    });
    let mut when_text = time
        .map(visible_text)
        .filter(|value| !value.is_empty())
        .or_else(|| region.map(visible_text).filter(|value| !value.is_empty()));
    let starts_at = timestamp
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(date::epoch_to_seoul)
        .or_else(|| {
            time.and_then(|node| node.value().attr("datetime"))
                .and_then(date::normalize_datetime)
        })
        .or_else(|| {
            linked_timestamp
                .as_deref()
                .and_then(|value| value.parse::<i64>().ok())
                .and_then(date::epoch_to_seoul)
        })
        .or_else(|| {
            when_text
                .as_deref()
                .and_then(|value| date::calendar_datetime(value, today))
        });
    if starts_at.is_none()
        && when_text.is_none()
        && (timestamp.is_some()
            || linked_timestamp.is_some()
            || time.is_some_and(|node| node.value().attr("datetime").is_some()))
    {
        when_text = Some("[unrecognized event timestamp]".into());
    }
    Ok((starts_at, when_text))
}

fn module_kind(url: &Url) -> Option<String> {
    let parts: Vec<_> = url.path_segments()?.collect();
    parts
        .windows(2)
        .find_map(|pair| (pair[0] == "mod").then(|| pair[1].to_owned()))
}

#[cfg(test)]
mod tests {
    use super::{calendar_page, calendar_page_on};
    use url::Url;

    const BASE: &str = "https://klms.kaist.ac.kr";

    #[test]
    fn calendar_cards_use_the_heading_and_timestamp_not_the_action_link() {
        let fixture = include_str!("../../tests/fixtures/localized/calendar.html");
        let base = Url::parse(BASE).unwrap();
        for label in ["내일", "Tomorrow", "morgen"] {
            let page =
                calendar_page_on(&fixture.replace("내일", label), &base, "2030-03-16").unwrap();
            let event = &page.events[0];
            assert_eq!(event.title, "Reading response is due");
            assert_eq!(event.reference.as_deref(), Some("assign:7"));
            assert_eq!(event.url, format!("{BASE}/mod/assign/view.php?id=7"));
            assert_eq!(
                event.starts_at.as_deref(),
                Some("2030-03-17T23:50:00+09:00")
            );
            assert!(page.complete);
            assert_eq!(page.unparsed_times, 0);
            assert_eq!(page.undated_events, 0);
        }
    }

    #[test]
    fn visible_dates_without_timestamps_are_not_silently_undated() {
        let fixture = include_str!("../../tests/fixtures/localized/calendar.html")
            .replace("&amp;time=1899989400", "");
        let base = Url::parse(BASE).unwrap();
        let page = calendar_page_on(&fixture, &base, "2030-12-31").unwrap();
        assert_eq!(
            page.events[0].starts_at.as_deref(),
            Some("2031-01-01T23:50:00+09:00")
        );
        let unknown = calendar_page_on(
            &fixture.replace("내일", "unknown date"),
            &base,
            "2030-12-31",
        )
        .unwrap();
        assert_eq!(unknown.unparsed_times, 1);
        assert_eq!(unknown.undated_events, 0);
        assert!(
            unknown.events[0]
                .when_text
                .as_deref()
                .unwrap()
                .contains("unknown date")
        );
    }

    #[test]
    fn ambiguous_resources_and_invalid_timestamp_markers_fail_visibly() {
        let base = Url::parse(BASE).unwrap();
        let ambiguous = "<main class='calendarwrapper'><div class='event'><a href='/mod/assign/view.php?id=7'>Work</a><a href='/mod/assign/view.php?id=8'>Other work</a></div></main>";
        assert!(calendar_page(ambiguous, &base).is_err());
        let invalid = "<main class='calendarwrapper'><div class='event'><a href='/mod/assign/view.php?id=7'>Work</a><time datetime='invalid'></time></div></main>";
        let page = calendar_page(invalid, &base).unwrap();
        assert_eq!(page.unparsed_times, 1);
        assert_eq!(page.undated_events, 0);
    }

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
    fn counts_events_without_any_time_as_undated_not_unparsed() {
        let page = calendar_page(
            "<main class='calendarwrapper'><div class='event'><a href='/mod/quiz/view.php?id=9'>Attempt quiz now</a></div></main>",
            &Url::parse(BASE).unwrap(),
        )
        .unwrap();
        assert_eq!(page.unparsed_times, 0);
        assert_eq!(page.undated_events, 1);
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
