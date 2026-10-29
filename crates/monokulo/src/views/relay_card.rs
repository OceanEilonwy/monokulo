//! The Server tab's Push relay card (the push relay inside monokulo,
//! `crate::relay`): `relay.enabled`, the relay's state as chips, one panel
//! saying what is missing and how to fix it, and, opened (`?relay=edit`, a
//! plain link), every `[relay.*]` setting in sub-sections, its apps as rows.
//! Everything in it comes from the relay's handle (`relay::Handle::view`)
//! and the relay's own registry, so it can't say the relay listens where it
//! doesn't.

use maud::{html, Markup, PreEscaped};

use super::{
    card, field_id, help_id, icon, scalar_field, source_chip, AdminScalarFieldView,
    AdminSettingsViewModel, Group, SettingKindView, SettingsTab,
};
use crate::relay::{failure_fix, Missing, RelayState, RelayView, NOT_RUNNING};
use crate::views::settings::Field;

/// The card's state, as the handler read it.
#[derive(Debug, Clone)]
pub struct RelayCardView {
    pub relay: RelayView,
    /// Showing every setting (`?relay=edit`, or a refused save of it).
    pub open: bool,
    /// The relay's apps as rows, as saved, or as a refused save or a row's
    /// button sent them.
    pub apps: Vec<RelayAppRow>,
    /// How many apps are saved: the page's script counts what changed
    /// against it.
    pub saved_apps: usize,
    /// When it started listening, or last failed to, in the admin's clock
    /// ("3 days ago").
    pub since: Option<String>,
}

/// One app of `handles.apps`, as a row of the card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayAppRow {
    pub name: String,
    pub apns_topic: String,
    pub fcm_project_id: String,
    /// The saved app at its place, when what the row shows isn't what is
    /// saved (a refused save, a row's button): for the page's script, and
    /// Discard.
    pub saved: Option<Box<RelayAppRow>>,
    /// Whether it is a saved app at all (one added since isn't).
    pub is_saved: bool,
    /// What a refused save found wrong with it.
    pub errors: Vec<(RelayAppField, String)>,
}

/// A part of an app's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayAppField {
    Name,
    ApnsTopic,
    FcmProjectId,
}

impl RelayAppField {
    pub const ALL: [RelayAppField; 3] = [
        RelayAppField::Name,
        RelayAppField::ApnsTopic,
        RelayAppField::FcmProjectId,
    ];

    /// The part's name in a row's form fields (`relay_app_0_name`).
    pub fn id(self) -> &'static str {
        match self {
            RelayAppField::Name => "name",
            RelayAppField::ApnsTopic => "apns_topic",
            RelayAppField::FcmProjectId => "fcm_project_id",
        }
    }

    fn label(self) -> &'static str {
        match self {
            RelayAppField::Name => "Name",
            RelayAppField::ApnsTopic => "iOS bundle ID (apns_topic)",
            RelayAppField::FcmProjectId => "Firebase project ID (fcm_project_id)",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            RelayAppField::Name => "monokular",
            RelayAppField::ApnsTopic => "com.example.app",
            RelayAppField::FcmProjectId => "my-project",
        }
    }
}

impl RelayAppRow {
    pub fn value(&self, field: RelayAppField) -> &str {
        match field {
            RelayAppField::Name => &self.name,
            RelayAppField::ApnsTopic => &self.apns_topic,
            RelayAppField::FcmProjectId => &self.fcm_project_id,
        }
    }
}

/// The name a row's part is sent under.
pub fn app_field_name(at: usize, field: RelayAppField) -> String {
    format!("relay_app_{at}_{}", field.id())
}

/// The field saying the rows were on the form, so none left means no app.
pub const APPS_SENT: &str = "relay_apps";

/// The name of a row button (`remove-app:<at>`, `add-app`).
pub const APP_ACTION: &str = "relay_action";

/// `handles.apps`, the relay's key edited as rows.
pub const APPS_KEY: &str = "handles.apps";

/// The link that opens the card, and the one that closes it.
pub fn card_href(open: bool) -> String {
    if open {
        format!("{}&relay=edit#card-relay", SettingsTab::Server.href())
    } else {
        format!("{}#card-relay", SettingsTab::Server.href())
    }
}

