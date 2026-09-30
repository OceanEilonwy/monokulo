//! The interactive prompt's line editor ([`reedline`]): history with
//! search, word-wise movement and selection, a Tab completion menu that
//! explains each candidate, grey hints (from history, or the command's
//! usage), and highlighting of the command word.
//!
//! What completes is read from the clap command definitions, so it can
//! never drift from what the parser accepts, plus the open wallet's own
//! data (txids, key images, address book, accounts) - refreshed after every
//! command by [`CompletionData::refresh`].

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Subcommand;
use cli_wallet::amount::{format_amount, Unit};
use cli_wallet::WalletKeys;
use reedline::{
    default_emacs_keybindings, DescriptionMode, EditCommand, Emacs, FileBackedHistory, Hinter, History, IdeMenu, KeyCode,
    KeyModifiers, Keybindings, MenuBuilder, MouseClickMode, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline,
    ReedlineEvent, ReedlineMenu, SearchQuery, Span, StyledText, Suggestion,
};
use nu_ansi_term::{Color as AnsiColor, Style};
use reedline::Color;

use crate::args::PRIORITY_NAMES;
use crate::commands::Command;

/// One command as the editor knows it: its name, one-line description, and
/// usage with the name itself left off.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub name: String,
    pub about: String,
    pub usage: String,
}

/// Every command the prompt accepts, straight from clap, plus the prompt's
/// own `help` and `exit`.
pub fn command_specs() -> Vec<CommandSpec> {
    let root = Command::augment_subcommands(clap::Command::new("prompt"));
    let mut specs: Vec<CommandSpec> = root
        .get_subcommands()
        .map(|sub| {
            let name = sub.get_name().to_string();
            let usage = sub.clone().render_usage().to_string();
            let usage = usage.trim_start_matches("Usage:").trim();
            // Multi-line usages (account) repeat the name on each line.
            let usage = usage.lines().map(|line| line.trim().trim_start_matches(name.as_str()).trim()).collect::<Vec<_>>().join(" | ");
            CommandSpec { about: sub.get_about().map(|about| about.to_string()).unwrap_or_default(), name, usage }
        })
        .collect();
    specs.push(CommandSpec { name: "help".to_string(), about: "List commands, or show one command's usage.".to_string(), usage: "[<command>]".to_string() });
    specs.push(CommandSpec { name: "exit".to_string(), about: "Close the wallet.".to_string(), usage: String::new() });
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    specs
}

/// What the open wallet can offer as completions - refreshed after every
/// command, so a new txid or address book entry completes straight away.
#[derive(Debug, Clone, Default)]
pub struct CompletionData {
    /// `(txid, description)`.
    pub txids: Vec<(String, String)>,
    /// `(key image, description)`, unspent outputs only.
    pub key_images: Vec<(String, String)>,
    /// `(global index spec "0/<n>", description)`.
    pub outputs: Vec<(String, String)>,
    /// `(address, description)`: address book entries, then this wallet's
    /// own addresses.
    pub addresses: Vec<(String, String)>,
    /// `(index, label)`.
    pub accounts: Vec<(String, String)>,
    pub account_tags: Vec<String>,
    pub address_book_rows: Vec<(String, String)>,
    /// For the right-hand prompt.
    pub current_account: String,
}

