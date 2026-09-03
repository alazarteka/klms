//! Executable command grammar derived from the Clap declaration.
//!
//! `klms spec` prints one grammar line per leaf command; `--json` emits the
//! full argument tree so agents can discover the interface without parsing
//! `--help`. `docs/COMMAND_CONTRACT.md` embeds the grammar and a contract test
//! keeps the two identical.

use clap::{Arg, ArgAction, ArgGroup, Command, CommandFactory};
use serde::Serialize;
use serde_json::json;

use crate::{cli::Cli, error::AppError, output::CommandResult};

#[derive(Debug, Serialize)]
pub struct Spec {
    pub name: String,
    pub version: String,
    pub global_args: Vec<ArgSpec>,
    pub commands: Vec<CommandSpec>,
}

#[derive(Debug, Serialize)]
pub struct CommandSpec {
    pub path: Vec<String>,
    pub usage: String,
    pub about: Option<String>,
    pub args: Vec<ArgSpec>,
    pub groups: Vec<GroupSpec>,
}

/// An explicit argument group: `required` means at least one member must be
/// given; `multiple` false means at most one may be.
#[derive(Debug, Serialize)]
pub struct GroupSpec {
    pub name: String,
    pub args: Vec<String>,
    pub required: bool,
    pub multiple: bool,
}

#[derive(Debug, Serialize)]
pub struct ArgSpec {
    pub name: String,
    pub kind: &'static str,
    pub required: bool,
    pub value: Option<String>,
    pub choices: Vec<String>,
    pub default: Option<String>,
    pub help: Option<String>,
}

pub fn run() -> Result<CommandResult, AppError> {
    let spec = build();
    let grammar = grammar(&spec);
    crate::output::result("spec", &spec, grammar)
}

pub fn completions(shell: clap_complete::Shell) -> Result<CommandResult, AppError> {
    let mut command = Cli::command();
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut command, "klms", &mut script);
    let script = String::from_utf8(script)
        .map_err(|error| AppError::internal(format!("completion script is not UTF-8: {error}")))?;
    let data = json!({"shell": shell.to_string(), "script": script});
    crate::output::result("completions", &data, script.clone())
}

pub fn build() -> Spec {
    let mut root = Cli::command();
    root.build();
    let global_args = root
        .get_arguments()
        .filter(|arg| arg.is_global_set() && visible(arg))
        .map(arg_spec)
        .collect();
    let mut commands = Vec::new();
    collect(&root, Vec::new(), &mut commands);
    Spec {
        name: root.get_name().to_owned(),
        version: root.get_version().unwrap_or_default().to_owned(),
        global_args,
        commands,
    }
}

