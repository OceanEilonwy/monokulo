//! The declared settings as command-line options, with `clap`: one option
//! for every setting that accepts the command line, `--` and its key with
//! `-` for `.` and `_` (`--payment-reorg-check-depth`), under a heading per
//! key prefix, with the setting's own description, environment variable and
//! default as its help. So `--help` always says what the registry says.
//!
//! A value is checked by its setting's own rules as it is parsed: a bad one
//! stops the process with clap's message naming the option. The values
//! reach the registry through [`Env::with_cli`](crate::Env::with_cli).
//!
//! Every process also gets `--options <PATH>`, the options file to read
//! (otherwise `~/.config/monokulo/<file>`, see [`crate::paths`]), and
//! `--init`, which writes a commented options file there and exits.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::setting::{cli_flag, AnySetting};
use crate::value::SettingKind;

/// The heading the options file's own options are listed under.
const OPTIONS_FILE: &str = "Options file";

/// `command` with `--options` and `--init`, an option for every setting in
/// `declared` that accepts the command line, and, after them, the
/// environment variables of those that don't (secrets: the process list is
/// readable by every user on the machine), so the help names every way to
/// configure the process. What `command` already had after its help
/// (examples) comes last. `file` names the options file (`engine.toml`).
pub fn with_settings(
    command: Command,
    declared: &[&'static dyn AnySetting],
    file: &str,
) -> Command {
    let default = default_options_path(file);
    let mut command = command
        .arg(
            Arg::new("options")
                .long("options")
                .value_name("PATH")
                .value_parser(clap::value_parser!(PathBuf))
                .help_heading(OPTIONS_FILE)
                .help(format!(
                    "Read settings from this TOML file [default: {}]",
                    default.display()
                ))
                .long_help(format!(
                    "Read settings from this TOML file. The admin settings page saves to it, and reads it again when Reload is pressed. A command-line option wins over it.\n[default: {}]",
                    default.display()
                )),
        )
        .arg(
            Arg::new("init")
                .long("init")
                .action(ArgAction::SetTrue)
                .help_heading(OPTIONS_FILE)
                .help("Write an options file listing every setting with its default, commented out, then exit")
                .long_help("Write an options file listing every setting with what it is for and its default, commented out, then exit. It goes where --options says, or the default place, and is never written over one that exists."),
        );
    for setting in declared.iter().filter(|s| s.sources().cli) {
        command = command.arg(arg(*setting));
    }
    let env_only: Vec<&&'static dyn AnySetting> = declared
        .iter()
        .filter(|s| s.sources().env && !s.sources().cli)
        .collect();
    if env_only.is_empty() {
        return command;
    }
    let after = command.get_after_help().map(|text| text.to_string());
    let after_long = command
        .get_after_long_help()
        .map(|text| text.to_string())
        .or_else(|| after.clone());
    let list = |long: bool| {
        let mut text = String::from("Environment variables (no option: the process list is readable by every user on the machine):\n");
        for setting in &env_only {
            let required = if setting.required() {
                " [required]"
            } else {
                ""
            };
            let description = if long {
                setting.description().to_string()
            } else {
                first_sentence(setting.description())
            };
            text.push_str(&format!(
                "  {}{required}\n          {description}\n",
                setting.env_var()
            ));
        }
        text
    };
    let join = |list: String, rest: Option<String>| match rest {
        Some(rest) => format!("{list}\n{rest}"),
        None => list,
    };
    command
        .after_help(join(list(false), after))
        .after_long_help(join(list(true), after_long))
}

/// Where the options file is when `--options` doesn't say:
/// `~/.config/monokulo/<file>` (XDG), or `<file>` in the working directory
/// if that can't be used.
pub fn default_options_path(file: &str) -> PathBuf {
    crate::paths::usable_or_cwd(crate::paths::config_file(file), file)
}

/// What the command line asks of a process before it starts: the settings
/// it gave (over the environment), the options file to read, and whether
/// to write that file instead (`--init`).
#[derive(Debug)]
pub struct Start {
    pub env: crate::Env,
    pub options: PathBuf,
    pub init: bool,
}

/// [`Start`] from parsed arguments.
pub fn start(matches: &ArgMatches, declared: &[&'static dyn AnySetting], file: &str) -> Start {
    Start {
        env: crate::Env::process().with_cli(values(matches, declared)),
        options: matches
            .get_one::<PathBuf>("options")
            .cloned()
            .unwrap_or_else(|| default_options_path(file)),
        init: matches.get_flag("init"),
    }
}

/// `--init`: writes the commented options file for `program` at `path`,
/// refusing to replace one, and says where it went (on standard output,
/// for the person who asked).
pub fn init(
    program: &str,
    path: &std::path::Path,
    declared: &[&'static dyn AnySetting],
) -> Result<(), String> {
    crate::write_init(path, &crate::render_init(program, declared))?;
    println!("Wrote {}", path.display());
    Ok(())
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
    if setting.sources().toml {
        notes.push(format!("[options file: {}]", setting.key()));
    } else {
        notes.push("[command line only]".to_string());
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
