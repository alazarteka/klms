use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

const LIST_HELP: &str = "Examples:\n  klms courses list\n  klms --json courses list";

#[derive(Debug, Parser)]
#[command(
    name = "klms",
    version,
    about = "Fast, agent-friendly access to KAIST KLMS",
    long_about = "Read KAIST KLMS directly over authenticated HTTP. Human output is the default; --json emits one versioned document for agents and scripts.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Emit one versioned JSON document.
    #[arg(long, global = true)]
    pub json: bool,

    /// KLMS origin. HTTP is accepted only for loopback integration tests.
    #[arg(
        long,
        global = true,
        env = "KLMS_BASE_URL",
        default_value = "https://klms.kaist.ac.kr",
        hide = true
    )]
    pub base_url: String,

    /// Total timeout for each HTTP request, in seconds.
    #[arg(long, global = true, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=120))]
    pub timeout: u64,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install or inspect the companion Agent Skill.
    Skill(SkillArgs),
    /// Check configuration and the live session.
    #[command(after_help = "Example:\n  klms --json doctor")]
    Doctor,
    /// Inspect or extend the authenticated session.
    Auth(AuthArgs),
    /// Show dashboard courses and upcoming work.
    #[command(after_help = "Examples:\n  klms dashboard\n  klms --json dashboard --limit 20")]
    Dashboard(ListArgs),
    /// Show items scheduled for today in Korea time.
    #[command(after_help = "Examples:\n  klms today\n  klms --json today --course CS.30200")]
    Today(AgendaArgs),
    /// Show scheduled items through a bounded future window.
    #[command(
        after_help = "Examples:\n  klms upcoming\n  klms --json upcoming --through 7d --course CS.30200"
    )]
    Upcoming(UpcomingArgs),
    /// Discover, resolve, or inspect courses.
    Courses(CoursesArgs),
    /// List the typed weekly structure of a course.
    Activities(ActivitiesArgs),
    /// List or inspect assignments.
    Assignments(ModuleArgs),
    /// List or inspect quizzes.
    Quizzes(ModuleArgs),
    /// List upcoming calendar events.
    Calendar(CalendarArgs),
    /// List boards and inspect their posts.
    Boards(BoardsArgs),
    /// List course notices and inspect their content.
    Notices(NoticesArgs),
    /// List or download course files.
    Files(FilesArgs),
    /// List or inspect video metadata.
    Videos(ModuleArgs),
    /// Show the grade report for a course.
    Grades(CourseShowArgs),
    /// Show the attendance report for a course.
    Attendance(CourseShowArgs),
    /// Preview a known same-origin HTML or JSON read (experimental repair hatch).
    Request(RequestArgs),
    /// Inspect and synchronize the private versioned local library.
    Library(LibraryArgs),
    /// Print the executable command grammar; --json emits the full argument tree.
    #[command(after_help = "Examples:\n  klms spec\n  klms --json spec")]
    Spec,
    /// Print a shell completion script generated from the executable grammar.
    #[command(
        after_help = "Example:\n  klms completions bash > ~/.local/share/bash-completion/completions/klms"
    )]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Args)]
pub struct LibraryArgs {
    #[command(subcommand)]
    pub command: LibraryCommand,
}

