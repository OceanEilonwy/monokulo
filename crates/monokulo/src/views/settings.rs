//! The one way the site renders settings (issue #23): cards of settings in
//! one form, one save bar along the bottom of the window, and a toast after
//! a save. The admin settings page, the Account page, a store's settings
//! and a wallet's page all use these.
//!
//! Each piece is a `ui` component (`ui::SettingsForm`, `SettingsCard`,
//! `Setting`, `SaveBar`, `Toast`), complete without JavaScript: the form is
//! a real `<form method="post">`, the save bar is always shown, and a save
//! answers with the page again (a redirect with a toast, or the page with
//! the refusal and the values that were sent). Each is an island too; with
//! JavaScript their behaviours track what changed, mark it, offer Discard,
//! ask before leaving with changes, and save with fixi or a plain POST
//! ([`Save`]). See the `ui` crate's `settings` module for the events page
//! scripts hear (`mk-dirty`, `mk-saved`, `mk-failed`, `mk-count`,
//! `mk-discard`) and send (`mk-changed`).
//!
//! These builders are the site's: they say a page's settings in its own
//! terms and hand them to the components. A control counts as a setting
//! when it is inside a [`Field`]; the admin page's node rows count their
//! own changes (`static/admin-settings.js` listens for `mk-count` on their
//! card).

use maud::{html, Markup};
pub use ui::ToastKind;

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

/// The page's settings form, a real `<form method="post" id="settings-form">`
/// posting to `action`. `label` names what has unsaved changes when leaving
/// is asked about ("Abuse protection has unsaved changes"). One per page.
pub fn form(action: &str, save: Save, label: &str, body: Markup) -> Markup {
    let target = match save {
        Save::Fixi { target } => Some(target.to_string()),
        Save::Reload => None,
    };
    ui::SettingsForm::new(action, label, body)
        .maybe_fixi_target(target)
        .render()
}

/// A card's element id, from its name: `card-{name}`. Links to a card (the
/// save bar's, a toast's Show, the account menu's) use it.
pub fn card_id(name: &str) -> String {
    format!("card-{name}")
}

