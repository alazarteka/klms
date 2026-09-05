mod coursework;
mod download;
mod raw;

use serde::Serialize;
use url::Url;

use crate::{
    auth,
    cli::{
        ActivitiesCommand, AuthCommand, AuthMethodArg, AuthSecondFactorArg, Cli, Command,
        CourseShowCommand, CoursesCommand, LibraryCommand, LibraryDownloadArg, LibraryFieldArg,
        LibraryRelationsCommand, RequestCommand, SkillCommand,
    },
    client::{KlmsClient, validate_base_url},
    error::AppError,
    models::{Activity, Course, Report, SessionTime},
    output::{self, CommandResult},
    parse,
    reference::ResourceRef,
};

pub fn run(cli: &Cli) -> Result<CommandResult, AppError> {
    match &cli.command {
        Command::Update(args) => return crate::update::run(args.check, cli.timeout),
        Command::Install { destination } => return crate::update::install(destination),
        _ => {}
    }
    if let Command::Skill(args) = &cli.command {
        return match args.command {
            SkillCommand::Install => crate::skill::install(),
            SkillCommand::Status => crate::skill::status(),
        };
    }
    if let Command::Library(args) = &cli.command {
        if !matches!(args.command, LibraryCommand::Sync(_)) {
            return library_local(&args.command);
        }
    }
    if let Command::Spec = &cli.command {
        return crate::spec::run();
    }
    if let Command::Completions { shell } = &cli.command {
        return crate::spec::completions(*shell);
    }
    let base_url = validate_base_url(&cli.base_url)?;
    match &cli.command {
        Command::Auth(args) if matches!(args.command, AuthCommand::Login(_)) => {
            let AuthCommand::Login(login) = &args.command else {
                unreachable!()
            };
            let method = match login.method.unwrap_or(AuthMethodArg::Easy) {
                AuthMethodArg::Easy => auth::LoginMethod::Easy,
                AuthMethodArg::Password => auth::LoginMethod::Password,
            };
            if login.second_factor.is_some() && method != auth::LoginMethod::Password {
                return Err(AppError::usage(
                    "--second-factor applies only to password login",
                ));
            }
            let factor = login.second_factor.map(|factor| match factor {
                AuthSecondFactorArg::Email => auth::SecondFactor::Email,
                AuthSecondFactorArg::Sms => auth::SecondFactor::Sms,
            });
            let sso_url = if base_url.host_str() == Some("klms.kaist.ac.kr") {
                Url::parse("https://sso.kaist.ac.kr/").expect("valid built-in SSO URL")
            } else {
                base_url.clone()
            };
            return auth::login(&base_url, &sso_url, cli.timeout, method, factor);
        }
        Command::Auth(args) if matches!(args.command, AuthCommand::Logout) => {
            return auth::logout();
        }
        _ => {}
    }
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

fn library_local(command: &LibraryCommand) -> Result<CommandResult, AppError> {
    let mut corpus = crate::corpus::Corpus::open()?;
    match command {
        LibraryCommand::Status => library_status(&corpus),
        LibraryCommand::Search { query, list } => {
            let (fresh_through, source_complete) = corpus.coverage()?;
            let mut rows = corpus.search(query, list.limit.saturating_add(1))?;
            let truncated = rows.len() > list.limit;
            rows.truncate(list.limit);
            let human = rows
                .iter()
                .map(|row| format!("{}\t{}\t{}", row.reference, row.kind, row.title))
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.search",
                &rows,
                human,
                rows.len(),
                list.limit,
                !truncated,
                fresh_through,
                source_complete,
            )
        }
        LibraryCommand::Changes(list) => {
            let (fresh_through, source_complete) = corpus.coverage()?;
            let mut rows = corpus.changes(list.limit.saturating_add(1))?;
            let truncated = rows.len() > list.limit;
            rows.truncate(list.limit);
            let human = rows
                .iter()
                .map(|r| format!("{}\t{}\t{}", r.occurred_at, r.kind, r.subject_ref))
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.changes",
                &rows,
                human,
                rows.len(),
                list.limit,
                !truncated,
                fresh_through,
                source_complete,
            )
        }
        LibraryCommand::Activity(args) => {
            let mut rows =
                corpus.activity(args.subject.as_deref(), args.list.limit.saturating_add(1))?;
            let truncated = rows.len() > args.list.limit;
            rows.truncate(args.list.limit);
            let human = rows
                .iter()
                .map(|row| {
                    format!(
                        "{}\t{}\t{}\t{}",
                        row.created_at, row.actor, row.field, row.subject_ref
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.activity",
                &rows,
                human,
                rows.len(),
                args.list.limit,
                !truncated,
                None,
                None,
            )
        }
        LibraryCommand::Show { reference } => {
            let row = corpus.show(reference)?;
            let human = serde_json::to_string_pretty(&row)
                .map_err(|error| AppError::internal(error.to_string()))?;
            output::result("library.show", &row, human)
        }
        LibraryCommand::History { reference, list } => {
            let mut rows = corpus.history(reference, list.limit.saturating_add(1))?;
            let truncated = rows.len() > list.limit;
            rows.truncate(list.limit);
            let human = rows
                .iter()
                .map(|row| format!("{}\t{}\t{}", row.id, row.observed_at, row.digest))
                .collect::<Vec<_>>()
                .join("\n");
            output::local_collection(
                "library.history",
                &rows,
                human,
                rows.len(),
                list.limit,
                !truncated,
                None,
                None,
            )
        }
        LibraryCommand::Content {
            reference,
            max_bytes,
        } => library_content(&corpus, reference, *max_bytes),
        LibraryCommand::Export { reference, out } => library_export(&corpus, reference, out),
        LibraryCommand::Edit(args) => {
            let value = read_library_text(args.value.as_deref(), args.value_file.as_deref())?;
            let field = match args.field {
                LibraryFieldArg::Title => "title",
                LibraryFieldArg::Filename => "filename",
                LibraryFieldArg::Summary => "summary",
                LibraryFieldArg::Note => "note",
                LibraryFieldArg::Tag => "tag",
            };
            let row = corpus.edit(
                &args.reference,
                field,
                &value,
                &args.actor,
                args.expected_revision,
            )?;
            output::result(
                "library.edit",
                &row,
                format!(
                    "{} revision {}: {:?} -> {:?}",
                    row.reference, row.revision, row.before, row.after
                ),
            )
        }
        LibraryCommand::Retract(args) => {
            let row = corpus.retract(&args.reference, &args.actor)?;
            output::result(
                "library.retract",
                &row,
                format!("Retracted {}", row.target_ref),
            )
        }
        LibraryCommand::Relations(args) => match &args.command {
            LibraryRelationsCommand::Add {
                left,
                right,
                kind,
                actor,
            } => {
                let row = corpus.add_relation(left, right, kind, actor)?;
                let reference = row.reference.clone();
                output::result(
                    "library.relations.add",
                    &row,
                    format!("Recorded {reference}"),
                )
            }
        },
        LibraryCommand::Sync(_) => Err(AppError::internal(
            "sync was routed through the local library dispatcher",
        )),
    }
}

fn library_status(corpus: &crate::corpus::Corpus) -> Result<CommandResult, AppError> {
    let model = corpus.status()?;
    let mut human = format!(
        "Library storage: {}\nDatabase: {}\nObjects: {}\nSchema: {}\nCourses: {}\nResources: {}\nRepresentations: {}\nStored content: {} bytes",
        if model.created {
            "initialized"
        } else {
            "ready"
        },
        model.database_path,
        model.object_store_path,
        model.schema_version,
        model.courses,
        model.resources,
        model.representations,
        model.stored_bytes,
    );
    if let Some(sync) = &model.last_sync {
        human.push_str(&format!(
            "\nLast sync attempt: {} — {}\nScope: {}\nStarted: {}\nFinished: {}",
            sync.reference,
            sync.status,
            sync.scope,
            crate::date::epoch_to_seoul(sync.started_at).unwrap_or_else(|| "unknown".into()),
            sync.finished_at
                .and_then(crate::date::epoch_to_seoul)
                .unwrap_or_else(|| "not recorded".into()),
        ));
        if sync.scope != "all" {
            human.push_str("\nCourse-scoped syncs do not establish global coverage.");
        }
    } else {
        human.push_str("\nLast sync attempt: none");
    }
    human.push_str(&format!(
        "\nLast complete global sync: {}",
        model
            .fresh_through
            .and_then(crate::date::epoch_to_seoul)
            .unwrap_or_else(|| "none".into()),
    ));
    let mut result = output::result("library.status", &model, human)?;
    if model
        .last_sync
        .as_ref()
        .is_some_and(|sync| sync.status == "unfinished")
    {
        result.warnings.push(
            "This attempt did not record completion; it may still be active or may have been interrupted. Check the original process and retry the same command once it has stopped.".into(),
        );
    }
    Ok(result)
}

fn read_library_text(
    value: Option<&str>,
    path: Option<&std::path::Path>,
) -> Result<String, AppError> {
    use std::io::Read;
    let mut text = if let Some(value) = value {
        value.to_owned()
    } else if let Some(path) = path {
        if path == std::path::Path::new("-") {
            let mut value = String::new();
            std::io::stdin()
                .take(1_048_577)
                .read_to_string(&mut value)
                .map_err(|e| AppError::config(format!("cannot read stdin: {e}")))?;
            value
        } else {
            std::fs::read_to_string(path)
                .map_err(|e| AppError::config(format!("cannot read {}: {e}", path.display())))?
        }
    } else {
        unreachable!("clap requires exactly one of --value and --value-file");
    };
    if text.len() > 1_048_576 {
        return Err(AppError::limit("curation text exceeds 1 MiB"));
    }
    while text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

fn library_content(
    corpus: &crate::corpus::Corpus,
    reference: &str,
    max: usize,
) -> Result<CommandResult, AppError> {
    let model = corpus.preview(reference, max)?;
    let human = model
        .text
        .clone()
        .unwrap_or_else(|| "Binary content is available through `library export`.".into());
    output::result("library.content", &model, human)
}
fn library_export(
    corpus: &crate::corpus::Corpus,
    reference: &str,
    out: &std::path::Path,
) -> Result<CommandResult, AppError> {
    let bytes = corpus.export(reference, out)?;
    let model = serde_json::json!({"ref":reference,"path":out,"byte_length":bytes});
    output::result(
        "library.export",
        &model,
        format!("Exported {} bytes to {}", bytes, out.display()),
    )
}

fn auth_status(status: &auth::AuthStatus) -> Result<CommandResult, AppError> {
    let human = if status.configured {
        format!(
            "Owned session: {}\nSource: {}\nCookies: {}\nTrusted devices: {}",
            status.path, status.source, status.cookie_count, status.device_count,
        )
    } else {
        format!(
            "Owned session: not configured\nRun `klms auth login`.\nExpected path: {}",
            status.path
        )
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
        "klms {}\nOrigin: {}\nOwned session: {}\nSession: {}",
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
        Command::Skill(_)
        | Command::Spec
        | Command::Completions { .. }
        | Command::Update(_)
        | Command::Install { .. } => {
            unreachable!("handled before authenticated dispatch")
        }
        Command::Auth(args) => match args.command {
            AuthCommand::Login(_) | AuthCommand::Logout => {
                unreachable!("handled before live dispatch")
            }
            AuthCommand::Status => unreachable!("handled before live dispatch"),
            AuthCommand::TimeLeft => session_time(client, base_url, false),
            AuthCommand::Extend => session_time(client, base_url, true),
        },
        Command::Dashboard(args) => {
            let response = client.get("/my/")?;
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
        Command::Library(args) => match &args.command {
            LibraryCommand::Sync(args) => {
                let mut corpus = crate::corpus::Corpus::open()?;
                let model = corpus.sync(
                    client,
                    base_url,
                    args.course.as_deref(),
                    crate::corpus::SyncOptions {
                        notices: args.notices,
                        files: args.files || args.download.is_some(),
                        download_changed: matches!(
                            args.download,
                            Some(LibraryDownloadArg::Changed)
                        ),
                    },
                )?;
                let human = format!(
                    "{} — {}: {} courses, {} resources, {} representations, {} blobs, {} changes, {} truncated, {} failures",
                    model.reference,
                    model.status,
                    model.courses,
                    model.resources,
                    model.representations,
                    model.blobs_added,
                    model.changes,
                    model.truncated,
                    model.failures.len()
                );
                let mut result = output::result("library.sync", &model, human)?;
                result.warnings.extend(model.failures);
                Ok(result)
            }
            command => library_local(command),
        },
    }
}

fn session_time(
    client: &KlmsClient,
    _base_url: &Url,
    extend: bool,
) -> Result<CommandResult, AppError> {
    let dashboard = client.get("/my/")?;
    let key = parse::sesskey(&dashboard.text)?;
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
    Ok(parse::dashboard(&response.text, base_url)?.courses)
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
