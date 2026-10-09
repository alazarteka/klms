use crate::models::{
    Assignment, BoardPost, CalendarEvent, FileResource, Notice, Quiz, ResourceDetail,
};

fn table(
    empty: &str,
    title: &str,
    shown: usize,
    available: usize,
    header: &str,
    rows: impl Iterator<Item = String>,
) -> String {
    if shown == 0 {
        return empty.into();
    }
    let mut output = format!("{title} — showing {shown} of {available}\n{header}");
    for row in rows {
        output.push('\n');
        output.push_str(&row);
    }
    output
}

pub fn assignments(rows: &[Assignment], available: usize) -> String {
    table(
        "No assignments found.",
        "Assignments",
        rows.len(),
        available,
        "REF\tDUE\tSTATUS\tTITLE",
        rows.iter().map(|row| {
            format!(
                "{}\t{}\t{}\t{}",
                row.reference,
                row.due_at.as_deref().unwrap_or("unknown"),
                row.submission_status.as_deref().unwrap_or("unknown"),
                row.title
            )
        }),
    )
}

pub fn quizzes(rows: &[Quiz], available: usize) -> String {
    table(
        "No quizzes found.",
        "Quizzes",
        rows.len(),
        available,
        "REF\tCLOSES\tGRADE\tTITLE",
        rows.iter().map(|row| {
            format!(
                "{}\t{}\t{}\t{}",
                row.reference,
                row.closes_at.as_deref().unwrap_or("unknown"),
                row.grade.as_deref().unwrap_or("-"),
                row.title
            )
        }),
    )
}

fn event_row(row: &CalendarEvent) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        row.starts_at.as_deref().unwrap_or("unknown"),
        row.reference.as_deref().unwrap_or("-"),
        row.course.as_deref().unwrap_or("-"),
        row.title
    )
}

pub fn calendar(rows: &[CalendarEvent], available: usize) -> String {
    table(
        "No upcoming calendar events found.",
        "Calendar",
        rows.len(),
        available,
        "WHEN\tREF\tCOURSE\tTITLE",
        rows.iter().map(event_row),
    )
}

pub fn agenda(rows: &[CalendarEvent], available: usize, start: &str, through: &str) -> String {
    if rows.is_empty() {
        return if start == through {
            format!("Nothing scheduled for {start}.")
        } else {
            format!("Nothing scheduled from {start} through {through}.")
        };
    }
    let title = if start == through {
        format!("Today ({start})")
    } else {
        format!("Upcoming ({start} through {through})")
    };
    let mut output = format!(
        "{title} — showing {} of {available} items\nWHEN\tREF\tCOURSE\tTITLE",
        rows.len()
    );
    for row in rows {
        output.push('\n');
        output.push_str(&event_row(row));
    }
    output
}

pub fn notices(rows: &[Notice], available: usize) -> String {
    table(
        "No notices found.",
        "Notices",
        rows.len(),
        available,
        "POSTED\tREF\tTITLE",
        rows.iter().map(|row| {
            format!(
                "{}\t{}\t{}",
                row.posted_at
                    .as_deref()
                    .or(row.posted_text.as_deref())
                    .unwrap_or("unknown"),
                row.reference,
                row.title
            )
        }),
    )
}

pub fn board_posts(rows: &[BoardPost], available: usize) -> String {
    table(
        "No board posts found.",
        "Board posts",
        rows.len(),
        available,
        "REF\tPOSTED\tTITLE",
        rows.iter().map(|row| {
            format!(
                "{}\t{}\t{}",
                row.reference.as_deref().unwrap_or("-"),
                row.posted.as_deref().unwrap_or("-"),
                row.title
            )
        }),
    )
}

pub fn files(rows: &[FileResource], available: usize) -> String {
    table(
        "No course files found.",
        "Files",
        rows.len(),
        available,
        "REF\tTYPE\tDOWNLOAD\tTITLE",
        rows.iter().map(|row| {
            format!(
                "{}\t{}\t{}\t{}",
                row.reference.as_deref().unwrap_or("-"),
                row.kind,
                if row.downloadable { "yes" } else { "no" },
                row.title
            )
        }),
    )
}

pub fn detail(detail: &ResourceDetail) -> String {
    let mut output = format!(
        "{}\nType: {}\nRef: {}\nURL: {}",
        detail.title,
        detail.kind,
        detail.reference.as_deref().unwrap_or("-"),
        detail.url
    );
    if !detail.text.is_empty() {
        output.push_str(&format!("\n\n{}", detail.text));
    }
    if detail.text_truncated {
        output.push_str("\n\n[Detail text truncated]");
    }
    if !detail.links.is_empty() {
        output.push_str("\n\nLinks:");
        for link in &detail.links {
            output.push_str(&format!("\n{}\t{}", link.title, link.url));
        }
    }
    if detail.links_truncated {
        output.push_str("\n[Link list truncated]");
    }
    output
}