#[derive(Debug, Subcommand)]
pub enum LibraryCommand {
    /// Initialize if needed and report local corpus state without contacting KLMS.
    #[command(after_help = "Example:\n  klms --json library status")]
    Status,
    /// Synchronize finite typed KLMS surfaces into the local library.
    Sync(LibrarySyncArgs),
    /// Search source text and effective local curation.
    Search {
        #[arg(value_name = "QUERY", value_parser = nonempty_operand)]
        query: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// List remote-source changes independently of local curation.
    Changes(ListArgs),
    /// List local curation activity independently of remote changes.
    Activity(LibraryActivityArgs),
    /// Show bounded source/effective state for a stable reference.
    Show {
        #[arg(value_name = "REF")]
        reference: String,
    },
    /// Show bounded immutable history for a stable reference.
    History {
        #[arg(value_name = "REF")]
        reference: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Preview downloaded file bytes (UTF-8 text when available).
    #[command(
        after_help = "Stored notice text is available through `klms library show REF` (JSON: data.source.text). Non-file links are metadata: inspect their URL with `klms library show REF`. This command does not download files or follow links."
    )]
    Content {
        #[arg(value_name = "REF")]
        reference: String,
        #[arg(long, value_name = "N", default_value_t = 1_048_576, value_parser = parse_preview_limit)]
        max_bytes: usize,
    },
    /// Export downloaded file bytes without overwriting.
    #[command(
        after_help = "Stored notice text is available through `klms library show REF` (JSON: data.source.text). Non-file links are metadata: inspect their URL with `klms library show REF`. This command does not download files or follow links."
    )]
    Export {
        #[arg(value_name = "REF")]
        reference: String,
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Append a revisioned local curation assertion.
    Edit(LibraryEditArgs),
    /// Retract a curation assertion or relation without deleting history.
    Retract(LibraryRetractArgs),
    /// Add typed relationships.
    Relations(LibraryRelationsArgs),
}

#[derive(Debug, Args)]
pub struct LibrarySyncArgs {
    #[arg(long, value_name = "COURSE")]
    pub course: Option<String>,
    #[arg(long)]
    pub notices: bool,
    #[arg(long)]
    pub files: bool,
    #[arg(long, value_enum)]
    pub download: Option<LibraryDownloadArg>,
}

#[derive(Debug, Args)]
pub struct LibraryActivityArgs {
    #[arg(long, value_name = "REF")]
    pub subject: Option<String>,
    #[command(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LibraryDownloadArg {
    Changed,
}

#[derive(Debug, Args)]
#[command(group = clap::ArgGroup::new("value_source").required(true).multiple(false))]
pub struct LibraryEditArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[arg(long, value_enum)]
    pub field: LibraryFieldArg,
    /// Inline text; exactly one of --value and --value-file is required.
    #[arg(long, value_name = "TEXT", group = "value_source")]
    pub value: Option<String>,
    /// Read the text from a file, or from stdin with `-`.
    #[arg(long, value_name = "PATH", group = "value_source")]
    pub value_file: Option<PathBuf>,
    #[arg(long, value_name = "ACTOR", default_value = "human")]
    pub actor: String,
    #[arg(long, value_name = "N")]
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LibraryFieldArg {
    Title,
    Filename,
    Summary,
    Note,
    Tag,
}

#[derive(Debug, Args)]
pub struct LibraryRetractArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[arg(long, value_name = "ACTOR", default_value = "human")]
    pub actor: String,
}

#[derive(Debug, Args)]
pub struct LibraryRelationsArgs {
    #[command(subcommand)]
    pub command: LibraryRelationsCommand,
}

#[derive(Debug, Subcommand)]
pub enum LibraryRelationsCommand {
    /// Record a typed relation between two library subjects.
    Add {
        #[arg(value_name = "LEFT")]
        left: String,
        #[arg(value_name = "RIGHT")]
        right: String,
        #[arg(long, value_name = "KIND")]
        kind: String,
        #[arg(long, value_name = "ACTOR", default_value = "human")]
        actor: String,
    },
}

#[derive(Debug, Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub command: SkillCommand,
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Install the embedded skill and its cross-client discovery link.
    Install,
    /// Report the managed skill and discovery-link state.
    Status,
}

#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    /// Maximum number of rows to return.
    #[arg(long, value_name = "N", default_value_t = 100, value_parser = parse_list_limit)]
    pub limit: usize,
}

