mod library;

use std::path::Path;

use serde::Serialize;

use crate::{
    auth,
    cli::{
        ActivitiesCommand, AuthCommand, BoardsCommand, CalendarCommand, Cli, Command,
        CourseShowCommand, CoursesCommand, FilesCommand, LibraryCommand, ModuleCommand,
        NoticesCommand, RequestCommand,
    },
    client::{KlmsClient, validate_base_url},
    course_pages, date,
    error::AppError,
    models::{
        Activity, CalendarEvent, Course, DownloadResult, FileResource, Notice, RawGet, Report,
        SessionTime,
    },
    output::{self, CommandResult, body, cell, detail, listing, row, table},
    parse,
    private_fs::{PublishError, publish_new},
    reference::{ResourceRef, valid_id},
    safe_url,
    url::Url,
};

const MAX_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;
const VIDEO_KINDS: &[&str] = &["vod", "panoptocourseembed", "panopto", "lti"];
const CALENDAR_PATH: &str = "/calendar/view.php?view=upcoming";

/// An authenticated client and the origin it talks to.
struct Ctx<'a> {
    client: &'a KlmsClient,
    base: &'a Url,
}

pub fn run(cli: &Cli) -> Result<CommandResult, AppError> {
    match &cli.command {
        Command::Update(args) => return crate::update::run(args.check, cli.timeout),
        Command::Install { destination } => return crate::update::install(destination),
        Command::Library { command } if !matches!(command, LibraryCommand::Sync(_)) => {
            return library::local(command);
        }
        Command::Spec => return crate::spec::run(),
        Command::Completions { shell } => return crate::spec::completions(*shell),
        _ => {}
    }
    let base = validate_base_url(&cli.base_url)?;
    if let Command::Auth { command } = &cli.command {
        match command {
            AuthCommand::Login(login) => {
                let options = auth::LoginOptions {
                    user: login.user.clone(),
                    method: login.method,
                    factor: login.second_factor,
                    remember_password: login.remember_password,
                    insecure_storage: login.insecure_storage,
                    code: login.code.clone(),
                };
                let sso = match base.host_str() == Some("klms.kaist.ac.kr") {
                    true => Url::parse("https://sso.kaist.ac.kr/").expect("valid SSO URL"),
                    false => base.clone(),
                };
                return auth::login(&base, &sso, cli.timeout, &options, cli.json);
            }
            AuthCommand::Logout => return auth::logout(),
            AuthCommand::Forget => return auth::forget(),
            _ => {}
        }
    }
    let session = auth::load(&base)?;
    match &cli.command {
        Command::Auth {
            command: AuthCommand::Status,
        } => auth_status(&session.status),
        Command::Doctor => doctor(&base, session, cli.timeout),
        command => {
            let cookie = (session.cookie_header.as_deref())
                .ok_or_else(|| AppError::auth_required("no usable KLMS session was found"))?;
            let client = KlmsClient::new(base.as_str(), Some(cookie), cli.timeout)?;
            let ctx = Ctx {
                client: &client,
                base: &base,
            };
            ctx.live(command)
        }
    }
}

