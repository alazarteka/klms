//! Executable command grammar derived from the Clap declaration.
//!
//! `klms spec` prints one grammar line per leaf command; `--json` emits the
//! full argument tree so agents can discover the interface without parsing
//! `--help`. `klms completions` reuses the same Clap tree to emit shell
//! completion scripts.

use clap::{Arg, ArgAction, ArgGroup, Command, CommandFactory};
use serde::Serialize;
use serde_json::json;

use crate::{cli::Cli, error::AppError, output::CommandResult};

#[derive(Debug, Serialize)]
struct Spec {
    name: String,
    version: String,
    /// Top-level long help: workflow, JSON/exit-code contract, ref formats.
    overview: Option<String>,
    global_args: Vec<ArgSpec>,
    commands: Vec<CommandSpec>,
}

#[derive(Debug, Serialize)]
struct CommandSpec {
    path: Vec<String>,
    usage: String,
    about: Option<String>,
    /// Extended description and examples (the command's `--help` footer).
    details: Option<String>,
    args: Vec<ArgSpec>,
    groups: Vec<GroupSpec>,
}

/// An explicit argument group: `required` means at least one member must be
/// given; `multiple` false means at most one may be.
#[derive(Debug, Serialize)]
struct GroupSpec {
    name: String,
    args: Vec<String>,
    required: bool,
    multiple: bool,
}

#[derive(Debug, Serialize)]
struct ArgSpec {
    name: String,
    kind: &'static str,
    required: bool,
    requires: Vec<String>,
    conflicts: Vec<String>,
    value: Option<String>,
    choices: Vec<String>,
    default: Option<String>,
    help: Option<String>,
}

pub fn run() -> Result<CommandResult, AppError> {
    let spec = build();
    crate::output::result("spec", &spec, grammar(&spec))
}

pub fn completions(shell: clap_complete::Shell) -> Result<CommandResult, AppError> {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "klms", &mut script);
    let script = String::from_utf8(script)
        .map_err(|error| AppError::internal(format!("completion script is not UTF-8: {error}")))?;
    let data = json!({"shell": shell.to_string(), "script": script});
    crate::output::result("completions", &data, script.clone())
}

fn build() -> Spec {
    let mut root = Cli::command();
    root.build();
    let mut commands = Vec::new();
    collect(&root, Vec::new(), &mut commands);
    Spec {
        name: root.get_name().to_owned(),
        version: root.get_version().unwrap_or_default().to_owned(),
        overview: root.get_after_long_help().map(ToString::to_string),
        global_args: (root.get_arguments())
            .filter(|arg| arg.is_global_set() && visible(arg))
            .map(|arg| arg_spec(&root, arg))
            .collect(),
        commands,
    }
}

fn grammar(spec: &Spec) -> String {
    let usages: Vec<_> = spec.commands.iter().map(|c| c.usage.as_str()).collect();
    usages.join("\n")
}

fn collect(command: &Command, path: Vec<String>, out: &mut Vec<CommandSpec>) {
    let mut subcommands = command.get_subcommands();
    if !subcommands.any(|sub| !sub.is_hide_set() && sub.get_name() != "help") {
        let args: Vec<ArgSpec> = (command.get_arguments())
            .filter(|arg| !arg.is_global_set() && visible(arg))
            .map(|arg| arg_spec(command, arg))
            .collect();
        // Clap derive adds one unconstrained group per flattened struct;
        // only groups that require or exclude something are contractual.
        let groups: Vec<GroupSpec> = (command.get_groups())
            .map(|group| group_spec(command, group))
            .filter(|group| !group.args.is_empty() && (group.required || !group.multiple))
            .collect();
        let parts: Vec<_> = [
            command.get_long_about(),
            command.get_after_long_help().or(command.get_after_help()),
        ]
        .into_iter()
        .flatten()
        .map(ToString::to_string)
        .collect();
        out.push(CommandSpec {
            usage: usage(&path, &args, &groups),
            path,
            about: command.get_about().map(ToString::to_string),
            details: (!parts.is_empty()).then(|| parts.join("\n\n")),
            args,
            groups,
        });
        return;
    }
    for sub in command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
    {
        collect(
            sub,
            [path.clone(), vec![sub.get_name().to_owned()]].concat(),
            out,
        );
    }
}

fn visible(arg: &Arg) -> bool {
    !arg.is_hide_set() && !matches!(arg.get_id().as_str(), "help" | "version")
}

