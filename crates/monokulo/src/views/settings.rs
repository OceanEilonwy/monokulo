//! The one way the site renders settings (issue #23): cards of settings in
//! one form, one save bar along the bottom of the window, and a toast after
//! a save. The admin settings page, the Account page, a store's settings
//! and a wallet's page all use these.
//!
//! Each piece is a custom element `static/settings-form.js` defines, but
//! the markup is complete here, so everything works without JavaScript: the
//! form is a real `<form method="post">`, the save bar is always shown, and
//! a save answers with the page again (a redirect with a toast, or the page
//! with the refusal and the values that were sent). With JavaScript the
//! components add the rest:
//!
//! - [`form`] is `<mk-settings-form>` around the `<form>`. It tracks what
//!   changed, asks before leaving with changes, and saves either with fixi
//!   (the panel is swapped in place) or with a plain POST and a reload
//!   ([`Save`]), per form. It sends `mk-dirty` (`{count, groups}`) when the
//!   number of changes moves, `mk-saved` (`{groups}`) when the page answers
//!   a save, and `mk-failed` (`{group, message}`) for each card a save
//!   refused.
//! - [`Card`] is `<mk-settings-card>`: a group of settings, with its title,
//!   a badge for its unsaved changes or a refusal, "Saved {at}" after a
//!   save, and a Discard button for its changes.
//! - [`Field`] is `<mk-setting>`: one setting, its label, help and control,
//!   with the "changed" mark an unsaved change shows.
//! - [`save_bar`] is `<mk-save-bar>`: the page's one Save, shown only while
//!   something is changed (or a save was refused) when JavaScript runs.
//! - [`toast_region`] is the corner of the window a save's toast shows in.
//!
//! A control counts as a setting when it is inside an `<mk-setting>`; the
//! admin page's node rows count their own changes (`static/admin-settings.js`
//! listens for `mk-count` on their card).

use std::fmt::Write as _;

use maud::{html, Markup, PreEscaped};

/// How a settings form saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Save<'a> {
    /// With fixi, swapping the answer into `target` (a selector), so the
    /// page isn't reloaded; without JavaScript, a plain POST.
    Fixi { target: &'a str },
    /// A plain POST, and the page it redirects to: for a page whose save
    /// changes more than the form (the nav, the heading).
    Reload,
}

/// The page's settings form: `<mk-settings-form>` around a real
/// `<form method="post" id="settings-form">` posting to `action`. `label`
/// names what has unsaved changes when leaving is asked about ("Abuse
/// protection has unsaved changes"). One per page.
pub fn form(action: &str, save: Save, label: &str, body: Markup) -> Markup {
    let target = match save {
        Save::Fixi { target } => Some(target),
        Save::Reload => None,
    };
    html! {
        mk-settings-form label=(label) {
            form method="post" action=(action) id="settings-form"
                fx-action=[target.map(|_| action)] fx-method=[target.map(|_| "POST")] fx-target=[target] {
                (body)
            }
        }
    }
}

/// A card's element id, from its name: `card-{name}`. Links to a card (the
/// save bar's, a toast's Show, the account menu's) use it.
pub fn card_id(name: &str) -> String {
    format!("card-{name}")
}

/// One card of settings, `<mk-settings-card name="…">`, built up and then
/// rendered around its fields with [`Card::render`].
#[derive(Debug, Clone)]
pub struct Card<'a> {
    name: &'a str,
    title: String,
    class: Option<&'a str>,
    attrs: Vec<(&'static str, String)>,
    head: Option<Markup>,
    badge: Option<Markup>,
    failed: bool,
    message: Option<&'a str>,
    saved: Option<String>,
    focus_saved: bool,
    readonly: bool,
    hidden: bool,
    shown_by: Option<String>,
}

