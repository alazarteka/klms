use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "klms", version, about, arg_required_else_help = true)]
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

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check local configuration and, when possible, the live session.
    Doctor,
    /// Inspect authentication without exposing credentials.
    Auth(AuthArgs),
    /// Show the current dashboard summary.
    Dashboard,
    /// List or inspect courses.
    Courses(CoursesArgs),
    /// List activities in a course.
    Activities(ActivitiesArgs),
    /// Show the grade report for a course.
    Grades(GradesArgs),
    /// Show the attendance report for a course.
    Attendance(AttendanceArgs),
}

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Report the selected storage-state source and cookie metadata.
    Status,
}

#[derive(Debug, Args)]
pub struct CoursesArgs {
    #[command(subcommand)]
    pub command: CoursesCommand,
}

#[derive(Debug, Subcommand)]
pub enum CoursesCommand {
    /// List courses visible on the selected dashboard term.
    List,
    /// Show one course by numeric id, code, or unambiguous title fragment.
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
    List {
        #[arg(long)]
        course: String,
        #[arg(long)]
        week: Option<u32>,
    },
}

#[derive(Debug, Args)]
pub struct GradesArgs {
    #[command(subcommand)]
    pub command: GradesCommand,
}

#[derive(Debug, Subcommand)]
pub enum GradesCommand {
    /// Show the user grade report.
    Show {
        #[arg(long)]
        course: String,
    },
}

#[derive(Debug, Args)]
pub struct AttendanceArgs {
    #[command(subcommand)]
    pub command: AttendanceCommand,
}

#[derive(Debug, Subcommand)]
pub enum AttendanceCommand {
    /// Show the attendance report.
    Show {
        #[arg(long)]
        course: String,
    },
}
