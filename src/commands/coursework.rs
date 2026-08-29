use url::Url;

use super::{activity_result, course_activities, module_path, query_value, resolve_course};
use crate::{
    cli::{
        AgendaArgs, BoardsCommand, CalendarCommand, FilesCommand, ModuleCommand, NoticesCommand,
        UpcomingArgs,
    },
    client::KlmsClient,
    date,
    error::AppError,
    models::{Assignment, FileResource, Notice, Quiz},
    output::{self, CommandResult},
    parse, present,
    reference::ResourceRef,
};

pub(super) fn assignments(
    client: &KlmsClient,
    base_url: &Url,
    command: &ModuleCommand,
) -> Result<CommandResult, AppError> {
    match command {
        ModuleCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let response = client.get(&format!("/mod/assign/index.php?id={}", resolved.id))?;
            let mut rows = parse::assignments(&response.text, &response.url, &resolved)?;
            let available = rows.len();
            rows.truncate(list.limit);
            assignment_result(rows, list.limit, available)
        }
        ModuleCommand::Show { target } => show_module(
            client,
            base_url,
            target,
            &["assign"],
            "assign",
            "assignments.show",
        ),
    }
}

pub(super) fn quizzes(
    client: &KlmsClient,
    base_url: &Url,
    command: &ModuleCommand,
) -> Result<CommandResult, AppError> {
    match command {
        ModuleCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let response = client.get(&format!("/mod/quiz/index.php?id={}", resolved.id))?;
            let mut rows = parse::quizzes(&response.text, &response.url, &resolved)?;
            let available = rows.len();
            rows.truncate(list.limit);
            quiz_result(rows, list.limit, available)
        }
        ModuleCommand::Show { target } => {
            show_module(client, base_url, target, &["quiz"], "quiz", "quizzes.show")
        }
    }
}

pub(super) fn videos(
    client: &KlmsClient,
    base_url: &Url,
    command: &ModuleCommand,
) -> Result<CommandResult, AppError> {
    const KINDS: &[&str] = &["vod", "panoptocourseembed", "panopto", "lti"];
    match command {
        ModuleCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let mut rows = course_activities(client, base_url, &resolved, None)?;
            rows.retain(parse::is_video_activity);
            let available = rows.len();
            rows.truncate(list.limit);
            activity_result("videos.list", &resolved, rows, list.limit, available)
        }
        ModuleCommand::Show { target } => {
            let path = module_path(target, KINDS)?;
            let response = client.get(&path)?;
            let reference = ResourceRef::parse(target)
                .ok()
                .or_else(|| ResourceRef::from_url(&response.url))
                .ok_or_else(|| {
                    AppError::shape("module detail URL had no supported resource kind")
                })?;
            let detail_kind = reference.activity_kind().ok_or_else(|| {
                AppError::shape("module detail URL had no supported activity kind")
            })?;
            if !KINDS.contains(&detail_kind) {
                return Err(AppError::shape(
                    "module detail redirected to an unexpected resource kind",
                ));
            }
            let detail =
                parse::resource_detail(&response.text, base_url, &response.url, detail_kind)?;
            output::result("videos.show", &detail, present::detail(&detail))
        }
    }
}

pub(super) fn calendar(
    client: &KlmsClient,
    base_url: &Url,
    command: &CalendarCommand,
) -> Result<CommandResult, AppError> {
    match command {
        CalendarCommand::List(list) => {
            let response = client.get("/calendar/view.php?view=upcoming")?;
            let page = parse::calendar_page(&response.text, base_url)?;
            let source_complete = page.complete;
            let mut rows = page.events;
            let available = rows.len();
            rows.truncate(list.limit);
            let human = present::calendar(&rows, available);
            output::collection(
                "calendar.list",
                &rows,
                human,
                rows.len(),
                list.limit,
                available,
                source_complete,
            )
        }
    }
}

pub(super) fn upcoming(
    client: &KlmsClient,
    base_url: &Url,
    args: &UpcomingArgs,
) -> Result<CommandResult, AppError> {
    agenda(
        client,
        base_url,
        &args.clone().into(),
        args.through,
        "upcoming",
    )
}

impl From<UpcomingArgs> for AgendaArgs {
    fn from(args: UpcomingArgs) -> Self {
        Self {
            course: args.course,
            list: args.list,
        }
    }
}

pub(super) fn agenda(
    client: &KlmsClient,
    base_url: &Url,
    args: &AgendaArgs,
    days: u32,
    label: &'static str,
) -> Result<CommandResult, AppError> {
    let course_id = match &args.course {
        Some(course) => Some(resolve_course(client, base_url, course)?.id),
        None => None,
    };
    let response = client.get("/calendar/view.php?view=upcoming")?;
    let today = date::seoul_today();
    let through = date::add_days(&today, days as i64).expect("valid current date");
    let page = parse::calendar_page(&response.text, base_url)?;
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
    let mut rows: Vec<_> = page
        .events
        .into_iter()
        .filter(|event| {
            let date = event.starts_at.as_deref().and_then(|value| value.get(..10));
            date.is_some_and(|date| date >= today.as_str() && date <= through.as_str())
                && course_id
                    .as_ref()
                    .is_none_or(|course_id| event.course_id.as_ref() == Some(course_id))
        })
        .collect();
    rows.sort_by(|left, right| left.starts_at.cmp(&right.starts_at));
    let available = rows.len();
    rows.truncate(args.list.limit);
    let human = present::agenda(&rows, available, &today, &through);
    output::collection(
        label,
        &rows,
        human,
        rows.len(),
        args.list.limit,
        available,
        true,
    )
}

