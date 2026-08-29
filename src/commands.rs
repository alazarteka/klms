use std::{fs::OpenOptions, io::Write, path::Path};

use serde::Serialize;
use url::Url;

use crate::{
    auth,
    cli::{
        ActivitiesCommand, AuthCommand, BoardsCommand, CalendarCommand, Cli, Command,
        CourseShowCommand, CoursesCommand, FilesCommand, ModuleCommand, RequestCommand,
    },
    client::{KlmsClient, validate_base_url},
    error::AppError,
    models::{Activity, Course, DownloadResult, RawGet, Report, SessionTime},
    output::{self, CommandResult},
    parse,
};

const MAX_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;

pub fn run(cli: &Cli) -> Result<CommandResult, AppError> {
    let base_url = validate_base_url(&cli.base_url)?;
    let session = auth::load(&base_url)?;
    match &cli.command {
        Command::Auth(args) if matches!(args.command, AuthCommand::Status) => {
            auth_status(&session.status)
        }
        Command::Doctor => doctor(&base_url, session, cli.timeout),
        command => {
            let cookie = session.cookie_header.as_deref().ok_or_else(|| {
                AppError::auth(
                    "no usable KLMS session was found",
                    "Set KLMS_STORAGE_STATE or refresh an existing KLMS storage-state session.",
                )
            })?;
            let client = KlmsClient::new(base_url.as_str(), Some(cookie), cli.timeout)?;
            live(command, &client, &base_url)
        }
    }
}

fn auth_status(status: &auth::AuthStatus) -> Result<CommandResult, AppError> {
    let human = if status.configured {
        format!(
            "Storage state: {}\nSource: {}\nCookies: {} total, {} applicable\nExpired cookies present: {}",
            status.path.as_deref().unwrap_or("unknown"),
            status.source,
            status.cookie_count,
            status.matching_cookie_count,
            yes_no(status.has_expired_cookies)
        )
    } else {
        "Storage state: not configured\nUse KLMS_STORAGE_STATE or ~/.config/klms/storage-state.json"
            .into()
    };
    output::result("auth.status", status, human)
}

#[derive(Serialize)]
struct Doctor {
    version: &'static str,
    base_url: String,
    auth: auth::AuthStatus,
    session_status: &'static str,
    session_error: Option<DoctorError>,
    dashboard_url: Option<String>,
    check_may_have_extended_session: bool,
}

#[derive(Serialize)]
struct DoctorError {
    code: &'static str,
    message: String,
    retryable: bool,
}

fn doctor(
    base_url: &Url,
    session: auth::AuthSession,
    timeout: u64,
) -> Result<CommandResult, AppError> {
    let mut session_status = "not_configured";
    let mut session_error = None;
    let mut dashboard_url = None;
    let mut check_may_have_extended_session = false;
    if let Some(cookie) = session.cookie_header.as_deref() {
        check_may_have_extended_session = true;
        match KlmsClient::new(base_url.as_str(), Some(cookie), timeout)
            .and_then(|client| client.get("/my/"))
        {
            Ok(response) => {
                cache_page_sesskey(base_url, &response.text);
                session_status = "valid";
                dashboard_url = Some(response.url.into());
            }
            Err(error) => {
                session_status = match error.code {
                    "AUTH_REQUIRED" => "expired",
                    "NETWORK_ERROR" => "unreachable",
                    _ => "error",
                };
                session_error = Some(DoctorError {
                    code: error.code,
                    message: error.message,
                    retryable: error.retryable,
                });
            }
        }
    }
    let model = Doctor {
        version: env!("CARGO_PKG_VERSION"),
        base_url: base_url.to_string(),
        auth: session.status,
        session_status,
        session_error,
        dashboard_url,
        check_may_have_extended_session,
    };
    let mut human = format!(
        "klms {}\nOrigin: {}\nStorage state: {}\nSession: {}",
        model.version,
        model.base_url,
        if model.auth.configured {
            &model.auth.source
        } else {
            "missing"
        },
        model.session_status
    );
    if let Some(error) = &model.session_error {
        human.push_str(&format!("\nCheck: {} — {}", error.code, error.message));
    }
    let mut result = output::result("doctor", &model, human)?;
    if model.check_may_have_extended_session {
        result.warnings.push(
            "The dashboard request used to validate the session may refresh KLMS activity time."
                .into(),
        );
    }
    Ok(result)
}

