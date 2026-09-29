//! Admin-only pages: first-run setup (`GET`/`POST /admin/setup`), the
//! settings page (`GET`/`POST /dashboard/admin/settings`,
//! `POST /dashboard/admin/scanner-settings`), the invites page
//! (`GET /dashboard/admin/invites` and its create-link/delete actions), and
//! the public `GET`/`POST /request-invite` form.

use maud::{html, Markup};

use super::{layout, PageChrome};

/// `error` means the same thing every other page's own re-render-on-
/// rejection `error` field does; `email` is echoed back into the form on a
/// rejected submission (a duplicate email, mismatched passwords) so the
/// operator doesn't have to retype it - the two password fields are never
/// echoed back, same no-echo-a-password convention every other credential
/// field in this codebase follows.
pub struct SetupViewModel {
    pub error: Option<String>,
    pub email: String,
}

pub fn setup_page(chrome: &PageChrome, data: &SetupViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/" { "Home" } }
            h1 { "Set up your admin account" }
            p {
                "This is a one-time step. The account created here is this instance's single administrator, "
                "and can configure every monokulo and scanner setting from the admin page afterward."
            }
            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }
            form method="post" action="/admin/setup" {
                label { "Email " input type="email" name="email" value=(data.email) required; }
                label { "Password " input type="password" name="password" required minlength="12"; }
                label { "Confirm password " input type="password" name="confirm_password" required minlength="12"; }
                button type="submit" class="btn-primary" { "Create admin account" }
            }
        }
    };
    layout(chrome, "Set up admin account - Monokulo", body)
}

pub struct RequestInviteViewModel {
    pub error: Option<String>,
    /// `true` after a successful `POST` - the page shows a plain thank-you
    /// message instead of the form again, so a visitor can't accidentally
    /// double-submit by refreshing.
    pub submitted: bool,
}

pub fn request_invite_page(chrome: &PageChrome, data: &RequestInviteViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/" { "Home" } }
            h1 { "Request an invite" }
            @if data.submitted {
                p { "Thanks - we'll be in touch if there's a spot for you." }
            } @else {
                @if let Some(error) = &data.error {
                    p class="error" { (error) }
                }
                p { "This instance is invite-only right now. Tell us a bit about what you'd like to use it for and we'll follow up." }
                form method="post" action="/request-invite" {
                    label { "Email " input type="email" name="email" required; }
                    label { "Message " textarea name="message" required {} }
                    button type="submit" class="btn-primary" { "Request an invite" }
                }
            }
        }
    };
    layout(chrome, "Request an invite - Monokulo", body)
}

/// One row on the admin invites page's pending-requests table - the
/// `mailto:` link is built server-side (real HTML `<a href>`, no JS) and is
/// `None` only for the pathological case of a request whose linked invite
/// couldn't be decrypted.
pub struct AdminInviteRequestRow {
    pub id: String,
    pub email: String,
    pub message: String,
    /// When it arrived, in the admin's zone (`views::time`).
    pub created_at_display: Markup,
    pub mailto_href: Option<String>,
    /// Set only for the one row this page just deleted - rendered struck-
    /// through, as a one-time confirmation, outside the page's own real
    /// pagination count.
    pub just_deleted: bool,
}

pub struct AdminInvitesViewModel {
    pub error: Option<String>,
    pub success: Option<String>,
    pub just_deleted_row: Option<AdminInviteRequestRow>,
    pub rows: Vec<AdminInviteRequestRow>,
    pub page: u32,
    pub total_pages: u32,
    pub has_previous: bool,
    pub has_next: bool,
    pub previous_page: u32,
    pub next_page: u32,
    /// The freshly generated standalone link from "create invite link" -
    /// shown exactly once, on this one response, never stored reversibly.
    pub created_link: Option<String>,
}

fn invite_row(row: &AdminInviteRequestRow, page: u32) -> Markup {
    html! {
        tr class=[row.just_deleted.then_some("row-deleted")] {
            td { (row.email) }
            td { (row.message) }
            td { (row.created_at_display) }
            td {
                @if row.just_deleted {
                    "deleted"
                } @else {
                    @if let Some(href) = &row.mailto_href {
                        a href=(href) { "email invite" }
                    }
                    form method="post" action=(format!("/dashboard/admin/invites/{}/delete?page={}", row.id, page)) class="inline-form"
                        fx-action=(format!("/dashboard/admin/invites/{}/delete?page={}", row.id, page)) fx-method="POST" fx-target="#invites" {
                        button type="submit" { "delete" }
                    }
                }
            }
        }
    }
}

/// Everything on the invites page below its heading: what fixi swaps back
/// after any button here, or a page change.
pub fn invites_section(data: &AdminInvitesViewModel) -> Markup {
    let target = "#invites";
    html! {
        section id="invites" {
            @if let Some(error) = &data.error {
                p class="error" role="alert" data-fx-focus tabindex="-1" { (error) }
            }
            @if let Some(success) = &data.success {
                p class="success" role="status" data-fx-focus tabindex="-1" { (success) }
            }

            form method="post" action="/dashboard/admin/invites/create-link"
                fx-action="/dashboard/admin/invites/create-link" fx-method="POST" fx-target=(target) {
                button type="submit" class="btn-primary" { "Create invite link" }
            }
            @if let Some(link) = &data.created_link {
                p { "Share this link - it works once:" }
                pre data-fx-focus tabindex="-1" { (link) }
            }

            h2 { "Pending requests" }
            @if !data.rows.is_empty() {
                table {
                    thead { tr { th { "Email" } th { "Message" } th { "Requested" } th { "Actions" } } }
                    tbody {
                        @if let Some(deleted) = &data.just_deleted_row {
                            (invite_row(deleted, data.page))
                        }
                        @for row in &data.rows {
                            (invite_row(row, data.page))
                        }
                    }
                }
                nav class="pagination" {
                    @if data.has_previous {
                        @let href = format!("/dashboard/admin/invites?page={}", data.previous_page);
                        a href=(href) fx-action=(href) fx-target=(target) fx-push-url { "Previous" }
                    }
                    span { "Page " (data.page) " of " (data.total_pages) }
                    @if data.has_next {
                        @let href = format!("/dashboard/admin/invites?page={}", data.next_page);
                        a href=(href) fx-action=(href) fx-target=(target) fx-push-url { "Next" }
                    }
                }
                form method="post" action="/dashboard/admin/invites/delete-all"
                    fx-action="/dashboard/admin/invites/delete-all" fx-method="POST" fx-target=(target) {
                    button type="submit" { "Delete all" }
                }
            } @else if let Some(deleted) = &data.just_deleted_row {
                table {
                    thead { tr { th { "Email" } th { "Message" } th { "Requested" } th { "Actions" } } }
                    tbody { (invite_row(deleted, data.page)) }
                }
            } @else {
                p class="hint" { "No pending invite requests." }
            }
        }
    }
}

