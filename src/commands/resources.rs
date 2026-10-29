use super::{Ctx, download::download};
use crate::{
    cli::{
        ActivitiesCommand, BoardsCommand, Command, CourseShowCommand, FilesCommand, ModuleCommand,
        NoticesCommand,
    },
    course_pages, date,
    error::AppError,
    models::{Activity, Course, FileResource, Notice, Report},
    output::{self, CommandResult, body, cell, detail, listing, row, table},
    parse,
    reference::{ResourceRef, valid_id},
    url::{Url, query_value},
};

impl Ctx<'_> {
    pub(super) fn resources(&self, command: &Command) -> Result<CommandResult, AppError> {
        let client = self.client;
        match command {
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
            Command::Boards { command } => match command {
                BoardsCommand::List { course, list } => {
                    let keep = |row: &Activity| row.kind.eq_ignore_ascii_case("courseboard");
                    self.activity_list(course, None, list.limit, "boards.list", keep)
                }
                BoardsCommand::Posts { board, list } => {
                    let path = module_path(board, &["courseboard"])?;
                    client.validate_operand(&path)?;
                    // Every page is read even under --limit: stopping early
                    // would drop meta.total and report source_complete:false
                    // for a board that is merely truncated locally.
                    let mut posts = Vec::new();
                    let exhausted =
                        crate::board_pages::walk(client, &path, |_, post| posts.push(post))?;
                    listing(
                        "boards.posts",
                        posts,
                        list.limit,
                        exhausted,
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
                    let files: Vec<_> = course_pages::activities(client, &resolved.id)?
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
            _ => unreachable!("resource command"),
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
        let mut rows = course_pages::activities(self.client, &resolved.id)?;
        rows.retain(|row| keep(row) && week.is_none_or(|week| row.week == Some(week)));
        activity_table(command, &resolved, rows, limit)
    }

    /// Shared list/show dispatch for assignments and quizzes.
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
        self.client.validate_operand(&path)?;
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

    fn notices(&self, course: &str, limit: usize) -> Result<CommandResult, AppError> {
        let resolved = self.resolve_course(course)?;
        let activities = course_pages::activities(self.client, &resolved.id)?;
        let mut rows = Vec::new();
        let mut source_complete = true;
        for board in activities.into_iter().filter(parse::is_notice_board) {
            let Some(board_ref) = board.reference else {
                return Err(AppError::shape(
                    "notice board contained no canonical module reference",
                ));
            };
            let path = ResourceRef::parse(&board_ref)?.path();
            source_complete &= crate::board_pages::walk(self.client, &path, |reference, post| {
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
            })?;
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
        let target = module_path(post, &["courseboard-post"])?;
        self.client.validate_operand(&target)?;
        let response = self.client.get(&target)?;
        let numeric = |key| query_value(&response.url, key).is_some_and(|id| valid_id(&id));
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
    pub(super) fn validate_detail_identity(
        &self,
        target: &str,
        final_url: &Url,
    ) -> Result<(), AppError> {
        let requested_url =
            (self.base.join(target)).map_err(|_| AppError::usage("invalid module detail URL"))?;
        let Some(requested) = ResourceRef::from_url(&requested_url) else {
            return Ok(());
        };
        let same_id = |key| match (
            query_value(&requested_url, key),
            query_value(final_url, key),
        ) {
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

pub(super) fn module_path(target: &str, kinds: &[&str]) -> Result<String, AppError> {
    let url_like = target.starts_with("https://") || target.starts_with("http://");
    let matches = |reference: &ResourceRef| {
        reference.matches_module(kinds)
            || (kinds == ["courseboard-post"] && matches!(reference, ResourceRef::BoardPost { .. }))
    };
    if target.starts_with('/') || url_like {
        let base = Url::parse("https://klms.invalid/").expect("fixed URL");
        let url = base
            .join(target)
            .map_err(|_| AppError::usage("invalid detail URL"))?;
        if crate::url::has_userinfo(&url) {
            return Err(AppError::usage("detail URL must not contain userinfo"));
        }
        let reference = crate::client::policy::detail_ref(&url)
            .filter(matches)
            .ok_or_else(|| AppError::usage("expected this resource kind's detail endpoint with only its identity parameters"))?;
        // Preserve an absolute operand's origin for the client's same-origin check.
        return if url_like || target.starts_with("//") {
            Ok(url.join(&reference.path()).expect("canonical path").into())
        } else {
            Ok(reference.path())
        };
    }
    let reference = if valid_id(target) {
        match kinds {
            [kind] if *kind != "courseboard-post" => {
                return module_path(&format!("/mod/{kind}/view.php?id={target}"), kinds);
            }
            _ => {
                return Err(AppError::usage(
                    "numeric id is ambiguous; use a typed reference",
                ));
            }
        }
    } else {
        ResourceRef::parse(target)?
    };
    if !matches(&reference) {
        return Err(AppError::usage(
            "resource reference does not identify this command's resource kind",
        ));
    }
    Ok(reference.path())
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