impl CompletionData {
    /// Reads the wallet file again. A failure (the file mid-rewrite, say)
    /// just keeps the previous data - completion is a convenience.
    pub fn refresh(&mut self, keys: &WalletKeys) {
        let Ok(file) = keys.load() else { return };
        let data = file.data;
        let unit = data.meta.settings.unit;
        let money = |amount: u64| format!("{} {}", format_amount(amount, unit), short_unit(unit));
        let outputs = keys.outputs(&data).unwrap_or_default();

        let mut txids: Vec<(String, String)> = Vec::new();
        for sent in data.sent.iter().rev() {
            let amount: u64 = sent.destinations.iter().map(|d| d.amount_piconero).sum();
            txids.push((sent.txid.clone(), format!("out {}", money(amount))));
        }
        for output in outputs.iter().rev() {
            if !txids.iter().any(|(txid, _)| *txid == output.txid) {
                txids.push((output.txid.clone(), format!("in {} at {}", money(output.amount()), output.height)));
            }
        }
        for pending in &data.pending {
            if !txids.iter().any(|(txid, _)| *txid == pending.txid) {
                txids.push((pending.txid.clone(), "pending".to_string()));
            }
        }
        self.txids = txids;

        self.key_images = outputs
            .iter()
            .filter(|o| !o.spent)
            .map(|o| (hex::encode(o.key_image), format!("{}{}", money(o.amount()), if o.frozen { ", frozen" } else { "" })))
            .collect();
        self.outputs = outputs
            .iter()
            .map(|o| (format!("0/{}", o.global_index()), format!("{}{}", money(o.amount()), if o.spent { ", spent" } else { "" })))
            .collect();

        let accounts = data.meta.accounts();
        let current = data.meta.current_account;
        let mut addresses: Vec<(String, String)> =
            data.meta.address_book.iter().map(|entry| (entry.address.clone(), format!("address book: {}", entry.description))).collect();
        if let Some(account) = accounts.get(current as usize) {
            for (index, label) in account.subaddress_labels.iter().enumerate() {
                addresses.push((keys.subaddress(current, index as u32), format!("own address {index}: {label}")));
            }
        }
        self.addresses = addresses;
        self.accounts = accounts.iter().enumerate().map(|(index, account)| (index.to_string(), account.label.clone())).collect();
        let mut tags: Vec<String> = accounts.iter().filter_map(|account| account.tag.clone()).collect();
        tags.dedup();
        self.account_tags = tags;
        self.address_book_rows =
            data.meta.address_book.iter().enumerate().map(|(index, entry)| (index.to_string(), format!("{} {}", &entry.address[..entry.address.len().min(12)], entry.description))).collect();
        self.current_account = format!("account {current}: {}", accounts.get(current as usize).map(|a| a.label.as_str()).unwrap_or(""));
    }
}

fn short_unit(unit: Unit) -> &'static str {
    match unit {
        Unit::Monero => "XMR",
        other => other.name(),
    }
}

/// The word being completed: its start in the line, and the words before
/// it.
fn current_word(line: &str, pos: usize) -> (usize, Vec<&str>, &str) {
    let before = &line[..pos];
    let start = before.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let previous: Vec<&str> = before[..start].split_whitespace().collect();
    (start, previous, &before[start..])
}

fn words(list: &[&str]) -> Vec<(String, String)> {
    list.iter().map(|word| (word.to_string(), String::new())).collect()
}

fn described(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(word, description)| (word.to_string(), description.to_string())).collect()
}

const HISTORY_FILTERS: [(&str, &str); 7] = [
    ("in", "incoming transfers"),
    ("out", "outgoing transfers"),
    ("all", "everything"),
    ("pending", "sent, not yet confirmed"),
    ("pool", "expected, not yet confirmed"),
    ("failed", "always empty here"),
    ("coinbase", "always empty here"),
];

