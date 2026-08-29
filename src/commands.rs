mod coursework;
mod download;
mod raw;

use serde::Serialize;
use url::Url;

use crate::{
    auth,
    cli::{
        ActivitiesCommand, AuthCommand, Cli, Command, CourseShowCommand, CoursesCommand,
        RequestCommand,
    },
    client::{KlmsClient, validate_base_url},
    error::AppError,
    models::{Activity, Course, Report, SessionTime},
    output::{self, CommandResult},
    parse,
    reference::ResourceRef,
};

pub fn run(cli: &Cli) -> Result<CommandResult, AppError> {
    let base_url = validate_base_url(&cli.base_url)?;
    let session = auth::load(&base_url)?;
    match &cli.command {
        Command::Auth(args) if matches!(args.command, AuthCommand::Status) => {
            auth_status(&session.status)
        }
        Command::Doctor => doctor(&base_url, session, cli.timeout),
        command => {
            let cookie = session
                .cookie_header
                .as_deref()
                .ok_or_else(|| AppError::auth_required("no usable KLMS session was found"))?;
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
    session_error: Option<AppError>,
    dashboard_url: Option<String>,
    check_may_have_extended_session: bool,
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
    let mut failure = None;
    if let Some(cookie) = session.cookie_header.as_deref() {
        check_may_have_extended_session = true;
        match KlmsClient::new(base_url.as_str(), Some(cookie), timeout)
            .and_then(|client| client.get("/my/"))
        {
            Ok(response) => {
                cache_page_sesskey(base_url, &response.text);
                session_status = "valid";
                dashboard_url = Some(crate::safe_url::display(&response.url));
            }
            Err(error) => {
                session_status = match error.code {
                    "AUTH_REQUIRED" => "expired",
                    "NETWORK_ERROR" => "unreachable",
                    _ => "error",
                };
                session_error = Some(error.clone());
                failure = Some(error);
            }
        }
    } else {
        let error = AppError::auth_required("no usable KLMS session was found");
        session_error = Some(error.clone());
        failure = Some(error);
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
    if let Some(error) = failure {
        let details = serde_json::to_value(&model).map_err(|encode_error| {
            AppError::internal(format!(
                "failed to encode doctor diagnostics: {encode_error}"
            ))
        })?;
        return Err(error.with_details(details));
    }
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
            model.courses_complete = model.courses.len() == model.course_count;
            model.upcoming_complete = model.upcoming.len() == model.upcoming_count;
            let mut human = format!(
                "{} — {} courses, {} upcoming",
                model.term.as_deref().unwrap_or("Current dashboard"),
                model.course_count,
                model.upcoming_count,
            );
            human.push_str("\n\nCourses:\nREF\tCODE\tTITLE");
            for course in &model.courses {
                human.push_str(&format!(
                    "\n{}\t{}\t{}",
                    course.reference,
                    course.code.as_deref().unwrap_or("-"),
                    course.title
                ));
            }
            if !model.courses_complete {
                human.push_str(&format!(
                    "\n[Showing {} of {} courses]",
                    model.courses.len(),
                    model.course_count
                ));
            }
            human.push_str("\n\nUpcoming:");
            if model.upcoming.is_empty() {
                human.push_str("\nNone shown on the dashboard.");
            } else {
                for item in &model.upcoming {
                    human.push_str(&format!("\n{}\t{}", item.title, item.url));
                }
                if !model.upcoming_complete {
                    human.push_str(&format!(
                        "\n[Showing {} of {} upcoming items]",
                        model.upcoming.len(),
                        model.upcoming_count
                    ));
                }
            }
            output::result("dashboard", &model, human)
        }
        Command::Today(args) => coursework::agenda(client, base_url, args, 0, "today"),
        Command::Upcoming(args) => coursework::upcoming(client, base_url, args),
        Command::Courses(args) => match &args.command {
            CoursesCommand::List(list) => {
                let mut courses = dashboard_courses(client, base_url)?;
                let available = courses.len();
                courses.truncate(list.limit);
                output::collection(
                    "courses.list",
                    &courses,
                    render_courses(&courses, available),
                    courses.len(),
                    list.limit,
                    available,
                    true,
                )
            }
            CoursesCommand::Resolve { query, list } => {
                let mut matches = matching_courses(dashboard_courses(client, base_url)?, query);
                let available = matches.len();
                matches.truncate(list.limit);
                output::collection(
                    "courses.resolve",
                    &matches,
                    render_courses(&matches, available),
                    matches.len(),
                    list.limit,
                    available,
                    true,
                )
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
                let available = rows.len();
                rows.truncate(list.limit);
                activity_result("activities.list", &resolved, rows, list.limit, available)
            }
        },
        Command::Assignments(args) => coursework::assignments(client, base_url, &args.command),
        Command::Quizzes(args) => coursework::quizzes(client, base_url, &args.command),
        Command::Videos(args) => coursework::videos(client, base_url, &args.command),
        Command::Calendar(args) => coursework::calendar(client, base_url, &args.command),
        Command::Boards(args) => coursework::boards(client, base_url, &args.command),
        Command::Notices(args) => coursework::notices(client, base_url, &args.command),
        Command::Files(args) => coursework::files(client, base_url, &args.command),
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
            RequestCommand::Get { path, max_bytes } => raw::get(client, path, *max_bytes),
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
    limit: usize,
    available: usize,
) -> Result<CommandResult, AppError> {
    let mut human = format!(
        "{} — showing {} of {} items\nREF\tTYPE\tWEEK\tTITLE",
        course.title,
        rows.len(),
        available
    );
    for row in &rows {
        human.push_str(&format!(
            "\n{}\t{}\t{}\t{}",
            row.reference.as_deref().unwrap_or("-"),
            row.kind,
            row.week
                .map(|week| week.to_string())
                .as_deref()
                .unwrap_or("-"),
            row.title
        ));
    }
    output::collection(command, &rows, human, rows.len(), limit, available, true)
}

fn module_path(target: &str, kinds: &[&str]) -> Result<String, AppError> {
    if target.contains(':') && !target.starts_with("https://") && !target.starts_with("http://") {
        let reference = ResourceRef::parse(target)?;
        if !reference.matches_module(kinds) {
            return Err(AppError::usage(format!(
                "resource reference {target:?} does not identify one of: {}",
                kinds.join(", ")
            )));
        }
        return Ok(reference.path());
    }
    if !target.is_empty() && target.chars().all(|c| c.is_ascii_digit()) {
        if kinds.len() == 1 {
            Ok(format!("/mod/{}/view.php?id={target}", kinds[0]))
        } else {
            Err(AppError::usage(format!(
                "numeric id {target} is ambiguous; use the typed reference returned by the list command"
            )))
        }
    } else if target.starts_with('/')
        || target.starts_with("https://")
        || target.starts_with("http://")
    {
        Ok(target.into())
    } else {
        Err(AppError::usage(
            "expected a canonical resource reference, numeric module id, or same-origin KLMS URL",
        ))
    }
}

fn dashboard_courses(client: &KlmsClient, base_url: &Url) -> Result<Vec<Course>, AppError> {
    let response = client.get("/my/")?;
    cache_page_sesskey(base_url, &response.text);
    Ok(parse::dashboard(&response.text, base_url)?.courses)
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
    if query.starts_with("course:") {
        let ResourceRef::Course(id) = ResourceRef::parse(query)? else {
            unreachable!("course prefix parses only as a course")
        };
        return resolve_course(client, base_url, &id);
    }
    if !query.is_empty() && query.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Course {
            id: query.into(),
            reference: format!("course:{query}"),
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
        _ => {
            let candidates = matches
                .iter()
                .take(5)
                .map(|course| {
                    format!(
                        "{} ({})",
                        course.code.as_deref().unwrap_or(&course.reference),
                        course.title
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(AppError::usage(format!(
                "course query {query:?} is ambiguous: {candidates}; use an exact code or course reference"
            )))
        }
    }
}

fn render_courses(courses: &[Course], available: usize) -> String {
    if courses.is_empty() {
        return "No courses found.".into();
    }
    let mut output = format!(
        "Courses — showing {} of {available}\nREF\tCODE\tTITLE",
        courses.len()
    );
    for course in courses {
        output.push_str(&format!(
            "\n{}\t{}\t{}",
            course.reference,
            course.code.as_deref().unwrap_or("-"),
            course.title
        ));
    }
    output
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

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