pub fn grammar(spec: &Spec) -> String {
    spec.commands
        .iter()
        .map(|command| command.usage.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect(command: &Command, path: Vec<String>, out: &mut Vec<CommandSpec>) {
    let subcommands: Vec<_> = command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
        .collect();
    if subcommands.is_empty() {
        let args: Vec<ArgSpec> = command
            .get_arguments()
            .filter(|arg| !arg.is_global_set() && visible(arg))
            .map(arg_spec)
            .collect();
        // Clap derive adds one unconstrained group per flattened struct;
        // only groups that require or exclude something are contractual.
        let groups: Vec<GroupSpec> = command
            .get_groups()
            .map(|group| group_spec(command, group))
            .filter(|group| !group.args.is_empty() && (group.required || !group.multiple))
            .collect();
        out.push(CommandSpec {
            usage: usage(&path, &args, &groups),
            path,
            about: command.get_about().map(ToString::to_string),
            args,
            groups,
        });
        return;
    }
    for sub in subcommands {
        let mut sub_path = path.clone();
        sub_path.push(sub.get_name().to_owned());
        collect(sub, sub_path, out);
    }
}

fn visible(arg: &Arg) -> bool {
    !arg.is_hide_set() && !matches!(arg.get_id().as_str(), "help" | "version")
}

fn arg_spec(arg: &Arg) -> ArgSpec {
    let choices: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect();
    // Clap fills an unset value name with the upper-cased id at build time;
    // treat that placeholder as unset so enumerated choices show instead.
    let placeholder = arg.get_id().as_str().to_ascii_uppercase();
    let named_value = arg
        .get_value_names()
        .and_then(|names| names.first())
        .map(ToString::to_string)
        .filter(|name| choices.is_empty() || *name != placeholder);
    let kind = if arg.is_positional() {
        "positional"
    } else if matches!(
        arg.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    ) {
        "flag"
    } else {
        "option"
    };
    let value = match kind {
        "flag" => None,
        _ if named_value.is_some() => named_value,
        _ if !choices.is_empty() => Some(choices.join("|")),
        _ => Some(arg.get_id().as_str().to_ascii_uppercase()),
    };
    let name = if arg.is_positional() {
        value.clone().unwrap_or_default()
    } else {
        format!("--{}", arg.get_long().unwrap_or(arg.get_id().as_str()))
    };
    ArgSpec {
        name,
        kind,
        required: arg.is_required_set(),
        value,
        choices,
        default: arg
            .get_default_values()
            .first()
            .map(|value| value.to_string_lossy().into_owned()),
        help: arg.get_help().map(ToString::to_string),
    }
}

fn group_spec(command: &Command, group: &ArgGroup) -> GroupSpec {
    let args = group
        .get_args()
        .filter_map(|id| command.get_arguments().find(|arg| arg.get_id() == id))
        .filter(|arg| visible(arg))
        .map(|arg| arg_spec(arg).name)
        .collect();
    GroupSpec {
        name: group.get_id().to_string(),
        args,
        required: group.is_required_set(),
        multiple: group.clone().is_multiple(),
    }
}

fn render(arg: &ArgSpec) -> String {
    match &arg.value {
        Some(value) if arg.kind != "positional" => format!("{} {value}", arg.name),
        _ => arg.name.clone(),
    }
}

/// Positionals first, then options in declaration order. Members of an
/// exclusive group (`multiple` false) render once, at the first member's
/// position, as `(a|b)` when required or `[a|b]` otherwise.
fn usage(path: &[String], args: &[ArgSpec], groups: &[GroupSpec]) -> String {
    let mut words = vec!["klms".to_owned()];
    words.extend(path.iter().cloned());
    for arg in args.iter().filter(|arg| arg.kind == "positional") {
        words.push(optional(arg.required, arg.name.clone()));
    }
    let mut rendered_groups = Vec::new();
    for arg in args.iter().filter(|arg| arg.kind != "positional") {
        let exclusive = groups
            .iter()
            .find(|group| !group.multiple && group.args.contains(&arg.name));
        match exclusive {
            Some(group) if rendered_groups.contains(&group.name) => {}
            Some(group) => {
                rendered_groups.push(group.name.clone());
                let members = group
                    .args
                    .iter()
                    .filter_map(|name| args.iter().find(|arg| &arg.name == name))
                    .map(render)
                    .collect::<Vec<_>>()
                    .join("|");
                words.push(if group.required {
                    format!("({members})")
                } else {
                    format!("[{members}]")
                });
            }
            None => words.push(optional(arg.required, render(arg))),
        }
    }
    words.join(" ")
}

fn optional(required: bool, text: String) -> String {
    if required { text } else { format!("[{text}]") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_lists_every_leaf_command_once_without_globals() {
        let spec = build();
        let paths: Vec<String> = spec.commands.iter().map(|c| c.path.join(" ")).collect();
        assert!(paths.contains(&"library edit".to_owned()));
        assert!(paths.contains(&"spec".to_owned()));
        let unique: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len());
        assert!(spec.global_args.iter().any(|arg| arg.name == "--json"));
        assert!(!grammar(&spec).contains("--json"));
        assert!(!grammar(&spec).contains("--base-url"));
    }

    #[test]
    fn usage_marks_required_and_optional_arguments() {
        let spec = build();
        let edit = spec
            .commands
            .iter()
            .find(|c| c.path == ["library", "edit"])
            .unwrap();
        assert_eq!(
            edit.usage,
            "klms library edit REF --field title|filename|summary|note|tag \
             (--value TEXT|--value-file PATH) [--actor ACTOR] --expected-revision N"
        );
        let group = &edit.groups[0];
        assert_eq!(group.args, ["--value", "--value-file"]);
        assert!(group.required && !group.multiple);
    }
}