/// The head's words for how the relay stands.
fn state_words(card: &RelayCardView) -> String {
    match (&card.relay.state, &card.since) {
        (RelayState::Running { .. }, Some(since)) => format!("On, since {since}"),
        (RelayState::Running { .. }, None) => "On".to_owned(),
        (RelayState::Failed { .. }, _) => "Not running".to_owned(),
        (RelayState::Off, _) if card.relay.enabled => "Not running".to_owned(),
        (RelayState::Off, _) => "Off".to_owned(),
    }
}

fn pencil() -> Markup {
    icon(html! { path d="M4 20h4L19 9l-4-4L4 16z" {} path d="m13.5 6.5 4 4" {} })
}

fn cross() -> Markup {
    icon(html! { path d="M6 6l12 12M18 6 6 18" {} })
}

fn antenna() -> Markup {
    icon(html! {
        path d="M12 12v9" {}
        circle cx="12" cy="10" r="2" {}
        path d="M7.8 5.8a6 6 0 0 0 0 8.4" {}
        path d="M16.2 5.8a6 6 0 0 1 0 8.4" {}
    })
}

/// The head's right side: the state in words, and Edit settings or Close,
/// a link (fixi swaps the panel, as the tab links do; the URL changes).
fn head_link(card: &RelayCardView) -> Markup {
    let href = crate::base::url(card_href(!card.open));
    let (words, glyph) = if card.open {
        ("Close", cross())
    } else {
        ("Edit settings", pencil())
    };
    html! {
        span class="relay-head" {
            span class="relay-state-words" { (state_words(card)) }
            (ui::LinkButton::new(html! { (glyph) (words) }, href.clone())
                .size(ui::ControlSize::Compact)
                .attr("fx-action", href)
                .attr("fx-target", "#settings-panel")
                .attr("fx-push-url", "")
                .attr("data-relay-toggle", ""))
        }
    }
}

/// The relay's state, one chip each, in order: where it listens, its
/// public address, its apps, the handles stored, and which platform is
/// ready.
fn chips(relay: &RelayView) -> Markup {
    let apps = match relay.apps.as_slice() {
        [] => "no apps".to_owned(),
        [one] => format!("1 app: {one}"),
        all => format!("{} apps: {}", all.len(), all.join(", ")),
    };
    let running = matches!(relay.state, RelayState::Running { .. });
    let handles = match relay.handles_stored {
        Some(0) => "no handles stored".to_owned(),
        Some(1) => "1 handle".to_owned(),
        Some(n) => format!("{} handles", crate::views::scaling::thousands(n)),
        None => "handles not counted".to_owned(),
    };
    let ready = |name: &str, ready: bool, missing: &str| {
        if ready {
            ui::Tag::new(format!("{name} ready"))
                .tone(ui::TagTone::Ok)
                .icon(html! { (PreEscaped(ui::assets::icons::CHECK)) })
        } else {
            ui::Tag::new(missing.to_owned())
                .tone(ui::TagTone::Warning)
                .icon(html! { (PreEscaped(ui::assets::icons::WARNING)) })
        }
    };
    html! {
        div class="relay-chips" role="group" aria-labelledby="relay-chips-title" {
            h4 id="relay-chips-title" class="visually-hidden" { "The relay's state" }
            @if let RelayState::Running { address, .. } = &relay.state {
                (ui::Tag::new(format!("Listening on {address}")).icon(antenna()))
            }
            @if let Some(uri) = &relay.public_uri {
                (ui::Tag::new(html! { code { (uri) } }).icon(html! { (PreEscaped(ui::assets::icons::GLOBE)) }))
            }
            (ui::Tag::new(apps))
            @if running { (ui::Tag::new(handles)) }
            (ready("FCM", relay.fcm_ready, "FCM credentials not set"))
            (ready("APNs", relay.apns_ready, "APNs key not set"))
        }
    }
}

/// One thing the panel says: in bold what is wrong, then what to do.
fn item(title: &str, text: &str) -> Markup {
    html! { p { strong { (title) } " " (text) } }
}

