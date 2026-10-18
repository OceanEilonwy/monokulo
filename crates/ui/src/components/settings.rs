//! Settings pages: cards of settings in one form, one save bar along the
//! bottom of the window, and a toast after a save. The admin settings page,
//! the Account page, a store's settings and a wallet's page all use them.
//!
//! The server renders everything, so a page works without JavaScript: the
//! form is a real `<form method="post">`, the save bar is always shown, and
//! a save answers with the page again. Each piece is an island; their
//! behaviours share one store per form (`web/components/settings.store.ts`)
//! and add the rest: what changed, marked on its setting and card and
//! counted on the bar; Discard; a question before leaving with changes; the
//! Save button busy while a save is out.
//!
//! Page scripts hear about it with events on the form's host element:
//! `mk-dirty` (`{count, groups}`), `mk-saved` (`{groups}`) and `mk-failed`
//! (`{group, message}`); on a card, `mk-count` (`{changes}`: add changes the
//! page tracks itself; the card leaves out the settings inside an element
//! marked `data-counted-by-page`) and `mk-discard` (cancel it to put the card back by
//! loading the page). A page script that changes what isn't a setting's
//! control (a node row moved) sends `mk-changed` to the form.

use crate::{component, Attrs, Slot, UiEnum};

/// How a form is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum FormMethod {
    Post,
    /// A page that only reads (the stylebook's examples).
    Get,
}

component! {
    /// The page's settings form: one per page. `label` names what has
    /// unsaved changes when leaving is asked about ("Abuse protection has
    /// unsaved changes"). With `fixi_target`, a save swaps its answer into
    /// that element without reloading, the way `fixi_swap` says (fixi's
    /// `fx-swap`; the site's glue adds `page`, for an answer that is the
    /// whole page); without, it's a plain POST and the page it redirects to.
    pub struct SettingsForm {
        action: String,
        label: String,
        fixi_target: Option<String>,
        fixi_swap: Option<String>,
        /// The cards, the save bar and anything else the form sends.
        children: Slot,
        #[default(FormMethod::Post)]
        method: FormMethod,
        /// The form's id: a page has one settings form, `settings-form`.
        #[default(String::from("settings-form"))]
        form_id: String,
    }
    behaviour "settings-form.behaviour.ts" {
        uses(label),
        ref host,
        ref form,
    }
    html {
        div class="mk-settings-form" data-label=(label) ref=host {
            form method=(method) action=(action) id=(form_id) fx-action=(action) if fixi_target
                fx-method="POST" if fixi_target fx-target=[fixi_target] fx-swap=[fixi_swap] ref=form {
                (children)
            }
        }
    }
}