pub(super) fn boards(
    client: &KlmsClient,
    base_url: &Url,
    command: &BoardsCommand,
) -> Result<CommandResult, AppError> {
    match command {
        BoardsCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let mut rows = course_activities(client, base_url, &resolved, None)?;
            rows.retain(|row| row.kind.eq_ignore_ascii_case("courseboard"));
            let available = rows.len();
            rows.truncate(list.limit);
            activity_result("boards.list", &resolved, rows, list.limit, available)
        }
        BoardsCommand::Posts { board, list } => {
            let path = module_path(board, &["courseboard"])?;
            let response = client.get(&path)?;
            let board_id = query_value(&response.url, "id");
            let mut posts = parse::board_posts(&response.text, base_url, board_id)?;
            let available = posts.len();
            posts.truncate(list.limit);
            let human = present::board_posts(&posts, available);
            output::collection(
                "boards.posts",
                &posts,
                human,
                posts.len(),
                list.limit,
                available,
                !parse::has_next_page(&response.text)?,
            )
        }
        BoardsCommand::Show { post } => show_board_post(client, base_url, post, "boards.show"),
    }
}

pub(super) fn notices(
    client: &KlmsClient,
    base_url: &Url,
    command: &NoticesCommand,
) -> Result<CommandResult, AppError> {
    match command {
        NoticesCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let boards: Vec<_> = course_activities(client, base_url, &resolved, None)?
                .into_iter()
                .filter(parse::is_notice_board)
                .collect();
            let mut rows = Vec::new();
            let mut source_complete = true;
            for board in boards {
                let Some(board_ref) = board.reference else {
                    return Err(AppError::shape(
                        "notice board contained no canonical module reference",
                    ));
                };
                let path = ResourceRef::parse(&board_ref)?.path();
                let response = client.get(&path)?;
                source_complete &= !parse::has_next_page(&response.text)?;
                let board_id = query_value(&response.url, "id");
                for post in parse::board_posts(&response.text, base_url, board_id)? {
                    let Some(reference) = post.reference else {
                        return Err(AppError::shape(
                            "notice post contained no canonical post reference",
                        ));
                    };
                    rows.push(Notice {
                        reference,
                        board_ref: board_ref.clone(),
                        course_id: resolved.id.clone(),
                        course_ref: resolved.reference.clone(),
                        title: post.title,
                        posted_at: post.posted.as_deref().and_then(date::normalize_datetime),
                        posted_text: post.posted,
                        url: post.url,
                    });
                }
            }
            let available = rows.len();
            rows.truncate(list.limit);
            let human = present::notices(&rows, available);
            output::collection(
                "notices.list",
                &rows,
                human,
                rows.len(),
                list.limit,
                available,
                source_complete,
            )
        }
        NoticesCommand::Show { notice } => {
            show_board_post(client, base_url, notice, "notices.show")
        }
    }
}

pub(super) fn files(
    client: &KlmsClient,
    base_url: &Url,
    command: &FilesCommand,
) -> Result<CommandResult, AppError> {
    match command {
        FilesCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let mut rows = course_activities(client, base_url, &resolved, None)?;
            rows.retain(|row| {
                matches!(
                    row.kind.as_str(),
                    "resource" | "folder" | "page" | "coursefile" | "url"
                )
            });
            let mut files: Vec<_> = rows
                .into_iter()
                .map(|activity| FileResource {
                    reference: activity.reference,
                    id: activity.id,
                    downloadable: matches!(activity.kind.as_str(), "resource" | "coursefile")
                        && activity.url.is_some(),
                    kind: activity.kind,
                    title: activity.title,
                    course_id: resolved.id.clone(),
                    course_ref: resolved.reference.clone(),
                    week: activity.week,
                    section: activity.section,
                    url: activity.url,
                })
                .collect();
            let available = files.len();
            files.truncate(list.limit);
            let human = present::files(&files, available);
            output::collection(
                "files.list",
                &files,
                human,
                files.len(),
                list.limit,
                available,
                true,
            )
        }
        FilesCommand::Download { url, out } => {
            let source = if url.starts_with("file:") {
                ResourceRef::parse(url)?.path()
            } else {
                url.clone()
            };
            super::download::download(client, &source, out)
        }
    }
}

fn show_module(
    client: &KlmsClient,
    base_url: &Url,
    target: &str,
    kinds: &[&str],
    detail_kind: &str,
    command: &'static str,
) -> Result<CommandResult, AppError> {
    let path = module_path(target, kinds)?;
    let response = client.get(&path)?;
    let detail = parse::resource_detail(&response.text, base_url, &response.url, detail_kind)?;
    output::result(command, &detail, present::detail(&detail))
}

fn show_board_post(
    client: &KlmsClient,
    base_url: &Url,
    post: &str,
    command: &'static str,
) -> Result<CommandResult, AppError> {
    let target = if post.starts_with("board-post:") {
        ResourceRef::parse(post)?.path()
    } else if post.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::usage(
            "board post show requires a board-post:BOARD:POST reference or article URL",
        ));
    } else {
        post.into()
    };
    let response = client.get(&target)?;
    let detail =
        parse::resource_detail(&response.text, base_url, &response.url, "courseboard-post")?;
    output::result(command, &detail, present::detail(&detail))
}

fn assignment_result(
    rows: Vec<Assignment>,
    limit: usize,
    available: usize,
) -> Result<CommandResult, AppError> {
    let human = present::assignments(&rows, available);
    output::collection(
        "assignments.list",
        &rows,
        human,
        rows.len(),
        limit,
        available,
        true,
    )
}

fn quiz_result(rows: Vec<Quiz>, limit: usize, available: usize) -> Result<CommandResult, AppError> {
    let human = present::quizzes(&rows, available);
    output::collection(
        "quizzes.list",
        &rows,
        human,
        rows.len(),
        limit,
        available,
        true,
    )
}