/// What completes after `previous` words - `(candidate, description)`.
pub fn candidates(specs: &[CommandSpec], data: &CompletionData, previous: &[&str]) -> Vec<(String, String)> {
    let Some(&command) = previous.first() else {
        return specs.iter().map(|spec| (spec.name.clone(), spec.about.clone())).collect();
    };
    let args = &previous[1..];
    let priorities = || words(&PRIORITY_NAMES);
    match (command, args) {
        ("help", []) => specs.iter().map(|spec| (spec.name.clone(), spec.about.clone())).collect(),
        ("balance", []) => described(&[("detail", "per-address balances")]),
        ("account", []) => described(&[
            ("new", "<label>"),
            ("switch", "<index>"),
            ("label", "<index> <label>"),
            ("tag", "<tag> <index>..."),
            ("untag", "<index>..."),
            ("tag_description", "<tag> <description>"),
        ]),
        ("account", ["switch" | "label", ..]) if args.len() == 1 => data.accounts.clone(),
        ("account", ["untag", ..]) => data.accounts.clone(),
        ("account", ["tag", _, ..]) => data.accounts.clone(),
        ("account", ["tag" | "tag_description"]) => data.account_tags.iter().map(|tag| (tag.clone(), String::new())).collect(),
        ("address", []) => described(&[
            ("all", "every address of this account"),
            ("new", "<label>"),
            ("label", "<index> <label>"),
            ("one-off", "<account> <subaddress>"),
        ]),
        ("show_transfers" | "export_transfers", _) => described(&HISTORY_FILTERS),
        ("incoming_transfers", _) => described(&[("available", "unspent"), ("unavailable", "spent"), ("verbose", "with key images")]),
        ("set", []) => described(&[
            ("priority", "default fee priority"),
            ("unit", "display and input unit"),
            ("always-confirm-transfers", "ask before sending"),
            ("default-ring-size", "always 16"),
        ]),
        ("set", ["priority"]) => priorities(),
        ("set", ["unit"]) => words(&["monero", "millinero", "micronero", "nanonero", "piconero"]),
        ("set", ["always-confirm-transfers"]) => words(&["1", "0"]),
        ("set", ["default-ring-size"]) => words(&["16"]),
        ("transfer", _) => {
            // Priority only fits before the first address.
            let mut list = if args.iter().any(|arg| arg.starts_with('5') || arg.starts_with('7')) { vec![] } else { priorities() };
            list.extend(data.addresses.clone());
            list
        }
        ("sweep_all" | "sweep_account" | "sweep_below", _) => {
            let mut list = priorities();
            list.extend(data.addresses.clone());
            list
        }
        ("sweep_single", _) if !args.iter().any(|arg| arg.len() == 64) => {
            let mut list = priorities();
            list.extend(data.key_images.clone());
            list
        }
        ("sweep_single", _) => data.addresses.clone(),
        ("freeze" | "thaw" | "frozen", []) => data.key_images.clone(),
        ("show_transfer" | "get_tx_note" | "set_tx_note", []) => data.txids.clone(),
        ("mark_output_spent" | "mark_output_unspent" | "is_output_spent", []) => data.outputs.clone(),
        ("address_book", []) => described(&[("add", "<address> [<description>]"), ("delete", "<index>")]),
        ("address_book", ["delete"]) => data.address_book_rows.clone(),
        ("address_book", ["add"]) => data.addresses.iter().filter(|(_, description)| !description.starts_with("address book")).cloned().collect(),
        _ => vec![],
    }
}

pub struct WalletCompleter {
    specs: Vec<CommandSpec>,
    data: Arc<Mutex<CompletionData>>,
}

impl reedline::Completer for WalletCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let (start, previous, partial) = current_word(line, pos);
        let data = self.data.lock().expect("completion data lock").clone();
        candidates(&self.specs, &data, &previous)
            .into_iter()
            .filter(|(value, _)| value.starts_with(partial) && value != partial)
            .map(|(value, description)| Suggestion {
                value,
                description: Some(description).filter(|d| !d.is_empty()),
                span: Span::new(start, pos),
                append_whitespace: true,
                ..Suggestion::default()
            })
            .collect()
    }
}

/// Grey text after the cursor: the rest of a matching history entry
/// (accepted with →), else the rest of a command name only one command
/// starts with (also accepted), else - once a command is typed - its usage,
/// shown for reference only.
pub struct WalletHinter {
    specs: Vec<CommandSpec>,
    /// What → would accept; empty for a usage hint.
    acceptable: String,
}

impl WalletHinter {
    /// `(hint text, whether → accepts it)` for `line` with no history match.
    pub fn hint_without_history(specs: &[CommandSpec], line: &str) -> (String, bool) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return (String::new(), false);
        }
        match trimmed.split_once(char::is_whitespace) {
            None => {
                let matching: Vec<&CommandSpec> = specs.iter().filter(|spec| spec.name.starts_with(trimmed)).collect();
                match matching.as_slice() {
                    [only] if only.name != trimmed => (only.name[trimmed.len()..].to_string(), true),
                    [only] if !only.usage.is_empty() => (format!("  {}", only.usage), false),
                    _ => (String::new(), false),
                }
            }
            Some((command, rest)) => match specs.iter().find(|spec| spec.name == command) {
                Some(spec) if !spec.usage.is_empty() => {
                    let usage = matching_forms(&spec.usage, rest, line.ends_with(char::is_whitespace));
                    let forms: Vec<String> = usage.iter().map(|form| format!("{command} {form}")).collect();
                    (format!("{}  {}", if line.ends_with(' ') { "" } else { " " }, forms.join(" | ")), false)
                }
                _ => (String::new(), false),
            },
        }
    }
}