component! {
    /// One card of settings, saved together: its title, what its unsaved
    /// changes or a refusal say, "Saved" after a save, and Discard for its
    /// changes. Its id is `card-{name}`: links to it (the save bar's, a
    /// toast's Show) use it.
    pub struct SettingsCard {
        /// The card's name: `card-{name}` is its id, and events name it.
        name: String,
        title: String,
        /// Its settings.
        children: Slot,
        /// Another class beside `settings-card`.
        extra_class: Option<String>,
        /// After the title: a chip, a line of meta.
        head: Option<Slot>,
        /// At the right of the head: where more about it is shown.
        link: Option<Slot>,
        /// In the head while the card isn't refused ("Restart needed").
        badge: Option<Slot>,
        /// The last save refused this card: "Not saved", and `message`.
        #[default(false)]
        failed: bool,
        message: Option<String>,
        /// The last save saved this card: "Saved" and when (or nothing).
        saved: Option<String>,
        /// The "Saved" mark takes focus once fixi swaps it in.
        #[default(false)]
        focus_saved: bool,
        /// Nothing on it can be changed here.
        #[default(false)]
        readonly: bool,
        /// Hidden now: `shown_by` shows it.
        #[default(false)]
        hidden: bool,
        /// `name=value`: shown only while that checkbox is ticked, its
        /// controls off (not sent) while hidden.
        shown_by: Option<String>,
        /// `data-*` attributes of the page's own.
        rest: Attrs,
    }
    behaviour "settings-card.behaviour.ts" {
        uses(name, title, failed, hidden, shown_by),
        /// It has unsaved changes.
        dirty: bool,
        /// Refused, and not put back since.
        refused: bool,
        /// The refusal was put back: its marks go.
        cleared: bool,
        /// "2 unsaved".
        unsaved: Option<String>,
        /// Discard has nothing to do.
        settled: bool,
        /// Hidden by `shown_by`.
        off: bool,
        fn discard,
        ref root,
    }
    html {
        section id={ "card-" (name) }
            class=join["settings-card", { (extra_class) } if extra_class, "is-failed" if failed]
            class=join["settings-card", { (extra_class) } if extra_class, "is-dirty" if b.dirty, "is-failed" if b.refused]
            data-name=(name) data-shown-by=[shown_by] hidden[hidden] hidden[b.off] role="region"
            aria-labelledby={ "card-" (name) "-title" } ..rest ref=root {
            header class="card-head" {
                h3 id={ "card-" (name) "-title" } { (title) }
                @if let Some(head) = head { (head) }
                span class="card-state" data-card-state {
                    @if enhanced {
                        @if let Some(text) = b.unsaved {
                            span class="mk-status-text" data-tone="changed" data-unsaved { (text) }
                        }
                    }
                    @if failed {
                        span class="mk-status-text" data-tone="error" data-card-failed hidden[b.cleared] { "Not saved" }
                    } @else if let Some(badge) = badge {
                        (badge)
                    }
                }
                span class="card-spacer" {}
                @if let Some(link) = link { (link) }
                @if readonly {
                    span class="card-meta" { "Can't be changed here" }
                } @else {
                    @if saved {
                        @if !failed {
                            span class="card-meta card-saved" data-card-saved tabindex="-1" if focus_saved
                                data-fx-focus[focus_saved] hidden[b.dirty] {
                                "Saved"
                                @if let Some(at) = saved { @if at { " " (at) } }
                            }
                        }
                    }
                    @if enhanced {
                        button type="button" class="card-discard" data-card-discard hidden[b.settled] on:click=discard { "Discard" }
                    }
                }
            }
            div class="card-body" {
                @if message {
                    p class="error" role="alert" hidden[b.cleared] {
                        @if let Some(message) = message { (message) }
                    }
                }
                (children)
            }
        }
    }
}

/// How a setting lays out its name and its control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum SettingLayout {
    /// The name and help above, the control across the whole card: a
    /// control whose content needs the width (a wallet picker, a long
    /// value such as an address or a URL).
    Stacked,
}

component! {
    /// One setting: its name and what it's for on the left, its control on
    /// the right (a phone, or a wide control such as radio cards, a list of
    /// choices, a text area or a table, puts the control under them), and
    /// "changed" while it differs from what's saved (a control's
    /// `data-saved` after a refused save, its default value otherwise).
    pub struct Setting {
        label: String,
        /// The id of the control the label is for; for a group, the
        /// label's own id, which names the group.
        id: String,
        /// The control, and anything under it.
        children: Slot,
        /// Several controls (radio buttons, checkboxes): a group named by
        /// its label.
        #[default(false)]
        group: bool,
        /// Beside the label: where its value comes from.
        chip: Option<Slot>,
        /// What it's for, under the label.
        help: Option<Slot>,
        /// The help's id, for the control to point at.
        help_id: Option<String>,
        /// A group's description: its help's id.
        described_by: Option<String>,
        /// The label's own id, for a control that names itself by it.
        label_id: Option<String>,
        /// Stacked: the control under the name, full width, at any width.
        layout: Option<SettingLayout>,
        /// `data-*` attributes of the page's own, `hidden`.
        rest: Attrs,
    }
    behaviour "setting.behaviour.ts" {
        uses(),
        changed: bool,
        ref root,
    }
    html {
        div class="setting-field"
            class=join["setting-field", "is-changed" if b.changed]
            role="group" if group aria-labelledby=(id) if group aria-describedby=[described_by] data-layout=[layout] ..rest ref=root {
            div class="setting-about" {
                div class="setting-label-row" {
                    @if group {
                        span class="setting-label" id=(id) { (label) }
                    } @else {
                        label class="setting-label" id=[label_id] for=(id) { (label) }
                    }
                    @if let Some(chip) = chip { (chip) }
                    span class="changed-mark" { "changed" }
                }
                @if let Some(help) = help {
                    p class="field-help" id=[help_id] { (help) }
                }
            }
            div class="setting-control" { (children) }
        }
    }
}