pub fn admin_invites_page(chrome: &PageChrome, data: &AdminInvitesViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Invites" }
            (invites_section(data))
        }
    };
    layout(chrome, "Invites - Monokulo", body)
}

/// What kind of value a setting takes, which decides its input control
/// (task 4.6). Mirrors `live_settings::SettingKind`, owned, so the engine's
/// settings (fetched as JSON) and monokulo's own share one renderer.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingKindView {
    Integer { min: Option<i64>, max: Option<i64> },
    Bool,
    Choice { choices: Vec<String> },
    ChoiceList { choices: Vec<String> },
    Url,
    Address,
    Path,
    #[default]
    Text,
    Secret,
    Json,
    /// A Unix time until which something stays on (development logging),
    /// chosen as "off" or "on for N hours" from `now`. Never sent by the
    /// engine; set by the handler for keys it knows (structured_logging.md
    /// 1.3).
    #[serde(skip)]
    TimeLimit { now: u64 },
}

impl From<live_settings::SettingKind> for SettingKindView {
    fn from(kind: live_settings::SettingKind) -> Self {
        use live_settings::SettingKind as K;
        let owned = |choices: Vec<&'static str>| choices.into_iter().map(str::to_string).collect();
        match kind {
            K::Integer { min, max } => SettingKindView::Integer { min, max },
            K::Bool => SettingKindView::Bool,
            K::Choice { choices } => SettingKindView::Choice { choices: owned(choices) },
            K::ChoiceList { choices } => SettingKindView::ChoiceList { choices: owned(choices) },
            K::Url => SettingKindView::Url,
            K::Address => SettingKindView::Address,
            K::Path => SettingKindView::Path,
            K::Text => SettingKindView::Text,
            K::Secret => SettingKindView::Secret,
            K::Json => SettingKindView::Json,
        }
    }
}

/// One editable field on the admin settings page - either one of monokulo's
/// own settings or one of the engine's, fetched live over HTTP.
#[derive(Debug, Clone, Default)]
pub struct AdminScalarFieldView {
    /// The stable settings-table key (also the form field's `name`).
    pub key: String,
    pub label: String,
    /// The field's current *effective* value - what wins under
    /// `env > database > default`. Masked for secrets.
    pub value: String,
    /// `"environment variable"`, `"saved value"`, or `"default"`.
    pub source_label: String,
    /// What the setting is for (task 4.1).
    pub help: Option<String>,
    pub kind: SettingKindView,
    pub example: Option<String>,
    /// Only applies after a restart.
    pub restart_only: bool,
    /// Saved, but still waiting for that restart.
    pub pending_restart: bool,
    /// Why the value in effect isn't the one set, if it isn't.
    pub problem: Option<String>,
}

/// One `monero_node.<network>` entry on the engine half of the page - shown
/// and edited as a single JSON text field.
#[derive(Debug, Clone, Default)]
pub struct AdminNetworkFieldView {
    pub network: String,
    /// Empty when this network has no node configured yet.
    pub value_json: String,
    pub description: Option<String>,
    pub example: Option<String>,
    /// Stores on this network, for the confirmation before clearing it.
    pub tenant_count: u64,
}

/// A banner shown at the top of the page after a save (task 4.5).
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Saved and applied, but something needs attention (a restart).
    Warning(String),
    /// Saved, but something is now broken (stores without a node, an
    /// engine that doesn't answer).
    Error(String),
    /// For information (an environment variable still wins).
    Info(String),
}

/// Which process a setting belongs to: monokulo's own registry, or the
/// engine's (fetched and saved over its admin API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingOwner {
    Monokulo,
    Engine,
}

/// The admin settings page's tabs, grouped by job rather than by which
/// process owns a setting (nicer_admin_screen.md T1). Each is its own URL,
/// `/dashboard/admin/settings?tab=<id>`, so switching works as a plain link
/// and a save can send the browser back to the tab it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Nodes,
    Payments,
    Custody,
    Abuse,
    Server,
    Logging,
    /// Engine (or monokulo) settings added after this map was written and
    /// not placed yet. Only shown while something is in it.
    Other,
}

impl SettingsTab {
    /// Every tab, in the order the tab bar shows them. General comes first:
    /// the engine's tabs stay empty until its connection there works.
    pub const ALL: [SettingsTab; 8] = [
        SettingsTab::General,
        SettingsTab::Nodes,
        SettingsTab::Payments,
        SettingsTab::Custody,
        SettingsTab::Abuse,
        SettingsTab::Server,
        SettingsTab::Logging,
        SettingsTab::Other,
    ];

    /// The tab's `?tab=` value.
    pub fn id(self) -> &'static str {
        match self {
            SettingsTab::General => "general",
            SettingsTab::Nodes => "nodes",
            SettingsTab::Payments => "payments",
            SettingsTab::Custody => "custody",
            SettingsTab::Abuse => "abuse",
            SettingsTab::Server => "server",
            SettingsTab::Logging => "logging",
            SettingsTab::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Nodes => "Monero nodes",
            SettingsTab::Payments => "Payments",
            SettingsTab::Custody => "Key custody",
            SettingsTab::Abuse => "Abuse protection",
            SettingsTab::Server => "Server",
            SettingsTab::Logging => "Logging",
            SettingsTab::Other => "Other",
        }
    }

    /// The tab a `?tab=` value names; General for a missing or unknown one,
    /// so an old bookmark or a typo still lands somewhere useful.
    pub fn from_id(id: Option<&str>) -> SettingsTab {
        SettingsTab::ALL.into_iter().find(|tab| Some(tab.id()) == id).unwrap_or(SettingsTab::General)
    }

    /// The tab's page.
    pub fn href(self) -> String {
        format!("/dashboard/admin/settings?tab={}", self.id())
    }

    /// The groups the tab's settings are shown in, in order: an optional
    /// heading and whose settings go under it. The Nodes tab's one group is
    /// the node form, not scalar fields.
    pub fn groups(self) -> &'static [(Option<&'static str>, SettingOwner)] {
        use SettingOwner::{Engine, Monokulo};
        match self {
            SettingsTab::General | SettingsTab::Abuse => &[(None, Monokulo)],
            SettingsTab::Nodes | SettingsTab::Custody => &[(None, Engine)],
            SettingsTab::Payments => &[(None, Engine), (Some("Webhooks"), Engine), (Some("Exchange rates"), Monokulo)],
            SettingsTab::Server | SettingsTab::Other => &[(None, Engine), (None, Monokulo)],
            SettingsTab::Logging => &[(Some("Monokulo"), Monokulo), (Some("Engine"), Engine)],
        }
    }

    /// Whether every setting on this tab is the engine's, so the tab has
    /// nothing to show or save while the engine can't be reached.
    pub fn engine_only(self) -> bool {
        self.groups().iter().all(|(_, owner)| *owner == SettingOwner::Engine)
    }
}