fn arg_spec(command: &Command, arg: &Arg) -> ArgSpec {
    let choices: Vec<String> = (arg.get_possible_values().iter())
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect();
    // Clap fills an unset value name with the upper-cased id at build time;
    // treat that placeholder as unset so enumerated choices show instead.
    let placeholder = arg.get_id().as_str().to_ascii_uppercase();
    let named = (arg.get_value_names().and_then(|names| names.first()))
        .map(ToString::to_string)
        .filter(|name| choices.is_empty() || *name != placeholder);
    let positional = arg.is_positional();
    let flag = matches!(
        arg.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    );
    let value = match (positional, flag) {
        (false, true) => None,
        _ => Some(named.unwrap_or_else(|| match choices.is_empty() {
            true => placeholder,
            false => choices.join("|"),
        })),
    };
    let name = match positional {
        true => value.clone().unwrap_or_default(),
        false => format!("--{}", arg.get_long().unwrap_or(arg.get_id().as_str())),
    };
    ArgSpec {
        name,
        kind: match (positional, flag) {
            (true, _) => "positional",
            (false, true) => "flag",
            _ => "option",
        },
        required: arg.is_required_set(),
        requires: requirements(command, arg),
        conflicts: command
            .get_arg_conflicts_with(arg)
            .into_iter()
            .filter(|other| visible(other))
            .map(argument_name)
            .collect(),
        value,
        choices,
        default: (arg.get_default_values().first())
            .map(|value| value.to_string_lossy().into_owned()),
        help: arg.get_help().map(ToString::to_string),
    }
}

fn argument_name(arg: &Arg) -> String {
    match arg.get_long() {
        Some(long) => format!("--{long}"),
        None => arg.get_id().to_string(),
    }
}

/// Clap has no public requires getter. Its usage renderer expands unconditional
/// requirements when the source argument is required, without parsing values.
fn requirements(command: &Command, arg: &Arg) -> Vec<String> {
    let mut probe = command.clone().mut_args(|other| other.required(false));
    for group in command.get_groups() {
        probe = probe.mut_group(group.get_id().as_str(), |group| group.required(false));
    }
    let usage = probe
        .mut_arg(arg.get_id().as_str(), |source| source.required(true))
        .render_usage()
        .to_string();
    command
        .get_arguments()
        .filter(|other| other.get_id() != arg.get_id() && visible(other))
        .filter(|other| other.get_long().is_some())
        .map(argument_name)
        .filter(|name| usage.split_whitespace().any(|word| word == name))
        .collect()
}

fn group_spec(command: &Command, group: &ArgGroup) -> GroupSpec {
    GroupSpec {
        name: group.get_id().to_string(),
        args: (group.get_args())
            .filter_map(|id| command.get_arguments().find(|arg| arg.get_id() == id))
            .filter(|arg| visible(arg))
            .map(argument_name)
            .collect(),
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
    let optional = |required: bool, text: String| if required { text } else { format!("[{text}]") };
    let mut words = vec!["klms".to_owned()];
    words.extend(path.iter().cloned());
    for arg in args.iter().filter(|arg| arg.kind == "positional") {
        words.push(optional(arg.required, arg.name.clone()));
    }
    let mut rendered_groups = Vec::new();
    let mut rendered_args = Vec::new();
    for arg in args.iter().filter(|arg| arg.kind != "positional") {
        if rendered_args.contains(&arg.name) {
            continue;
        }
        let exclusive = groups
            .iter()
            .find(|group| !group.multiple && group.args.contains(&arg.name));
        match exclusive {
            Some(group) if rendered_groups.contains(&group.name) => {}
            Some(group) => {
                rendered_groups.push(group.name.clone());
                let members: Vec<_> = (group.args.iter())
                    .filter_map(|name| args.iter().find(|arg| &arg.name == name))
                    .map(render)
                    .collect();
                let members = members.join("|");
                words.push(if group.required {
                    format!("({members})")
                } else {
                    format!("[{members}]")
                });
            }
            None => {
                let mut members = vec![render(arg)];
                rendered_args.push(arg.name.clone());
                for dependent in args
                    .iter()
                    .filter(|other| other.requires.contains(&arg.name))
                {
                    if rendered_args.contains(&dependent.name) {
                        continue;
                    }
                    let mutual = arg.requires.contains(&dependent.name);
                    members.push(optional(mutual || dependent.required, render(dependent)));
                    rendered_args.push(dependent.name.clone());
                }
                words.push(optional(arg.required, members.join(" ")));
            }
        }
    }
    words.join(" ")
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

    #[test]
    fn usage_renders_mutual_and_one_way_dependencies_together() {
        let spec = build();
        let courses = spec
            .commands
            .iter()
            .find(|c| c.path == ["courses", "list"])
            .unwrap();
        assert!(
            courses
                .usage
                .contains("[--year YEAR --semester spring|summer|fall|winter]")
        );
        assert!(!courses.usage.contains("[--semester"));
        let login = spec
            .commands
            .iter()
            .find(|c| c.path == ["auth", "login"])
            .unwrap();
        assert!(
            login
                .usage
                .contains("[--remember-password [--insecure-storage]]")
        );
    }
}