#[derive(Debug, Clone, Args)]
pub struct AgendaArgs {
    /// Restrict results to one course id, code, or title.
    #[arg(long, value_name = "COURSE")]
    pub course: Option<String>,
    #[command(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Clone, Args)]
pub struct UpcomingArgs {
    /// Include today through this many days ahead (for example, 7d).
    #[arg(long, value_name = "Nd", default_value = "7d", value_parser = parse_days)]
    pub through: u32,
    /// Restrict results to one course id, code, or title.
    #[arg(long, value_name = "COURSE")]
    pub course: Option<String>,
    #[command(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Sign in directly through KAIST SSO and save a private KLMS session.
    Login(AuthLoginArgs),
    /// Remove the locally saved KLMS session.
    Logout,
    /// Report owned-session metadata without exposing secrets.
    #[command(after_help = "Example:\n  klms --json auth status")]
    Status,
    /// Ask KLMS for the server-authoritative time remaining.
    #[command(after_help = "Example:\n  klms --json auth time-left")]
    TimeLeft,
    /// Refresh the current KLMS session timer.
    #[command(after_help = "Example:\n  klms --json auth extend")]
    Extend,
}

#[derive(Debug, Args)]
pub struct AuthLoginArgs {
    /// KAIST sign-in method.
    #[arg(long, value_enum)]
    pub method: Option<AuthMethodArg>,

    /// Password-login verification channel.
    #[arg(long, value_enum, requires = "method")]
    pub second_factor: Option<AuthSecondFactorArg>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AuthMethodArg {
    Easy,
    Password,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AuthSecondFactorArg {
    Email,
    Sms,
}

#[derive(Debug, Args)]
pub struct CoursesArgs {
    #[command(subcommand)]
    pub command: CoursesCommand,
}

#[derive(Debug, Subcommand)]
pub enum CoursesCommand {
    /// List courses visible on the selected dashboard term.
    #[command(after_help = LIST_HELP)]
    List(ListArgs),
    /// Return matching courses without guessing.
    #[command(
        after_help = "Examples:\n  klms courses resolve CS.30200\n  klms --json courses resolve 'machine learning' --limit 5"
    )]
    Resolve {
        #[arg(value_name = "QUERY", value_parser = nonempty_operand)]
        query: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show one course by numeric id, code, or unambiguous title fragment.
    #[command(
        after_help = "Examples:\n  klms courses show 189705\n  klms --json courses show CS.30200"
    )]
    Show {
        #[arg(value_name = "COURSE", value_parser = nonempty_operand)]
        course: String,
    },
}

#[derive(Debug, Args)]
pub struct ActivitiesArgs {
    #[command(subcommand)]
    pub command: ActivitiesCommand,
}

#[derive(Debug, Subcommand)]
pub enum ActivitiesCommand {
    /// List the visible activities in a course.
    #[command(
        after_help = "Examples:\n  klms activities list --course CS.30200\n  klms --json activities list --course 189705 --week 3 --kind quiz"
    )]
    List {
        #[arg(long, value_name = "COURSE")]
        course: String,
        #[arg(long, value_name = "N")]
        week: Option<u32>,
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
}

#[derive(Debug, Args)]
pub struct ModuleArgs {
    #[command(subcommand)]
    pub command: ModuleCommand,
}

#[derive(Debug, Subcommand)]
pub enum ModuleCommand {
    /// List this resource type in a course.
    #[command(
        after_help = "The parent command selects assignments, quizzes, or videos.\n\nExamples:\n  klms assignments list --course CS.30200\n  klms quizzes list --course 189705\n  klms videos list --course 189705"
    )]
    List {
        #[arg(long, value_name = "COURSE")]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show one resource by canonical ref, numeric id where unambiguous, or URL.
    #[command(
        after_help = "Examples:\n  klms assignments show assign:1210516\n  klms quizzes show quiz:1210517\n  klms videos show lti:1265520"
    )]
    Show {
        #[arg(value_name = "REF")]
        target: String,
    },
}

#[derive(Debug, Args)]
pub struct CalendarArgs {
    #[command(subcommand)]
    pub command: CalendarCommand,
}

#[derive(Debug, Subcommand)]
pub enum CalendarCommand {
    /// List upcoming calendar events.
    #[command(after_help = "Example:\n  klms --json calendar list --limit 50")]
    List(ListArgs),
}

#[derive(Debug, Args)]
pub struct BoardsArgs {
    #[command(subcommand)]
    pub command: BoardsCommand,
}

#[derive(Debug, Args)]
pub struct NoticesArgs {
    #[command(subcommand)]
    pub command: NoticesCommand,
}

