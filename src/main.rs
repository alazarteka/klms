mod auth;
mod cli;
mod client;
mod commands;
mod corpus;
mod date;
mod error;
mod models;
mod output;
mod parse;
mod present;
mod reference;
mod safe_url;
mod skill;
mod spec;
mod update;

use std::process::ExitCode;

use clap::Parser;

use crate::{cli::Cli, error::AppError};

fn main() -> ExitCode {
    let json_requested = std::env::args_os()
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if json_requested
                && matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                let (command, data) = match error.kind() {
                    clap::error::ErrorKind::DisplayVersion => (
                        "version",
                        serde_json::json!({"name": "klms", "version": env!("CARGO_PKG_VERSION")}),
                    ),
                    _ => ("help", serde_json::json!({"text": error.to_string()})),
                };
                let result = output::result(command, &data, error.to_string())
                    .expect("informational output is serializable");
                output::print_success(&result, true);
                return ExitCode::SUCCESS;
            }
            if json_requested {
                let app_error = AppError::usage(error.to_string());
                output::print_error(&app_error, true);
                return ExitCode::from(app_error.exit_code());
            }
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            return ExitCode::from(code);
        }
    };

    match commands::run(&cli) {
        Ok(result) => {
            output::print_success(&result, cli.json);
            ExitCode::SUCCESS
        }
        Err(error) => {
            output::print_error(&error, cli.json);
            ExitCode::from(error.exit_code())
        }
    }
}