fn live(command: &Command, client: &KlmsClient, base_url: &Url) -> Result<CommandResult, AppError> {
    match command {
        Command::Auth(args) => match args.command {
            AuthCommand::Status => unreachable!("handled before live dispatch"),
            AuthCommand::TimeLeft => session_time(client, base_url, false),
            AuthCommand::Extend => session_time(client, base_url, true),
        },
        Command::Dashboard(args) => {
            let response = client.get("/my/")?;
            cache_page_sesskey(base_url, &response.text);
            let mut model = parse::dashboard(&response.text, base_url)?;
            model.courses.truncate(args.limit);
            model.upcoming.truncate(args.limit);
            let mut human = format!(
                "{} — {} courses",
                model.term.as_deref().unwrap_or("Current dashboard"),
                model.course_count
            );
            for course in &model.courses {
                human.push_str(&format!(
                    "\n{}\t{}\t{}",
                    course.id,
                    course.code.as_deref().unwrap_or("-"),
                    course.title
                ));
            }
            output::result("dashboard", &model, human)
        }
        Command::Courses(args) => match &args.command {
            CoursesCommand::List(list) => {
                let mut courses = dashboard_courses(client, base_url)?;
                courses.truncate(list.limit);
                output::result("courses.list", &courses, render_courses(&courses))
            }
            CoursesCommand::Resolve { query, list } => {
                let mut matches = matching_courses(dashboard_courses(client, base_url)?, query);
                matches.truncate(list.limit);
                output::result("courses.resolve", &matches, render_courses(&matches))
            }
            CoursesCommand::Show { course } => {
                let resolved = resolve_course(client, base_url, course)?;
                let response = client.get(&format!("/course/view.php?id={}", resolved.id))?;
                let model = parse::course_detail(&response.text, base_url, resolved)?;
                let human = format!(
                    "{}\nID: {}\nCode: {}\nProfessors: {}\nActivities: {}\n{}",
                    model.course.title,
                    model.course.id,
                    model.course.code.as_deref().unwrap_or("unknown"),
                    if model.professors.is_empty() {
                        "unknown".into()
                    } else {
                        model.professors.join(", ")
                    },
                    model.activity_count,
                    model.course.url
                );
                output::result("courses.show", &model, human)
            }
        },
        Command::Activities(args) => match &args.command {
            ActivitiesCommand::List {
                course,
                week,
                kind,
                list,
            } => {
                let resolved = resolve_course(client, base_url, course)?;
                let mut rows = course_activities(client, base_url, &resolved, *week)?;
                if let Some(kind) = kind {
                    rows.retain(|row| row.kind.eq_ignore_ascii_case(kind));
                }
                rows.truncate(list.limit);
                activity_result("activities.list", &resolved, rows)
            }
        },
        Command::Assignments(args) => {
            module_command(client, base_url, &args.command, "assign", "assignments")
        }
        Command::Quizzes(args) => {
            module_command(client, base_url, &args.command, "quiz", "quizzes")
        }
        Command::Videos(args) => module_command_multi(
            client,
            base_url,
            &args.command,
            &["vod", "panoptocourseembed", "panopto"],
            "videos",
        ),
        Command::Calendar(args) => match &args.command {
            CalendarCommand::List(list) => {
                let response = client.get("/calendar/view.php?view=upcoming")?;
                let rows = parse::calendar(&response.text, base_url, list.limit)?;
                let human = render_links(&rows);
                output::result("calendar.list", &rows, human)
            }
        },
        Command::Boards(args) => boards(client, base_url, &args.command),
        Command::Files(args) => files(client, base_url, &args.command),
        Command::Grades(args) => match &args.command {
            CourseShowCommand::Show { course } => {
                let resolved = resolve_course(client, base_url, course)?;
                let response =
                    client.get(&format!("/grade/report/user/index.php?id={}", resolved.id))?;
                report_result(
                    "grades.show",
                    &resolved.title,
                    parse::grades(&response.text, resolved.id)?,
                )
            }
        },
        Command::Attendance(args) => match &args.command {
            CourseShowCommand::Show { course } => {
                let resolved = resolve_course(client, base_url, course)?;
                let response = client.get(&format!(
                    "/local/lmsattendance/index.php?id={}",
                    resolved.id
                ))?;
                report_result(
                    "attendance.show",
                    &resolved.title,
                    parse::attendance(&response.text, resolved.id)?,
                )
            }
        },
        Command::Request(args) => match &args.command {
            RequestCommand::Get { path, max_bytes } => raw_get(client, path, *max_bytes),
        },
        Command::Doctor => unreachable!("handled before live dispatch"),
    }
}