/// Where a setting shows on the admin page: its tab, and the heading it
/// sits under on a tab that has more than one group. The one map both the
/// page and the save use, so a setting can't be shown on one tab and then
/// dropped when that tab is saved.
pub fn setting_placement(key: &str, owner: SettingOwner) -> (SettingsTab, Option<&'static str>) {
    let prefix = key.split('.').next().unwrap_or("");
    match owner {
        SettingOwner::Monokulo => match prefix {
            "signup" | "engine" | "public_url" => (SettingsTab::General, None),
            "exchange_rate" => (SettingsTab::Payments, Some("Exchange rates")),
            "abuse" | "rate_limit" => (SettingsTab::Abuse, None),
            "http_cache" => (SettingsTab::Server, None),
            "logging" => (SettingsTab::Logging, Some("Monokulo")),
            _ => (SettingsTab::Other, None),
        },
        SettingOwner::Engine => match prefix {
            "monero_node" => (SettingsTab::Nodes, None),
            // How much memory a scan may use is about the machine, not
            // about payments.
            "payment" if key == "payment.scan_chunk_memory_budget_mb" => (SettingsTab::Server, None),
            "payment" => (SettingsTab::Payments, None),
            "webhooks" => (SettingsTab::Payments, Some("Webhooks")),
            "key_custody" => (SettingsTab::Custody, None),
            "server" => (SettingsTab::Server, None),
            "logging" => (SettingsTab::Logging, Some("Engine")),
            _ => (SettingsTab::Other, None),
        },
    }
}

/// The two halves of the admin settings page, each saved (and, with
/// fixi, swapped back) on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Monokulo,
    Engine,
}

#[derive(Default)]
pub struct AdminSettingsViewModel {
    /// Which half was just saved: its banners show inside it.
    pub saved_section: Option<SettingsSection>,
    /// The tab a save was for, or the tab holding the setting a refused
    /// save was about: its banners show there.
    pub saved_tab: Option<SettingsTab>,
    pub error: Option<String>,
    pub success: Option<String>,
    pub notices: Vec<Notice>,
    pub monokulo_fields: Vec<AdminScalarFieldView>,
    /// `true` once `engine.url`/`engine.admin_token` are both non-empty.
    pub scanner_configured: bool,
    /// `true` only after a real, successful fetch of the scanner's own
    /// settings.
    pub scanner_reachable: bool,
    pub scanner_error: Option<String>,
    pub scanner_fields: Vec<AdminScalarFieldView>,
    pub scanner_networks: Vec<AdminNetworkFieldView>,
}

/// The id of a setting's control, which its label and help point at.
fn field_id(key: &str) -> String {
    format!("setting-{key}")
}

/// The id of a setting's help text, when it has some.
fn help_id(field: &AdminScalarFieldView) -> Option<String> {
    field.help.as_ref().map(|_| format!("setting-help-{}", field.key))
}

fn scalar_input(field: &AdminScalarFieldView) -> Markup {
    let name = field.key.as_str();
    let id = field_id(name);
    let help = help_id(field);
    match &field.kind {
        SettingKindView::Integer { min, max } => html! {
            input type="number" name=(name) value=(field.value) min=[min] max=[max] step="1" id=(id) aria-describedby=[help];
        },
        SettingKindView::Bool => html! {
            select name=(name) id=(id) aria-describedby=[help] {
                option value="true" selected[field.value == "true"] { "true" }
                option value="false" selected[field.value == "false"] { "false" }
            }
        },
        SettingKindView::Choice { choices } => html! {
            select name=(name) id=(id) aria-describedby=[help] {
                @for choice in choices {
                    option value=(choice) selected[&field.value == choice] { (choice) }
                }
            }
        },
        // Every choice ticked is sent under the one name; an empty one
        // first, so ticking none still says so.
        SettingKindView::ChoiceList { choices } => {
            let chosen: Vec<&str> = field.value.split(',').map(str::trim).collect();
            html! {
                input type="hidden" name=(name) value="";
                @for choice in choices {
                    label class="inline" {
                        input type="checkbox" name=(name) value=(choice) checked[chosen.contains(&choice.as_str())];
                        " " (choice)
                    }
                }
            }
        }
        SettingKindView::Url => html! { input type="url" name=(name) value=(field.value) id=(id) aria-describedby=[help]; },
        // Never echoed back: left empty means "keep the current one".
        SettingKindView::Secret => html! {
            input type="password" name=(name) value="" autocomplete="off" id=(id) aria-describedby=[help]
                placeholder=(if field.value.is_empty() { "not set" } else { "set - leave empty to keep it" });
            @if !field.value.is_empty() {
                label class="inline" {
                    input type="checkbox" name=(format!("clear:{name}")) value="on";
                    " Clear it"
                }
            }
        },
        SettingKindView::Json => html! { textarea name=(name) rows="4" id=(id) aria-describedby=[help] { (field.value) } },
        SettingKindView::TimeLimit { now } => {
            let until: u64 = field.value.trim().parse().unwrap_or(0);
            let on = until > *now;
            html! {
                select name=(name) id=(id) aria-describedby=[help] {
                    @if on {
                        option value=(until) selected { "On until " (telemetry::format_unix_utc(until)) }
                    }
                    option value="0" selected[!on] { "Off" }
                    @for (hours, label) in [(1, "On for 1 hour"), (4, "On for 4 hours"), (24, "On for 24 hours")] {
                        option value=(now + hours * 3600) { (label) }
                    }
                }
            }
        }
        _ => html! { input type="text" name=(name) value=(field.value) id=(id) aria-describedby=[help]; },
    }
}