/// The one panel under the chips: why the relay isn't running, or what is
/// missing and how to fix it; none when all is well.
fn panel(relay: &RelayView) -> Markup {
    if let RelayState::Failed { error, .. } = &relay.state {
        return ui::Alert::new(item(NOT_RUNNING, &failure_fix(error)), ui::Tone::Error)
            .icon(html! { (PreEscaped(ui::assets::icons::WARNING)) })
            .block(true)
            .quiet(true)
            .attr("data-relay-panel", "")
            .render();
    }
    let mut items = Vec::new();
    for missing in &relay.missing {
        let title = match missing {
            Missing::PublicUri => "No public address.",
            Missing::HandleKey => "No handle key.",
        };
        items.push(item(title, missing.sentence()));
    }
    if !relay.apns_ready {
        items.push(item(
            "Apple (APNs) key not set.",
            // The relay's settings refuse a team or key ID without the key,
            // so none is saved while it isn't set, and none can be until it is.
            "iOS registrations are refused until MONOKULO_RELAY_APNS_KEY is set in the environment and monokulo restarts. No team ID or key ID is saved for it yet: once the key is set, set them under Edit settings.",
        ));
    }
    if !relay.fcm_ready {
        items.push(item(
            "Google (FCM) credentials not set.",
            "Android registrations are refused until MONOKULO_RELAY_FCM_CREDENTIALS is set in the environment and monokulo restarts.",
        ));
    }
    if items.is_empty() {
        return html! {};
    }
    ui::Alert::new(html! { @for item in &items { (item) } }, ui::Tone::Warning)
        .icon(html! { (PreEscaped(ui::assets::icons::WARNING)) })
        .block(true)
        .quiet(true)
        .attr("data-relay-panel", "")
        .render()
}

/// The sub-section a relay setting starts, before it.
fn subheading(key: &str) -> Option<&'static str> {
    match key {
        "server.listen" => Some("Server"),
        "handles.ttl_days" => Some("Handles and apps"),
        "limits.per_sender_per_minute" => Some("Limits, a minute each"),
        "apns.endpoint" => Some("Apple (APNs)"),
        "fcm.endpoint" => Some("Google (FCM)"),
        _ => None,
    }
}

/// A secret of the relay's, from the environment alone: set (dots, never
/// its value) or not set (in the warning tone), with the variable to set.
/// No `name`, so never sent.
fn secret_field(field: &AdminScalarFieldView, note: &str) -> Markup {
    let control = field_id(field.form_name());
    let help = help_id(field);
    let set = !field.value.is_empty();
    let mut setting = Field::new(&field.label, &control).chip(source_chip(field.source));
    if let Some(text) = &field.help {
        setting = setting.help(help.as_deref(), html! { (text) });
    }
    let locked = ui::LockedValue::new(if set { "" } else { "Not set" })
        .id(control.clone())
        .secret(set)
        .warning(!set);
    let locked = match help.clone() {
        Some(help) => locked.described_by(help),
        None => locked,
    };
    setting.render(html! {
        (locked)
        @if set {
            (ui::FieldNote::new(note))
        } @else {
            (ui::FieldNote::new(note).tone(ui::NoteTone::Pending))
        }
        (super::field_status(field))
    })
}

/// What a relay secret's note says when it isn't set.
fn unset_secret_note(key: &str, env_var: &str) -> String {
    let until = match key {
        "apns.key" => "Until then iOS registrations are refused.",
        "fcm.credentials" => "Until then Android registrations are refused.",
        _ => "The relay can't start until it is set.",
    };
    format!("Set {env_var} in the environment and restart monokulo. {until}")
}

/// The apps (`handles.apps`) as rows: name, iOS bundle ID and Firebase
/// project ID, each with Remove, then Add an app. Remove and Add are the
/// tab form's submit buttons: without JavaScript they show the tab again
/// with the rows changed and nothing saved; with it they act in place and
/// count as a change, saved with the bar.
fn apps_field(field: &AdminScalarFieldView, card: &RelayCardView) -> Markup {
    let group_id = format!("{}-label", field_id(field.form_name()));
    let help = help_id(field);
    let mut setting = Field::group(&field.label, &group_id).chip(source_chip(field.source));
    if let Some(text) = &field.help {
        setting = setting.help(help.as_deref(), html! { (text) });
    }
    let editable = field.access == super::Access::Editable;
    let setting = if editable {
        setting.counted_by_page()
    } else {
        setting
    };
    setting.stacked().render(html! {
        @if editable {
            div class="relay-apps" data-relay-apps data-saved-count=(card.saved_apps) {
                input type="hidden" name=(APPS_SENT) value="";
                @for (at, row) in card.apps.iter().enumerate() {
                    (app_row(at, row))
                }
                template data-relay-app-template {
                    (app_row(usize::MAX, &RelayAppRow::default()))
                }
                (ui::Button::new(html! { (PreEscaped(ui::assets::icons::PLUS)) "Add an app" })
                    .kind(ui::ButtonType::Submit)
                    .attr("name", APP_ACTION)
                    .attr("value", "add-app")
                    .flag("data-relay-app-add"))
            }
        } @else {
            ul class="relay-apps-locked" {
                @for row in &card.apps {
                    li { code { (row.name) } @if !row.apns_topic.is_empty() { " · iOS " code { (row.apns_topic) } } @if !row.fcm_project_id.is_empty() { " · Android " code { (row.fcm_project_id) } } }
                }
            }
            @if let super::Access::Locked(reason) = &field.access { (ui::FieldNote::new(reason)) }
        }
        // Its example is the setting's JSON, which the rows stand for.
        (super::field_status(&AdminScalarFieldView { example: None, ..field.clone() }))
    })
}