fn session_time(
    client: &KlmsClient,
    base_url: &Url,
    extend: bool,
) -> Result<CommandResult, AppError> {
    if let Some(key) = auth::cached_sesskey(base_url) {
        if let Ok(seconds) = query_session_time(client, &key, extend) {
            return session_time_result(seconds, extend, false);
        }
    }
    let dashboard = client.get("/my/")?;
    let key = parse::sesskey(&dashboard.text)?;
    auth::cache_sesskey(base_url, &key);
    let seconds = query_session_time(client, &key, extend)?;
    session_time_result(seconds, extend, true)
}

fn query_session_time(client: &KlmsClient, key: &str, extend: bool) -> Result<u64, AppError> {
    if extend {
        client.ajax(key, "core_session_touch")?;
    }
    let data = client.ajax(key, "core_session_time_remaining")?;
    data.get("timeremaining")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| data.as_u64())
        .ok_or_else(|| AppError::shape("session time response did not contain timeremaining"))
}

fn session_time_result(
    seconds: u64,
    extend: bool,
    bootstrap: bool,
) -> Result<CommandResult, AppError> {
    let model = SessionTime {
        remaining_seconds: seconds,
        remaining: duration(seconds),
        bootstrap_may_have_extended_session: bootstrap,
        extended: extend,
    };
    let mut result = output::result(
        if extend {
            "auth.extend"
        } else {
            "auth.time-left"
        },
        &model,
        format!(
            "Session time remaining: {}{}",
            model.remaining,
            if extend { " (extended)" } else { "" }
        ),
    )?;
    if bootstrap {
        result.warnings.push(
            "A dashboard request was needed to discover the session key and may have refreshed KLMS activity time."
                .into(),
        );
    }
    Ok(result)
}

fn module_command(
    client: &KlmsClient,
    base_url: &Url,
    command: &ModuleCommand,
    kind: &str,
    label: &'static str,
) -> Result<CommandResult, AppError> {
    module_command_multi(client, base_url, command, &[kind], label)
}

fn module_command_multi(
    client: &KlmsClient,
    base_url: &Url,
    command: &ModuleCommand,
    kinds: &[&str],
    label: &'static str,
) -> Result<CommandResult, AppError> {
    match command {
        ModuleCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let mut rows = course_activities(client, base_url, &resolved, None)?;
            rows.retain(|row| {
                kinds.iter().any(|kind| row.kind.eq_ignore_ascii_case(kind))
                    || (label == "videos"
                        && (row.title.to_ascii_lowercase().contains("panopto")
                            || row.title.to_ascii_lowercase().contains("vod")))
            });
            rows.truncate(list.limit);
            activity_result(command_name(label, "list"), &resolved, rows)
        }
        ModuleCommand::Show { target } => {
            let path = module_path(target, kinds[0])?;
            let response = client.get(&path)?;
            let detail = parse::resource_detail(&response.text, base_url, &response.url, kinds[0])?;
            output::result(
                command_name(label, "show"),
                &detail,
                format!("{}\n{}", detail.title, detail.text),
            )
        }
    }
}