/// A setting's name, then what it's for, then its control, then where its
/// value came from. A list of choices is a group of checkboxes, named by a
/// legend rather than a label.
fn scalar_field(field: &AdminScalarFieldView) -> Markup {
    let help = html! {
        @if let (Some(help), Some(id)) = (&field.help, help_id(field)) {
            span class="field-help" id=(id) { (help) }
        }
    };
    html! {
        @if matches!(field.kind, SettingKindView::ChoiceList { .. }) {
            fieldset class="setting-field" aria-describedby=[help_id(field)] {
                legend class="setting-label" { (field.label) }
                (help)
                div class="setting-choices" { (scalar_input(field)) }
                (field_status(field))
            }
        } @else {
            div class="setting-field" {
                label class="setting-label" for=(field_id(&field.key)) { (field.label) }
                (help)
                (scalar_input(field))
                (field_status(field))
            }
        }
    }
}

/// Under a setting's control: an example, where its value came from, and
/// anything wrong with it.
fn field_status(field: &AdminScalarFieldView) -> Markup {
    html! {
        @if let Some(example) = &field.example {
            span class="field-help" { "Example: " code { (example) } }
        }
        span class="setting-source" {
            "(" (field.source_label)
            @if field.restart_only { ", applies after a restart" }
            ")"
        }
        @if field.pending_restart {
            span class="setting-pending" { "Saved - restart needed for it to take effect." }
        }
        @if let Some(problem) = &field.problem {
            span class="setting-problem" { (problem) }
        }
    }
}

fn notices(items: &[Notice]) -> Markup {
    html! {
        @for notice in items {
            @match notice {
                Notice::Error(text) => p class="error" role="alert" { (text) },
                Notice::Warning(text) => p class="warning" role="status" { (text) },
                Notice::Info(text) => p class="notice" { (text) },
            }
        }
    }
}

/// With JavaScript, confirm before saving an engine settings form that
/// clears a network stores still use (task 4.4). Without it, the form posts
/// and the red banner after the save says what happened. Listens on the
/// document, before fixi (capture), so it still works on the form fixi
/// swaps in after a save; cancelling stops fixi too (`static/fx-glue.js`).
const CONFIRM_CLEARED_NETWORK_SCRIPT: &str = r#"(function () {
  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (form.id !== "scanner-settings-form") return;
    var fields = form.querySelectorAll("textarea[data-tenant-count]");
    for (var i = 0; i < fields.length; i++) {
      var field = fields[i];
      var count = parseInt(field.getAttribute("data-tenant-count"), 10) || 0;
      if (count > 0 && field.value.trim() === "" && field.defaultValue.trim() !== "") {
        var network = field.getAttribute("data-network");
        var stores = count === 1 ? "1 store uses" : count + " stores use";
        if (!window.confirm(stores + " the " + network + " network. Without a node, their payments won't be detected. Save anyway?")) {
          event.preventDefault();
          return;
        }
      }
    }
  }, true);
})();"#;

/// Settings shown under "Abuse protection" (`crate::abuse`).
fn is_abuse_field(key: &str) -> bool {
    key.starts_with("abuse.") || key.starts_with("rate_limit.")
}

/// Settings shown under "Logging".
fn is_logging_field(key: &str) -> bool {
    key.starts_with("logging.")
}

/// The engine's settings, grouped (task 4.7).
fn engine_group(key: &str) -> &'static str {
    match key.split('.').next().unwrap_or("") {
        "key_custody" => "Key custody",
        "payment" => "Payments",
        "server" => "Server",
        "webhooks" => "Webhooks",
        "logging" => "Logging",
        _ => "Other",
    }
}

const ENABLED_BACKENDS: &str = "key_custody.enabled_backends";

/// A group's settings, in the engine's order except that the key custody
/// backends to turn on come before the choice among them.
fn in_group<'a>(fields: &'a [AdminScalarFieldView], group: &'a str) -> impl Iterator<Item = &'a AdminScalarFieldView> {
    let fields = move || fields.iter().filter(move |f| engine_group(&f.key) == group);
    fields().filter(|f| f.key == ENABLED_BACKENDS).chain(fields().filter(|f| f.key != ENABLED_BACKENDS))
}

/// The key custody backends the engine offers, and whether each is turned
/// on. Empty from an engine that doesn't say.
fn custody_backends(fields: &[AdminScalarFieldView]) -> Vec<(String, bool)> {
    let Some(field) = fields.iter().find(|f| f.key == ENABLED_BACKENDS) else { return Vec::new() };
    let SettingKindView::ChoiceList { choices } = &field.kind else { return Vec::new() };
    let enabled: Vec<&str> = field.value.split(',').map(str::trim).collect();
    choices.iter().map(|choice| (choice.clone(), enabled.contains(&choice.as_str()))).collect()
}

/// The backend a key custody setting belongs to: one only that backend
/// uses is named `key_custody.<backend>_...` (`key_custody.socket_path`).
fn custody_backend_of<'a>(field: &AdminScalarFieldView, backends: &'a [(String, bool)]) -> Option<&'a str> {
    let rest = field.key.strip_prefix("key_custody.")?;
    backends
        .iter()
        .map(|(backend, _)| backend.as_str())
        .find(|backend| rest.strip_prefix(backend).is_some_and(|after| after.starts_with('_')))
}

/// A section of its own for each backend, shown only while it's turned on
/// (at once with JavaScript, after saving without).
fn custody_backend_sections(fields: &[AdminScalarFieldView], backends: &[(String, bool)]) -> Markup {
    html! {
        @for (backend, enabled) in backends {
            section class="custody-backend" data-custody-backend=(backend) hidden[!enabled] {
                h3 { "Key custody: " (backend) }
                @let own: Vec<&AdminScalarFieldView> =
                    fields.iter().filter(|f| custody_backend_of(f, backends) == Some(backend.as_str())).collect();
                @if own.is_empty() {
                    p class="hint" { "Nothing to set up for this backend." }
                }
                @for field in own { (scalar_field(field)) }
            }
        }
    }
}

/// With JavaScript, a key custody backend's section shows or hides as its
/// box is ticked, before saving. Listens on the document, so it still
/// works on the form fixi swaps in after a save.
const CUSTODY_BACKENDS_SCRIPT: &str = r#"(function () {
  document.addEventListener("change", function (event) {
    var box = event.target;
    if (box.name !== "key_custody.enabled_backends") return;
    var section = document.querySelector('[data-custody-backend="' + box.value + '"]');
    if (section) section.hidden = !box.checked;
  });
})();"#;

