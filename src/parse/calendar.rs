use std::collections::HashSet;

use crate::url::Url;
use scraper::{ElementRef, Html};

use super::shared::{
    NEXT_PAGE_SELECTORS, has_any, href_url, module_view, query_id, sel, text, visible_text,
};
use crate::{
    date,
    error::AppError,
    models::CalendarEvent,
    reference::{ResourceRef, module_kind},
    safe_url,
};

pub struct CalendarPage {
    pub events: Vec<CalendarEvent>,
    pub complete: bool,
    pub skipped: usize,
    pub unparsed_times: usize,
    pub undated_events: usize,
    pub missing_course_ids: usize,
}

pub fn calendar_page(html: &str, base_url: &Url, today: &str) -> Result<CalendarPage, AppError> {
    let document = Html::parse_document(html);
    let titles = sel(".card-header .name, h3.name, [data-region=event-name]");
    let course_links = sel("a[href*='course/view.php']");
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    if !has_any(
        &document,
        &[".calendarwrapper", "[data-region=event-list-content]"],
    ) {
        return Err(AppError::shape("calendar region is missing"));
    }
    let mut skipped = 0;
    for event in document.select(&sel(".calendarwrapper .event, .calendarwrapper [data-region=event-list-item], [data-region=event-list-content] .event, [data-region=event-list-content] [data-region=event-list-item]")) {
        // Recognizable event records without a usable action link are incomplete.
        let Some((anchor, url)) = event_link(event, base_url) else {
            skipped += usize::from(event.value().attr("data-event-id").is_some()
                || event.value().attr("data-region") == Some("event-list-item"));
            continue;
        };
        let attr = |name: &str| event.value().attr(name);
        let kind = attr("data-event-component")
            .and_then(|value| value.strip_prefix("mod_"))
            .map(str::to_owned)
            .or_else(|| module_kind(&url).map(str::to_owned))
            .unwrap_or_else(|| "event".into());
        let reference = (kind != "event")
            .then(|| ResourceRef::from_activity(&kind, None, Some(&url)))
            .flatten()
            .map(|reference| reference.to_string());
        let (starts_at, when_text) = event_time(event, base_url, today);
        let course_link = event.select(&course_links).next();
        let title = event
            .select(&titles)
            .next()
            .map(visible_text)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| text(anchor));
        let identity = attr("data-event-id")
            .or_else(|| attr("data-eventid"))
            .map(|id| format!("event:{id}"))
            .unwrap_or_else(|| format!("{}|{}|{}", url, starts_at.as_deref().unwrap_or(""), title));
        if !seen.insert(identity) {
            continue;
        }
        rows.push(CalendarEvent {
            reference,
            kind,
            title,
            course_id: course_link
                .and_then(|link| href_url(link, base_url))
                .and_then(|url| query_id(&url, &["id"]))
                .or_else(|| attr("data-course-id").map(str::to_owned)),
            course: course_link.map(text).filter(|value| !value.is_empty()),
            starts_at,
            when_text,
            url: safe_url::display(&url),
        });
    }
    // A time that is present but unreadable suggests changed markup; an event
    // with no time at all (e.g. a quiz without a close date) is simply undated.
    let count = |keep: &dyn Fn(&CalendarEvent) -> bool| rows.iter().filter(|e| keep(e)).count();
    let unparsed_times = count(&|e| e.starts_at.is_none() && e.when_text.is_some());
    let undated_events = count(&|e| e.starts_at.is_none() && e.when_text.is_none());
    let missing_course_ids = count(&|e| e.course_id.is_none());
    let has_next = has_any(&document, NEXT_PAGE_SELECTORS);
    Ok(CalendarPage {
        events: rows,
        complete: !has_next && skipped == 0,
        skipped,
        unparsed_times,
        undated_events,
        missing_course_ids,
    })
}

fn event_link<'a>(event: ElementRef<'a>, base_url: &Url) -> Option<(ElementRef<'a>, Url)> {
    // Description HTML may cross-link unrelated modules; only actions identify this event.
    let actions = sel(
        ".card-header a[href], .card-footer a[href], h3.name a[href], [data-region=event-name] a[href]",
    );
    let mut links = event.select(&actions).filter_map(|anchor| {
        let mut url = href_url(anchor, base_url)?;
        if url.origin() != base_url.origin()
            || !(module_view(&url) || url.path() == "/calendar/event.php")
        {
            return None;
        }
        // Actions identify the module, but are never read targets: discard their parameters.
        let id = query_id(&url, &["id"])?;
        url.set_query(None);
        url.set_fragment(None);
        url.query_pairs_mut().append_pair("id", &id);
        Some((anchor, url))
    });
    let selected = links.next()?;
    links.all(|(_, url)| url == selected.1).then_some(selected)
}

