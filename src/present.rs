use crate::models::{
    Assignment, BoardPost, CalendarEvent, FileResource, Notice, Quiz, ResourceDetail,
};

pub fn assignments(rows: &[Assignment], available: usize) -> String {
    if rows.is_empty() {
        return "No assignments found.".into();
    }
    let mut output = format!(
        "Assignments — showing {} of {available}\nREF\tDUE\tSTATUS\tTITLE",
        rows.len(),
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

pub fn quizzes(rows: &[Quiz], available: usize) -> String {
    if rows.is_empty() {
        return "No quizzes found.".into();
    }
    let mut output = format!(
        "Quizzes — showing {} of {available}\nREF\tCLOSES\tGRADE\tTITLE",
        rows.len(),
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

pub fn calendar(rows: &[CalendarEvent], available: usize) -> String {
    if rows.is_empty() {
        return "No upcoming calendar events found.".into();
    }
    let mut output = format!(
        "Calendar — showing {} of {available}\nWHEN\tREF\tCOURSE\tTITLE",
        rows.len(),
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

pub fn agenda(rows: &[CalendarEvent], available: usize, start: &str, through: &str) -> String {
    if rows.is_empty() {
        return if start == through {
            format!("Nothing scheduled for {start}.")
        } else {
            format!("Nothing scheduled from {start} through {through}.")
        };
    }
    let heading = if start == through {
        format!(
            "Today ({start}) — showing {} of {available} items",
            rows.len()
        )
    } else {
        format!(
            "Upcoming ({start} through {through}) — showing {} of {available} items",
            rows.len(),
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

pub fn notices(rows: &[Notice], available: usize) -> String {
    if rows.is_empty() {
        return "No notices found.".into();
    }
    let mut output = format!(
        "Notices — showing {} of {available}\nPOSTED\tREF\tTITLE",
        rows.len()
    );
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

pub fn board_posts(rows: &[BoardPost], available: usize) -> String {
    if rows.is_empty() {
        return "No board posts found.".into();
    }
    let mut output = format!(
        "Board posts — showing {} of {available}\nREF\tPOSTED\tTITLE",
        rows.len()
    );
    for row in rows {
        output.push_str(&format!(
            "\n{}\t{}\t{}",
            row.reference.as_deref().unwrap_or("-"),
            row.posted.as_deref().unwrap_or("-"),
            row.title
        ));
    }
    output
}

pub fn files(rows: &[FileResource], available: usize) -> String {
    if rows.is_empty() {
        return "No course files found.".into();
    }
    let mut output = format!(
        "Files — showing {} of {available}\nREF\tTYPE\tDOWNLOAD\tTITLE",
        rows.len()
    );
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