impl<'a> Card<'a> {
    /// A card named `name` (its `name`, and `card-{name}` its id) with
    /// `title` for its heading.
    pub fn new(name: &'a str, title: impl Into<String>) -> Card<'a> {
        Card {
            name,
            title: title.into(),
            class: None,
            attrs: Vec::new(),
            head: None,
            badge: None,
            failed: false,
            message: None,
            saved: None,
            focus_saved: false,
            readonly: false,
            hidden: false,
            shown_by: None,
        }
    }

    /// Another class beside `settings-card`.
    pub fn class(mut self, class: &'a str) -> Self {
        self.class = Some(class);
        self
    }

    /// A `data-*` attribute of the page's own (a page script reads it).
    pub fn data(mut self, name: &'static str, value: impl ToString) -> Self {
        debug_assert!(name.starts_with("data-"), "{name}");
        self.attrs.push((name, value.to_string()));
        self
    }

    /// What follows the title in the card's head: a chip, a line of meta.
    pub fn head(mut self, head: Markup) -> Self {
        self.head = Some(head);
        self
    }

    /// A badge in the head while the card isn't refused ("Restart needed").
    pub fn badge(mut self, badge: Option<Markup>) -> Self {
        self.badge = badge;
        self
    }

    /// The last save refused this card: red, with "Not saved", and
    /// `message` (when given) at the top of its body.
    pub fn failed(mut self, failed: bool, message: Option<&'a str>) -> Self {
        self.failed = failed;
        self.message = message;
        self
    }

    /// The last save saved this card: "Saved" and `at` (a time, or
    /// nothing). `focus`: the mark takes focus once fixi swaps it in (the
    /// save bar it was pressed in has gone).
    pub fn saved(mut self, at: Option<&str>, focus: bool) -> Self {
        self.saved = at.map(str::to_string);
        self.focus_saved = focus;
        self
    }

    /// Nothing on it can be changed here: no Saved or Discard.
    pub fn readonly(mut self, readonly: bool) -> Self {
        self.readonly = readonly;
        self
    }

    /// Hidden now (JavaScript shows it as `shown_by` says).
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// Shown only while the checkbox `name` with `value` is ticked: at
    /// once with JavaScript (its controls disabled while hidden, so they
    /// aren't sent), after saving without.
    pub fn shown_by(mut self, name: &str, value: &str) -> Self {
        self.shown_by = Some(format!("{name}={value}"));
        self
    }

    /// The card around `body` (its settings).
    pub fn render(self, body: Markup) -> Markup {
        let id = card_id(self.name);
        let title_id = format!("{id}-title");
        let mut open = String::from("<mk-settings-card");
        attr(&mut open, "id", &id);
        let class = match self.class {
            Some(more) => format!("settings-card {more}"),
            None => "settings-card".to_string(),
        };
        let class = if self.failed {
            format!("{class} is-failed")
        } else {
            class
        };
        attr(&mut open, "class", &class);
        attr(&mut open, "name", self.name);
        for (name, value) in &self.attrs {
            attr(&mut open, name, value);
        }
        if let Some(shown_by) = &self.shown_by {
            attr(&mut open, "data-shown-by", shown_by);
        }
        if self.hidden {
            open.push_str(" hidden");
        }
        attr(&mut open, "role", "region");
        attr(&mut open, "aria-labelledby", &title_id);
        open.push('>');
        let focus = self.focus_saved.then_some("-1");
        html! {
            (PreEscaped(open))
            header class="card-head" {
                h3 id=(title_id) { (self.title) }
                @if let Some(head) = self.head { (head) }
                span class="card-state" data-card-state {
                    @if self.failed {
                        span class="badge badge-error" { "Not saved" }
                    } @else if let Some(badge) = self.badge {
                        (badge)
                    }
                }
                span class="card-spacer" {}
                @if self.readonly {
                    span class="card-meta" { "Can't be changed here" }
                } @else {
                    @if let (Some(at), false) = (&self.saved, self.failed) {
                        span class="card-meta card-saved" data-card-saved tabindex=[focus] data-fx-focus[self.focus_saved] {
                            "Saved" @if !at.is_empty() { " " (at) }
                        }
                    }
                    button type="button" class="card-discard js-only" data-card-discard hidden { "Discard" }
                }
            }
            div class="card-body" {
                @if let Some(message) = self.message {
                    p class="error" role="alert" { (message) }
                }
                (body)
            }
            (PreEscaped("</mk-settings-card>"))
        }
    }
}

/// A card for what isn't saved with the bar (actions with buttons of their
/// own: add a webhook, move the keys): the same look, nothing tracked.
pub fn plain_card(name: &str, title: &str, body: Markup) -> Markup {
    plain_card_with_meta(name, title, None, body)
}

/// [`plain_card`] with a line of meta at the end of its head ("3
/// webhooks").
pub fn plain_card_with_meta(name: &str, title: &str, meta: Option<&str>, body: Markup) -> Markup {
    let id = card_id(name);
    html! {
        section id=(id) class="settings-card" aria-labelledby=(format!("{id}-title")) {
            header class="card-head" {
                h3 id=(format!("{id}-title")) { (title) }
                @if let Some(meta) = meta {
                    span class="card-spacer" {}
                    span class="card-meta" { (meta) }
                }
            }
            div class="card-body" { (body) }
        }
    }
}

/// One setting, `<mk-setting>`, built up and then rendered around its
/// control with [`Field::render`].
#[derive(Debug, Clone)]
pub struct Field<'a> {
    label: &'a str,
    /// The id of the control the label is for; for a group (radio cards,
    /// checkboxes), the label's own id, which names the group.
    id: &'a str,
    group: bool,
    chip: Option<Markup>,
    help: Option<(Option<&'a str>, Markup)>,
}

impl<'a> Field<'a> {
    /// A setting whose one control has the id `control`.
    pub fn new(label: &'a str, control: &'a str) -> Field<'a> {
        Field {
            label,
            id: control,
            group: false,
            chip: None,
            help: None,
        }
    }