/// The forms of a `form | form | ...` usage that fit the words typed so
/// far: each typed word must equal the form's literal word at that
/// position, or sit where the form has a placeholder (`<...>`, `[...]`,
/// `(...)`). The word still being typed only has to be a prefix. All forms
/// if none fit.
fn matching_forms<'a>(usage: &'a str, typed: &str, last_word_complete: bool) -> Vec<&'a str> {
    let forms: Vec<&str> = usage.split(" | ").collect();
    let typed: Vec<&str> = typed.split_whitespace().collect();
    let fits = |form: &&str| {
        let tokens: Vec<&str> = form.split_whitespace().collect();
        typed.iter().enumerate().all(|(i, word)| match tokens.get(i) {
            Some(token) if token.starts_with(['<', '[', '(']) => true,
            Some(token) if i + 1 == typed.len() && !last_word_complete => token.starts_with(word),
            Some(token) => token == word,
            None => false,
        })
    };
    let matching: Vec<&str> = forms.iter().copied().filter(fits).collect();
    if matching.is_empty() {
        forms
    } else {
        matching
    }
}

impl Hinter for WalletHinter {
    fn handle(&mut self, line: &str, pos: usize, history: &dyn History, use_ansi_coloring: bool, _cwd: &str) -> String {
        self.acceptable.clear();
        if pos != line.len() || line.is_empty() {
            return String::new();
        }
        let from_history = history
            .search(SearchQuery::last_with_prefix(line.to_string(), history.session()))
            .ok()
            .and_then(|entries| entries.into_iter().next())
            .and_then(|entry| entry.command_line.get(line.len()..).map(str::to_string))
            .filter(|rest| !rest.is_empty());
        let (hint, acceptable) = match from_history {
            Some(rest) => (rest, true),
            None => Self::hint_without_history(&self.specs, line),
        };
        if acceptable {
            self.acceptable = hint.clone();
        }
        if use_ansi_coloring && !hint.is_empty() {
            let style = if acceptable { Style::new().fg(AnsiColor::DarkGray) } else { Style::new().fg(AnsiColor::DarkGray).italic() };
            style.paint(hint).to_string()
        } else {
            hint
        }
    }

    fn complete_hint(&self) -> String {
        self.acceptable.clone()
    }

    fn next_hint_token(&self) -> String {
        let trimmed = self.acceptable.trim_start();
        let leading = self.acceptable.len() - trimmed.len();
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        self.acceptable[..leading + end].to_string()
    }
}

/// Colours the command word: green if it's a command, red if not.
pub struct WalletHighlighter {
    specs: Vec<CommandSpec>,
}

impl reedline::Highlighter for WalletHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        let leading = line.len() - line.trim_start().len();
        let command_end = line[leading..].find(char::is_whitespace).map_or(line.len(), |i| leading + i);
        let command = &line[leading..command_end];
        let known = self.specs.iter().any(|spec| spec.name == command) || command == "quit" || command == "q";
        styled.push((Style::new(), line[..leading].to_string()));
        styled.push((if known { Style::new().fg(AnsiColor::Green).bold() } else { Style::new().fg(AnsiColor::Red) }, command.to_string()));
        styled.push((Style::new(), line[command_end..].to_string()));
        styled
    }
}

/// `[wallet 5648a3]: ` on the left, as in the reference wallet; the
/// current account on the right.
pub struct WalletPrompt {
    pub left: String,
    pub data: Arc<Mutex<CompletionData>>,
}

impl Prompt for WalletPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Owned(self.data.lock().expect("completion data lock").current_account.clone())
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(": ")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("::: ")
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let status = match search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!("({status}reverse-search: {}) ", search.term))
    }

    fn get_prompt_color(&self) -> Color {
        Color::Cyan
    }

    fn get_indicator_color(&self) -> Color {
        Color::Cyan
    }

    fn get_prompt_right_color(&self) -> Color {
        Color::DarkGrey
    }
}

/// Where prompt history is kept: `$XDG_STATE_HOME/stagenet-wallet-cli/`,
/// else `~/.local/state/stagenet-wallet-cli/`. `None` if neither is known.
fn history_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    let dir = base.join("stagenet-wallet-cli");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history"))
}

fn edit(command: EditCommand) -> ReedlineEvent {
    ReedlineEvent::Edit(vec![command])
}