/// One app's row; `at` is `usize::MAX` for the script's blank template
/// (`__i__` in its names, numbered as it is added).
fn app_row(at: usize, row: &RelayAppRow) -> Markup {
    let index = if at == usize::MAX {
        "__i__".to_owned()
    } else {
        at.to_string()
    };
    let name = |field: RelayAppField| format!("relay_app_{index}_{}", field.id());
    let id = |field: RelayAppField| format!("relay-app-{index}-{}", field.id());
    let shown_name = if row.name.is_empty() {
        "this app".to_owned()
    } else {
        format!("the app {}", row.name)
    };
    html! {
        div class="relay-app" data-relay-app=(index) data-relay-app-saved[row.is_saved] {
            @for field in RelayAppField::ALL {
                @let error = row.errors.iter().find(|(f, _)| *f == field).map(|(_, e)| e.clone());
                @let saved = row.saved.as_ref().map(|saved| saved.value(field).to_owned());
                (ui::Field::new(field.label(), id(field), name(field))
                    .value(row.value(field))
                    .placeholder(field.placeholder())
                    .autocomplete("off")
                    .maybe_error(error)
                    .attr("spellcheck", "false")
                    .maybe_attr("data-saved", saved))
            }
            (ui::Button::new(html! { (PreEscaped(ui::assets::icons::TRASH)) "Remove" })
                .kind(ui::ButtonType::Submit)
                .attr("name", APP_ACTION)
                .attr("value", format!("remove-app:{index}"))
                .attr("aria-label", format!("Remove {shown_name}"))
                .flag("data-relay-app-remove"))
        }
    }
}

/// The card.
pub(super) fn render(data: &AdminSettingsViewModel, relay_card: &RelayCardView) -> Markup {
    let fields = super::group_fields(data, Group::Relay);
    let enabled: Vec<&AdminScalarFieldView> = fields
        .iter()
        .copied()
        .filter(|f| data.monokulo_fields.iter().any(|m| std::ptr::eq(m, *f)))
        .collect();
    let relay_fields: Vec<&AdminScalarFieldView> = fields
        .iter()
        .copied()
        .filter(|f| data.relay_fields.iter().any(|r| std::ptr::eq(r, *f)))
        .collect();
    let name = Group::Relay.to_string();
    let failure = data.failure(Group::Relay);
    let readonly = fields.iter().all(|f| !f.access.changeable());
    card(data, Group::Relay, &name, "Monokulo", None, &fields)
        .failed(failure.is_some(), failure.map(|f| f.message.as_str()))
        .readonly(readonly)
        .class(if relay_card.open {
            "relay-card is-open"
        } else {
            "relay-card"
        })
        .link(head_link(relay_card))
        .render(html! {
            @for field in &enabled { (scalar_field(field)) }
            (chips(&relay_card.relay))
            (panel(&relay_card.relay))
            @if relay_card.open {
                @for field in &relay_fields {
                    @if let Some(heading) = subheading(&field.key) { h4 { (heading) } }
                    @if field.key == APPS_KEY {
                        (apps_field(field, relay_card))
                    } @else if field.kind == SettingKindView::Secret {
                        @let note = if field.value.is_empty() {
                            unset_secret_note(&field.key, &field.env_var)
                        } else {
                            format!("This is set with {} when the process starts; change it there.", field.env_var)
                        };
                        (secret_field(field, &note))
                    } @else {
                        (scalar_field(field))
                    }
                }
            } @else {
                (ui::Hint::new(html! {
                    "The relay's other settings are the options file's " code { "[relay.*]" }
                    " tables. Edit shows them here; saving writes the file and restarts the relay's listener."
                }))
            }
        })
}