fn boards(
    client: &KlmsClient,
    base_url: &Url,
    command: &BoardsCommand,
) -> Result<CommandResult, AppError> {
    match command {
        BoardsCommand::List { course, list } => {
            let resolved = resolve_course(client, base_url, course)?;
            let mut rows = course_activities(client, base_url, &resolved, None)?;
            rows.retain(|row| row.kind.eq_ignore_ascii_case("courseboard"));
            rows.truncate(list.limit);
            activity_result("boards.list", &resolved, rows)
        }
        BoardsCommand::Posts { board, list } => {
            let path = module_path(board, "courseboard")?;
            let response = client.get(&path)?;
            let board_id = query_value(&response.url, "id");
            let mut posts = parse::board_posts(&response.text, base_url, board_id)?;
            posts.truncate(list.limit);
            let human = posts
                .iter()
                .map(|post| {
                    format!(
                        "{}\t{}\t{}",
                        post.id.as_deref().unwrap_or("-"),
                        post.posted.as_deref().unwrap_or("-"),
                        post.title
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            output::result("boards.posts", &posts, human)
        }
        BoardsCommand::Show { post } => {
            if post.chars().all(|c| c.is_ascii_digit()) {
                return Err(AppError::usage(
                    "board post show requires the article URL returned by `boards posts`",
                ));
            }
            let response = client.get(post)?;
            let detail = parse::resource_detail(
                &response.text,
                base_url,
                &response.url,
                "courseboard-post",
            )?;
            output::result(
                "boards.show",
                &detail,
                format!("{}\n{}", detail.title, detail.text),
            )
        }
    }
}

fn files(
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
            rows.truncate(list.limit);
            activity_result("files.list", &resolved, rows)
        }
        FilesCommand::Download { url, out } => download(client, url, out),
    }
}

fn download(client: &KlmsClient, source: &str, out: &Path) -> Result<CommandResult, AppError> {
    if out.exists() {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let parent = out
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        return Err(AppError::config(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }
    let response = client.get_bytes(source, MAX_DOWNLOAD_BYTES)?;
    let temp = parent.join(format!(".klms-download-{}.part", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| AppError::config(format!("cannot create temporary download: {error}")))?;
    if let Err(error) = file
        .write_all(&response.bytes)
        .and_then(|_| file.sync_all())
    {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::network(format!(
            "failed to write download: {error}"
        )));
    }
    std::fs::hard_link(&temp, out).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        if out.exists() {
            AppError::config(format!("destination already exists: {}", out.display()))
        } else {
            AppError::config(format!("failed to finalize download: {error}"))
        }
    })?;
    std::fs::remove_file(&temp).map_err(|error| {
        AppError::config(format!(
            "download completed but temporary link cleanup failed: {error}"
        ))
    })?;
    let model = DownloadResult {
        path: out.display().to_string(),
        bytes: response.bytes.len(),
        source_url: response.url.into(),
        content_type: response.content_type,
    };
    output::result(
        "files.download",
        &model,
        format!("Downloaded {} bytes to {}", model.bytes, model.path),
    )
}

fn raw_get(client: &KlmsClient, path: &str, max_bytes: usize) -> Result<CommandResult, AppError> {
    let response = client.get_preview(path, max_bytes)?;
    let is_text = response.content_type.as_deref().is_none_or(|value| {
        let value = value.to_ascii_lowercase();
        value.starts_with("text/") || value.contains("json") || value.contains("xml")
    });
    if !is_text {
        return Err(AppError::usage(
            "request get previews text responses only; use `files download` for binary content",
        ));
    }
    let source = String::from_utf8_lossy(&response.bytes);
    let body = if response
        .content_type
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
    {
        redact_secrets(&parse::safe_html_preview(&source))
    } else {
        redact_secrets(&source)
    };
    let safe_url = redact_url(&response.url);
    let model = RawGet {
        url: safe_url,
        content_type: response.content_type,
        bytes: response.bytes.len(),
        body,
        truncated: response.truncated,
        redacted: true,
    };
    output::result("request.get", &model, model.body.clone())
}

fn redact_url(url: &Url) -> String {
    let mut safe = url.clone();
    if safe.query().is_some() {
        let pairs: Vec<(String, String)> = safe
            .query_pairs()
            .map(|(key, value)| {
                let value = if is_secret_key(&key) {
                    "[REDACTED]".to_owned()
                } else {
                    value.into_owned()
                };
                (key.into_owned(), value)
            })
            .collect();
        safe.query_pairs_mut().clear().extend_pairs(pairs);
    }
    safe.into()
}

fn redact_secrets(value: &str) -> String {
    let mut output = value.to_owned();
    for key in ["sesskey", "logintoken", "moodlesession", "token"] {
        output = redact_key_values(&output, key);
    }
    output
}

fn redact_key_values(value: &str, key: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(key) {
        let start = cursor + relative;
        result.push_str(&value[cursor..start + key.len()]);
        let bytes = value.as_bytes();
        let mut position = start + key.len();
        while position < bytes.len()
            && matches!(bytes[position], b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'\'')
        {
            result.push(bytes[position] as char);
            position += 1;
        }
        if position >= bytes.len() || !matches!(bytes[position], b':' | b'=') {
            cursor = start + key.len();
            continue;
        }
        result.push(bytes[position] as char);
        position += 1;
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            result.push(bytes[position] as char);
            position += 1;
        }
        let quote = bytes
            .get(position)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''));
        if let Some(quote) = quote {
            result.push(quote as char);
            position += 1;
        }
        result.push_str("[REDACTED]");
        while position < bytes.len() {
            let byte = bytes[position];
            if quote.is_some_and(|quote| byte == quote)
                || quote.is_none()
                    && (byte.is_ascii_whitespace() || matches!(byte, b'&' | b',' | b'}' | b']'))
            {
                break;
            }
            position += 1;
        }
        cursor = position;
    }
    result.push_str(&value[cursor..]);
    result
}

