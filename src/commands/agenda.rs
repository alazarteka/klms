use super::Ctx;
use crate::{
    date,
    error::AppError,
    models::CalendarEvent,
    output::{CommandResult, body, cell, listing, row, table},
    parse,
};
use std::collections::BTreeSet;
const CALENDAR_PATH: &str = "/calendar/view.php?view=upcoming";

impl Ctx<'_> {
    pub(super) fn calendar(&self, list: &crate::cli::ListArgs) -> Result<CommandResult, AppError> {
        let (client, base) = (self.client, self.base);

        let page =
            parse::calendar_page(&client.get(CALENDAR_PATH)?.text, base, &date::seoul_today())?;
        let header = "WHEN\tREF\tCOURSE\tTITLE";
        let mut result = listing(
            "calendar.list",
            page.events,
            list.limit,
            page.complete,
            |rows, avail| match rows.is_empty() {
                true => "No upcoming calendar events found.".into(),
                false => table(rows, avail, "Calendar", header, event_row),
            },
        )?;
        if page.skipped > 0 {
            result.warnings.push(format!(
                "{} calendar event(s) skipped: no unambiguous header/footer action link",
                page.skipped
            ));
        }
        Ok(result)
    }
    pub(super) fn agenda(
        &self,
        course: Option<&str>,
        kinds: &[String],
        limit: usize,
        days: u32,
        label: &'static str,
    ) -> Result<CommandResult, AppError> {
        let course_id = (course.map(|c| self.resolve_course(c).map(|c| c.id))).transpose()?;
        let response = self.client.get(CALENDAR_PATH)?;
        let today = date::seoul_today();
        let through = date::add_days(&today, days as i64).expect("valid current date");
        let page = parse::calendar_page(&response.text, self.base, &today)?;
        if !page.complete || page.unparsed_times > 0 {
            return Err(AppError::shape(
                "cannot build a complete agenda from the current calendar page",
            ));
        }
        if course_id.is_some() && page.missing_course_ids > 0 {
            return Err(AppError::shape(
                "cannot apply a course filter because a calendar event has no course identity",
            ));
        }
        let undated = page.undated_events;
        let in_range: Vec<_> = (page.events.into_iter())
            .filter(|event| {
                let date = event.starts_at.as_deref().and_then(|value| value.get(..10));
                date.is_some_and(|d| d >= today.as_str() && d <= through.as_str())
                    && course_id
                        .as_ref()
                        .is_none_or(|id| event.course_id.as_ref() == Some(id))
            })
            .collect();
        let present: BTreeSet<_> = in_range
            .iter()
            .map(|event| event.kind.to_ascii_lowercase())
            .collect();
        let unmatched: BTreeSet<_> = kinds
            .iter()
            .filter(|kind| !present.contains(*kind))
            .collect();
        let mut rows: Vec<_> = (in_range.into_iter())
            .filter(|event| kinds.is_empty() || kinds.contains(&event.kind.to_ascii_lowercase()))
            .collect();
        rows.sort_by(|left, right| left.starts_at.cmp(&right.starts_at));
        let single = today == through;
        let span = match single {
            true => format!("for {today}"),
            false => format!("from {today} through {through}"),
        };
        let mut result = listing(label, rows, limit, true, |rows, available| {
            if rows.is_empty() {
                return match kinds.is_empty() {
                    true => format!("Nothing scheduled {span}."),
                    false => format!("Nothing of kind {} scheduled {span}.", kinds.join(", ")),
                };
            }
            let title = match single {
                true => format!("Today ({today})"),
                false => format!("Upcoming ({today} through {through})"),
            };
            body(
                rows,
                available,
                (&title, " items", "WHEN\tREF\tCOURSE\tTITLE"),
                event_row,
            )
        })?;
        if !unmatched.is_empty() {
            let unmatched: Vec<_> = unmatched.into_iter().map(String::as_str).collect();
            let present = match present.is_empty() {
                true => "none".to_owned(),
                false => present.into_iter().collect::<Vec<_>>().join(", "),
            };
            result.warnings.push(format!(
                "--kind {} matched no calendar row {span} (kinds present: {present})",
                unmatched.join(", ")
            ));
        }
        if undated > 0 {
            result.warnings.push(format!(
                "{undated} calendar event(s) have no date and are not shown; see `klms calendar list`"
            ));
        }
        Ok(result)
    }
}

fn event_row(r: &CalendarEvent) -> String {
    let (when, reference, course) = (
        cell(&r.starts_at, "unknown"),
        cell(&r.reference, "-"),
        cell(&r.course, "-"),
    );
    row(&[when, reference, course, &r.title])
}