    /// A setting made of several controls (radio buttons, checkboxes): a
    /// group named by its label, which gets the id `id`.
    pub fn group(label: &'a str, id: &'a str) -> Field<'a> {
        Field {
            group: true,
            ..Field::new(label, id)
        }
    }

    /// Beside the label: where its value comes from.
    pub fn chip(mut self, chip: Markup) -> Self {
        self.chip = Some(chip);
        self
    }

    /// What it's for, under the label; `id` for the control to point at.
    pub fn help(mut self, id: Option<&'a str>, help: Markup) -> Self {
        self.help = Some((id, help));
        self
    }

    /// The setting around `control` (and anything under it).
    pub fn render(self, control: Markup) -> Markup {
        let help_id = self.help.as_ref().and_then(|(id, _)| *id);
        let help = html! {
            @if let Some((id, help)) = &self.help {
                p class="field-help" id=[id] { (help) }
            }
        };
        html! {
            @if self.group {
                mk-setting class="setting-field" role="group" aria-labelledby=(self.id) aria-describedby=[help_id] {
                    div class="setting-label-row" {
                        span class="setting-label" id=(self.id) { (self.label) }
                        @if let Some(chip) = &self.chip { (chip) }
                        span class="changed-mark" { "changed" }
                    }
                    (help)
                    (control)
                }
            } @else {
                mk-setting class="setting-field" {
                    div class="setting-label-row" {
                        label class="setting-label" for=(self.id) { (self.label) }
                        @if let Some(chip) = &self.chip { (chip) }
                        span class="changed-mark" { "changed" }
                    }
                    (help)
                    (control)
                }
            }
        }
    }
}

/// The page's one save bar, `<mk-save-bar>`, along the bottom of the
/// window: `message`, then Discard changes (a link to `discard_href`, the
/// page as saved) and Save. Red after a save that refused something
/// (`failed`); `focus` gives its message focus after a save fixi swapped in.
pub fn save_bar(failed: bool, focus: bool, message: Markup, discard_href: &str) -> Markup {
    html! {
        mk-save-bar id="save-bar" class={ "save-bar" @if failed { " is-failed" } } role="region" aria-label="Save changes" {
          div class="wrap save-bar-inner" {
            p class="save-bar-message" data-save-bar-message tabindex="-1" data-fx-focus[focus] {
                (message)
            }
            div class="save-bar-actions" {
                a class="btn" href=(discard_href) data-discard-all { "Discard changes" }
                button type="submit" class="btn-primary" data-save { "Save" }
            }
          }
        }
    }
}

