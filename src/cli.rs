use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

const LIST_HELP: &str = "Examples:\n  klms courses list\n  klms --json courses list";

#[derive(Debug, Parser)]
#[command(
    name = "klms",
    version,
    about = "Fast, agent-friendly access to KAIST KLMS",
    long_about = "Read KAIST KLMS directly over authenticated HTTP. Human output is the default; --json emits one stable document for agents and scripts.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Emit one stable JSON document.
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
    /// Preview a same-origin text response (experimental repair hatch).
    Request(RequestArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    /// Maximum number of rows to return.
    #[arg(long, default_value_t = 100, value_parser = parse_list_limit)]
    pub limit: usize,
}

#[derive(Debug, Clone, Args)]
pub struct AgendaArgs {
    /// Restrict results to one course id, code, or title.
    #[arg(long)]
    pub course: Option<String>,
    #[command(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Clone, Args)]
pub struct UpcomingArgs {
    /// Include today through this many days ahead (for example, 7d).
    #[arg(long, default_value = "7d", value_parser = parse_days)]
    pub through: u32,
    /// Restrict results to one course id, code, or title.
    #[arg(long)]
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
    /// Report the selected storage-state source and cookie metadata.
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
        query: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show one course by numeric id, code, or unambiguous title fragment.
    #[command(
        after_help = "Examples:\n  klms courses show 189705\n  klms --json courses show CS.30200"
    )]
    Show { course: String },
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
        #[arg(long)]
        course: String,
        #[arg(long)]
        week: Option<u32>,
        #[arg(long)]
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
    /// List modules in a course.
    #[command(
        after_help = "Examples:\n  klms assignments list --course CS.30200\n  klms quizzes list --course 189705\n  klms videos list --course 189705"
    )]
    List {
        #[arg(long)]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a module by numeric id or same-origin KLMS URL.
    #[command(
        after_help = "Examples:\n  klms assignments show 1210516\n  klms quizzes show 1210517\n  klms videos show 'https://klms.kaist.ac.kr/mod/lti/view.php?id=1265520'"
    )]
    Show { target: String },
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
        #[arg(long)]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a notice by the reference returned from `notices list`.
    #[command(after_help = "Example:\n  klms notices show board-post:1189554:420856")]
    Show { notice: String },
}

#[derive(Debug, Subcommand)]
pub enum BoardsCommand {
    /// List course boards.
    #[command(after_help = "Example:\n  klms --json boards list --course CS.30200")]
    List {
        #[arg(long)]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// List posts in a board by module id or URL.
    #[command(after_help = "Example:\n  klms --json boards posts 1265521 --limit 50")]
    Posts {
        board: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a post by same-origin article URL.
    #[command(
        after_help = "Example:\n  klms --json boards show 'https://klms.kaist.ac.kr/mod/courseboard/article.php?id=1265521&bwid=42'"
    )]
    Show { post: String },
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
        #[arg(long)]
        course: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Download a same-origin KLMS file without overwriting.
    #[command(
        after_help = "Example:\n  klms files download 'https://klms.kaist.ac.kr/pluginfile.php/...' --out ./notes.pdf"
    )]
    Download {
        url: String,
        #[arg(long)]
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
        #[arg(long)]
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
        path: String,
        #[arg(long, default_value_t = 65_536, value_parser = parse_preview_limit)]
        max_bytes: usize,
    },
}

fn parse_list_limit(value: &str) -> Result<usize, String> {
    parse_bounded(value, 1, 1_000, "limit")
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
