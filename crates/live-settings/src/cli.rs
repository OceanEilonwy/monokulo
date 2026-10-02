//! The declared settings as command-line options, with `clap`: one option
//! for every setting that accepts the command line, `--` and its key with
//! `-` for `.` and `_` (`--payment-reorg-check-depth`), under a heading per
//! key prefix, with the setting's own description, environment variable and
//! default as its help. So `--help` always says what the registry says.
//!
//! A value is checked by its setting's own rules as it is parsed: a bad one
//! stops the process with clap's message naming the option, rather than
//! falling back to the default the way an environment or stored value does.
//! The values reach the registry through [`Env::with_cli`](crate::Env::with_cli).

use std::collections::HashMap;

use clap::{Arg, ArgMatches, Command};

use crate::setting::{cli_flag, AnySetting};
use crate::value::SettingKind;

/// `command` with an option for every setting in `declared` that accepts the
/// command line.
pub fn with_settings(mut command: Command, declared: &[&'static dyn AnySetting]) -> Command {
    for setting in declared.iter().filter(|s| s.sources().cli) {
        command = command.arg(arg(*setting));
    }
    command
}

/// The values given on the command line, by setting key, in their stored
/// form: what [`Env::with_cli`](crate::Env::with_cli) takes.
pub fn values(
    matches: &ArgMatches,
    declared: &[&'static dyn AnySetting],
) -> HashMap<String, String> {
    declared
        .iter()
        .filter(|s| s.sources().cli)
        .filter_map(|s| {
            matches
                .get_one::<String>(s.key())
                .map(|value| (s.key().to_string(), value.clone()))
        })
        .collect()
}

fn arg(setting: &'static dyn AnySetting) -> Arg {
    let description = setting.description();
    let mut notes = Vec::new();
    if setting.sources().env {
        notes.push(format!("[env: {}]", setting.env_var()));
    }
    if setting.required() {
        notes.push("[required]".to_string());
    } else {
        let default = setting.default_shown();
        notes.push(if default.is_empty() {
            "[default: none]".to_string()
        } else {
            format!("[default: {default}]")
        });
    }
    if !setting.sources().database {
        notes.push("[not saved: give it at every start]".to_string());
    }
    if setting.kind() == SettingKind::Secret && setting.sources().env {
        // The argument list is readable by every user on the machine
        // (`ps`) and kept by the shell's history.
        notes.push(format!(
            "[an option is visible to other users on this machine: prefer {}]",
            setting.env_var()
        ));
    }
    Arg::new(setting.key())
        .long(cli_flag(setting.key()))
        .value_name(value_name(&setting.kind()))
        .help(first_sentence(description))
        .long_help(format!("{description}\n{}", notes.join("\n")))
        .help_heading(heading(setting.key()))
        .num_args(1)
        .value_parser(move |raw: &str| setting.normalise(raw))
}

/// What goes in the usage line for a value of `kind`.
fn value_name(kind: &SettingKind) -> String {
    match kind {
        SettingKind::Integer { .. } => "NUMBER".to_string(),
        SettingKind::Bool => "true|false".to_string(),
        SettingKind::Choice { choices } => choices.join("|"),
        SettingKind::ChoiceList { .. } => "LIST".to_string(),
        SettingKind::Url => "URL".to_string(),
        SettingKind::Address => "ADDRESS".to_string(),
        SettingKind::Path => "PATH".to_string(),
        SettingKind::Text => "TEXT".to_string(),
        SettingKind::Secret => "SECRET".to_string(),
        SettingKind::Json => "JSON".to_string(),
    }
}

/// The heading a setting's option is listed under: its key's prefix,
/// `key_custody.default_backend` under "Key custody settings".
fn heading(key: &str) -> String {
    let prefix = key.split('.').next().unwrap_or(key).replace('_', " ");
    let mut chars = prefix.chars();
    let title: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    format!("{title} settings")
}

/// The description's first sentence, for `-h`; `--help` shows it all.
fn first_sentence(text: &str) -> String {
    match text.find(". ") {
        Some(end) => text[..=end].to_string(),
        None => text.to_string(),
    }
}
