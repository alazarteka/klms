use crate::models::{Assignment, CalendarEvent, FileResource, Notice, Quiz, ResourceDetail};

pub fn assignments(rows: &[Assignment]) -> String {
    if rows.is_empty() {
        return "No assignments found.".into();
    }
    let mut output = format!(
        "Assignments — showing {}\nREF\tDUE\tSTATUS\tTITLE",
        rows.len()
    );
    for row in rows {
        output.push_str(&format!(
            "\n{}\t{}\t{}\t{}",
            row.reference,
            row.due_at.as_deref().unwrap_or("unknown"),
            row.submission_status.as_deref().unwrap_or("unknown"),
            row.title
        ));
    }
    output
}

pub fn quizzes(rows: &[Quiz]) -> String {
    if rows.is_empty() {
        return "No quizzes found.".into();
    }
    let mut output = format!(
        "Quizzes — showing {}\nREF\tCLOSES\tGRADE\tTITLE",
        rows.len()
    );
    for row in rows {
        output.push_str(&format!(
            "\n{}\t{}\t{}\t{}",
            row.reference,
            row.closes_at.as_deref().unwrap_or("unknown"),
            row.grade.as_deref().unwrap_or("-"),
            row.title
        ));
    }
    output
}

pub fn calendar(rows: &[CalendarEvent]) -> String {
    if rows.is_empty() {
        return "No upcoming calendar events found.".into();
    }
    let mut output = format!(
        "Calendar — showing {}\nWHEN\tREF\tCOURSE\tTITLE",
        rows.len()
    );
    for row in rows {
        output.push_str(&format!(
            "\n{}\t{}\t{}\t{}",
            row.starts_at.as_deref().unwrap_or("unknown"),
            row.reference.as_deref().unwrap_or("-"),
            row.course.as_deref().unwrap_or("-"),
            row.title
        ));
    }
    output
}

pub fn agenda(rows: &[CalendarEvent], start: &str, through: &str) -> String {
    if rows.is_empty() {
        return if start == through {
            format!("Nothing scheduled for {start}.")
        } else {
            format!("Nothing scheduled from {start} through {through}.")
        };
    }
    let heading = if start == through {
        format!("Today ({start}) — {} items", rows.len())
    } else {
        format!(
            "Upcoming ({start} through {through}) — {} items",
            rows.len()
        )
    };
    let mut output = format!("{heading}\nWHEN\tREF\tCOURSE\tTITLE");
    for row in rows {
        output.push_str(&format!(
            "\n{}\t{}\t{}\t{}",
            row.starts_at.as_deref().unwrap_or("unknown"),
            row.reference.as_deref().unwrap_or("-"),
            row.course.as_deref().unwrap_or("-"),
            row.title
        ));
    }
    output
}

pub fn notices(rows: &[Notice]) -> String {
    if rows.is_empty() {
        return "No notices found.".into();
    }
    let mut output = format!("Notices — showing {}\nPOSTED\tREF\tTITLE", rows.len());
    for row in rows {
        output.push_str(&format!(
            "\n{}\t{}\t{}",
            row.posted_at
                .as_deref()
                .or(row.posted_text.as_deref())
                .unwrap_or("unknown"),
            row.reference,
            row.title
        ));
    }
    output
}

pub fn files(rows: &[FileResource]) -> String {
    if rows.is_empty() {
        return "No course files found.".into();
    }
    let mut output = format!("Files — showing {}\nREF\tTYPE\tDOWNLOAD\tTITLE", rows.len());
    for row in rows {
        output.push_str(&format!(
            "\n{}\t{}\t{}\t{}",
            row.reference.as_deref().unwrap_or("-"),
            row.kind,
            if row.downloadable { "yes" } else { "no" },
            row.title
        ));
    }
    output
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