fn is_secret_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "sesskey" | "logintoken" | "moodlesession" | "token"
    )
}

fn course_activities(
    client: &KlmsClient,
    base_url: &Url,
    course: &Course,
    week: Option<u32>,
) -> Result<Vec<Activity>, AppError> {
    let response = client.get(&format!("/course/view.php?id={}", course.id))?;
    parse::activities(&response.text, base_url, week)
}

fn activity_result(
    command: &'static str,
    course: &Course,
    rows: Vec<Activity>,
) -> Result<CommandResult, AppError> {
    let mut human = format!("{} — {} items", course.title, rows.len());
    for row in &rows {
        human.push_str(&format!(
            "\n{}\t{}\t{}",
            row.id.as_deref().unwrap_or("-"),
            row.kind,
            row.title
        ));
    }
    output::result(command, &rows, human)
}

fn module_path(target: &str, kind: &str) -> Result<String, AppError> {
    if !target.is_empty() && target.chars().all(|c| c.is_ascii_digit()) {
        Ok(format!("/mod/{kind}/view.php?id={target}"))
    } else if target.starts_with('/')
        || target.starts_with("https://")
        || target.starts_with("http://")
    {
        Ok(target.into())
    } else {
        Err(AppError::usage(format!(
            "expected a numeric {kind} module id or same-origin KLMS URL"
        )))
    }
}

fn dashboard_courses(client: &KlmsClient, base_url: &Url) -> Result<Vec<Course>, AppError> {
    let response = client.get("/my/")?;
    cache_page_sesskey(base_url, &response.text);
    parse::courses(&response.text, base_url)
}

