//! Course activities across every page KLMS splits a course into.

use std::collections::HashSet;

use crate::{client::KlmsClient, error::AppError, models::Activity, parse};

/// Activities from the default course page, plus every earlier week when the
/// course uses KLMS's paged week format (its "All" view is `&section=0`).
pub fn activities(client: &KlmsClient, course_id: &str) -> Result<Vec<Activity>, AppError> {
    let first = client.get(&format!("/course/view.php?id={course_id}"))?;
    let mut rows = parse::activities(&first.text, &first.url, None)?;
    if parse::has_all_weeks_view(&first.text, course_id) {
        let all = client.get(&format!("/course/view.php?id={course_id}&section=0"))?;
        let key = |row: &Activity| {
            row.id
                .clone()
                .or_else(|| row.url.clone())
                .unwrap_or_else(|| row.title.clone())
        };
        let mut seen: HashSet<String> = rows.iter().map(key).collect();
        for row in parse::activities(&all.text, &all.url, None)? {
            if seen.insert(key(&row)) {
                rows.push(row);
            }
        }
    }
    Ok(rows)
}