/// Emacs-style keys (Ctrl+A/E, Ctrl+W, Ctrl+R, Shift+arrows to select,
/// Ctrl+Shift+X/C/V for the system clipboard, ...) plus the modern
/// affordances on top.
fn keybindings() -> Keybindings {
    let mut keys = default_emacs_keybindings();
    let (alt, shift) = (KeyModifiers::ALT, KeyModifiers::SHIFT);
    // Tab opens the completion menu, then steps through it.
    keys.add_binding(KeyModifiers::NONE, KeyCode::Tab, ReedlineEvent::UntilFound(vec![ReedlineEvent::Menu("completion_menu".to_string()), ReedlineEvent::MenuNext]));
    keys.add_binding(shift, KeyCode::BackTab, ReedlineEvent::MenuPrevious);
    // Word-wise movement and selection with Alt, as in macOS text fields
    // (Ctrl works too, from the defaults).
    keys.add_binding(alt, KeyCode::Left, edit(EditCommand::MoveWordLeft { select: false }));
    keys.add_binding(alt, KeyCode::Right, ReedlineEvent::UntilFound(vec![ReedlineEvent::HistoryHintWordComplete, edit(EditCommand::MoveWordRight { select: false })]));
    keys.add_binding(alt | shift, KeyCode::Left, edit(EditCommand::MoveWordLeft { select: true }));
    keys.add_binding(alt | shift, KeyCode::Right, edit(EditCommand::MoveWordRight { select: true }));
    keys.add_binding(alt, KeyCode::Backspace, edit(EditCommand::BackspaceWord));
    keys.add_binding(alt, KeyCode::Delete, edit(EditCommand::DeleteWord));
    keys.add_binding(shift, KeyCode::Home, edit(EditCommand::MoveToLineStart { select: true }));
    keys.add_binding(shift, KeyCode::End, edit(EditCommand::MoveToLineEnd { select: true }));
    keys.add_binding(alt, KeyCode::Char('a'), edit(EditCommand::SelectAll));
    keys
}

