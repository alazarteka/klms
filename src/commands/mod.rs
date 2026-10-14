mod agenda;
mod auth;
mod courses;
mod download;
mod library;
mod preview;
mod resources;
mod videos;

use crate::{
    auth as authentication,
    cli::{AuthCommand, CalendarCommand, Cli, Command, LibraryCommand, RequestCommand},
    client::{KlmsClient, validate_base_url},
    error::AppError,
    output::CommandResult,
    url::Url,
};

/// An authenticated client and the origin it talks to.
struct Ctx<'a> {
    client: &'a KlmsClient,
    base: &'a Url,
    timeout: u64,
}

pub(crate) fn run(cli: &Cli) -> Result<CommandResult, AppError> {
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
                let options = authentication::LoginOptions {
                    user: login.user.clone(),
                    method: login.method,
                    factor: login.second_factor,
                    remember_password: login.remember_password,
                    insecure_storage: login.insecure_storage,
                    code: login.code.clone(),
                };
                return authentication::login(&base, cli.timeout, &options, cli.json);
            }
            AuthCommand::Logout => return authentication::logout(),
            AuthCommand::Forget => return authentication::forget(),
            _ => {}
        }
    }
    let session = authentication::load(&base)?;
    match &cli.command {
        Command::Auth {
            command: AuthCommand::Status,
        } => auth::auth_status(&session.status),
        Command::Doctor => auth::doctor(&base, session, cli.timeout),
        command => {
            let cookie = (session.cookie_header.as_deref())
                .ok_or_else(|| AppError::auth_required("no usable KLMS session was found"))?;
            let client = KlmsClient::new(base.as_str(), Some(cookie), cli.timeout)?;
            let ctx = Ctx {
                client: &client,
                base: &base,
                timeout: cli.timeout,
            };
            ctx.live(command)
        }
    }
}

impl Ctx<'_> {
    fn live(&self, command: &Command) -> Result<CommandResult, AppError> {
        match command {
            Command::Auth { command } => self.session_time(matches!(command, AuthCommand::Extend)),
            Command::Dashboard(args) => self.dashboard(args.limit),
            Command::Today(a) => {
                self.agenda(a.course.as_deref(), &a.kind, a.list.limit, 0, "today")
            }
            Command::Upcoming(a) => self.agenda(
                a.course.as_deref(),
                &a.kind,
                a.list.limit,
                a.through,
                "upcoming",
            ),
            Command::Courses { command } => self.courses_command(command),
            Command::Activities { .. }
            | Command::Assignments(_)
            | Command::Quizzes(_)
            | Command::Boards { .. }
            | Command::Notices { .. }
            | Command::Files { .. }
            | Command::Grades(_)
            | Command::Attendance(_) => self.resources(command),
            Command::Videos { command } => self.videos(command),
            Command::Calendar {
                command: CalendarCommand::List(list),
            } => self.calendar(list),
            Command::Request { command } => {
                let RequestCommand::Get { path, max_bytes } = command;
                preview::raw_get(self.client, path, *max_bytes)
            }
            Command::Library { command } => match command {
                LibraryCommand::Sync(args) => library::sync(self.client, self.base, args),
                command => library::local(command),
            },
            _ => unreachable!("handled before authenticated dispatch"),
        }
    }
}
