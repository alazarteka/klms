use serde::Serialize;
use url::Url;

use crate::{
    auth,
    cli::{
        ActivitiesCommand, AttendanceCommand, AuthCommand, Cli, Command, CoursesCommand,
        GradesCommand,
    },
    client::{KlmsClient, validate_base_url},
    error::AppError,
    models::{Course, Report},
    output::{self, CommandResult},
    parse,
};

pub fn run(cli: &Cli) -> Result<CommandResult, AppError> {
    let base_url = validate_base_url(&cli.base_url)?;
    let session = auth::load(&base_url)?;
    match &cli.command {
        Command::Auth(args) => match args.command {
            AuthCommand::Status => auth_status(&session.status),
        },
        Command::Doctor => doctor(&base_url, session),
        command => {
            let cookie = session.cookie_header.as_deref().ok_or_else(|| {
                AppError::auth(
                    "no usable KLMS session was found",
                    "Set KLMS_STORAGE_STATE or sign in once with kaist-cli to create the legacy session file.",
                )
            })?;
            let client = KlmsClient::new(base_url.as_str(), Some(cookie))?;
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
    live_session: bool,
    dashboard_url: Option<String>,
}

fn doctor(base_url: &Url, session: auth::AuthSession) -> Result<CommandResult, AppError> {
    let mut live_session = false;
    let mut dashboard_url = None;
    if let Some(cookie) = session.cookie_header.as_deref() {
        if let Ok(response) =
            KlmsClient::new(base_url.as_str(), Some(cookie)).and_then(|client| client.get("/my/"))
        {
            live_session = true;
            dashboard_url = Some(response.url.into());
        }
    }
    let model = Doctor {
        version: env!("CARGO_PKG_VERSION"),
        base_url: base_url.to_string(),
        auth: session.status,
        live_session,
        dashboard_url,
    };
    let human = format!(
        "klms {}\nOrigin: {}\nStorage state: {}\nLive session: {}",
        model.version,
        model.base_url,
        if model.auth.configured {
            &model.auth.source
        } else {
            "missing"
        },
        yes_no(model.live_session)
    );
    output::result("doctor", &model, human)
}

fn live(command: &Command, client: &KlmsClient, base_url: &Url) -> Result<CommandResult, AppError> {
    match command {
        Command::Dashboard => {
            let response = client.get("/my/")?;
            let model = parse::dashboard(&response.text, base_url)?;
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
            CoursesCommand::List => {
                let response = client.get("/my/")?;
                let courses = parse::courses(&response.text, base_url)?;
                let human = render_courses(&courses);
                output::result("courses.list", &courses, human)
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
            ActivitiesCommand::List { course, week } => {
                let resolved = resolve_course(client, base_url, course)?;
                let response = client.get(&format!("/course/view.php?id={}", resolved.id))?;
                let rows = parse::activities(&response.text, base_url, *week)?;
                let mut human = format!("{} — {} activities", resolved.title, rows.len());
                for row in &rows {
                    human.push_str(&format!(
                        "\n{}\t{}\t{}",
                        row.week
                            .map(|v| format!("week {v}"))
                            .unwrap_or_else(|| "-".into()),
                        row.kind,
                        row.title
                    ));
                }
                output::result("activities.list", &rows, human)
            }
        },
        Command::Grades(args) => match &args.command {
            GradesCommand::Show { course } => {
                let resolved = resolve_course(client, base_url, course)?;
                let response =
                    client.get(&format!("/grade/report/user/index.php?id={}", resolved.id))?;
                let report = parse::grades(&response.text, resolved.id)?;
                report_result("grades.show", &resolved.title, report)
            }
        },
        Command::Attendance(args) => match &args.command {
            AttendanceCommand::Show { course } => {
                let resolved = resolve_course(client, base_url, course)?;
                let response = client.get(&format!(
                    "/local/lmsattendance/index.php?id={}",
                    resolved.id
                ))?;
                let report = parse::attendance(&response.text, resolved.id)?;
                report_result("attendance.show", &resolved.title, report)
            }
        },
        Command::Doctor | Command::Auth(_) => unreachable!("handled before live dispatch"),
    }
}

fn resolve_course(client: &KlmsClient, base_url: &Url, query: &str) -> Result<Course, AppError> {
    if !query.is_empty() && query.chars().all(|character| character.is_ascii_digit()) {
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
    let response = client.get("/my/")?;
    let courses = parse::courses(&response.text, base_url)?;
    let needle = query.to_ascii_lowercase();
    let exact: Vec<_> = courses
        .iter()
        .filter(|course| {
            course
                .code
                .as_ref()
                .is_some_and(|code| code.eq_ignore_ascii_case(query))
                || course.title.eq_ignore_ascii_case(query)
        })
        .cloned()
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    let partial: Vec<_> = courses
        .into_iter()
        .filter(|course| {
            course.title.to_ascii_lowercase().contains(&needle)
                || course
                    .code
                    .as_ref()
                    .is_some_and(|code| code.to_ascii_lowercase().contains(&needle))
        })
        .collect();
    match partial.as_slice() {
        [course] => Ok(course.clone()),
        [] => Err(AppError::not_found(format!(
            "no dashboard course matches {query:?}"
        ))),
        _ => Err(AppError::usage(format!(
            "course query {query:?} is ambiguous; use a numeric id or exact code"
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

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