/// A small message after a save, in the corner of the window (it fades
/// on its own unless it says something wasn't saved).
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: String,
    pub lines: Vec<String>,
    /// The card to go to (its name): the one a refusal is about.
    pub show: Option<String>,
}

impl Toast {
    pub fn new(kind: ToastKind, title: impl Into<String>) -> Toast {
        Toast {
            kind,
            title: title.into(),
            lines: Vec::new(),
            show: None,
        }
    }

    pub fn line(mut self, line: impl Into<String>) -> Toast {
        self.lines.push(line.into());
        self
    }

    pub fn show(mut self, card: impl Into<String>) -> Toast {
        self.show = Some(card.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The corner of the window a settings page's toast shows in, with `toast`
/// in it. A new one comes with every save, so saving twice shows twice.
/// Without JavaScript it fades by itself (CSS); with it, one saying
/// something wasn't saved stays until it's closed. `oob` marks it for fixi's
/// glue to put in place of the page's own when a swap brings it.
pub fn toast_region(toast: Option<&Toast>, oob: bool) -> Markup {
    html! {
        div id="settings-toasts" class="toasts" data-fx-oob[oob] {
            @if let Some(toast) = toast {
                @let (class, icon, role) = match toast.kind {
                    ToastKind::Success => ("toast toast-success", "\u{2713}", "status"),
                    ToastKind::Warning => ("toast toast-warning", "!", "status"),
                    ToastKind::Error => ("toast toast-error", "!", "alert"),
                    ToastKind::Neutral => ("toast toast-neutral", "\u{2022}", "status"),
                };
                div class=(class) role=(role) data-toast {
                    span class="toast-icon" aria-hidden="true" { (icon) }
                    div class="toast-text" {
                        strong { (toast.title) }
                        @for line in &toast.lines { span class="toast-line" { (line) } }
                        @if let Some(card) = &toast.show {
                            a class="toast-show" href=(format!("#{}", card_id(card))) data-show-card=(card) { "Show" }
                        }
                    }
                    button type="button" class="toast-close js-only" aria-label="Dismiss" data-toast-close { "\u{00D7}" }
                }
            }
        }
    }
}

/// ` name="value"`, escaped, onto an opening tag being written by hand (a
/// card's attributes vary by page).
fn attr(tag: &mut String, name: &str, value: &str) {
    tag.push(' ');
    tag.push_str(name);
    tag.push_str("=\"");
    let _ = write!(maud::Escaper::new(tag), "{value}");
    tag.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_saves_with_fixi_or_reloads_as_its_page_asks() {
        let fixi = form(
            "/x",
            Save::Fixi {
                target: "#settings-panel",
            },
            "Payments",
            html! { "body" },
        )
        .into_string();
        assert_eq!(
            fixi,
            r##"<mk-settings-form label="Payments"><form method="post" action="/x" id="settings-form" fx-action="/x" fx-method="POST" fx-target="#settings-panel">body</form></mk-settings-form>"##
        );
        let reload = form("/y", Save::Reload, "Profile", html! {}).into_string();
        assert_eq!(
            reload,
            r#"<mk-settings-form label="Profile"><form method="post" action="/y" id="settings-form"></form></mk-settings-form>"#
        );
    }

    #[test]
    fn a_card_says_how_the_last_save_went_and_offers_discard() {
        let clean = Card::new("time", "Time")
            .render(html! { "fields" })
            .into_string();
        assert_eq!(
            clean,
            r#"<mk-settings-card id="card-time" class="settings-card" name="time" role="region" aria-labelledby="card-time-title"><header class="card-head"><h3 id="card-time-title">Time</h3><span class="card-state" data-card-state></span><span class="card-spacer"></span><button type="button" class="card-discard js-only" data-card-discard hidden>Discard</button></header><div class="card-body">fields</div></mk-settings-card>"#
        );
        let saved = Card::new("time", "Time")
            .saved(Some("14:02"), true)
            .render(html! {})
            .into_string();
        assert!(
            saved.contains(r#"<span class="card-meta card-saved" data-card-saved tabindex="-1" data-fx-focus>Saved 14:02</span>"#),
            "{saved}"
        );
        let just_saved = Card::new("time", "Time")
            .saved(Some(""), false)
            .render(html! {})
            .into_string();
        assert!(
            just_saved.contains(r#"data-card-saved>Saved</span>"#),
            "{just_saved}"
        );
        let failed = Card::new("time", "Time")
            .failed(true, Some("That zone isn't one we know."))
            .saved(Some("14:02"), false)
            .render(html! {})
            .into_string();
        assert!(
            failed
                .starts_with(r#"<mk-settings-card id="card-time" class="settings-card is-failed""#),
            "{failed}"
        );
        assert!(
            failed.contains(r#"<span class="badge badge-error">Not saved</span>"#),
            "{failed}"
        );
        assert!(
            failed.contains(r#"<div class="card-body"><p class="error" role="alert">That zone isn't one we know.</p>"#),
            "{failed}"
        );
        assert!(!failed.contains("data-card-saved"), "{failed}");
        let readonly = Card::new("cli", "Command line")
            .readonly(true)
            .render(html! {})
            .into_string();
        assert!(readonly.contains("Can't be changed here"), "{readonly}");
        assert!(!readonly.contains("data-card-discard"), "{readonly}");
    }

    #[test]
    fn a_cards_own_attributes_are_escaped() {
        let card = Card::new("custody-snp", "SEV-SNP")
            .class("node-network")
            .data("data-note", "a \"quoted\" <b>")
            .shown_by("key_custody.enabled_backends", "snp")
            .hidden(true)
            .render(html! {})
            .into_string();
        assert!(
            card.starts_with(r#"<mk-settings-card id="card-custody-snp" class="settings-card node-network" name="custody-snp" data-note="a &quot;quoted&quot; &lt;b&gt;" data-shown-by="key_custody.enabled_backends=snp" hidden role="region""#),
            "{card}"
        );
    }

    #[test]
    fn a_setting_has_its_label_its_changed_mark_and_its_help() {
        let field = Field::new("Name", "name")
            .help(Some("name-help"), html! { "Only you see it." })
            .render(html! { input id="name" name="name"; })
            .into_string();
        assert_eq!(
            field,
            r#"<mk-setting class="setting-field"><div class="setting-label-row"><label class="setting-label" for="name">Name</label><span class="changed-mark">changed</span></div><p class="field-help" id="name-help">Only you see it.</p><input id="name" name="name"></mk-setting>"#
        );
        let group = Field::group("Theme", "theme-label")
            .render(html! {})
            .into_string();
        assert_eq!(
            group,
            r#"<mk-setting class="setting-field" role="group" aria-labelledby="theme-label"><div class="setting-label-row"><span class="setting-label" id="theme-label">Theme</span><span class="changed-mark">changed</span></div></mk-setting>"#
        );
    }

    #[test]
    fn the_save_bar_saves_and_discards() {
        let bar = save_bar(true, false, html! { "Nothing saved." }, "/account").into_string();
        assert_eq!(
            bar,
            r#"<mk-save-bar id="save-bar" class="save-bar is-failed" role="region" aria-label="Save changes"><div class="wrap save-bar-inner"><p class="save-bar-message" data-save-bar-message tabindex="-1">Nothing saved.</p><div class="save-bar-actions"><a class="btn" href="/account" data-discard-all>Discard changes</a><button type="submit" class="btn-primary" data-save>Save</button></div></div></mk-save-bar>"#
        );
    }

    #[test]
    fn a_toast_links_to_the_card_it_is_about() {
        let toast = Toast::new(ToastKind::Error, "Not saved")
            .line("Pick a zone.")
            .show("time");
        let html = toast_region(Some(&toast), false).into_string();
        assert!(
            html.contains(r#"<div class="toast toast-error" role="alert" data-toast>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r##"<a class="toast-show" href="#card-time" data-show-card="time">Show</a>"##
            ),
            "{html}"
        );
    }
}