/// A short word on the save beside the Save button, where the person who
/// pressed it is looking; focused after a fixi swap (the banners above say
/// more).
fn save_status(data: &AdminSettingsViewModel, section: SettingsSection) -> Markup {
    html! {
        @if data.saved_section == Some(section) {
            @if data.error.is_some() {
                span class="save-status error" role="alert" data-fx-focus tabindex="-1" { "Not saved - see the message above." }
            } @else if data.success.is_some() {
                span class="save-status success" role="status" data-fx-focus tabindex="-1" { "Saved." }
            }
        }
    }
}

/// The page's banners after a save: the error, the success message and
/// any notices.
fn banners(data: &AdminSettingsViewModel) -> Markup {
    html! {
        div class="save-banners" {
            @if let Some(error) = &data.error {
                p class="error" role="alert" { (error) }
            }
            @if let Some(success) = &data.success {
                p class="success" role="status" { (success) }
            }
            (notices(&data.notices))
        }
    }
}

/// Monokulo's own settings: one form, saved and swapped back as a whole.
pub fn monokulo_section(data: &AdminSettingsViewModel) -> Markup {
    html! {
        section id="monokulo-settings" {
            h2 { "Monokulo" }
            @if data.saved_section == Some(SettingsSection::Monokulo) { (banners(data)) }
            p class="hint" { "Saved settings apply straight away. An environment variable, where set, always wins over the value saved here - saving still works, it just won't take effect until that variable is unset." }
            form method="post" action="/dashboard/admin/settings" fx-action="/dashboard/admin/settings" fx-method="POST" fx-target="#monokulo-settings" {
                @for field in data.monokulo_fields.iter().filter(|f| !is_abuse_field(&f.key) && !is_logging_field(&f.key)) {
                    (scalar_field(field))
                }
                h3 id="abuse-protection" { "Abuse protection" }
                p class="hint" {
                    "How this instance tells visitors apart and slows down anyone sending too many requests. A visitor "
                    "past the soft limit is asked to pass a short check (automatic with JavaScript, a 10-second wait "
                    "without); past the hard limit they're refused until the minute is up. Signed-in merchants and "
                    "plugins using their store's secret key are never checked."
                }
                @for field in data.monokulo_fields.iter().filter(|f| is_abuse_field(&f.key)) {
                    (scalar_field(field))
                }
                h3 id="logging" { "Logging" }
                @for field in data.monokulo_fields.iter().filter(|f| is_logging_field(&f.key)) {
                    (scalar_field(field))
                }
                button type="submit" { "Save monokulo settings" }
                (save_status(data, SettingsSection::Monokulo))
            }
        }
    }
}

/// The engine's settings. `oob` marks it for fixi's glue to put in place
/// of the page's own copy when it comes back with the monokulo section
/// (saving the engine connection there changes this half too).
pub fn engine_section(data: &AdminSettingsViewModel, oob: bool) -> Markup {
    let groups = ["Key custody", "Payments", "Server", "Webhooks", "Logging", "Other"];
    let backends = custody_backends(&data.scanner_fields);
    html! {
        section id="engine-settings" data-fx-oob[oob] {
            h2 { "Engine" }
            @if data.saved_section == Some(SettingsSection::Engine) { (banners(data)) }
            @if !data.scanner_configured {
                p { "Set " code { "engine.url" } " and " code { "engine.admin_token" } " above and save to manage this instance's engine settings from here." }
            } @else if data.scanner_reachable {
                form method="post" action="/dashboard/admin/scanner-settings" id="scanner-settings-form"
                    fx-action="/dashboard/admin/scanner-settings" fx-method="POST" fx-target="#engine-settings" {
                    h3 { "Monero nodes" }
                    @for network in &data.scanner_networks {
                        @let id = format!("setting-monero_node_{}", network.network);
                        @let help = network.description.as_ref().map(|_| format!("setting-help-monero_node_{}", network.network));
                        div class="setting-field" {
                            label class="setting-label" for=(id) { "Monero node (" (network.network) ")" }
                            @if let (Some(description), Some(help)) = (&network.description, &help) {
                                span class="field-help" id=(help) { (description) }
                            }
                            textarea name=(format!("monero_node_{}", network.network)) rows="4" id=(id) aria-describedby=[help]
                                data-network=(network.network) data-tenant-count=(network.tenant_count) { (network.value_json) }
                            span class="setting-source" {
                                @if network.tenant_count == 1 { "Used by 1 store." } @else { "Used by " (network.tenant_count) " stores." }
                            }
                            @if let Some(example) = &network.example {
                                details class="field-help" {
                                    summary { "Example" }
                                    pre { code { (example) } }
                                    p {
                                        code { "host" } " and " code { "port" } ": the node's address. "
                                        code { "ssl" } " (default false): connect with TLS. "
                                        code { "accept_self_signed_certs" } " (default true): accept a self-signed TLS certificate. "
                                        code { "fallbacks" } ": more nodes in the same shape, tried in order when the one before fails; a fallback can't have fallbacks of its own."
                                    }
                                }
                            }
                        }
                    }
                    @for group in groups {
                        @if in_group(&data.scanner_fields, group).next().is_some() {
                            h3 { (group) }
                            @for field in in_group(&data.scanner_fields, group).filter(|f| custody_backend_of(f, &backends).is_none()) {
                                (scalar_field(field))
                            }
                            @if group == "Key custody" { (custody_backend_sections(&data.scanner_fields, &backends)) }
                        }
                    }
                    button type="submit" { "Save engine settings" }
                    (save_status(data, SettingsSection::Engine))
                }
            } @else {
                p class="error" role="alert" {
                    "Could not reach the configured engine: "
                    @if let Some(scanner_error) = &data.scanner_error { (scanner_error) }
                }
            }
        }
    }
}

