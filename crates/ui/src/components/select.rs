use crate::{component, Attrs, UiEnum, UiRecord};

/// One option of a [`Select`]. Its text says everything in words, for a
/// browser without JavaScript (`Label (detail) [Network] [chip] - note`);
/// the enhanced list draws the same parts on their own: a logo, a label, a
/// monospace detail, a network badge, a chip, a note.
#[derive(Clone, Debug, Default, PartialEq, Eq, UiRecord)]
#[ui(new(value, label), skip(data_label, text))]
pub struct Choice {
    value: String,
    label: String,
    /// `data-label`: the label on its own, when the text has more parts.
    data_label: Option<String>,
    logo: Option<String>,
    detail: Option<String>,
    network: Option<String>,
    chip: Option<String>,
    /// The chip is Current: the one in use now, the only green one.
    current: bool,
    note: Option<String>,
    selected: bool,
    disabled: bool,
    /// The option's text: every part, in words.
    text: String,
}

impl Choice {
    pub fn new(value: impl AsRef<str>, label: impl AsRef<str>) -> Choice {
        Choice {
            value: value.as_ref().to_owned(),
            label: label.as_ref().to_owned(),
            ..Choice::default()
        }
        .words()
    }

    /// The "Choose…" prompt of a required dropdown with nothing picked
    /// yet: no value, never pickable, shown until something is.
    pub fn prompt(label: impl AsRef<str>, shown: bool) -> Choice {
        Choice::new("", label).disabled(true).selected(shown)
    }

    /// A small picture before the label (a wallet app's logo), by its
    /// URL. Drawn by the enhanced list only: the text has no room for it.
    pub fn logo(mut self, url: impl AsRef<str>) -> Choice {
        self.logo = Some(url.as_ref().to_owned());
        self.words()
    }

    /// Monospace and muted after the label: an address, a currency code.
    pub fn detail(mut self, detail: impl AsRef<str>) -> Choice {
        self.detail = Some(detail.as_ref().to_owned());
        self.words()
    }

    /// A wallet's Monero network (`mainnet`, `stagenet`, `testnet`), drawn
    /// as the site's network badge.
    pub fn network(mut self, network: impl AsRef<str>) -> Choice {
        self.network = Some(network.as_ref().to_owned());
        self.words()
    }

    /// A short tag after the label, in plain colours.
    pub fn chip(mut self, chip: impl AsRef<str>) -> Choice {
        self.chip = Some(chip.as_ref().to_owned());
        self.current = false;
        self.words()
    }

    /// The green Current chip, when `current`.
    pub fn current(mut self, current: bool) -> Choice {
        if current {
            self.chip = Some("Current".into());
            self.current = true;
        }
        self.words()
    }

    /// Muted, at the end: a count, a date, why it can't be picked.
    pub fn note(mut self, note: impl AsRef<str>) -> Choice {
        self.note = Some(note.as_ref().to_owned());
        self.words()
    }

    pub fn selected(mut self, selected: bool) -> Choice {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Choice {
        self.disabled = disabled;
        self
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    /// The option's text, as a browser without JavaScript shows it. An
    /// option with no label (a network on its own) starts at its first
    /// part: `[Stagenet]`.
    pub fn text(&self) -> &str {
        &self.text
    }

    fn words(mut self) -> Choice {
        let mut text = self.label.clone();
        let mut push = |part: String| {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&part);
        };
        if let Some(detail) = &self.detail {
            push(format!("({detail})"));
        }
        if let Some(network) = &self.network {
            push(format!("[{}]", network_name(network)));
        }
        if let Some(chip) = &self.chip {
            push(format!("[{chip}]"));
        }
        if let Some(note) = &self.note {
            text.push_str(&format!(" - {note}"));
        }
        self.text = text;
        let parts = self.logo.is_some()
            || self.detail.is_some()
            || self.network.is_some()
            || self.chip.is_some()
            || self.note.is_some();
        self.data_label = parts.then(|| self.label.clone());
        self
    }
}

/// `stagenet` as people read it: `Stagenet`.
pub fn network_name(network: &str) -> String {
    let mut chars = network.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// An ARIA state that takes the words `true` and `false`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum AriaBool {
    True,
    False,
}

/// A popover that only script opens and closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum Popover {
    Manual,
}

/// Whether the list has a find box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum SearchMode {
    /// From 12 options up.
    Auto,
    Show,
    Hide,
}

/// An option as the enhanced button and list draw it (`select.behaviour.ts`).
#[derive(Clone, Debug, UiRecord)]
pub struct SelectItem {
    pub index: usize,
    pub id: String,
    pub label: Option<String>,
    pub logo: Option<String>,
    pub detail: Option<String>,
    /// The network's name, for its badge.
    pub network: Option<String>,
    pub mainnet: bool,
    pub chip: Option<String>,
    pub current: bool,
    /// Only in the list, not the button.
    pub note: Option<String>,
    /// `aria-selected`.
    pub chosen: AriaBool,
    pub active: bool,
    pub disabled: bool,
}