fn auth_status(status: &auth::AuthStatus) -> Result<CommandResult, AppError> {
    let mut human = match status.configured {
        true => format!(
            "Owned session: {}\nSource: {}\nCookies: {}\nTrusted devices: {}",
            status.path, status.source, status.cookie_count, status.device_count,
        ),
        false => format!(
            "Owned session: not configured\nRun `klms auth login`.\nExpected path: {}",
            status.path
        ),
    };
    human.push_str(&match (&status.remembered, &status.remembered_error) {
        (Some(login), _) => {
            let factor = (login.second_factor.as_deref())
                .map(|factor| format!(", {factor} code"))
                .unwrap_or_default();
            format!(
                "\nRemembered login: {} ({}{factor})\nStored password: {}",
                login.username, login.method, login.password_backend
            )
        }
        (None, Some(error)) => format!("\nRemembered login: unreadable ({error})"),
        (None, None) => "\nRemembered login: none".into(),
    });
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

fn doctor(base: &Url, session: auth::AuthSession, timeout: u64) -> Result<CommandResult, AppError> {
    let checked = session.cookie_header.is_some();
    let probe = match session.cookie_header.as_deref() {
        Some(cookie) => KlmsClient::new(base.as_str(), Some(cookie), timeout)
            .and_then(|client| client.get("/my/")),
        None => Err(AppError::auth_required("no usable KLMS session was found")),
    };
    let (session_status, session_error, dashboard_url) = match probe {
        Ok(response) => ("valid", None, Some(safe_url::display(&response.url))),
        Err(error) => {
            let status = match error.code {
                _ if !checked => "not_configured",
                "AUTH_REQUIRED" => "expired",
                "NETWORK_ERROR" => "unreachable",
                _ => "error",
            };
            (status, Some(error), None)
        }
    };
    let model = Doctor {
        version: env!("CARGO_PKG_VERSION"),
        base_url: base.to_string(),
        auth: session.status,
        session_status,
        session_error,
        dashboard_url,
        check_may_have_extended_session: checked,
    };
    if let Some(error) = model.session_error.clone() {
        let details = serde_json::to_value(&model).map_err(|error| {
            AppError::internal(format!("failed to encode doctor diagnostics: {error}"))
        })?;
        return Err(error.with_details(details));
    }
    let owned = if model.auth.configured {
        model.auth.source
    } else {
        "missing"
    };
    let human = format!(
        "klms {}\nOrigin: {}\nOwned session: {owned}\nSession: {}",
        model.version, model.base_url, model.session_status
    );
    let mut result = output::result("doctor", &model, human)?;
    result.warnings.push(
        "The dashboard request used to validate the session may refresh KLMS activity time.".into(),
    );
    Ok(result)
}

impl Ctx<'_> {
    fn live(&self, command: &Command) -> Result<CommandResult, AppError> {
        let (client, base) = (self.client, self.base);
        match command {
            Command::Auth { command } => self.session_time(matches!(command, AuthCommand::Extend)),
            Command::Dashboard(args) => self.dashboard(args.limit),
            Command::Today(a) => self.agenda(a.course.as_deref(), a.list.limit, 0, "today"),
            Command::Upcoming(a) => {
                self.agenda(a.course.as_deref(), a.list.limit, a.through, "upcoming")
            }
            Command::Courses { command } => match command {
                CoursesCommand::List(list) => listing(
                    "courses.list",
                    self.courses()?,
                    list.limit,
                    true,
                    courses_table,
                ),
                CoursesCommand::Resolve { query, list } => {
                    let matches = matching_courses(self.courses()?, query);
                    listing("courses.resolve", matches, list.limit, true, courses_table)
                }
                CoursesCommand::Show { course } => {
                    let resolved = self.resolve_course(course)?;
                    let response = client.get(&format!("/course/view.php?id={}", resolved.id))?;
                    let model = parse::course_detail(&response.text, base, resolved);
                    let professors = match model.professors.is_empty() {
                        true => "unknown".into(),
                        false => model.professors.join(", "),
                    };
                    let human = format!(
                        "{}\nID: {}\nCode: {}\nProfessors: {professors}\nActivities: {}\n{}",
                        model.course.title,
                        model.course.id,
                        cell(&model.course.code, "unknown"),
                        model.activity_count,
                        model.course.url
                    );
                    output::result("courses.show", &model, human)
                }
            },
            Command::Activities { command } => {
                let ActivitiesCommand::List {
                    course,
                    week,
                    kind,
                    list,
                } = command;
                let keep = |row: &Activity| {
                    kind.as_ref()
                        .is_none_or(|kind| row.kind.eq_ignore_ascii_case(kind))
                };
                self.activity_list(course, *week, list.limit, "activities.list", keep)
            }
            Command::Assignments(args) => self.module(
                &args.command,
                "assignments.show",
                &["assign"],
                |course, limit| {
                    let response =
                        client.get(&format!("/mod/assign/index.php?id={}", course.id))?;
                    let rows = parse::assignments(&response.text, &response.url, course)?;
                    listing("assignments.list", rows, limit, true, |rows, available| {
                        let header = "REF\tDUE\tSTATUS\tTITLE";
                        table(rows, available, "Assignments", header, |r| {
                            let due = cell(&r.due_at, "unknown");
                            let status = cell(&r.submission_status, "unknown");
                            row(&[&r.reference, due, status, &r.title])
                        })
                    })
                },
            ),
            Command::Quizzes(args) => {
                self.module(&args.command, "quizzes.show", &["quiz"], |course, limit| {
                    let response = client.get(&format!("/mod/quiz/index.php?id={}", course.id))?;
                    let rows = parse::quizzes(&response.text, &response.url, course)?;
                    listing("quizzes.list", rows, limit, true, |rows, available| {
                        let header = "REF\tCLOSES\tGRADE\tTITLE";
                        table(rows, available, "Quizzes", header, |r| {
                            let closes = cell(&r.closes_at, "unknown");
                            row(&[&r.reference, closes, cell(&r.grade, "-"), &r.title])
                        })
                    })
                })
            }
            Command::Videos(args) => self.module(
                &args.command,
                "videos.show",
                VIDEO_KINDS,
                |course, limit| {
                    let mut rows = course_pages::activities(client, base, &course.id)?;
                    rows.retain(parse::is_video_activity);
                    activity_table("videos.list", course, rows, limit)
                },
            ),
            Command::Calendar { command } => {
                let CalendarCommand::List(list) = command;
                let page = parse::calendar_page(
                    &client.get(CALENDAR_PATH)?.text,
                    base,
                    &date::seoul_today(),
                )?;
                let header = "WHEN\tREF\tCOURSE\tTITLE";
                let events = page.events;
                listing(
                    "calendar.list",
                    events,
                    list.limit,
                    page.complete,
                    |rows, avail| match rows.is_empty() {
                        true => "No upcoming calendar events found.".into(),
                        false => table(rows, avail, "Calendar", header, event_row),
                    },
                )
            }
            Command::Boards { command } => match command {
                BoardsCommand::List { course, list } => {
                    let keep = |row: &Activity| row.kind.eq_ignore_ascii_case("courseboard");
                    self.activity_list(course, None, list.limit, "boards.list", keep)
                }
                BoardsCommand::Posts { board, list } => {
                    let response = client.get(&module_path(board, &["courseboard"])?)?;
                    let board_id = response.url.query_value("id");
                    let posts = parse::board_posts(&response.text, base, board_id)?;
                    let complete = !parse::has_next_page(&response.text);
                    listing(
                        "boards.posts",
                        posts,
                        list.limit,
                        complete,
                        |rows, avail| {
                            table(rows, avail, "Board posts", "REF\tPOSTED\tTITLE", |r| {
                                row(&[cell(&r.reference, "-"), cell(&r.posted, "-"), &r.title])
                            })
                        },
                    )
                }
                BoardsCommand::Show { post } => self.show_board_post(post, "boards.show"),
            },
            Command::Notices { command } => match command {
                NoticesCommand::List { course, list } => self.notices(course, list.limit),
                NoticesCommand::Show { notice } => self.show_board_post(notice, "notices.show"),
            },
            Command::Files { command } => match command {
                FilesCommand::List { course, list } => {
                    let resolved = self.resolve_course(course)?;
                    let kinds = ["resource", "folder", "page", "coursefile", "url"];
                    let files: Vec<_> = course_pages::activities(client, base, &resolved.id)?
                        .into_iter()
                        .filter(|row| kinds.contains(&row.kind.as_str()))
                        .map(|a| FileResource {
                            downloadable: matches!(a.kind.as_str(), "resource" | "coursefile")
                                && a.url.is_some(),
                            reference: a.reference,
                            id: a.id,
                            kind: a.kind,
                            title: a.title,
                            course_id: resolved.id.clone(),
                            course_ref: resolved.reference.clone(),
                            week: a.week,
                            section: a.section,
                            url: a.url,
                        })
                        .collect();
                    listing("files.list", files, list.limit, true, |rows, avail| {
                        if rows.is_empty() {
                            return "No course files found.".into();
                        }
                        let head = ("Files", "", "REF\tTYPE\tDOWNLOAD\tTITLE");
                        body(rows, avail, head, |r| {
                            let download = if r.downloadable { "yes" } else { "no" };
                            row(&[cell(&r.reference, "-"), &r.kind, download, &r.title])
                        })
                    })
                }
                FilesCommand::Download { source, out } => {
                    let source = match source.starts_with("file:") {
                        true => ResourceRef::parse(source)?.path(),
                        false => source.clone(),
                    };
                    download(client, &source, out)
                }
            },
            Command::Grades(args) | Command::Attendance(args) => {
                let CourseShowCommand::Show { course } = &args.command;
                let resolved = self.resolve_course(course)?;
                let (name, path, parse): (_, _, fn(&str, String) -> _) = match command {
                    Command::Grades(_) => {
                        ("grades.show", "/grade/report/user/index.php", parse::grades)
                    }
                    _ => (
                        "attendance.show",
                        "/local/lmsattendance/index.php",
                        parse::attendance,
                    ),
                };
                let page = client.get(&format!("{path}?id={}", resolved.id))?;
                report_result(name, &resolved.title, parse(&page.text, resolved.id)?)
            }
            Command::Request { command } => {
                let RequestCommand::Get { path, max_bytes } = command;
                raw_get(client, path, *max_bytes)
            }
            Command::Library { command } => match command {
                LibraryCommand::Sync(args) => library::sync(client, base, args),
                command => library::local(command),
            },
            _ => unreachable!("handled before authenticated dispatch"),
        }
    }

    fn session_time(&self, extend: bool) -> Result<CommandResult, AppError> {
        let key = parse::sesskey(&self.client.get("/my/")?.text)?;
        if extend {
            self.client.ajax(&key, "core_session_touch")?;
        }
        let data = self.client.ajax(&key, "core_session_time_remaining")?;
        let seconds = (data.get("timeremaining"))
            .and_then(serde_json::Value::as_u64)
            .or_else(|| data.as_u64())
            .ok_or_else(|| {
                AppError::shape("session time response did not contain timeremaining")
            })?;
        let model = SessionTime {
            remaining_seconds: seconds,
            remaining: format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds % 3600 / 60,
                seconds % 60
            ),
            bootstrap_may_have_extended_session: true,
            extended: extend,
        };
        let command = if extend {
            "auth.extend"
        } else {
            "auth.time-left"
        };
        let suffix = if extend { " (extended)" } else { "" };
        let human = format!("Session time remaining: {}{suffix}", model.remaining);
        let mut result = output::result(command, &model, human)?;
        result.warnings.push(
            "A dashboard request was needed to discover the session key and may have refreshed KLMS activity time."
                .into(),
        );
        Ok(result)
    }

    fn dashboard(&self, limit: usize) -> Result<CommandResult, AppError> {
        let mut model = parse::dashboard(&self.client.get("/my/")?.text, self.base)?;
        model.courses.truncate(limit);
        model.upcoming.truncate(limit);
        model.courses_complete = model.courses.len() == model.course_count;
        model.upcoming_complete = model.upcoming.len() == model.upcoming_count;
        let mut human = format!(
            "{} — {} courses, {} upcoming\n\nCourses:\nREF\tCODE\tTITLE",
            model.term.as_deref().unwrap_or("Current dashboard"),
            model.course_count,
            model.upcoming_count,
        );
        for course in &model.courses {
            human.push_str(&format!("\n{}", course_row(course)));
        }
        let (shown, total) = (model.courses.len(), model.course_count);
        if !model.courses_complete {
            human.push_str(&format!("\n[Showing {shown} of {total} courses]"));
        }
        human.push_str("\n\nUpcoming:");
        if model.upcoming.is_empty() {
            human.push_str("\nNone shown on the dashboard.");
        }
        for item in &model.upcoming {
            human.push_str(&format!("\n{}\t{}", item.title, item.url));
        }
        let (shown, total) = (model.upcoming.len(), model.upcoming_count);
        if shown > 0 && !model.upcoming_complete {
            human.push_str(&format!("\n[Showing {shown} of {total} upcoming items]"));
        }
        output::result("dashboard", &model, human)
    }

    fn courses(&self) -> Result<Vec<Course>, AppError> {
        Ok(parse::dashboard(&self.client.get("/my/")?.text, self.base)?.courses)
    }

    fn resolve_course(&self, query: &str) -> Result<Course, AppError> {
        if query.starts_with("course:") {
            let ResourceRef::Course(id) = ResourceRef::parse(query)? else {
                unreachable!("course prefix parses only as a course")
            };
            return self.resolve_course(&id);
        }
        if valid_id(query) {
            let url = self.base.join(&format!("/course/view.php?id={query}"));
            return Ok(Course {
                id: query.into(),
                reference: format!("course:{query}"),
                title: format!("Course {query}"),
                code: None,
                term: None,
                url: url.expect("valid path").into(),
            });
        }
        let matches = matching_courses(self.courses()?, query);
        let exact: Vec<_> = matches.iter().filter(|c| is_exact(c, query)).collect();
        if let [course] = exact.as_slice() {
            return Ok((*course).clone());
        }
        match matches.as_slice() {
            [course] => Ok(course.clone()),
            [] => Err(AppError::not_found(format!(
                "no dashboard course matches {query:?}"
            ))),
            _ => {
                let candidates: Vec<_> = (matches.iter().take(5))
                    .map(|c| {
                        format!(
                            "{} ({})",
                            c.code.as_deref().unwrap_or(&c.reference),
                            c.title
                        )
                    })
                    .collect();
                Err(AppError::usage(format!(
                    "course query {query:?} is ambiguous: {}; use an exact code or course reference",
                    candidates.join(", ")
                )))
            }
        }
    }

    fn activity_list(
        &self,
        course: &str,
        week: Option<u32>,
        limit: usize,
        command: &'static str,
        keep: impl Fn(&Activity) -> bool,
    ) -> Result<CommandResult, AppError> {
        let resolved = self.resolve_course(course)?;
        let mut rows = course_pages::activities(self.client, self.base, &resolved.id)?;
        rows.retain(|row| keep(row) && week.is_none_or(|week| row.week == Some(week)));
        activity_table(command, &resolved, rows, limit)
    }

    /// Shared list/show dispatch for assignments, quizzes and videos.
    fn module(
        &self,
        command: &ModuleCommand,
        show_command: &'static str,
        kinds: &[&str],
        list: impl FnOnce(&Course, usize) -> Result<CommandResult, AppError>,
    ) -> Result<CommandResult, AppError> {
        let target = match command {
            ModuleCommand::List { course, list: args } => {
                return list(&self.resolve_course(course)?, args.limit);
            }
            ModuleCommand::Show { target } => target,
        };
        let path = module_path(target, kinds)?;
        let response = self.client.get(&path)?;
        let shape = AppError::shape;
        let reference = ResourceRef::from_url(&response.url)
            .ok_or_else(|| shape("module detail URL had no supported resource kind"))?;
        if !reference.matches_module(kinds) {
            return Err(shape(
                "module detail redirected to an unexpected resource kind",
            ));
        }
        self.validate_detail_identity(&path, &response.url)?;
        let kind = (reference.activity_kind())
            .ok_or_else(|| shape("module detail URL had no supported activity kind"))?;
        let model = parse::resource_detail(&response.text, self.base, &response.url, kind)?;
        output::result(show_command, &model, detail(&model))
    }

    fn agenda(
        &self,
        course: Option<&str>,
        limit: usize,
        days: u32,
        label: &'static str,
    ) -> Result<CommandResult, AppError> {
        let course_id = (course.map(|c| self.resolve_course(c).map(|c| c.id))).transpose()?;
        let response = self.client.get(CALENDAR_PATH)?;
        let today = date::seoul_today();
        let through = date::add_days(&today, days as i64).expect("valid current date");
        let page = parse::calendar_page(&response.text, self.base, &today)?;
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
        let undated = page.undated_events;
        let mut rows: Vec<_> = (page.events.into_iter())
            .filter(|event| {
                let date = event.starts_at.as_deref().and_then(|value| value.get(..10));
                date.is_some_and(|d| d >= today.as_str() && d <= through.as_str())
                    && course_id
                        .as_ref()
                        .is_none_or(|id| event.course_id.as_ref() == Some(id))
            })
            .collect();
        rows.sort_by(|left, right| left.starts_at.cmp(&right.starts_at));
        let mut result = listing(label, rows, limit, true, |rows, available| {
            let single = today == through;
            if rows.is_empty() {
                return match single {
                    true => format!("Nothing scheduled for {today}."),
                    false => format!("Nothing scheduled from {today} through {through}."),
                };
            }
            let title = match single {
                true => format!("Today ({today})"),
                false => format!("Upcoming ({today} through {through})"),
            };
            body(
                rows,
                available,
                (&title, " items", "WHEN\tREF\tCOURSE\tTITLE"),
                event_row,
            )
        })?;
        if undated > 0 {
            result.warnings.push(format!(
                "{undated} calendar event(s) have no date and are not shown; see `klms calendar list`"
            ));
        }
        Ok(result)
    }

    fn notices(&self, course: &str, limit: usize) -> Result<CommandResult, AppError> {
        let resolved = self.resolve_course(course)?;
        let activities = course_pages::activities(self.client, self.base, &resolved.id)?;
        let mut rows = Vec::new();
        let mut source_complete = true;
        for board in activities.into_iter().filter(parse::is_notice_board) {
            let Some(board_ref) = board.reference else {
                return Err(AppError::shape(
                    "notice board contained no canonical module reference",
                ));
            };
            let response = self.client.get(&ResourceRef::parse(&board_ref)?.path())?;
            source_complete &= !parse::has_next_page(&response.text);
            let board_id = response.url.query_value("id");
            for post in parse::board_posts(&response.text, self.base, board_id)? {
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
        listing(
            "notices.list",
            rows,
            limit,
            source_complete,
            |rows, avail| {
                table(rows, avail, "Notices", "POSTED\tREF\tTITLE", |r| {
                    let posted = r.posted_at.as_ref().or(r.posted_text.as_ref());
                    row(&[
                        posted.map_or("unknown", |p| p.as_str()),
                        &r.reference,
                        &r.title,
                    ])
                })
            },
        )
    }

    fn show_board_post(
        &self,
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
        let response = self.client.get(&target)?;
        let numeric = |key| {
            response
                .url
                .query_value(key)
                .is_some_and(|id| valid_id(&id))
        };
        if response.url.path() != "/mod/courseboard/article.php"
            || !numeric("id")
            || !numeric("bwid")
        {
            return Err(AppError::shape(
                "board post detail redirected to an unexpected resource kind",
            ));
        }
        self.validate_detail_identity(&target, &response.url)?;
        let kind = "courseboard-post";
        let model = parse::resource_detail(&response.text, self.base, &response.url, kind)?;
        output::result(command, &model, detail(&model))
    }

    /// A known module or article URL identifies the requested object even when
    /// the server redirects. Extra query parameters and numeric zero padding do not.
    fn validate_detail_identity(&self, target: &str, final_url: &Url) -> Result<(), AppError> {
        let requested_url =
            (self.base.join(target)).map_err(|_| AppError::usage("invalid module detail URL"))?;
        let Some(requested) = ResourceRef::from_url(&requested_url) else {
            return Ok(());
        };
        let same_id = |key| match (requested_url.query_value(key), final_url.query_value(key)) {
            (Some(a), Some(b)) => a.trim_start_matches('0') == b.trim_start_matches('0'),
            (a, b) => a.is_none() && b.is_none(),
        };
        let final_kind = ResourceRef::from_url(final_url);
        if requested.activity_kind() != final_kind.as_ref().and_then(ResourceRef::activity_kind)
            || !same_id("id")
            || (requested_url.path() == "/mod/courseboard/article.php" && !same_id("bwid"))
        {
            return Err(AppError::shape(
                "module detail redirected to a different resource identity",
            ));
        }
        Ok(())
    }
}

fn activity_table(
    command: &'static str,
    course: &Course,
    rows: Vec<Activity>,
    limit: usize,
) -> Result<CommandResult, AppError> {
    listing(command, rows, limit, true, |rows, available| {
        let head = (course.title.as_str(), " items", "REF\tTYPE\tWEEK\tTITLE");
        body(rows, available, head, |r| {
            let week = r.week.map_or("-".into(), |week| week.to_string());
            row(&[cell(&r.reference, "-"), &r.kind, &week, &r.title])
        })
    })
}

fn event_row(r: &CalendarEvent) -> String {
    let (when, reference, course) = (
        cell(&r.starts_at, "unknown"),
        cell(&r.reference, "-"),
        cell(&r.course, "-"),
    );
    row(&[when, reference, course, &r.title])
}

fn module_path(target: &str, kinds: &[&str]) -> Result<String, AppError> {
    let url_like = target.starts_with("https://") || target.starts_with("http://");
    if target.contains(':') && !url_like {
        let reference = ResourceRef::parse(target)?;
        if !reference.matches_module(kinds) {
            return Err(AppError::usage(format!(
                "resource reference {target:?} does not identify one of: {}",
                kinds.join(", ")
            )));
        }
        Ok(reference.path())
    } else if valid_id(target) {
        match kinds {
            [kind] => Ok(format!("/mod/{kind}/view.php?id={target}")),
            _ => Err(AppError::usage(format!(
                "numeric id {target} is ambiguous; use the typed reference returned by the list command"
            ))),
        }
    } else if target.starts_with('/') || url_like {
        Ok(target.into())
    } else {
        Err(AppError::usage(
            "expected a canonical resource reference, numeric module id, or same-origin KLMS URL",
        ))
    }
}

fn is_exact(course: &Course, query: &str) -> bool {
    course.title.eq_ignore_ascii_case(query)
        || (course.code.as_ref()).is_some_and(|code| code.eq_ignore_ascii_case(query))
}

fn matching_courses(courses: Vec<Course>, query: &str) -> Vec<Course> {
    let needle = query.to_ascii_lowercase();
    let contains = |text: &str| text.to_ascii_lowercase().contains(&needle);
    let mut rows: Vec<_> = (courses.into_iter())
        .filter(|c| c.id == query || contains(&c.title) || c.code.as_deref().is_some_and(contains))
        .collect();
    rows.sort_by_key(|course| !(course.id == query || is_exact(course, query)));
    rows
}

fn course_row(course: &Course) -> String {
    row(&[&course.reference, cell(&course.code, "-"), &course.title])
}

fn courses_table(rows: &[Course], available: usize) -> String {
    table(rows, available, "Courses", "REF\tCODE\tTITLE", course_row)
}

fn report_result(
    command: &'static str,
    title: &str,
    report: Report,
) -> Result<CommandResult, AppError> {
    let mut lines = vec![format!("{title} — {} rows", report.rows.len())];
    if !report.headers.is_empty() {
        lines.push(report.headers.join("\t"));
    }
    lines.extend(report.rows.iter().map(|r| r.join("\t")));
    output::result(command, &report, lines.join("\n"))
}

fn download(client: &KlmsClient, source: &str, out: &Path) -> Result<CommandResult, AppError> {
    if out.exists() {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let parent =
        (out.parent().filter(|path| !path.as_os_str().is_empty())).unwrap_or(Path::new("."));
    if !parent.exists() {
        return Err(AppError::config(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }
    let temp = parent.join(format!(".klms-download-{}.part", std::process::id()));
    let (response, linked) = publish_new(&temp, out, |file| {
        client.download_to(source, MAX_DOWNLOAD_BYTES, file)
    })
    .map_err(|error| match error {
        PublishError::Fill(error) => error,
        PublishError::Create(error) => {
            AppError::config(format!("cannot create temporary download: {error}"))
        }
        PublishError::Sync(error) => AppError::config(format!("failed to write download: {error}")),
        PublishError::Link(error) => {
            AppError::config(format!("failed to finalize download: {error}"))
        }
    })?;
    if linked.existed {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let cleanup_warning = linked.leftover.map(|error| {
        format!(
            "download completed, but temporary link cleanup failed for {}: {error}",
            linked.temporary.display()
        )
    });
    let model = DownloadResult {
        path: out
            .canonicalize()
            .unwrap_or_else(|_| out.to_path_buf())
            .display()
            .to_string(),
        bytes: response.bytes,
        source_url: safe_url::display(&response.url),
        content_type: response.content_type,
    };
    let human = format!("Downloaded {} bytes to {}", model.bytes, model.path);
    let mut result = output::result("files.download", &model, human)?;
    result.warnings.extend(cleanup_warning);
    Ok(result)
}

fn raw_get(client: &KlmsClient, path: &str, max_bytes: usize) -> Result<CommandResult, AppError> {
    validate_read_target(path)?;
    let response = client.get_preview(path, max_bytes)?;
    let kind = (response.content_type.as_deref())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let html = kind.contains("text/html");
    if !html && !kind.contains("json") {
        return Err(AppError::usage(
            "request get previews HTML and JSON only; use `files download` for other content",
        ));
    }
    let body = if html {
        redact_secrets(&parse::safe_html_preview(&String::from_utf8_lossy(
            &response.bytes,
        )))
    } else if response.truncated {
        "[JSON preview omitted because the bounded response is incomplete]".into()
    } else {
        let mut value: serde_json::Value = serde_json::from_slice(&response.bytes)
            .map_err(|error| AppError::shape(format!("invalid JSON response: {error}")))?;
        redact_json(&mut value);
        value.to_string()
    };
    let model = RawGet {
        url: safe_url::display(&response.url),
        content_type: response.content_type,
        bytes: response.bytes.len(),
        body,
        truncated: response.truncated,
        redacted: true,
    };
    output::result("request.get", &model, model.body.clone())
}

fn validate_read_target(value: &str) -> Result<(), AppError> {
    let base = Url::parse("https://klms.invalid/").expect("valid fixed URL");
    let url =
        (base.join(value)).map_err(|e| AppError::usage(format!("invalid request target: {e}")))?;
    let path = url.path();
    let module =
        path.starts_with("/mod/") && (path.ends_with("/view.php") || path.ends_with("/index.php"));
    let exact = [
        "/my/",
        "/course/view.php",
        "/calendar/view.php",
        "/local/lmsattendance/index.php",
    ];
    if !(module
        || exact.contains(&path)
        || path.starts_with("/grade/report/")
        || path.starts_with("/pluginfile.php/"))
    {
        return Err(AppError::usage(
            "request get accepts known content-read paths only; use a typed command when available",
        ));
    }
    let refused = |key: &str| {
        safe_url::sensitive_key(key)
            || ["action", "delete", "confirm", "logout"]
                .contains(&key.to_ascii_lowercase().as_str())
    };
    if url.query_pairs().any(|(key, _)| refused(&key)) {
        return Err(AppError::usage(
            "request get refuses action and secret query parameters",
        ));
    }
    Ok(())
}

fn redact_secrets(value: &str) -> String {
    let keys = ["sesskey", "logintoken", "moodlesession", "token"];
    keys.iter()
        .fold(value.to_owned(), |text, key| redact_key_values(&text, key))
}

fn redact_json(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Object(values) => {
            values
                .iter_mut()
                .for_each(|(key, value)| match safe_url::sensitive_key(key) {
                    true => *value = Value::String("[REDACTED]".into()),
                    false => redact_json(value),
                })
        }
        Value::Array(values) => values.iter_mut().for_each(redact_json),
        Value::String(value) => *value = redact_secrets(value),
        _ => {}
    }
}

/// Replace the value after `key` (`key: v`, `key=v`, `"key":"v"`) with
/// `[REDACTED]`, keeping the surrounding punctuation and quotes.
fn redact_key_values(value: &str, key: &str) -> String {
    let (lower, bytes) = (value.to_ascii_lowercase(), value.as_bytes());
    let skip = |from: usize, keep: &dyn Fn(u8) -> bool| {
        from + bytes[from..].iter().take_while(|&&byte| keep(byte)).count()
    };
    let (mut result, mut cursor) = (String::with_capacity(value.len()), 0);
    while let Some(found) = lower[cursor..].find(key) {
        let after = cursor + found + key.len();
        let separator = skip(after, &|b| {
            matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'\'')
        });
        if !matches!(bytes.get(separator), Some(b':' | b'=')) {
            result.push_str(&value[cursor..after]);
            cursor = after;
            continue;
        }
        let mut start = skip(separator + 1, &|b| b.is_ascii_whitespace());
        let quote = bytes
            .get(start)
            .copied()
            .filter(|b| matches!(b, b'"' | b'\''));
        start += usize::from(quote.is_some());
        let end = skip(start, &|b| match quote {
            Some(quote) => b != quote,
            None => !(b.is_ascii_whitespace() || matches!(b, b'&' | b',' | b'}' | b']')),
        });
        result.push_str(&value[cursor..start]);
        result.push_str("[REDACTED]");
        cursor = end;
    }
    result.push_str(&value[cursor..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_preview_redacts_secret_assignments_and_json_fields() {
        let redacted = redact_secrets(r#"{"sesskey":"abc123","name":"safe"}&token=xyz789"#);
        assert!(!redacted.contains("abc123") && !redacted.contains("xyz789"));
        assert!(redacted.contains(r#""name":"safe""#));
        assert_eq!(redacted.matches("[REDACTED]").count(), 2);
        assert_eq!(redact_secrets("the token is here"), "the token is here");

        let mut value = serde_json::json!({"data": {"access_token": "abc123", "name": "safe"}});
        redact_json(&mut value);
        assert_eq!(value["data"]["access_token"], "[REDACTED]");
        assert_eq!(value["data"]["name"], "safe");
    }

    #[test]
    fn raw_preview_rejects_action_routes_and_secret_queries() {
        assert!(validate_read_target("/login/logout.php?sesskey=abc").is_err());
        assert!(validate_read_target("/mod/assign/view.php?id=7&action=delete").is_err());
        assert!(validate_read_target("/mod/assign/view.php?id=7").is_ok());
    }
}