fn event_time(
    event: ElementRef<'_>,
    base_url: &Url,
    today: &str,
) -> (Option<String>, Option<String>) {
    let time = event.select(&sel("time")).next();
    let region = event
        .select(&sel(
            ".description > .row:first-child, [data-region=event-date], .eventdate",
        ))
        .next();
    let epoch = |value: &str| value.parse::<i64>().ok().and_then(date::epoch_to_seoul);
    let timestamp = event
        .value()
        .attr("data-event-timestart")
        .or_else(|| event.value().attr("data-timestart"));
    let linked_timestamp = region.and_then(|region| {
        region.select(&sel("a[href]")).find_map(|anchor| {
            let url = href_url(anchor, base_url)?;
            if url.origin() != base_url.origin() || url.path() != "/calendar/view.php" {
                return None;
            }
            url.query_pairs()
                .find(|(key, _)| key == "time")
                .map(|(_, value)| value.into_owned())
        })
    });
    let datetime = time.and_then(|node| node.value().attr("datetime"));
    let mut when_text = time
        .into_iter()
        .chain(region)
        .map(visible_text)
        .find(|value| !value.is_empty());
    let starts_at = timestamp
        .and_then(epoch)
        .or_else(|| datetime.and_then(date::normalize_datetime))
        .or_else(|| linked_timestamp.as_deref().and_then(epoch))
        .or_else(|| {
            when_text
                .as_deref()
                .and_then(|value| date::calendar_datetime(value, today))
        });
    if starts_at.is_none()
        && when_text.is_none()
        && (timestamp.is_some() || linked_timestamp.is_some() || datetime.is_some())
    {
        when_text = Some("[unrecognized event timestamp]".into());
    }
    (starts_at, when_text)
}

#[cfg(test)]
mod tests {
    use super::calendar_page;
    use crate::url::Url;

    const BASE: &str = "https://klms.kaist.ac.kr";

    fn page(inner: &str) -> super::CalendarPage {
        let html = format!("<main class='calendarwrapper'>{inner}</main>");
        calendar_page(
            &html,
            &Url::parse(BASE).unwrap(),
            &crate::date::seoul_today(),
        )
        .unwrap()
    }

    #[test]
    fn calendar_cards_use_the_heading_and_timestamp_not_the_action_link() {
        let fixture = include_str!("../../tests/fixtures/localized/calendar.html");
        let base = Url::parse(BASE).unwrap();
        for label in ["내일", "Tomorrow", "morgen"] {
            let html = fixture.replace("내일", label);
            let page = calendar_page(&html, &base, "2030-03-16").unwrap();
            let event = &page.events[0];
            assert_eq!(event.title, "Reading response is due");
            assert_eq!(event.reference.as_deref(), Some("assign:7"));
            assert_eq!(event.course_id.as_deref(), Some("42"));
            assert_eq!(event.url, format!("{BASE}/mod/assign/view.php?id=7"));
            assert_eq!(
                event.starts_at.as_deref(),
                Some("2030-03-17T23:50:00+09:00")
            );
            assert!(page.complete);
            assert_eq!((page.unparsed_times, page.undated_events), (0, 0));
        }
    }

    #[test]
    fn visible_dates_without_timestamps_are_not_silently_undated() {
        let fixture = include_str!("../../tests/fixtures/localized/calendar.html")
            .replace("&amp;time=1899989400", "");
        let base = Url::parse(BASE).unwrap();
        let page = calendar_page(&fixture, &base, "2030-12-31").unwrap();
        assert_eq!(
            page.events[0].starts_at.as_deref(),
            Some("2031-01-01T23:50:00+09:00")
        );
        let html = fixture.replace("내일", "unknown date");
        let unknown = calendar_page(&html, &base, "2030-12-31").unwrap();
        assert_eq!((unknown.unparsed_times, unknown.undated_events), (1, 0));
        let when = unknown.events[0].when_text.as_deref().unwrap();
        assert!(when.contains("unknown date"));
    }

    #[test]
    fn ambiguous_actions_are_skipped_and_missing_regions_fail() {
        let ambiguous = page(
            "<div class='event' data-event-id='1'><div class='card-footer'><a href='/mod/assign/view.php?id=7'>Work</a><a href='/mod/assign/view.php?id=8'>Other</a></div></div>",
        );
        assert!(!ambiguous.complete);
        assert_eq!(ambiguous.skipped, 1);
        assert!(
            calendar_page(
                "<main>Maintenance</main>",
                &Url::parse(BASE).unwrap(),
                "2030-12-31"
            )
            .is_err()
        );
        assert!(page("").complete);
    }

    #[test]
    fn counts_unreadable_and_undated_events_and_accepts_empty_chrome() {
        for (time, unparsed, undated) in [
            ("<time datetime='invalid'></time>", 1, 0),
            ("<time>sometime</time>", 1, 0),
            ("", 0, 1),
        ] {
            let parsed = page(&format!(
                "<div class='event'><div class='card-footer'><a href='/mod/quiz/view.php?id=9'>Quiz</a></div>{time}</div>"
            ));
            assert_eq!(
                (parsed.unparsed_times, parsed.undated_events),
                (unparsed, undated)
            );
            assert_eq!(parsed.missing_course_ids, 1);
        }
        assert!(page("<div class='event'>No detail link</div>").complete);
    }

    #[test]
    fn preserves_distinct_events_that_share_a_module_url() {
        let page = page(
            "<div class='event'><div class='card-footer'><a href='/mod/quiz/view.php?id=8'>Quiz opens</a></div><time datetime='2026-09-01T09:00:00+09:00'>open</time></div>\
             <div class='event'><div class='card-footer'><a href='/mod/quiz/view.php?id=8'>Quiz closes</a></div><time datetime='2026-09-02T18:00:00+09:00'>close</time></div>",
        );
        assert_eq!(page.events.len(), 2);
        assert!(page.complete);
    }
}