component! {
    /// The site's one dropdown. The page sends an ordinary `select`: without
    /// JavaScript it is all there is, and each option's text says everything.
    /// Once its island attaches, a button and a list that floats over the
    /// page draw the options' parts, with search, type-ahead and the keys a
    /// listbox takes; the select stays the field, so forms, `required`,
    /// fixi and other scripts that read it keep working.
    pub struct Select {
        /// The form field's name.
        name: String,
        options: Vec<Choice>,
        id: Option<String>,
        /// The accessible name, for a select with no `<label>`.
        label: Option<String>,
        /// The toolbar size.
        #[default(false)]
        compact: bool,
        #[default(SearchMode::Auto)]
        search: SearchMode,
        #[default(false)]
        required: bool,
        #[default(false)]
        disabled: bool,
        /// More attributes on the select: `aria-describedby`, `data-saved`,
        /// `fx-*`.
        rest: Attrs,
    }
    behaviour "select.behaviour.ts" {
        expanded: AriaBool,
        /// The button's accessible name: the select's label.
        name: Option<String>,
        described: Option<String>,
        list_id: String,
        active_id: Option<String>,
        /// `aria-invalid` once the browser refused the field.
        invalid: Option<AriaBool>,
        disabled: bool,
        /// The chosen option, unless it's the prompt.
        current: Option<SelectItem>,
        /// The prompt's text while nothing is chosen.
        prompt: String,
        /// Where the browser has the Popover API.
        popover: Option<Popover>,
        /// Hidden while closed, where there's no Popover API.
        hidden: bool,
        /// Opened upwards, for want of room below.
        up: bool,
        searching: bool,
        shown: Vec<SelectItem>,
        /// Whether the find box matches anything.
        matches: bool,
        fn key,
        fn leave,
        fn toggle,
        fn find,
        fn hold,
        fn choose(usize),
        ref root,
        ref native,
        ref button,
        ref popup,
        ref finder,
    }
    html {
        div class="mk-select" data-compact[compact] ref=root on:keydown=key on:focusout=leave {
            select class=join["mk-input", "mk-native" if enhanced] data-size="compact" if compact name=(name) id=[id] aria-label=[label] required[required]
                disabled[disabled] tabindex="-1" if enhanced aria-hidden="true" if enhanced ref=native ..rest {
                @for choice in options {
                    option value=(choice.value) selected[choice.selected] disabled[choice.disabled]
                        data-label=[choice.data_label] data-logo=[choice.logo] data-detail=[choice.detail]
                        data-network=[choice.network] data-chip=[choice.chip] data-chip-tone="current" if choice.current
                        data-note=[choice.note] { (choice.text) }
                }
            }
            @if enhanced {
                button class="mk-button" type="button" role="combobox" aria-haspopup="listbox"
                    aria-expanded=(b.expanded) aria-label=[b.name] aria-describedby=[b.described]
                    aria-controls=(b.list_id) aria-activedescendant=[b.active_id] aria-invalid=[b.invalid]
                    disabled[b.disabled] ref=button on:click=toggle {
                    span class="mk-value" {
                        @if let Some(item) = b.current {
                            @if let Some(logo) = item.logo { img class="mk-logo" src=(logo) alt=""; }
                            @if let Some(label) = item.label { span class="mk-label" { (label) } }
                            @if let Some(detail) = item.detail { span class="mk-detail" { (detail) } }
                            @if let Some(network) = item.network {
                                span class=join["tag-network", "is-main" if item.mainnet, "is-test" if !item.mainnet] {
                                    @if item.mainnet {
                                        svg viewBox="0 0 24 24" aria-hidden="true" { circle cx="12" cy="12" r="9" {} path d="M7 15V9l5 5 5-5v6" {} }
                                    } @else {
                                        svg viewBox="0 0 24 24" aria-hidden="true" { path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 1.7 3h10.6a2 2 0 0 0 1.7-3l-5-9V3" {} path d="M7.5 15h9" {} }
                                    }
                                    (network)
                                }
                            }
                            @if let Some(chip) = item.chip {
                                span class=join["mk-current" if item.current, "tag tag-unknown mk-chip" if !item.current] { (chip) }
                            }
                        } @else {
                            span class="mk-prompt" { (b.prompt) }
                        }
                    }
                    span class="mk-caret" {}
                }
                div class=join["mk-popup", "mk-up" if b.up] id={ (b.list_id) "-popup" } popover=[b.popover] hidden[b.hidden] ref=popup {
                    @if b.searching {
                        input class="mk-input mk-find" type="search" placeholder="Type to find…" autocomplete="off"
                            aria-label="Find" role="combobox" aria-autocomplete="list" aria-expanded="true"
                            aria-controls=(b.list_id) aria-activedescendant=[b.active_id] ref=finder on:input=find;
                    }
                    ul class="mk-list" id=(b.list_id) role="listbox" aria-label=[b.name] on:mousedown=hold {
                        @for item in b.shown {
                            li class=join["mk-option", "mk-active" if item.active] id=(item.id) data-index=(item.index) role="option"
                                aria-selected=(item.chosen) aria-disabled="true" if item.disabled on:click=choose(item.index) {
                                @if let Some(logo) = item.logo { img class="mk-logo" src=(logo) alt=""; }
                                @if let Some(label) = item.label { span class="mk-label" { (label) } }
                                @if let Some(detail) = item.detail { span class="mk-detail" { (detail) } }
                                @if let Some(network) = item.network {
                                    span class=join["tag-network", "is-main" if item.mainnet, "is-test" if !item.mainnet] {
                                        @if item.mainnet {
                                            svg viewBox="0 0 24 24" aria-hidden="true" { circle cx="12" cy="12" r="9" {} path d="M7 15V9l5 5 5-5v6" {} }
                                        } @else {
                                            svg viewBox="0 0 24 24" aria-hidden="true" { path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 1.7 3h10.6a2 2 0 0 0 1.7-3l-5-9V3" {} path d="M7.5 15h9" {} }
                                        }
                                        (network)
                                    }
                                }
                                @if let Some(chip) = item.chip {
                                    span class=join["mk-current" if item.current, "tag tag-unknown mk-chip" if !item.current] { (chip) }
                                }
                                @if let Some(note) = item.note { span class="mk-note" { (note) } }
                            }
                        }
                    }
                    p class="mk-empty" role="status" hidden[b.matches] { "Nothing matches." }
                }
            }
        }
    }
}

/// The stylebook's own theme picker, in its header.
pub(super) fn book_theme() -> Select {
    Select::new(
        "theme",
        vec![
            Choice::new("system", "System").selected(true),
            Choice::new("light", "Light"),
            Choice::new("dark", "Dark"),
        ],
    )
    .id("book-theme")
    .compact(true)
}

/// The stylebook's form demo: reset, submit and a script changing it.
pub(super) fn select_demo() -> Select {
    Select::new(
        "currency",
        vec![
            Choice::new("AUD", "Australian dollar").selected(true),
            Choice::new("EUR", "Euro"),
            Choice::new("XMR", "Monero"),
        ],
    )
    .id("currency")
}

pub(super) fn select_examples() -> Vec<(&'static str, Select)> {
    let currencies = [
        ("AUD", "Australian Dollar"),
        ("CAD", "Canadian Dollar"),
        ("CHF", "Swiss Franc"),
        ("EUR", "Euro"),
        ("GBP", "Pound Sterling"),
        ("JPY", "Japanese Yen"),
        ("NZD", "New Zealand Dollar"),
        ("SEK", "Swedish Krona"),
        ("USD", "United States Dollar"),
        ("XMR", "Monero"),
        ("BRL", "Brazilian Real"),
        ("INR", "Indian Rupee"),
    ];
    vec![
        (
            "Currency, with a find box from 12 options",
            Select::new(
                "currency",
                currencies
                    .iter()
                    .map(|(code, name)| {
                        Choice::new(code, name)
                            .detail(code)
                            .selected(*code == "EUR")
                    })
                    .collect::<Vec<_>>(),
            )
            .id("book-currency")
            .label("Base currency"),
        ),
        (
            "Wallets, with networks, the current one and notes",
            Select::new(
                "wallet_id",
                vec![
                    Choice::prompt("Choose a wallet…", false),
                    Choice::new("w_1", "Copper Heron")
                        .detail("48xQ7…v3Rk")
                        .network("mainnet")
                        .current(true)
                        .note("2 stores")
                        .selected(true),
                    Choice::new("w_2", "Feather test")
                        .detail("5Bq2…9TtM")
                        .network("stagenet")
                        .note("1 store"),
                    Choice::new("w_3", "Old till")
                        .detail("47aa…0QeN")
                        .network("mainnet")
                        .note("retired")
                        .disabled(true),
                ],
            )
            .id("book-wallet")
            .label("Wallet")
            .required(true),
        ),
        (
            "A network on its own: the badge",
            Select::new(
                "network",
                ["mainnet", "stagenet", "testnet"]
                    .iter()
                    .map(|n| Choice::new(n, "").network(n).selected(*n == "stagenet"))
                    .collect::<Vec<_>>(),
            )
            .id("book-network")
            .label("Network"),
        ),
        (
            "Compact, for a toolbar",
            Select::new(
                "level",
                vec![
                    Choice::new("error", "Errors"),
                    Choice::new("warn", "Warnings and up").selected(true),
                    Choice::new("info", "Info and up"),
                    Choice::new("debug", "Everything"),
                ],
            )
            .id("book-level")
            .label("Level")
            .compact(true),
        ),
        (
            "Disabled",
            Select::new(
                "language",
                vec![Choice::new("en", "English").selected(true)],
            )
            .id("book-language")
            .label("Language")
            .disabled(true),
        ),
    ]
}