fn cache_page_sesskey(base_url: &Url, html: &str) {
    if let Ok(key) = parse::sesskey(html) {
        auth::cache_sesskey(base_url, &key);
    }
}

fn matching_courses(courses: Vec<Course>, query: &str) -> Vec<Course> {
    let needle = query.to_ascii_lowercase();
    let mut rows: Vec<_> = courses
        .into_iter()
        .filter(|course| {
            course.id == query
                || course.title.to_ascii_lowercase().contains(&needle)
                || course
                    .code
                    .as_ref()
                    .is_some_and(|code| code.to_ascii_lowercase().contains(&needle))
        })
        .collect();
    rows.sort_by_key(|course| {
        !(course.id == query
            || course.title.eq_ignore_ascii_case(query)
            || course
                .code
                .as_ref()
                .is_some_and(|code| code.eq_ignore_ascii_case(query)))
    });
    rows
}

fn resolve_course(client: &KlmsClient, base_url: &Url, query: &str) -> Result<Course, AppError> {
    if !query.is_empty() && query.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Course {
            id: query.into(),
            title: format!("Course {query}"),
            code: None,
            term: None,
            url: base_url
                .join(&format!("/course/view.php?id={query}"))
                .expect("valid path")
                .into(),
        });
    }
    let matches = matching_courses(dashboard_courses(client, base_url)?, query);
    let exact: Vec<_> = matches
        .iter()
        .filter(|course| {
            course.title.eq_ignore_ascii_case(query)
                || course
                    .code
                    .as_ref()
                    .is_some_and(|code| code.eq_ignore_ascii_case(query))
        })
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    match matches.as_slice() {
        [course] => Ok(course.clone()),
        [] => Err(AppError::not_found(format!(
            "no dashboard course matches {query:?}"
        ))),
        _ => Err(AppError::usage(format!(
            "course query {query:?} is ambiguous; run `klms courses resolve {query}`"
        ))),
    }
}

fn render_courses(courses: &[Course]) -> String {
    if courses.is_empty() {
        return "No courses found.".into();
    }
    courses
        .iter()
        .map(|course| {
            format!(
                "{}\t{}\t{}",
                course.id,
                course.code.as_deref().unwrap_or("-"),
                course.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_links(rows: &[crate::models::LinkItem]) -> String {
    if rows.is_empty() {
        "No events found.".into()
    } else {
        rows.iter()
            .map(|row| format!("{}\t{}", row.title, row.url))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn report_result(
    command: &'static str,
    title: &str,
    report: Report,
) -> Result<CommandResult, AppError> {
    let mut human = format!("{} — {} rows", title, report.rows.len());
    if !report.headers.is_empty() {
        human.push_str(&format!("\n{}", report.headers.join("\t")));
    }
    for row in &report.rows {
        human.push_str(&format!("\n{}", row.join("\t")));
    }
    output::result(command, &report, human)
}

fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

fn duration(seconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn command_name(resource: &str, verb: &str) -> &'static str {
    match (resource, verb) {
        ("assignments", "list") => "assignments.list",
        ("assignments", "show") => "assignments.show",
        ("quizzes", "list") => "quizzes.list",
        ("quizzes", "show") => "quizzes.show",
        ("videos", "list") => "videos.list",
        ("videos", "show") => "videos.show",
        _ => unreachable!("built-in resource and verb"),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::{redact_secrets, redact_url};
    use url::Url;

    #[test]
    fn raw_preview_redacts_common_secret_assignments() {
        let source = r#"{"sesskey":"abc123","name":"safe"}&token=xyz789"#;
        let redacted = redact_secrets(source);
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("xyz789"));
        assert!(redacted.contains(r#""name":"safe""#));
        assert_eq!(redacted.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn raw_preview_redacts_secrets_from_reported_url() {
        let url =
            Url::parse("https://klms.kaist.ac.kr/lib/ajax/service.php?sesskey=abc123&info=visible")
                .unwrap();
        let redacted = redact_url(&url);
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("info=visible"));
    }
}