/// A card in the save bar's message: one with unsaved changes.
#[derive(Clone, Debug, crate::UiRecord)]
pub struct ChangedCard {
    pub name: String,
    pub title: String,
    /// What comes before it: nothing, ", " or " and ".
    pub before: String,
}

component! {
    /// The page's one save bar, along the bottom of the window: `message`,
    /// then Discard changes (a link to the page as saved) and Save. Without
    /// JavaScript it's always there; with it, only while something changed,
    /// a save was refused, or leaving is asked about, and it names the
    /// cards changed. Red after a save that refused something.
    pub struct SaveBar {
        /// What saving does, or why the last save didn't.
        message: Slot,
        /// The page as saved.
        discard_href: String,
        #[default(false)]
        failed: bool,
        /// The message takes focus once fixi swaps it in.
        #[default(false)]
        focus: bool,
        /// In the page's footer, not along the bottom of the window.
        #[default(false)]
        inline: bool,
        /// What the Save button says.
        #[default(String::from("Save"))]
        save_label: String,
        /// Its id: a page has one save bar, `save-bar`.
        #[default(String::from("save-bar"))]
        id: String,
        /// Its region's name.
        #[default(String::from("Save changes"))]
        region_label: String,
    }
    behaviour "save-bar.behaviour.ts" {
        uses(failed),
        /// Nothing to save or say: out of sight.
        idle: bool,
        dirty: bool,
        failed: bool,
        leaving: bool,
        /// The behaviour's message stands in for the server's.
        own: bool,
        /// "3 unsaved changes".
        count: Option<String>,
        cards: Vec<ChangedCard>,
        /// "Profile has unsaved changes."
        leaving_title: Option<String>,
        /// "Save or discard them before going to Payments."
        leaving_text: Option<String>,
        saving: bool,
        fn discard_all,
        fn stay,
        fn discard_and_go,
        fn save_and_go,
        ref root,
        ref save,
        ref go_save,
    }
    html {
        div id=(id) class=join["save-bar", "is-failed" if failed]
            class=join["save-bar", "is-dirty" if b.dirty, "is-failed" if b.failed, "is-leaving" if b.leaving]
            data-placement="inline" if inline data-idle[b.idle] role="region" aria-label=(region_label) ref=root {
            div class=join["wrap" if !inline, "save-bar-inner"] {
                p class="save-bar-message" data-save-bar-message tabindex="-1" data-fx-focus[focus] {
                    span hidden[b.own] { (message) }
                    @if enhanced {
                        @if let Some(count) = b.count {
                            strong { (count) }
                            " in "
                            @for card in b.cards {
                                (card.before)
                                a href={ "#card-" (card.name) } data-show-card=(card.name) { (card.title) }
                            }
                        }
                        @if let Some(title) = b.leaving_title {
                            strong { (title) }
                            @if let Some(text) = b.leaving_text { " " (text) }
                        }
                    }
                }
                div class="save-bar-actions" hidden[b.leaving] {
                    a class="mk-btn" href=(discard_href) data-discard-all on:click=discard_all { "Discard changes" }
                    button type="submit" class="mk-btn" data-variant="primary" data-save disabled[b.saving] aria-busy="true" if b.saving ref=save {
                        @if enhanced {
                            @if b.saving { "Saving…" } @else { (save_label) }
                        } @else {
                            (save_label)
                        }
                    }
                }
                @if enhanced {
                    @if b.leaving {
                        div class="save-bar-actions" data-leave-actions {
                            button type="button" class="mk-btn" data-leave="stay" on:click=stay { "Stay here" }
                            button type="button" class="mk-btn" data-leave="discard" on:click=discard_and_go { "Discard and go" }
                            button type="button" class="mk-btn" data-variant="primary" data-leave="save" on:click=save_and_go ref=go_save { "Save and go" }
                        }
                    }
                }
            }
        }
    }
}