pub fn admin_settings_page(chrome: &PageChrome, data: &AdminSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Admin settings" }
            @if data.saved_section.is_none() { (banners(data)) }
            (monokulo_section(data))
            (engine_section(data, false))
            script { (maud::PreEscaped(CONFIRM_CLEARED_NETWORK_SCRIPT)) }
            script { (maud::PreEscaped(CUSTODY_BACKENDS_SCRIPT)) }
        }
    };
    layout(chrome, "Admin settings - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/admin/settings")
    }

    /// Every setting both registries have today, with the tab (and
    /// heading) the page shows it on (nicer_admin_screen.md section 2). A
    /// setting added later fails the registry cross-checks below until it
    /// is given a place here.
    const PLACEMENTS: &[(&str, SettingOwner, SettingsTab, Option<&str>)] = {
        use SettingOwner::{Engine as E, Monokulo as M};
        use SettingsTab::*;
        &[
            ("signup.mode", M, General, None),
            ("engine.url", M, General, None),
            ("engine.admin_token", M, General, None),
            ("public_url", M, General, None),
            ("exchange_rate.coingecko_enabled", M, Payments, Some("Exchange rates")),
            ("exchange_rate.coingecko_base_url", M, Payments, Some("Exchange rates")),
            ("exchange_rate.coinmarketcap_enabled", M, Payments, Some("Exchange rates")),
            ("exchange_rate.coinmarketcap_base_url", M, Payments, Some("Exchange rates")),
            ("exchange_rate.haveno_enabled", M, Payments, Some("Exchange rates")),
            ("exchange_rate.haveno_base_url", M, Payments, Some("Exchange rates")),
            ("exchange_rate.cache_seconds", M, Payments, Some("Exchange rates")),
            ("http_cache.max_mb", M, Server, None),
            ("abuse.soft_per_min", M, Abuse, None),
            ("abuse.hard_per_min", M, Abuse, None),
            ("abuse.signed_in_per_min", M, Abuse, None),
            ("abuse.client_logs_per_min", M, Abuse, None),
            ("abuse.challenge_bits", M, Abuse, None),
            ("abuse.under_attack", M, Abuse, None),
            ("abuse.trusted_proxies", M, Abuse, None),
            ("abuse.onion_listener", M, Abuse, None),
            ("abuse.stream_cap", M, Abuse, None),
            ("rate_limit.per_store_key_per_min", M, Abuse, None),
            ("logging.level", M, Logging, Some("Monokulo")),
            ("logging.dev_mode_until", M, Logging, Some("Monokulo")),
            ("logging.retention_days", M, Logging, Some("Monokulo")),
            ("logging.max_mb", M, Logging, Some("Monokulo")),
            ("logging.otlp_endpoint", M, Logging, Some("Monokulo")),
            ("logging.otlp_headers", M, Logging, Some("Monokulo")),
            ("monero_node.mainnet", E, Nodes, None),
            ("monero_node.stagenet", E, Nodes, None),
            ("monero_node.testnet", E, Nodes, None),
            ("key_custody.enabled_backends", E, Custody, None),
            ("key_custody.default_backend", E, Custody, None),
            ("key_custody.socket_path", E, Custody, None),
            ("payment.confirmations_required", E, Payments, None),
            ("payment.order_expiry_minutes", E, Payments, None),
            ("payment.expired_order_grace_period_minutes", E, Payments, None),
            ("payment.reorg_check_depth", E, Payments, None),
            ("payment.mempool_poll_interval_ms", E, Payments, None),
            ("payment.scan_chunk_memory_budget_mb", E, Server, None),
            ("webhooks.allow_private_urls", E, Payments, Some("Webhooks")),
            ("webhooks.delivery_timeout_ms", E, Payments, Some("Webhooks")),
            ("webhooks.max_attempts", E, Payments, Some("Webhooks")),
            ("server.bind", E, Server, None),
            ("server.worker_threads", E, Server, None),
            ("server.max_body_bytes", E, Server, None),
            ("server.rate_limit_per_token_per_min", E, Server, None),
            ("logging.level", E, Logging, Some("Engine")),
            ("logging.dev_mode_until", E, Logging, Some("Engine")),
            ("logging.retention_days", E, Logging, Some("Engine")),
            ("logging.max_mb", E, Logging, Some("Engine")),
            ("logging.otlp_endpoint", E, Logging, Some("Engine")),
            ("logging.otlp_headers", E, Logging, Some("Engine")),
        ]
    };

    #[test]
    fn every_setting_known_today_has_a_named_tab() {
        for (key, owner, tab, heading) in PLACEMENTS {
            assert_eq!(setting_placement(key, *owner), (*tab, *heading), "{key} ({owner:?})");
            assert_ne!(*tab, SettingsTab::Other, "{key}");
            // The heading is one the tab actually shows, for that owner.
            assert!(tab.groups().contains(&(*heading, *owner)), "{key} is placed under a group {tab:?} doesn't have");
        }
    }

    /// The list above is every setting of both registries, not a sample.
    #[test]
    fn the_tab_list_covers_both_registries() {
        let listed = |owner: SettingOwner| -> Vec<&str> {
            PLACEMENTS.iter().filter(|(_, o, _, _)| *o == owner).map(|(key, ..)| *key).collect()
        };
        let mut monokulo: Vec<&str> = crate::settings::ALL.iter().map(|s| s.key()).collect();
        monokulo.sort_unstable();
        let mut monokulo_listed = listed(SettingOwner::Monokulo);
        monokulo_listed.sort_unstable();
        assert_eq!(monokulo_listed, monokulo, "every monokulo setting has a tab");

        let mut engine: Vec<&str> = scanner::engine_settings::ALL.iter().map(|s| s.key()).collect();
        engine.sort_unstable();
        let mut engine_listed = listed(SettingOwner::Engine);
        engine_listed.sort_unstable();
        assert_eq!(engine_listed, engine, "every engine setting has a tab");
    }

    #[test]
    fn a_setting_the_map_does_not_know_goes_to_other() {
        assert_eq!(setting_placement("telemetry.sample_rate", SettingOwner::Engine), (SettingsTab::Other, None));
        assert_eq!(setting_placement("brand_new", SettingOwner::Engine), (SettingsTab::Other, None));
        assert_eq!(setting_placement("brand.new", SettingOwner::Monokulo), (SettingsTab::Other, None));
    }

    #[test]
    fn a_tab_is_found_by_its_id_and_anything_else_is_general() {
        for tab in SettingsTab::ALL {
            assert_eq!(SettingsTab::from_id(Some(tab.id())), tab);
        }
        assert_eq!(SettingsTab::from_id(None), SettingsTab::General);
        assert_eq!(SettingsTab::from_id(Some("nope")), SettingsTab::General);
        assert_eq!(SettingsTab::Nodes.href(), "/dashboard/admin/settings?tab=nodes");
        assert!(SettingsTab::Nodes.engine_only() && SettingsTab::Custody.engine_only());
        assert!(!SettingsTab::Payments.engine_only() && !SettingsTab::Server.engine_only() && !SettingsTab::Logging.engine_only());
    }

    #[test]
    fn setup_page_shows_the_echoed_email_and_an_error() {
        let data = SetupViewModel { error: Some("Passwords do not match.".to_string()), email: "owner@example.com".to_string() };
        let html = setup_page(&chrome(), &data).into_string();
        assert!(html.contains("Set up your admin account"));
        assert!(html.contains("Passwords do not match."));
        assert!(html.contains(r#"value="owner@example.com""#));
    }

    #[test]
    fn request_invite_page_shows_the_form_before_submission_and_a_thank_you_after() {
        let form = RequestInviteViewModel { error: None, submitted: false };
        let html = request_invite_page(&chrome(), &form).into_string();
        assert!(html.contains(r#"<form method="post" action="/request-invite">"#));

        let thanks = RequestInviteViewModel { error: None, submitted: true };
        let html = request_invite_page(&chrome(), &thanks).into_string();
        assert!(html.to_lowercase().contains("thanks"));
        assert!(!html.contains("<form method=\"post\" action=\"/request-invite\""), "a submitted confirmation must not still show the request form");
    }

    fn row(id: &str, email: &str) -> AdminInviteRequestRow {
        AdminInviteRequestRow {
            id: id.to_string(),
            email: email.to_string(),
            message: "let me in please".to_string(),
            created_at_display: maud::PreEscaped("just now".to_string()),
            mailto_href: Some(format!("mailto:{email}")),
            just_deleted: false,
        }
    }

    #[test]
    fn admin_invites_page_shows_no_pending_message_when_empty() {
        let data = AdminInvitesViewModel {
            error: None,
            success: None,
            just_deleted_row: None,
            rows: vec![],
            page: 1,
            total_pages: 1,
            has_previous: false,
            has_next: false,
            previous_page: 1,
            next_page: 1,
            created_link: None,
        };
        let html = admin_invites_page(&chrome(), &data).into_string();
        assert!(html.contains("No pending invite requests."));
        assert!(!html.contains("<table"));
    }

    #[test]
    fn admin_invites_page_lists_rows_with_a_mailto_link_and_a_delete_form_carrying_the_current_page() {
        let data = AdminInvitesViewModel {
            error: None,
            success: None,
            just_deleted_row: None,
            rows: vec![row("row-1", "hopeful@example.com")],
            page: 2,
            total_pages: 2,
            has_previous: true,
            has_next: false,
            previous_page: 1,
            next_page: 2,
            created_link: None,
        };
        let html = admin_invites_page(&chrome(), &data).into_string();
        assert!(html.contains("hopeful@example.com"));
        assert!(html.contains("mailto:hopeful@example.com"));
        assert!(html.contains("/dashboard/admin/invites/row-1/delete?page=2"));
        assert!(html.contains("Page 2 of 2"));
    }

    #[test]
    fn admin_invites_page_shows_the_struck_through_just_deleted_row_even_with_no_other_pending_rows() {
        let mut deleted = row("row-1", "gone@example.com");
        deleted.just_deleted = true;
        let data = AdminInvitesViewModel {
            error: None,
            success: None,
            just_deleted_row: Some(deleted),
            rows: vec![],
            page: 1,
            total_pages: 1,
            has_previous: false,
            has_next: false,
            previous_page: 1,
            next_page: 1,
            created_link: None,
        };
        let html = admin_invites_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"class="row-deleted""#));
        assert!(html.contains("gone@example.com"));
        assert!(!html.contains("No pending invite requests."));
    }

    #[test]
    fn admin_settings_page_prompts_for_scanner_configuration_when_unconfigured() {
        let data = AdminSettingsViewModel { monokulo_fields: vec![], scanner_configured: false, ..Default::default() };
        let html = admin_settings_page(&chrome(), &data).into_string();
        assert!(html.contains("Set <code>engine.url</code>"));
        assert!(!html.contains("/dashboard/admin/scanner-settings"));
    }

    #[test]
    fn admin_settings_page_shows_an_unreachable_scanner_error() {
        let data = AdminSettingsViewModel {
            monokulo_fields: vec![],
            scanner_configured: true,
            scanner_reachable: false,
            scanner_error: Some("connection refused".to_string()),
            ..Default::default()
        };
        let html = admin_settings_page(&chrome(), &data).into_string();
        assert!(html.contains("Could not reach the configured engine"));
        assert!(html.contains("connection refused"));
    }

    #[test]
    fn admin_settings_page_shows_scanner_fields_and_networks_when_reachable() {
        let data = AdminSettingsViewModel {
            monokulo_fields: vec![AdminScalarFieldView {
                key: "engine.url".to_string(),
                label: "engine url".to_string(),
                value: "http://scanner.internal".to_string(),
                source_label: "saved value".to_string(),
                help: Some("Where the engine listens.".to_string()),
                kind: SettingKindView::Url,
                ..Default::default()
            }],
            scanner_configured: true,
            scanner_reachable: true,
            scanner_fields: vec![AdminScalarFieldView {
                key: "payment.confirmations_required".to_string(),
                label: "payment confirmations required".to_string(),
                value: "10".to_string(),
                source_label: "default".to_string(),
                kind: SettingKindView::Integer { min: Some(0), max: Some(720) },
                example: Some("10".to_string()),
                ..Default::default()
            }],
            scanner_networks: vec![AdminNetworkFieldView {
                network: "mainnet".to_string(),
                value_json: "{}".to_string(),
                description: Some("The mainnet node.".to_string()),
                example: Some(r#"{"host":"node.example.com","port":18089}"#.to_string()),
                tenant_count: 2,
            }],
            ..Default::default()
        };
        let html = admin_settings_page(&chrome(), &data).into_string();
        assert!(html.contains("engine url"));
        // The name, then what it's for, then the box.
        assert!(
            html.contains(concat!(
                r#"<label class="setting-label" for="setting-engine.url">engine url</label>"#,
                r#"<span class="field-help" id="setting-help-engine.url">Where the engine listens.</span>"#,
                r#"<input type="url" name="engine.url""#,
            )),
            "{html}"
        );
        assert!(html.contains(r#"aria-describedby="setting-help-engine.url""#), "{html}");
        assert!(html.contains("Abuse protection"));
        assert!(html.contains(r#"type="url" name="engine.url" value="http://scanner.internal""#), "{html}");
        assert!(html.contains("payment confirmations required"));
        assert!(html.contains(r#"type="number" name="payment.confirmations_required" value="10" min="0" max="720""#), "{html}");
        assert!(html.contains("Example: <code>10</code>"));
        assert!(
            html.contains(concat!(
                r#"<label class="setting-label" for="setting-monero_node_mainnet">Monero node (mainnet)</label>"#,
                r#"<span class="field-help" id="setting-help-monero_node_mainnet">The mainnet node.</span><textarea"#,
            )),
            "{html}"
        );
        assert!(html.contains(r#"name="monero_node_mainnet""#));
        assert!(html.contains(r#"data-tenant-count="2""#));
        assert!(html.contains("Used by 2 stores."));
        assert!(html.contains("fallbacks"), "the node field explains its shape");
        assert!(html.contains("window.confirm"), "confirms before clearing a network in use");
    }

    #[test]
    fn each_key_custody_backend_has_its_own_section_shown_while_it_is_turned_on() {
        let field = |key: &str, value: &str, kind: SettingKindView| AdminScalarFieldView {
            key: key.to_string(),
            label: key.to_string(),
            value: value.to_string(),
            kind,
            ..Default::default()
        };
        let backends = || SettingKindView::ChoiceList { choices: vec!["plain".into(), "socket".into()] };
        let page = |enabled: &str| {
            let data = AdminSettingsViewModel {
                scanner_configured: true,
                scanner_reachable: true,
                scanner_fields: vec![
                    field("key_custody.enabled_backends", enabled, backends()),
                    field("key_custody.default_backend", "plain", SettingKindView::Choice { choices: vec!["plain".into(), "socket".into()] }),
                    field("key_custody.socket_path", "/run/kc.sock", SettingKindView::Path),
                ],
                ..Default::default()
            };
            admin_settings_page(&chrome(), &data).into_string()
        };

        let html = page("plain");
        // The backends are boxes to tick, after an empty value so ticking
        // none still says so.
        assert!(html.contains(r#"<input type="hidden" name="key_custody.enabled_backends" value="">"#), "{html}");
        assert!(html.contains(r#"<input type="checkbox" name="key_custody.enabled_backends" value="plain" checked>"#), "{html}");
        assert!(html.contains(r#"<input type="checkbox" name="key_custody.enabled_backends" value="socket">"#), "{html}");
        // The socket's path sits in the socket's own section, hidden while
        // socket is off; plain has nothing to set.
        let socket = html.find(r#"<section class="custody-backend" data-custody-backend="socket" hidden>"#).expect(&html);
        assert!(html.find(r#"name="key_custody.socket_path""#).unwrap() > socket, "{html}");
        assert!(html.find(r#"name="key_custody.default_backend""#).unwrap() < socket, "{html}");
        assert!(html.contains(r#"<section class="custody-backend" data-custody-backend="plain"><h3>Key custody: plain</h3><p class="hint">Nothing to set up"#), "{html}");
        assert!(html.contains(r#"name !== "key_custody.enabled_backends""#), "shown as soon as it's ticked, with JavaScript");

        let html = page("plain,socket");
        assert!(html.contains(r#"<section class="custody-backend" data-custody-backend="socket"><h3>Key custody: socket</h3>"#), "{html}");
        assert!(html.contains(r#"value="socket" checked"#), "{html}");
    }

    #[test]
    fn inputs_follow_the_kind_of_value_and_secrets_are_never_echoed() {
        let field = |kind: SettingKindView, value: &str| AdminScalarFieldView {
            key: "k".to_string(),
            label: "k".to_string(),
            value: value.to_string(),
            kind,
            ..Default::default()
        };
        let choice = scalar_field(&field(SettingKindView::Choice { choices: vec!["public".into(), "invite_only".into()] }, "public")).into_string();
        assert!(choice.contains(r#"<option value="public" selected>"#), "{choice}");
        let boolean = scalar_field(&field(SettingKindView::Bool, "false")).into_string();
        assert!(boolean.contains(r#"<option value="false" selected>"#), "{boolean}");
        let secret = scalar_field(&field(SettingKindView::Secret, "\u{2022}\u{2022}\u{2022}\u{2022}")).into_string();
        assert!(secret.contains(r#"type="password""#) && secret.contains(r#"value="""#), "{secret}");
        assert!(!secret.contains('\u{2022}'));
    }

    #[test]
    fn development_logging_is_chosen_as_off_or_a_number_of_hours_and_says_when_it_ends() {
        let now = 1_790_000_000;
        let field = |value: &str| AdminScalarFieldView {
            key: "logging.dev_mode_until".to_string(),
            label: "logging dev mode until".to_string(),
            value: value.to_string(),
            kind: SettingKindView::TimeLimit { now },
            ..Default::default()
        };
        let off = scalar_field(&field("0")).into_string();
        assert!(off.contains(r#"<option value="0" selected>Off</option>"#), "{off}");
        assert!(off.contains(&format!(r#"<option value="{}">On for 1 hour</option>"#, now + 3600)), "{off}");
        assert!(off.contains(&format!(r#"<option value="{}">On for 24 hours</option>"#, now + 86_400)), "{off}");
        assert!(!off.contains("On until"), "{off}");

        let on = scalar_field(&field(&(now + 600).to_string())).into_string();
        assert!(on.contains(&format!(r#"<option value="{}" selected>On until 2026-09-21 14:23 UTC</option>"#, now + 600)), "{on}");
        assert!(on.contains(r#"<option value="0">Off</option>"#), "{on}");

        let ended = scalar_field(&field(&(now - 1).to_string())).into_string();
        assert!(ended.contains(r#"<option value="0" selected>Off</option>"#), "a time already past is off: {ended}");
    }

    #[test]
    fn notices_render_with_their_level_and_restart_only_fields_say_so() {
        let data = AdminSettingsViewModel {
            notices: vec![
                Notice::Error("2 stores use the stagenet network, which no longer has any reachable nodes.".into()),
                Notice::Warning("Saved. These settings take effect after the engine restarts: server.worker_threads.".into()),
                Notice::Info("Saved, but set by an environment variable.".into()),
            ],
            scanner_configured: true,
            scanner_reachable: true,
            scanner_fields: vec![AdminScalarFieldView {
                key: "server.worker_threads".into(),
                label: "server worker threads".into(),
                value: "4".into(),
                source_label: "saved value".into(),
                restart_only: true,
                pending_restart: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let html = admin_settings_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<p class="error" role="alert">2 stores use the stagenet network"#));
        assert!(html.contains(r#"<p class="warning" role="status">Saved. These settings take effect after the engine restarts"#));
        assert!(html.contains(r#"<p class="notice">Saved, but set by an environment variable."#));
        assert!(html.contains("applies after a restart"));
        assert!(html.contains("restart needed"));
    }

}
