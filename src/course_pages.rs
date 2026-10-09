//! Course activities across every page KLMS splits a course into.

use std::collections::HashSet;

use url::Url;

use crate::{client::KlmsClient, error::AppError, models::Activity, parse};

/// Activities from the default course page, plus every earlier week when the
/// course uses KLMS's paged week format.
///
/// That format shows only the current weeks on `course/view.php?id=<id>`; its
/// "All" view at `&section=0` lists every week. Other formats are unchanged.
pub fn activities(
    client: &KlmsClient,
    base_url: &Url,
    course_id: &str,
) -> Result<Vec<Activity>, AppError> {
    let first = client.get(&format!("/course/view.php?id={course_id}"))?;
    let mut rows = parse::activities(&first.text, base_url, None)?;
    if parse::has_all_weeks_view(&first.text, course_id)? {
        let all = client.get(&format!("/course/view.php?id={course_id}&section=0"))?;
        let mut seen: HashSet<String> = rows.iter().map(activity_key).collect();
        for row in parse::activities(&all.text, base_url, None)? {
            if seen.insert(activity_key(&row)) {
                rows.push(row);
            }
        }
    }
    Ok(rows)
}

fn activity_key(row: &Activity) -> String {
    row.id
        .clone()
        .or_else(|| row.url.clone())
        .unwrap_or_else(|| row.title.clone())
}