/// How a save went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum ToastKind {
    /// Saved and applied.
    Success,
    /// Saved, but something waits for a restart.
    Warning,
    /// Something wasn't saved.
    Error,
    /// Nothing happened (nothing had changed).
    Neutral,
}

component! {
    /// A small message after a save, in the corner of the window. Without
    /// JavaScript it fades by itself; with it, one saying something wasn't
    /// saved stays until it's closed.
    pub struct Toast {
        kind: ToastKind,
        title: String,
        lines: Vec<String>,
        /// The card to go to (its name): the one a refusal is about.
        show: Option<String>,
        /// Stays until it's closed, whatever it says (a review copy).
        #[default(false)]
        persistent: bool,
    }
    behaviour "toast.behaviour.ts" {
        uses(),
        closing: bool,
        fn close,
        ref root,
    }
    html {
        div class=join["toast", "toast-success" if kind == ToastKind::Success, "toast-warning" if kind == ToastKind::Warning,
                "toast-error" if kind == ToastKind::Error, "toast-neutral" if kind == ToastKind::Neutral]
            class=join["toast", "toast-success" if kind == ToastKind::Success, "toast-warning" if kind == ToastKind::Warning,
                "toast-error" if kind == ToastKind::Error, "toast-neutral" if kind == ToastKind::Neutral, "is-closing" if b.closing]
            role=join["alert" if kind == ToastKind::Error, "status" if kind != ToastKind::Error] data-toast data-persistent[persistent] ref=root {
            span class="toast-icon" aria-hidden="true" {
                svg class="mk-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
                    stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
                    @if kind == ToastKind::Success {
                        path d="m4 12 5 5L20 6" {}
                    } @else if kind == ToastKind::Neutral {
                        circle cx="12" cy="12" r="9" {}
                        path d="M12 11v6M12 7v1" {}
                    } @else {
                        path d="m12 3 10 18H2Z M12 9v5M12 17v1" {}
                    }
                }
            }
            div class="toast-text" {
                strong { (title) }
                @for line in lines { span class="toast-line" { (line) } }
                @if let Some(card) = show {
                    a class="toast-show" href={ "#card-" (card) } data-show-card=(card) { "Show" }
                }
            }
            @if enhanced {
                button type="button" class="toast-close" aria-label="Dismiss" data-toast-close on:click=close { "×" }
            }
        }
    }
}

pub(super) fn settings_form_examples() -> Vec<(&'static str, SettingsForm)> {
    let card = |name: &str, title: &str, settings: Vec<Setting>| {
        SettingsCard::new(
            name,
            title,
            Slot::many(settings.into_iter().map(Slot::from)),
        )
    };
    let store_name = Setting::new(
        "Store name",
        "book-settings-name-input",
        crate::Input::new("name")
            .id("book-settings-name-input")
            .value("Corner store"),
    );
    vec![(
        "A card, its settings and the save bar",
        SettingsForm::new(
            "#component-settings-form",
            "Store settings",
            Slot::many([
                Slot::from(card("book-details", "Store details", vec![store_name])),
                Slot::from(
                    SaveBar::new(
                        "Saving changes the store straight away.",
                        "#component-settings-form",
                    )
                    .inline(true)
                    .id("book-form-save-bar")
                    .region_label("Example form's save changes"),
                ),
            ]),
        )
        .method(FormMethod::Get)
        .form_id("book-settings-form"),
    )]
}