/// One card of settings, built up and then rendered around its fields with
/// [`Card::render`].
#[derive(Debug, Clone)]
pub struct Card<'a> {
    name: &'a str,
    title: String,
    class: Option<&'a str>,
    attrs: Vec<(&'static str, String)>,
    head: Option<Markup>,
    link: Option<Markup>,
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
    /// A card named `name` (and `card-{name}` its id) with `title` for its
    /// heading.
    pub fn new(name: &'a str, title: impl Into<String>) -> Card<'a> {
        Card {
            name,
            title: title.into(),
            class: None,
            attrs: Vec::new(),
            head: None,
            link: None,
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

    /// A link at the right of the card's head, to where more about it is
    /// shown ("See it on the engine page").
    pub fn link(mut self, link: Markup) -> Self {
        self.link = Some(link);
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
        let mut card = ui::SettingsCard::new(self.name, self.title, body)
            .maybe_extra_class(self.class.map(str::to_string))
            .maybe_head(self.head.map(ui::Slot::from))
            .maybe_link(self.link.map(ui::Slot::from))
            .maybe_badge(self.badge.map(ui::Slot::from))
            .failed(self.failed)
            .maybe_message(self.message.map(str::to_string))
            .maybe_saved(self.saved)
            .focus_saved(self.focus_saved)
            .readonly(self.readonly)
            .hidden(self.hidden)
            .maybe_shown_by(self.shown_by);
        for (name, value) in self.attrs {
            card = card.attr(name, value);
        }
        card.render()
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

/// One setting, built up and then rendered around its control with
/// [`Field::render`].
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
        let help_id = self
            .help
            .as_ref()
            .and_then(|(id, _)| id.map(str::to_string));
        ui::Setting::new(self.label, self.id, control)
            .group(self.group)
            .maybe_chip(self.chip.map(ui::Slot::from))
            .maybe_help(self.help.map(|(_, help)| ui::Slot::from(help)))
            .maybe_described_by(help_id.clone().filter(|_| self.group))
            .maybe_help_id(help_id)
            .render()
    }
}

/// The page's one save bar, along the bottom of the window: `message`,
/// then Discard changes (a link to `discard_href`, the page as saved) and
/// Save. Red after a save that refused something (`failed`); `focus` gives
/// its message focus after a save fixi swapped in.
pub fn save_bar(failed: bool, focus: bool, message: Markup, discard_href: &str) -> Markup {
    ui::SaveBar::new(message, discard_href)
        .failed(failed)
        .focus(focus)
        .render()
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

/// The corner of the window a settings page's toast shows in, with `toast`
/// in it. A new one comes with every save, so saving twice shows twice.
/// `oob` marks it for fixi's glue to put in place of the page's own when a
/// swap brings it.
pub fn toast_region(toast: Option<&Toast>, oob: bool) -> Markup {
    html! {
        div id="settings-toasts" class="toasts" data-fx-oob[oob] {
            @if let Some(toast) = toast {
                (ui::Toast::new(toast.kind, toast.title.clone(), toast.lines.clone()).maybe_show(toast.show.clone()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The markup without the islands' props (the JSON the behaviours read),
    /// which the `ui` crate's tests cover.
    fn markup(html: Markup) -> String {
        let html = html.into_string();
        let mut out = String::new();
        let mut rest = html.as_str();
        while let Some(at) = rest.find(" data-props=\"") {
            out.push_str(&rest[..at]);
            let after = &rest[at + " data-props=\"".len()..];
            rest = &after[after.find('"').expect("closed") + 1..];
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn a_form_saves_with_fixi_or_reloads_as_its_page_asks() {
        let fixi = markup(form(
            "/x",
            Save::Fixi {
                target: "#settings-panel",
            },
            "Payments",
            html! { "body" },
        ));
        assert_eq!(
            fixi,
            r##"<div class="mk-settings-form" data-label="Payments" data-mk="1" data-island="SettingsForm"><form method="post" action="/x" id="settings-form" fx-action="/x" fx-method="POST" fx-target="#settings-panel" data-mk="2">body</form></div>"##
        );
        let reload = markup(form("/y", Save::Reload, "Profile", html! {}));
        assert_eq!(
            reload,
            r#"<div class="mk-settings-form" data-label="Profile" data-mk="1" data-island="SettingsForm"><form method="post" action="/y" id="settings-form" data-mk="2"></form></div>"#
        );
    }

    #[test]
    fn a_card_says_how_the_last_save_went() {
        let clean = markup(Card::new("time", "Time").render(html! { "fields" }));
        assert_eq!(
            clean,
            r#"<section id="card-time" class="settings-card" data-name="time" role="region" aria-labelledby="card-time-title" data-mk="1" data-island="SettingsCard"><header class="card-head"><h3 id="card-time-title">Time</h3><span class="card-state" data-card-state><!--mk:2--></span><span class="card-spacer"></span><!--mk:5--></header><div class="card-body">fields</div></section>"#
        );
        let saved = markup(
            Card::new("time", "Time")
                .saved(Some("14:02"), true)
                .render(html! {}),
        );
        assert!(
            saved.contains(r#"<span class="card-meta card-saved" data-card-saved tabindex="-1" data-fx-focus data-mk="4">Saved 14:02</span>"#),
            "{saved}"
        );
        let just_saved = markup(
            Card::new("time", "Time")
                .saved(Some(""), false)
                .render(html! {}),
        );
        assert!(
            just_saved.contains(r#"data-card-saved data-mk="4">Saved</span>"#),
            "{just_saved}"
        );
        let failed = markup(
            Card::new("time", "Time")
                .failed(true, Some("That zone isn't one we know."))
                .saved(Some("14:02"), false)
                .render(html! {}),
        );
        assert!(
            failed.starts_with(r#"<section id="card-time" class="settings-card is-failed""#),
            "{failed}"
        );
        assert!(
            failed.contains(r#"<span class="mk-status-text" data-tone="error" data-card-failed data-mk="3">Not saved</span>"#),
            "{failed}"
        );
        assert!(
            failed.contains(r#"<div class="card-body"><p class="error" role="alert" data-mk="6">That zone isn't one we know.</p>"#),
            "{failed}"
        );
        assert!(!failed.contains("data-card-saved"), "{failed}");
        let readonly = markup(
            Card::new("cli", "Command line")
                .readonly(true)
                .render(html! {}),
        );
        assert!(readonly.contains("Can't be changed here"), "{readonly}");
        // Discard is the behaviour's: only where something can change.
        assert!(!readonly.contains("<!--mk:5-->"), "{readonly}");
    }

    #[test]
    fn a_cards_own_attributes_are_escaped() {
        let card = markup(
            Card::new("custody-snp", "SEV-SNP")
                .class("node-network")
                .data("data-note", "a \"quoted\" <b>")
                .shown_by("key_custody.enabled_backends", "snp")
                .hidden(true)
                .render(html! {}),
        );
        assert!(
            card.starts_with(r#"<section id="card-custody-snp" class="settings-card node-network" data-name="custody-snp" data-shown-by="key_custody.enabled_backends=snp" hidden role="region" aria-labelledby="card-custody-snp-title" data-mk="1" data-island="SettingsCard" data-note="a &quot;quoted&quot; &lt;b&gt;""#),
            "{card}"
        );
    }

    #[test]
    fn a_setting_has_its_label_its_changed_mark_and_its_help() {
        let field = markup(
            Field::new("Name", "name")
                .help(Some("name-help"), html! { "Only you see it." })
                .render(html! { input id="name" name="name"; }),
        );
        assert_eq!(
            field,
            r#"<div class="setting-field" data-mk="1" data-island="Setting"><div class="setting-label-row"><label class="setting-label" for="name">Name</label><span class="changed-mark">changed</span></div><p class="field-help" id="name-help">Only you see it.</p><input id="name" name="name"></div>"#
        );
        let group = markup(Field::group("Theme", "theme-label").render(html! {}));
        assert_eq!(
            group,
            r#"<div class="setting-field" role="group" aria-labelledby="theme-label" data-mk="1" data-island="Setting"><div class="setting-label-row"><span class="setting-label" id="theme-label">Theme</span><span class="changed-mark">changed</span></div></div>"#
        );
    }

    #[test]
    fn the_save_bar_saves_and_discards() {
        let bar = markup(save_bar(
            true,
            false,
            html! { "Nothing saved." },
            "/account",
        ));
        assert_eq!(
            bar,
            r#"<div id="save-bar" class="save-bar is-failed" role="region" aria-label="Save changes" data-mk="1" data-island="SaveBar"><div class="wrap save-bar-inner"><p class="save-bar-message" data-save-bar-message tabindex="-1"><span data-mk="2">Nothing saved.</span><!--mk:3--></p><div class="save-bar-actions" data-mk="4"><a class="mk-btn" href="/account" data-discard-all data-mk="5">Discard changes</a><button type="submit" class="mk-btn" data-variant="primary" data-save data-mk="6"><!--mk:7-->Save<!--/mk:7--></button></div><!--mk:8--></div></div>"#
        );
    }

    #[test]
    fn a_toast_links_to_the_card_it_is_about() {
        let toast = Toast::new(ToastKind::Error, "Not saved")
            .line("Pick a zone.")
            .show("time");
        let html = markup(toast_region(Some(&toast), false));
        assert!(
            html.contains(r#"<div class="toast toast-error" role="alert" data-toast data-mk="1" data-island="Toast">"#),
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