/// Builds the editor for one session. `data` feeds completion and the
/// right-hand prompt; the caller refreshes it after each command.
pub fn line_editor(data: Arc<Mutex<CompletionData>>) -> Reedline {
    let specs = command_specs();
    let completion_menu = IdeMenu::default()
        .with_name("completion_menu")
        .with_default_border()
        .with_max_completion_width(72)
        .with_max_completion_height(12)
        .with_description_mode(DescriptionMode::PreferRight)
        .with_max_description_width(60);
    let mut editor = Reedline::create()
        .with_edit_mode(Box::new(Emacs::new(keybindings())))
        .with_completer(Box::new(WalletCompleter { specs: specs.clone(), data }))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu)))
        .with_hinter(Box::new(WalletHinter { specs: specs.clone(), acceptable: String::new() }))
        .with_highlighter(Box::new(WalletHighlighter { specs }))
        .with_partial_completions(true)
        .with_quick_completions(true)
        // Click to move the cursor, on terminals that send click events
        // (kitty, WezTerm, Ghostty); dragging still selects and copies
        // with the terminal's own selection everywhere.
        .with_mouse_click(MouseClickMode::EnabledWithOsc133)
        // A leading space keeps a line out of history (for anything you'd
        // rather not have saved).
        .with_history_exclusion_prefix(Some(" ".to_string()));
    if let Some(history) = history_path().and_then(|path| FileBackedHistory::with_file(1_000, path).ok()) {
        editor = editor.with_history(Box::new(history));
    }
    editor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> CompletionData {
        CompletionData {
            txids: vec![("ab12".repeat(16), "in 1.0 XMR".to_string())],
            key_images: vec![("cd34".repeat(16), "0.5 XMR".to_string())],
            outputs: vec![("0/42".to_string(), "0.5 XMR".to_string())],
            addresses: vec![("5AAAA".to_string(), "address book: shop".to_string()), ("5BBBB".to_string(), "own address 0: Primary account".to_string())],
            accounts: vec![("0".to_string(), "Primary account".to_string()), ("1".to_string(), "savings".to_string())],
            account_tags: vec![],
            address_book_rows: vec![("0".to_string(), "5AAAA shop".to_string())],
            current_account: "account 0: Primary account".to_string(),
        }
    }

    fn names(list: Vec<(String, String)>) -> Vec<String> {
        list.into_iter().map(|(value, _)| value).collect()
    }

    #[test]
    fn every_parser_command_is_known_to_the_editor_with_its_usage() {
        let specs = command_specs();
        for name in ["transfer", "sweep_all", "show_transfers", "balance", "account", "freeze", "add_output", "help", "exit"] {
            assert!(specs.iter().any(|spec| spec.name == name), "{name}");
        }
        let transfer = specs.iter().find(|spec| spec.name == "transfer").unwrap();
        assert!(transfer.usage.starts_with("[index=<N1>"), "usage without the name: {}", transfer.usage);
        assert!(!transfer.about.is_empty());
        let account = specs.iter().find(|spec| spec.name == "account").unwrap();
        assert!(account.usage.contains("new <label") && account.usage.contains(" | "), "{}", account.usage);
    }

    #[test]
    fn completions_follow_each_commands_grammar_and_the_wallets_data() {
        let specs = command_specs();
        let data = data();
        assert!(names(candidates(&specs, &data, &[])).contains(&"transfer".to_string()));
        assert_eq!(names(candidates(&specs, &data, &["account"])), ["new", "switch", "label", "tag", "untag", "tag_description"]);
        assert_eq!(names(candidates(&specs, &data, &["account", "switch"])), ["0", "1"]);
        assert!(names(candidates(&specs, &data, &["account", "switch", "1"])).is_empty());
        assert_eq!(names(candidates(&specs, &data, &["set", "unit"]))[0], "monero");
        assert_eq!(names(candidates(&specs, &data, &["freeze"])), ["cd34".repeat(16)]);
        assert_eq!(names(candidates(&specs, &data, &["show_transfer"])), ["ab12".repeat(16)]);
        assert_eq!(names(candidates(&specs, &data, &["is_output_spent"])), ["0/42"]);

        let transfer = names(candidates(&specs, &data, &["transfer"]));
        assert!(transfer.contains(&"elevated".to_string()) && transfer.contains(&"5AAAA".to_string()));
        let after_address = names(candidates(&specs, &data, &["transfer", "5AAAA", "1"]));
        assert!(!after_address.contains(&"elevated".to_string()), "priority only fits before the first address");
    }

    #[test]
    fn the_completer_replaces_just_the_word_being_typed() {
        let mut completer = WalletCompleter { specs: command_specs(), data: Arc::new(Mutex::new(data())) };
        let suggestions = reedline::Completer::complete(&mut completer, "account sw", 10);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].value, "switch");
        assert_eq!((suggestions[0].span.start, suggestions[0].span.end), (8, 10));
        assert_eq!(suggestions[0].description.as_deref(), Some("<index>"));
    }

    #[test]
    fn hints_finish_a_unique_command_then_show_its_usage() {
        let specs = command_specs();
        assert_eq!(WalletHinter::hint_without_history(&specs, "transf"), ("er".to_string(), true));
        let (usage, acceptable) = WalletHinter::hint_without_history(&specs, "transfer ");
        assert!(!acceptable, "a usage hint is for reading, not accepting");
        assert!(usage.contains("transfer [index=<N1>"), "{usage}");
        let (switch, _) = WalletHinter::hint_without_history(&specs, "account switch ");
        assert_eq!(switch.trim(), "account switch <index>", "only the forms that fit what's typed");
        let (partial, _) = WalletHinter::hint_without_history(&specs, "account ta");
        assert!(partial.contains("account tag <tag_name>") && partial.contains("account tag_description") && !partial.contains("switch"), "{partial}");
        assert_eq!(WalletHinter::hint_without_history(&specs, "s").0, "", "ambiguous: no hint");
        assert_eq!(WalletHinter::hint_without_history(&specs, "nonsense ").0, "");
    }

    #[test]
    fn the_command_word_is_green_when_known_and_red_when_not() {
        let highlighter = WalletHighlighter { specs: command_specs() };
        let known = reedline::Highlighter::highlight(&highlighter, "balance detail", 0);
        assert_eq!(known.buffer[1], (Style::new().fg(AnsiColor::Green).bold(), "balance".to_string()));
        let unknown = reedline::Highlighter::highlight(&highlighter, "  balanse", 0);
        assert_eq!(unknown.buffer[1].1, "balanse");
        assert_eq!(unknown.buffer[1].0, Style::new().fg(AnsiColor::Red));
    }
}