pub(super) fn setting_examples() -> Vec<(&'static str, Setting)> {
    vec![
        (
            "With help",
            Setting::new(
                "Confirmations",
                "book-setting-confirmations",
                crate::Input::new("confirmations")
                    .id("book-setting-confirmations")
                    .kind(crate::InputType::Number)
                    .value("10")
                    .described_by("book-setting-confirmations-help"),
            )
            .help("Blocks a payment waits for before it counts as paid.")
            .help_id("book-setting-confirmations-help"),
        ),
        (
            "A checkbox",
            Setting::new(
                "Use TLS",
                "book-setting-tls",
                crate::Checkbox::new()
                    .id("book-setting-tls")
                    .name("tls")
                    .described_by("book-setting-tls-help"),
            )
            .help("Connect with https. Most nodes on port 18081 don't use it.")
            .help_id("book-setting-tls-help"),
        ),
        (
            "Stacked: a control that needs the width",
            Setting::new(
                "Wallet",
                "book-setting-wallet",
                crate::Select::new(
                    "wallet",
                    vec![
                        crate::Choice::new("w_rh01", "Rachel's Handicrafts")
                            .detail("47UhAM…qbLgRU")
                            .network("mainnet")
                            .current(true)
                            .note("this store only")
                            .selected(true),
                        crate::Choice::new("w_ms02", "Market stall float")
                            .detail("48abcD…9xYzQ2")
                            .network("mainnet")
                            .note("used by 1 other store"),
                    ],
                )
                .id("book-setting-wallet")
                .attr("aria-describedby", "book-setting-wallet-help"),
            )
            .help("New payments go into this wallet. Only Mainnet wallets can be picked.")
            .help_id("book-setting-wallet-help")
            .layout(SettingLayout::Stacked),
        ),
        (
            "A group",
            Setting::new("Accept", "book-setting-accept", "Its checkboxes.")
                .group(true)
                .help("What a payment may arrive as.")
                .help_id("book-setting-accept-help")
                .described_by("book-setting-accept-help"),
        ),
    ]
}

pub(super) fn save_bar_examples() -> Vec<(&'static str, SaveBar)> {
    vec![
        (
            "Its message",
            SaveBar::new(
                "Saving writes the changes on this tab.",
                "#component-save-bar",
            )
            .inline(true)
            .id("book-save-bar")
            .region_label("Example save bar"),
        ),
        (
            "After a refused save",
            SaveBar::new(
                "Nothing saved. The URL must use https.",
                "#component-save-bar",
            )
            .failed(true)
            .inline(true)
            .id("book-save-bar-refused")
            .region_label("Example refused save bar"),
        ),
    ]
}

pub(super) fn settings_card_examples() -> Vec<(&'static str, SettingsCard)> {
    vec![
        (
            "Saved",
            SettingsCard::new("book-saved", "Payments", "Its settings.").saved("12 Oct, 09:30"),
        ),
        (
            "Refused",
            SettingsCard::new("book-refused", "Webhooks", "Its settings.")
                .failed(true)
                .message("The URL must use https."),
        ),
        (
            "Read only",
            SettingsCard::new("book-readonly", "Server", "Its settings.").readonly(true),
        ),
    ]
}

pub(super) fn toast_examples() -> Vec<(&'static str, Toast)> {
    vec![
        (
            "Saved",
            Toast::new(
                ToastKind::Success,
                "Changes saved and applied",
                Vec::<String>::new(),
            )
            .persistent(true),
        ),
        (
            "Waits for a restart",
            Toast::new(
                ToastKind::Warning,
                "Saved",
                vec!["Restart the engine to apply the new node.".to_string()],
            )
            .persistent(true),
        ),
        (
            "Refused",
            Toast::new(
                ToastKind::Error,
                "Nothing saved",
                vec!["The URL must use https.".to_string()],
            )
            .show("book-refused")
            .persistent(true),
        ),
        (
            "Nothing to save",
            Toast::new(ToastKind::Neutral, "Nothing changed", Vec::<String>::new())
                .persistent(true),
        ),
    ]
}