#[derive(Debug, Subcommand)]
pub enum NoticesCommand {
    /// List posts from the course notice board.
    #[command(after_help = "Example:\n  klms notices list --course CS.30200 --limit 20")]
    List {
        #[arg(long, value_name = "COURSE")]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a notice by the reference returned from `notices list`.
    #[command(after_help = "Example:\n  klms notices show board-post:1189554:420856")]
    Show {
        #[arg(value_name = "NOTICE")]
        notice: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum BoardsCommand {
    /// List course boards.
    #[command(after_help = "Example:\n  klms --json boards list --course CS.30200")]
    List {
        #[arg(long, value_name = "COURSE")]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// List posts by canonical board ref, module id, or URL.
    #[command(after_help = "Example:\n  klms --json boards posts board:1265521 --limit 50")]
    Posts {
        #[arg(value_name = "BOARD")]
        board: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a post by canonical ref or same-origin article URL.
    #[command(after_help = "Example:\n  klms --json boards show board-post:1265521:42")]
    Show {
        #[arg(value_name = "BOARD_POST")]
        post: String,
    },
}

#[derive(Debug, Args)]
pub struct FilesArgs {
    #[command(subcommand)]
    pub command: FilesCommand,
}

#[derive(Debug, Subcommand)]
pub enum FilesCommand {
    /// List file-like activities in a course.
    #[command(after_help = "Example:\n  klms --json files list --course CS.30200")]
    List {
        #[arg(long, value_name = "COURSE")]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Download a file ref or same-origin KLMS URL without overwriting.
    #[command(
        after_help = "Examples:\n  klms files download file:1205160 --out ./notes.pdf\n  klms files download 'https://klms.kaist.ac.kr/pluginfile.php/...' --out ./notes.pdf"
    )]
    Download {
        #[arg(value_name = "FILE_REF_OR_URL")]
        source: String,
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct CourseShowArgs {
    #[command(subcommand)]
    pub command: CourseShowCommand,
}

#[derive(Debug, Subcommand)]
pub enum CourseShowCommand {
    /// Show this resource for a course.
    #[command(
        after_help = "Examples:\n  klms --json grades show --course CS.30200\n  klms --json attendance show --course 189705"
    )]
    Show {
        #[arg(long, value_name = "COURSE")]
        course: String,
    },
}

#[derive(Debug, Args)]
pub struct RequestArgs {
    #[command(subcommand)]
    pub command: RequestCommand,
}

#[derive(Debug, Subcommand)]
pub enum RequestCommand {
    /// GET a same-origin path or URL and return a redacted, bounded text preview.
    #[command(
        after_help = "Example:\n  klms --json request get '/mod/assign/view.php?id=1210516' --max-bytes 65536"
    )]
    Get {
        #[arg(value_name = "PATH")]
        path: String,
        #[arg(long, value_name = "N", default_value_t = 65_536, value_parser = parse_preview_limit)]
        max_bytes: usize,
    },
}

fn parse_list_limit(value: &str) -> Result<usize, String> {
    parse_bounded(value, 1, 1_000, "limit")
}

fn nonempty_operand(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("course query must not be empty".into())
    } else {
        Ok(value.to_owned())
    }
}

fn parse_preview_limit(value: &str) -> Result<usize, String> {
    parse_bounded(value, 1, 1_048_576, "max-bytes")
}

fn parse_days(value: &str) -> Result<u32, String> {
    let value = value.strip_suffix('d').unwrap_or(value);
    let days = value
        .parse::<u32>()
        .map_err(|_| "through must be a day count such as 7d".to_owned())?;
    if (1..=90).contains(&days) {
        Ok(days)
    } else {
        Err("through must be between 1d and 90d".into())
    }
}

fn parse_bounded(value: &str, minimum: usize, maximum: usize, name: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{name} must be an integer"))?;
    if (minimum..=maximum).contains(&parsed) {
        Ok(parsed)
    } else {
        Err(format!("{name} must be between {minimum} and {maximum}"))
    }
}
