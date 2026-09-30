//! Admin-only pages: first-run setup (`GET`/`POST /admin/setup`), the
//! settings page (`GET`/`POST /dashboard/admin/settings?tab=<id>`), the invites page
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
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Bool,
    Choice {
        choices: Vec<String>,
    },
    ChoiceList {
        choices: Vec<String>,
    },
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
    TimeLimit {
        now: u64,
    },
}

impl From<live_settings::SettingKind> for SettingKindView {
    fn from(kind: live_settings::SettingKind) -> Self {
        use live_settings::SettingKind as K;
        let owned = |choices: Vec<&'static str>| choices.into_iter().map(str::to_string).collect();
        match kind {
            K::Integer { min, max } => SettingKindView::Integer { min, max },
            K::Bool => SettingKindView::Bool,
            K::Choice { choices } => SettingKindView::Choice {
                choices: owned(choices),
            },
            K::ChoiceList { choices } => SettingKindView::ChoiceList {
                choices: owned(choices),
            },
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
    /// The stable settings-table key.
    pub key: String,
    /// The form field's `name`, when it isn't `key`: an engine setting
    /// whose key monokulo also has (`logging.level`) is sent as
    /// `engine:<key>`, so the two can share a tab (and a form).
    pub name: String,
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

/// A node's status from the engine's `/status`, for its row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeStatusView {
    /// Its height, when it answered.
    pub height: Option<u64>,
    /// Why it didn't answer.
    pub error: Option<String>,
    /// The network it says it's on, when that isn't the one it's saved for.
    pub wrong_network: Option<String>,
    /// The node the engine is using now.
    pub in_use: bool,
    /// Skipped for a while after failing.
    pub resting: bool,
}

/// One node row on the Monero nodes tab.
#[derive(Debug, Clone, Default)]
pub struct NodeRowView {
    pub row: crate::admin_nodes::NodeRow,
    /// The engine's label for the node (`host:port` as saved), which its
    /// `/status` is found by. Empty for a row that isn't a node yet.
    pub label: String,
    /// `None` for a node with no status yet (just saved, or `/status`
    /// didn't answer).
    pub status: Option<NodeStatusView>,
}

/// One network's block on the Monero nodes tab: its nodes as rows, primary
/// first.
#[derive(Debug, Clone, Default)]
pub struct AdminNetworkFieldView {
    pub network: String,
    /// Empty when this network has no node configured yet.
    pub rows: Vec<NodeRowView>,
    /// An address to show as the example in the address help, from the
    /// engine's example for this network.
    pub example_address: Option<String>,
    /// Stores on this network, for the confirmation before clearing it.
    pub tenant_count: u64,
    /// Why the engine refused this network's nodes (a node on another
    /// network), shown at the top of its block.
    pub error: Option<String>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
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
        SettingsTab::ALL
            .into_iter()
            .find(|tab| Some(tab.id()) == id)
            .unwrap_or(SettingsTab::General)
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
            SettingsTab::Payments => &[
                (None, Engine),
                (Some("Webhooks"), Engine),
                (Some("Exchange rates"), Monokulo),
            ],
            SettingsTab::Server | SettingsTab::Other => &[(None, Engine), (None, Monokulo)],
            SettingsTab::Logging => &[(Some("Monokulo"), Monokulo), (Some("Engine"), Engine)],
        }
    }

    /// Whether every setting on this tab is the engine's, so the tab has
    /// nothing to show or save while the engine can't be reached.
    pub fn engine_only(self) -> bool {
        self.groups()
            .iter()
            .all(|(_, owner)| *owner == SettingOwner::Engine)
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
            "payment" if key == "payment.scan_chunk_memory_budget_mb" => {
                (SettingsTab::Server, None)
            }
            "payment" => (SettingsTab::Payments, None),
            "webhooks" => (SettingsTab::Payments, Some("Webhooks")),
            "key_custody" => (SettingsTab::Custody, None),
            "server" => (SettingsTab::Server, None),
            "logging" => (SettingsTab::Logging, Some("Engine")),
            _ => (SettingsTab::Other, None),
        },
    }
}

#[derive(Default)]
pub struct AdminSettingsViewModel {
    /// The tab on show.
    pub tab: SettingsTab,
    /// The tab a save was for, or the tab holding the setting a refused
    /// save was about: set only on the page a save answers with, so the
    /// word beside the Save button says how it went.
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
    /// Networks stores use that no node answers for, as far as monokulo
    /// knows (the engine's `/status`): the Monero nodes tab is marked.
    pub unreachable_networks: Vec<String>,
}

/// The id of a setting's control, which its label and help point at.
fn field_id(key: &str) -> String {
    format!("setting-{key}")
}

/// The id of a setting's help text, when it has some.
fn help_id(field: &AdminScalarFieldView) -> Option<String> {
    field
        .help
        .as_ref()
        .map(|_| format!("setting-help-{}", field.form_name()))
}

impl AdminScalarFieldView {
    /// The name the setting's control is sent under.
    pub fn form_name(&self) -> &str {
        if self.name.is_empty() {
            &self.key
        } else {
            &self.name
        }
    }
}

fn scalar_input(field: &AdminScalarFieldView) -> Markup {
    let name = field.form_name();
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
        SettingKindView::Url => {
            html! { input type="url" name=(name) value=(field.value) id=(id) aria-describedby=[help]; }
        }
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
        SettingKindView::Json => {
            html! { textarea name=(name) rows="4" id=(id) aria-describedby=[help] { (field.value) } }
        }
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
        _ => {
            html! { input type="text" name=(name) value=(field.value) id=(id) aria-describedby=[help]; }
        }
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
                label class="setting-label" for=(field_id(field.form_name())) { (field.label) }
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

/// With JavaScript, confirm before a save (or a Remove) on the Monero nodes
/// tab that would leave a network stores use with no node (task 4.4): a
/// network's rows are counted as the save would see them, blank ones not
/// counted and the row a pressed Remove is for left out. Without
/// JavaScript, the form posts and the red banner after the save says what
/// happened. Listens on the document, before fixi (capture), so it still
/// works on the form fixi swaps in after a save; cancelling stops fixi too
/// (`static/fx-glue.js`).
const CONFIRM_CLEARED_NETWORK_SCRIPT: &str = r#"(function () {
  function filled(block, skip) {
    var rows = block.querySelectorAll("[data-node-row]"), count = 0, before = 0;
    for (var i = 0; i < rows.length; i++) {
      var box = rows[i].querySelector('input[name$="_address"]');
      if (!box) continue;
      if (box.defaultValue.trim() !== "") before++;
      if (rows[i].getAttribute("data-node-row") !== skip && box.value.trim() !== "") count++;
    }
    return { now: count, before: before };
  }
  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (form.id !== "settings-form") return;
    var pressed = event.submitter && event.submitter.name === "node_action" ? event.submitter.value.split(":") : [];
    var blocks = form.querySelectorAll(".node-network[data-tenant-count]");
    for (var i = 0; i < blocks.length; i++) {
      var block = blocks[i];
      var stores = parseInt(block.getAttribute("data-tenant-count"), 10) || 0;
      var network = block.getAttribute("data-network");
      var skip = pressed[0] === "remove" && pressed[1] === network ? pressed[2] : null;
      var rows = filled(block, skip);
      if (stores > 0 && rows.now === 0 && rows.before > 0) {
        var use = stores === 1 ? "1 store uses" : stores + " stores use";
        if (!window.confirm(use + " the " + network + " network. Without a node, their payments won't be detected. Save anyway?")) {
          event.preventDefault();
          return;
        }
      }
    }
  }, true);
})();"#;

/// With JavaScript, the node form's conveniences, all bound on the document
/// so they keep working on a panel fixi swaps in:
/// - "Add another" adds a blank row after the others, numbered after the
///   highest row, and puts the cursor in its address. Without JavaScript
///   the one blank row does the same, a save at a time.
/// - A row's self-signed box shows only while its Use TLS box is ticked
///   (without JavaScript it's always there, and ignored without TLS).
const NODE_FORM_SCRIPT: &str = r#"(function () {
  function showSelfSigned(root) {
    var boxes = root.querySelectorAll("[data-node-tls]");
    for (var i = 0; i < boxes.length; i++) {
      var row = boxes[i].closest("[data-node-row]");
      var field = row && row.querySelector("[data-node-self-signed]");
      if (field) field.hidden = !boxes[i].checked;
    }
  }
  document.addEventListener("change", function (event) {
    if (event.target.matches && event.target.matches("[data-node-tls]")) showSelfSigned(event.target.closest("[data-node-row]"));
  });
  document.addEventListener("click", function (event) {
    var button = event.target.closest && event.target.closest("[data-node-add-another]");
    if (!button) return;
    var network = button.getAttribute("data-node-add-another");
    var list = document.querySelector('[data-node-rows="' + network + '"]');
    var blank = list && list.querySelector("[data-node-add]:last-of-type");
    if (!blank) return;
    var next = 0;
    list.querySelectorAll("[data-node-row]").forEach(function (row) {
      next = Math.max(next, (parseInt(row.getAttribute("data-node-row"), 10) || 0) + 1);
    });
    var old = blank.getAttribute("data-node-row");
    var row = blank.cloneNode(true);
    row.setAttribute("data-node-row", String(next));
    var renamed = function (value) {
      return value.split("node_" + network + "_" + old + "_").join("node_" + network + "_" + next + "_")
        .split("node-" + network + "-" + old + "-").join("node-" + network + "-" + next + "-");
    };
    row.querySelectorAll("[name],[id],[for],[aria-describedby]").forEach(function (el) {
      ["name", "id", "for", "aria-describedby"].forEach(function (attr) {
        if (el.hasAttribute(attr)) el.setAttribute(attr, renamed(el.getAttribute(attr)));
      });
    });
    row.querySelectorAll('input[type="text"]').forEach(function (box) { box.value = ""; box.defaultValue = ""; });
    row.querySelectorAll("[data-node-tls]").forEach(function (box) { box.checked = false; box.defaultChecked = false; });
    row.querySelectorAll('input[name$="_self_signed"]').forEach(function (box) { box.checked = true; box.defaultChecked = true; });
    list.appendChild(row);
    showSelfSigned(row);
    var address = row.querySelector('input[name$="_address"]');
    if (address) address.focus();
  });
  document.addEventListener("fx:swapped", function () { showSelfSigned(document); });
  showSelfSigned(document);
})();"#;

const ENABLED_BACKENDS: &str = "key_custody.enabled_backends";

/// The key custody backends the engine offers, and whether each is turned
/// on. Empty from an engine that doesn't say.
fn custody_backends(fields: &[AdminScalarFieldView]) -> Vec<(String, bool)> {
    let Some(field) = fields.iter().find(|f| f.key == ENABLED_BACKENDS) else {
        return Vec::new();
    };
    let SettingKindView::ChoiceList { choices } = &field.kind else {
        return Vec::new();
    };
    let enabled: Vec<&str> = field.value.split(',').map(str::trim).collect();
    choices
        .iter()
        .map(|choice| (choice.clone(), enabled.contains(&choice.as_str())))
        .collect()
}

/// The backend a key custody setting belongs to: one only that backend
/// uses is named `key_custody.<backend>_...` (`key_custody.socket_path`).
fn custody_backend_of<'a>(
    field: &AdminScalarFieldView,
    backends: &'a [(String, bool)],
) -> Option<&'a str> {
    let rest = field.key.strip_prefix("key_custody.")?;
    backends
        .iter()
        .map(|(backend, _)| backend.as_str())
        .find(|backend| {
            rest.strip_prefix(backend)
                .is_some_and(|after| after.starts_with('_'))
        })
}

/// A section of its own for each backend, shown only while it's turned on
/// (at once with JavaScript, after saving without).
fn custody_backend_sections(
    fields: &[AdminScalarFieldView],
    backends: &[(String, bool)],
) -> Markup {
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
fn save_status(data: &AdminSettingsViewModel) -> Markup {
    html! {
        @if data.saved_tab == Some(data.tab) {
            @if data.error.is_some() {
                span class="save-status error" role="alert" data-fx-focus tabindex="-1" { "Not saved - see the message above." }
            } @else if data.success.is_some() {
                span class="save-status success" role="status" data-fx-focus tabindex="-1" { "Saved." }
            }
        }
    }
}

/// The page-wide banners (nicer_admin_screen.md T4): a save's error, its
/// success and any notices, above the tab bar on every tab. `oob` marks it
/// for fixi's glue to put in place of the page's own copy when it comes
/// back with a swapped panel.
pub fn banners(data: &AdminSettingsViewModel, oob: bool) -> Markup {
    html! {
        div id="settings-banners" class="save-banners" data-fx-oob[oob] {
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

/// The engine is configured and answered, so its settings are on hand.
fn engine_available(data: &AdminSettingsViewModel) -> bool {
    data.scanner_configured && data.scanner_reachable
}

/// Where the engine's settings would be, while it can't be reached
/// (nicer_admin_screen.md T6).
fn engine_unavailable(data: &AdminSettingsViewModel) -> Markup {
    html! {
        @if !data.scanner_configured {
            p class="engine-unavailable" {
                "Set " code { "engine.url" } " and " code { "engine.admin_token" } " on the "
                a href=(SettingsTab::General.href()) { "General" } " tab and save to manage this instance's engine settings from here."
            }
        } @else {
            p class="error engine-unavailable" role="alert" {
                "Could not reach the configured engine: "
                @if let Some(scanner_error) = &data.scanner_error { (scanner_error) }
            }
        }
    }
}

/// A tab's settings from one owner under one heading, in the order they
/// are shown: the engine's in its (alphabetical) order, except that the key
/// custody backends to turn on come before the choice among them, and on
/// Server the engine's own `server.*` come before the scan memory budget.
fn group_fields<'a>(
    data: &'a AdminSettingsViewModel,
    tab: SettingsTab,
    heading: Option<&str>,
    owner: SettingOwner,
) -> Vec<&'a AdminScalarFieldView> {
    let fields = match owner {
        SettingOwner::Monokulo => &data.monokulo_fields,
        SettingOwner::Engine => &data.scanner_fields,
    };
    let mut own: Vec<&AdminScalarFieldView> = fields
        .iter()
        .filter(|f| setting_placement(&f.key, owner) == (tab, heading))
        .collect();
    if owner == SettingOwner::Engine {
        own.sort_by_key(|f| {
            (
                f.key != ENABLED_BACKENDS,
                f.key.starts_with("payment.") && tab == SettingsTab::Server,
            )
        });
    }
    own
}

/// Whether a tab has anything to show: always, except Other, which only
/// shows while a setting nobody placed is in it.
fn tab_shown(data: &AdminSettingsViewModel, tab: SettingsTab) -> bool {
    tab != SettingsTab::Other
        || tab
            .groups()
            .iter()
            .any(|(heading, owner)| !group_fields(data, tab, *heading, *owner).is_empty())
}

/// Whether a tab's label carries the marker (T5): a network stores use has
/// no node that answers (Monero nodes), or a saved setting on it waits for
/// a restart.
fn needs_attention(data: &AdminSettingsViewModel, tab: SettingsTab) -> bool {
    let unserved = tab == SettingsTab::Nodes
        && (!data.unreachable_networks.is_empty()
            || data
                .scanner_networks
                .iter()
                .any(|n| n.tenant_count > 0 && n.rows.is_empty()));
    let restart = tab.groups().iter().any(|(heading, owner)| {
        group_fields(data, tab, *heading, *owner)
            .iter()
            .any(|f| f.pending_restart)
    });
    unserved || restart
}

/// The tab bar: plain links, each its own page, so it works without
/// JavaScript; with fixi they swap just the panel and push the URL. Marked
/// tabs say so in words as well as with the dot.
pub fn tab_bar(data: &AdminSettingsViewModel, oob: bool) -> Markup {
    html! {
        nav id="settings-tabs" class="tab-bar" aria-label="Settings sections" data-fx-oob[oob] {
            @for tab in SettingsTab::ALL.into_iter().filter(|tab| tab_shown(data, *tab)) {
                @let href = tab.href();
                a href=(href) fx-action=(href) fx-target="#settings-panel" fx-push-url aria-current=[(tab == data.tab).then_some("page")] {
                    (tab.label())
                    @if needs_attention(data, tab) {
                        span class="tab-marker" aria-hidden="true" { "\u{25CF}" }
                        span class="visually-hidden" { " (needs attention)" }
                    }
                }
            }
        }
    }
}

/// `1234567` as `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A row's status line: whether the node answers and on which network,
/// then whether it's the one in use or resting after failures. Nothing
/// for a node with no status yet.
fn node_status(status: &NodeStatusView) -> Markup {
    let problem = status.wrong_network.is_some() || status.height.is_none();
    let mut parts: Vec<String> = Vec::new();
    if let Some(network) = &status.wrong_network {
        parts.push(format!("Wrong network: this node is on {network}"));
    } else if let Some(height) = status.height {
        parts.push(format!("Reachable, height {}", thousands(height)));
    } else if let Some(error) = &status.error {
        parts.push(format!("Not reachable: {error}"));
    }
    if status.in_use {
        parts.push("In use".to_string());
    }
    if status.resting {
        parts.push("Resting after failures".to_string());
    }
    html! {
        @if !parts.is_empty() {
            p class=(if problem { "node-status is-problem" } else { "node-status" }) { (parts.join(". ")) "." }
        }
    }
}

/// One node row: its address, TLS and self-signed boxes, its status and
/// its buttons. `position` is its place in the list (`None` for the blank
/// "Add a node" row, which has no status or buttons); `index` numbers its
/// fields. The buttons are submit buttons of the tab's form: pressing one
/// applies it to the submitted rows and saves, with or without JavaScript.
fn node_row(
    network: &AdminNetworkFieldView,
    index: usize,
    row: &NodeRowView,
    position: Option<(usize, usize)>,
) -> Markup {
    let n = &network.network;
    let name = |field: &str| format!("node_{n}_{index}_{field}");
    let id = |field: &str| format!("node-{n}-{index}-{field}");
    let legend = match position {
        None => "Add a node".to_string(),
        Some((0, _)) => "Primary".to_string(),
        Some((at, _)) => format!("Fallback {at}"),
    };
    let example = network
        .example_address
        .as_deref()
        .unwrap_or("node.example.com:18081");
    let error_id = row.row.error.as_ref().map(|_| id("error"));
    let described = match &error_id {
        Some(error) => format!("{} {error}", id("address-help")),
        None => id("address-help"),
    };
    html! {
        fieldset class="node-row" data-node-row=(index) data-node-add[position.is_none()] {
            legend class="node-row-name" { (legend) }
            div class="setting-field" {
                label class="setting-label" for=(id("address")) { "Address" }
                span class="field-help" id=(id("address-help")) {
                    "The node's host and port, like " code { (example) } ". "
                    code { "http://" } " or " code { "https://" } " in front is fine (" code { "https://" } " also ticks Use TLS); "
                    "an IPv6 address goes in brackets, like " code { "[::1]:18081" } "."
                }
                input type="text" name=(name("address")) id=(id("address")) value=(row.row.address) aria-describedby=(described)
                    aria-invalid=[row.row.error.as_ref().map(|_| "true")] autocomplete="off" spellcheck="false" inputmode="url";
                @if let (Some(error), Some(error_id)) = (&row.row.error, &error_id) {
                    span class="setting-problem" id=(error_id) { (error) }
                }
            }
            div class="setting-field node-tls" {
                label class="setting-label" for=(id("ssl")) { "Use TLS" }
                span class="field-help" id=(id("ssl-help")) { "Connect with TLS (https). Off by default; most nodes on port 18081 or 18089 don't use it." }
                input type="checkbox" name=(name("ssl")) id=(id("ssl")) value="on" checked[row.row.ssl] aria-describedby=(id("ssl-help")) data-node-tls;
            }
            div class="setting-field node-self-signed" data-node-self-signed {
                label class="setting-label" for=(id("self_signed")) { "Accept a self-signed certificate" }
                span class="field-help" id=(id("self_signed-help")) {
                    "Many community nodes use a self-signed TLS certificate; tick this to accept one. Only used with TLS."
                }
                input type="checkbox" name=(name("self_signed")) id=(id("self_signed")) value="on" checked[row.row.self_signed] aria-describedby=(id("self_signed-help"));
            }
            @if let Some(status) = &row.status { (node_status(status)) }
            @if let Some((at, count)) = position {
                div class="node-row-actions" {
                    @if at > 0 {
                        button type="submit" name="node_action" value=(format!("up:{n}:{index}")) { "Move up" }
                    }
                    @if at + 1 < count {
                        button type="submit" name="node_action" value=(format!("down:{n}:{index}")) { "Move down" }
                    }
                    button type="submit" name="node_action" value=(format!("remove:{n}:{index}")) data-node-remove { "Remove" }
                }
            }
        }
    }
}

/// `stagenet` as `Stagenet`, for a heading.
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// One network's block: its rows in order, then a blank "Add a node" row
/// (adding needs no JavaScript: fill it in and save). A network with no
/// nodes that no store uses starts closed, as "Add a node for <network>".
fn network_block(network: &AdminNetworkFieldView) -> Markup {
    let n = &network.network;
    let count = network.rows.len();
    let rows = html! {
        @if let Some(error) = &network.error {
            p class="error" role="alert" { (error) }
        }
        p class="hint" { "Fallbacks are tried in order when the one before fails." }
        div class="node-rows" data-node-rows=(n) {
            @for (at, row) in network.rows.iter().enumerate() {
                (node_row(network, at, row, Some((at, count))))
            }
            (node_row(network, count, &NodeRowView { row: crate::admin_nodes::NodeRow { self_signed: true, ..Default::default() }, ..Default::default() }, None))
        }
        button type="button" class="js-only node-add-another" data-node-add-another=(n) { "Add another" }
    };
    let used_by = html! {
        p class="setting-source" {
            @if network.tenant_count == 1 { "Used by 1 store." } @else { "Used by " (network.tenant_count) " stores." }
        }
    };
    html! {
        @if count > 0 || network.tenant_count > 0 || network.error.is_some() {
            section class="node-network" data-network=(n) data-tenant-count=(network.tenant_count) aria-labelledby=(format!("node-network-{n}")) {
                h3 id=(format!("node-network-{n}")) { (capitalized(n)) }
                (used_by)
                (rows)
            }
        } @else {
            details class="node-network" data-network=(n) data-tenant-count=(network.tenant_count) {
                summary { "Add a node for " (n) }
                (used_by)
                (rows)
            }
        }
    }
}

/// The Monero nodes tab's fields: a block per network.
fn node_fields(data: &AdminSettingsViewModel) -> Markup {
    html! {
        p class="hint" {
            "The Monero nodes the engine reads each network's chain from: a primary, and fallbacks tried when it fails. "
            "A network with no nodes isn't used. A node that doesn't answer is still saved; one on another network is refused."
        }
        @for network in &data.scanner_networks { (network_block(network)) }
    }
}

/// One tab's settings, group by group (`SettingsTab::groups`). Where the
/// engine's settings would be while it can't be reached, its message
/// stands in, once.
fn tab_fields(data: &AdminSettingsViewModel, tab: SettingsTab) -> Markup {
    let backends = custody_backends(&data.scanner_fields);
    let engine_down = !engine_available(data);
    let first_engine_group = tab
        .groups()
        .iter()
        .position(|(_, owner)| *owner == SettingOwner::Engine);
    html! {
        @for (i, (heading, owner)) in tab.groups().iter().enumerate() {
            @if *owner == SettingOwner::Engine && engine_down {
                @if first_engine_group == Some(i) {
                    @if let Some(heading) = heading { h3 { (heading) } }
                    (engine_unavailable(data))
                }
            } @else if tab == SettingsTab::Nodes {
                (node_fields(data))
            } @else {
                @let fields = group_fields(data, tab, *heading, *owner);
                @if !fields.is_empty() {
                    @if let Some(heading) = heading { h3 { (heading) } }
                    @for field in fields.iter().filter(|f| custody_backend_of(f, &backends).is_none()) {
                        (scalar_field(field))
                    }
                    @if tab == SettingsTab::Custody { (custody_backend_sections(&data.scanner_fields, &backends)) }
                }
            }
        }
    }
}

/// The open tab: its heading, its one form and its one Save button. What
/// fixi swaps, for a tab link or a save. `focus` marks the heading for the
/// glue to focus after a tab switch, so keyboard and screen reader users
/// land on the new tab's content.
pub fn settings_panel(data: &AdminSettingsViewModel, focus: bool) -> Markup {
    let tab = data.tab;
    html! {
        section id="settings-panel" aria-labelledby="settings-panel-title" {
            h2 id="settings-panel-title" tabindex="-1" data-fx-focus[focus] { (tab.label()) }
            p class="hint" { "Saved settings apply straight away. An environment variable, where set, always wins over the value saved here - saving still works, it just won't take effect until that variable is unset." }
            @if tab == SettingsTab::Abuse {
                p class="hint" {
                    "How this instance tells visitors apart and slows down anyone sending too many requests. A visitor "
                    "past the soft limit is asked to pass a short check (automatic with JavaScript, a 10-second wait "
                    "without); past the hard limit they're refused until the minute is up. Signed-in merchants and "
                    "plugins using their store's secret key are never checked."
                }
            }
            @if tab.engine_only() && !engine_available(data) {
                (engine_unavailable(data))
            } @else {
                form method="post" action="/dashboard/admin/settings" id="settings-form"
                    fx-action="/dashboard/admin/settings" fx-method="POST" fx-target="#settings-panel" {
                    input type="hidden" name="tab" value=(tab.id());
                    @if tab == SettingsTab::Nodes {
                        // Enter in a text box presses a form's first submit
                        // button; here that would be a row's Move or Remove.
                        // This one, first and out of sight, is Save.
                        button type="submit" class="visually-hidden" tabindex="-1" aria-hidden="true" { "Save" }
                    }
                    (tab_fields(data, tab))
                    div class="settings-actions" {
                        button type="submit" class="btn-primary" { "Save" }
                        (save_status(data))
                    }
                }
            }
        }
    }
}

/// What fixi gets back for a tab link or a save: the panel, with the
/// banners and the tab bar out of band (a save can change both: a marker
/// comes or goes, a banner appears).
pub fn settings_fragment(data: &AdminSettingsViewModel, focus_heading: bool) -> Markup {
    html! {
        (settings_panel(data, focus_heading))
        (banners(data, true))
        (tab_bar(data, true))
    }
}

pub fn admin_settings_page(chrome: &PageChrome, data: &AdminSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Admin settings" }
            (banners(data, false))
            (tab_bar(data, false))
            (settings_panel(data, false))
            script { (maud::PreEscaped(CONFIRM_CLEARED_NETWORK_SCRIPT)) }
            script { (maud::PreEscaped(CUSTODY_BACKENDS_SCRIPT)) }
            script { (maud::PreEscaped(NODE_FORM_SCRIPT)) }
        }
    };
    layout(
        chrome,
        &format!("{} - Admin settings - Monokulo", data.tab.label()),
        body,
    )
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
            (
                "exchange_rate.coingecko_enabled",
                M,
                Payments,
                Some("Exchange rates"),
            ),
            (
                "exchange_rate.coingecko_base_url",
                M,
                Payments,
                Some("Exchange rates"),
            ),
            (
                "exchange_rate.coinmarketcap_enabled",
                M,
                Payments,
                Some("Exchange rates"),
            ),
            (
                "exchange_rate.coinmarketcap_base_url",
                M,
                Payments,
                Some("Exchange rates"),
            ),
            (
                "exchange_rate.haveno_enabled",
                M,
                Payments,
                Some("Exchange rates"),
            ),
            (
                "exchange_rate.haveno_base_url",
                M,
                Payments,
                Some("Exchange rates"),
            ),
            (
                "exchange_rate.cache_seconds",
                M,
                Payments,
                Some("Exchange rates"),
            ),
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
            (
                "payment.expired_order_grace_period_minutes",
                E,
                Payments,
                None,
            ),
            ("payment.reorg_check_depth", E, Payments, None),
            ("payment.mempool_poll_interval_ms", E, Payments, None),
            ("payment.scan_chunk_memory_budget_mb", E, Server, None),
            ("webhooks.allow_private_urls", E, Payments, Some("Webhooks")),
            (
                "webhooks.delivery_timeout_ms",
                E,
                Payments,
                Some("Webhooks"),
            ),
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
            assert_eq!(
                setting_placement(key, *owner),
                (*tab, *heading),
                "{key} ({owner:?})"
            );
            assert_ne!(*tab, SettingsTab::Other, "{key}");
            // The heading is one the tab actually shows, for that owner.
            assert!(
                tab.groups().contains(&(*heading, *owner)),
                "{key} is placed under a group {tab:?} doesn't have"
            );
        }
    }

    /// The list above is every setting of both registries, not a sample.
    #[test]
    fn the_tab_list_covers_both_registries() {
        let listed = |owner: SettingOwner| -> Vec<&str> {
            PLACEMENTS
                .iter()
                .filter(|(_, o, _, _)| *o == owner)
                .map(|(key, ..)| *key)
                .collect()
        };
        let mut monokulo: Vec<&str> = crate::settings::ALL.iter().map(|s| s.key()).collect();
        monokulo.sort_unstable();
        let mut monokulo_listed = listed(SettingOwner::Monokulo);
        monokulo_listed.sort_unstable();
        assert_eq!(
            monokulo_listed, monokulo,
            "every monokulo setting has a tab"
        );

        let mut engine: Vec<&str> = scanner::engine_settings::ALL
            .iter()
            .map(|s| s.key())
            .collect();
        engine.sort_unstable();
        let mut engine_listed = listed(SettingOwner::Engine);
        engine_listed.sort_unstable();
        assert_eq!(engine_listed, engine, "every engine setting has a tab");
    }

    #[test]
    fn a_setting_the_map_does_not_know_goes_to_other() {
        assert_eq!(
            setting_placement("telemetry.sample_rate", SettingOwner::Engine),
            (SettingsTab::Other, None)
        );
        assert_eq!(
            setting_placement("brand_new", SettingOwner::Engine),
            (SettingsTab::Other, None)
        );
        assert_eq!(
            setting_placement("brand.new", SettingOwner::Monokulo),
            (SettingsTab::Other, None)
        );
    }

    #[test]
    fn a_tab_is_found_by_its_id_and_anything_else_is_general() {
        for tab in SettingsTab::ALL {
            assert_eq!(SettingsTab::from_id(Some(tab.id())), tab);
        }
        assert_eq!(SettingsTab::from_id(None), SettingsTab::General);
        assert_eq!(SettingsTab::from_id(Some("nope")), SettingsTab::General);
        assert_eq!(
            SettingsTab::Nodes.href(),
            "/dashboard/admin/settings?tab=nodes"
        );
        assert!(SettingsTab::Nodes.engine_only() && SettingsTab::Custody.engine_only());
        assert!(
            !SettingsTab::Payments.engine_only()
                && !SettingsTab::Server.engine_only()
                && !SettingsTab::Logging.engine_only()
        );
    }

    #[test]
    fn setup_page_shows_the_echoed_email_and_an_error() {
        let data = SetupViewModel {
            error: Some("Passwords do not match.".to_string()),
            email: "owner@example.com".to_string(),
        };
        let html = setup_page(&chrome(), &data).into_string();
        assert!(html.contains("Set up your admin account"));
        assert!(html.contains("Passwords do not match."));
        assert!(html.contains(r#"value="owner@example.com""#));
    }

    #[test]
    fn request_invite_page_shows_the_form_before_submission_and_a_thank_you_after() {
        let form = RequestInviteViewModel {
            error: None,
            submitted: false,
        };
        let html = request_invite_page(&chrome(), &form).into_string();
        assert!(html.contains(r#"<form method="post" action="/request-invite">"#));

        let thanks = RequestInviteViewModel {
            error: None,
            submitted: true,
        };
        let html = request_invite_page(&chrome(), &thanks).into_string();
        assert!(html.to_lowercase().contains("thanks"));
        assert!(
            !html.contains("<form method=\"post\" action=\"/request-invite\""),
            "a submitted confirmation must not still show the request form"
        );
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
    fn admin_invites_page_lists_rows_with_a_mailto_link_and_a_delete_form_carrying_the_current_page(
    ) {
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
    fn admin_invites_page_shows_the_struck_through_just_deleted_row_even_with_no_other_pending_rows(
    ) {
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

    /// A field as the handler builds it: an engine setting monokulo also
    /// has is sent as `engine:<key>`.
    fn field_for(key: &str, owner: SettingOwner) -> AdminScalarFieldView {
        let shared = owner == SettingOwner::Engine
            && PLACEMENTS
                .iter()
                .any(|(k, o, ..)| *k == key && *o == SettingOwner::Monokulo);
        AdminScalarFieldView {
            key: key.to_string(),
            name: if shared {
                format!("engine:{key}")
            } else {
                String::new()
            },
            label: key.replace(['.', '_'], " "),
            value: "1".to_string(),
            source_label: "default".to_string(),
            help: Some(format!("What {key} is for.")),
            kind: SettingKindView::Text,
            ..Default::default()
        }
    }

    /// The whole page's data, every setting known today, on `tab`.
    fn full_view(tab: SettingsTab) -> AdminSettingsViewModel {
        let fields = |owner: SettingOwner| -> Vec<AdminScalarFieldView> {
            PLACEMENTS
                .iter()
                .filter(|(key, o, ..)| *o == owner && !key.starts_with("monero_node."))
                .map(|(key, ..)| field_for(key, owner))
                .collect()
        };
        AdminSettingsViewModel {
            tab,
            monokulo_fields: fields(SettingOwner::Monokulo),
            scanner_configured: true,
            scanner_reachable: true,
            scanner_fields: fields(SettingOwner::Engine),
            scanner_networks: ["mainnet", "stagenet", "testnet"]
                .into_iter()
                .map(|network| AdminNetworkFieldView {
                    network: network.to_string(),
                    rows: vec![node_row_view("node.example.com:18081")],
                    example_address: Some("node.example.com:18089".to_string()),
                    tenant_count: 0,
                    error: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn node_row_view(address: &str) -> NodeRowView {
        NodeRowView {
            row: crate::admin_nodes::NodeRow {
                address: address.to_string(),
                ssl: false,
                self_signed: true,
                error: None,
            },
            label: address.to_string(),
            status: None,
        }
    }

    fn page(data: &AdminSettingsViewModel) -> String {
        admin_settings_page(&chrome(), data).into_string()
    }

    /// The tab bar's link to `tab`, with `aria-current` or not.
    fn tab_link(tab: SettingsTab, current: bool) -> String {
        let href = tab.href();
        let current = if current {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(
            r##"<a href="{href}" fx-action="{href}" fx-target="#settings-panel" fx-push-url{current}>{}"##,
            tab.label()
        )
    }

    #[test]
    fn each_tab_shows_only_its_own_settings() {
        for tab in SettingsTab::ALL {
            let html = page(&full_view(tab));
            for (key, owner, placed, _) in PLACEMENTS
                .iter()
                .filter(|(key, ..)| !key.starts_with("monero_node."))
            {
                let name = field_for(key, *owner).form_name().to_string();
                let shown = html.contains(&format!(r#"name="{name}""#));
                assert_eq!(shown, *placed == tab, "{name} on {tab:?}");
            }
            assert_eq!(
                html.contains(r#"name="node_stagenet_0_address""#),
                tab == SettingsTab::Nodes,
                "{tab:?}"
            );
            // One form, one Save, and the tab it's for.
            assert_eq!(html.matches("<form ").count(), 1, "{tab:?}");
            assert!(
                html.contains(&format!(
                    r#"<input type="hidden" name="tab" value="{}">"#,
                    tab.id()
                )),
                "{tab:?}"
            );
            assert_eq!(
                html.matches(r#"<button type="submit" class="btn-primary">Save</button>"#)
                    .count(),
                1,
                "{tab:?}"
            );
        }
    }

    #[test]
    fn tabs_with_two_owners_head_each_group() {
        let payments = page(&full_view(SettingsTab::Payments));
        let order = [
            "name=\"payment.confirmations_required\"",
            "<h3>Webhooks</h3>",
            "name=\"webhooks.max_attempts\"",
            "<h3>Exchange rates</h3>",
            "name=\"exchange_rate.cache_seconds\"",
        ];
        let at: Vec<usize> = order
            .iter()
            .map(|needle| {
                payments
                    .find(needle)
                    .unwrap_or_else(|| panic!("{needle}: {payments}"))
            })
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");

        let logging = page(&full_view(SettingsTab::Logging));
        let monokulo = logging.find("<h3>Monokulo</h3>").expect(&logging);
        let engine = logging.find("<h3>Engine</h3>").expect(&logging);
        assert!(monokulo < logging.find(r#"name="logging.level""#).unwrap());
        assert!(
            engine < logging.find(r#"name="engine:logging.level""#).unwrap() && monokulo < engine
        );

        let server = page(&full_view(SettingsTab::Server));
        assert!(
            server.find(r#"name="server.worker_threads""#).unwrap()
                < server
                    .find(r#"name="payment.scan_chunk_memory_budget_mb""#)
                    .unwrap()
        );
        assert!(server.contains(r#"name="http_cache.max_mb""#));

        let abuse = page(&full_view(SettingsTab::Abuse));
        assert!(
            abuse.contains("How this instance tells visitors apart"),
            "the explanation stays with its settings"
        );
        assert!(!page(&full_view(SettingsTab::General))
            .contains("How this instance tells visitors apart"));
    }

    #[test]
    fn general_is_the_default_and_the_open_tab_is_marked_current() {
        let html = page(&AdminSettingsViewModel {
            ..full_view(SettingsTab::General)
        });
        assert_eq!(AdminSettingsViewModel::default().tab, SettingsTab::General);
        assert!(
            html.contains(&tab_link(SettingsTab::General, true)),
            "{html}"
        );
        assert!(
            html.contains(&tab_link(SettingsTab::Nodes, false)),
            "{html}"
        );
        for tab in SettingsTab::ALL
            .into_iter()
            .filter(|tab| *tab != SettingsTab::Other)
        {
            let html = page(&full_view(tab));
            let bar = &html[html.find(r#"<nav id="settings-tabs""#).unwrap()..];
            let bar = &bar[..bar.find("</nav>").unwrap()];
            assert_eq!(bar.matches(r#"aria-current="page""#).count(), 1, "{tab:?}");
            assert!(html.contains(&tab_link(tab, true)), "{tab:?}: {html}");
            assert!(
                html.contains(&format!(
                    "<title>{} - Admin settings - Monokulo</title>",
                    tab.label()
                )),
                "{tab:?}"
            );
        }
    }

    /// Other only appears while a setting nobody placed is in it.
    #[test]
    fn the_other_tab_shows_only_with_something_in_it() {
        let html = page(&full_view(SettingsTab::General));
        assert!(!html.contains("tab=other"), "{html}");
        let mut data = full_view(SettingsTab::Other);
        data.scanner_fields
            .push(field_for("telemetry.sample_rate", SettingOwner::Engine));
        let html = page(&data);
        assert!(html.contains(&tab_link(SettingsTab::Other, true)), "{html}");
        assert!(html.contains(r#"name="telemetry.sample_rate""#), "{html}");
    }

    #[test]
    fn banners_are_above_the_tab_bar_on_every_tab() {
        for tab in SettingsTab::ALL {
            let data = AdminSettingsViewModel {
                error: Some("Something was refused.".into()),
                notices: vec![Notice::Warning(
                    "Saved. These settings take effect after the engine restarts: server.bind."
                        .into(),
                )],
                ..full_view(tab)
            };
            let html = page(&data);
            let banner = html.find("Something was refused.").expect(&html);
            let notice = html
                .find("take effect after the engine restarts")
                .expect(&html);
            let bar = html.find(r#"<nav id="settings-tabs""#).expect(&html);
            let panel = html.find(r#"<section id="settings-panel""#).expect(&html);
            assert!(banner < bar && notice < bar && bar < panel, "{tab:?}");
        }
    }

    #[test]
    fn a_tab_is_marked_while_something_on_it_needs_attention() {
        let marked = |html: &str, tab: SettingsTab| {
            let start = html.find(&format!(r#"href="{}""#, tab.href())).unwrap();
            let end = start + html[start..].find("</a>").unwrap();
            html[start..end].contains(r#"<span class="tab-marker" aria-hidden="true">●</span><span class="visually-hidden"> (needs attention)</span>"#)
        };
        let html = page(&full_view(SettingsTab::General));
        assert!(
            SettingsTab::ALL
                .iter()
                .filter(|t| **t != SettingsTab::Other)
                .all(|t| !marked(&html, *t)),
            "nothing to see: {html}"
        );

        // A network stores use that no node answers for.
        let html = page(&AdminSettingsViewModel {
            unreachable_networks: vec!["stagenet".into()],
            ..full_view(SettingsTab::General)
        });
        assert!(marked(&html, SettingsTab::Nodes), "{html}");
        assert!(!marked(&html, SettingsTab::Server));

        // A network stores use with no node at all.
        let mut data = full_view(SettingsTab::General);
        data.scanner_networks[1].rows.clear();
        data.scanner_networks[1].tenant_count = 2;
        assert!(marked(&page(&data), SettingsTab::Nodes));

        // A saved setting waiting for a restart, whichever process owns it.
        let mut data = full_view(SettingsTab::General);
        data.scanner_fields
            .iter_mut()
            .find(|f| f.key == "server.worker_threads")
            .unwrap()
            .pending_restart = true;
        let html = page(&data);
        assert!(
            marked(&html, SettingsTab::Server) && !marked(&html, SettingsTab::Nodes),
            "{html}"
        );
        let mut data = full_view(SettingsTab::General);
        data.monokulo_fields
            .iter_mut()
            .find(|f| f.key == "logging.level")
            .unwrap()
            .pending_restart = true;
        assert!(marked(&page(&data), SettingsTab::Logging));
    }

    /// T6: an engine that isn't set up or doesn't answer. Its own tabs say
    /// so instead of a form; the mixed ones still show and save monokulo's
    /// settings, with the message where the engine's would be.
    #[test]
    fn every_tab_copes_with_an_engine_it_cannot_reach() {
        let unreachable = |tab| AdminSettingsViewModel {
            scanner_reachable: false,
            scanner_error: Some("connection refused".into()),
            scanner_fields: vec![],
            ..full_view(tab)
        };
        let unconfigured = |tab| AdminSettingsViewModel {
            scanner_configured: false,
            scanner_reachable: false,
            scanner_fields: vec![],
            ..full_view(tab)
        };
        for tab in SettingsTab::ALL
            .into_iter()
            .filter(|tab| *tab != SettingsTab::Other)
        {
            for (data, message) in [
                (
                    unreachable(tab),
                    "Could not reach the configured engine: connection refused",
                ),
                (
                    unconfigured(tab),
                    "Set <code>engine.url</code> and <code>engine.admin_token</code> on the",
                ),
            ] {
                let html = page(&data);
                let engine_part = tab
                    .groups()
                    .iter()
                    .any(|(_, owner)| *owner == SettingOwner::Engine);
                assert_eq!(
                    html.matches(message).count(),
                    usize::from(engine_part),
                    "{tab:?}: {html}"
                );
                if tab.engine_only() {
                    assert!(!html.contains("<form "), "nothing to save on {tab:?}");
                } else {
                    assert!(
                        html.contains("<form ")
                            && html.contains(r#"class="btn-primary">Save</button>"#),
                        "{tab:?}"
                    );
                    assert!(
                        !html.contains("<h3>Webhooks</h3>"),
                        "one message, not a heading per group: {html}"
                    );
                }
            }
        }
        let payments = page(&unreachable(SettingsTab::Payments));
        assert!(
            payments.contains(r#"name="exchange_rate.cache_seconds""#),
            "{payments}"
        );
        let logging = page(&unreachable(SettingsTab::Logging));
        assert!(
            logging.find("<h3>Engine</h3>").unwrap() < logging.find("Could not reach").unwrap(),
            "{logging}"
        );
    }

    #[test]
    fn a_setting_shows_its_name_then_help_then_control() {
        let html = page(&AdminSettingsViewModel {
            monokulo_fields: vec![AdminScalarFieldView {
                key: "engine.url".to_string(),
                label: "engine url".to_string(),
                value: "http://scanner.internal".to_string(),
                source_label: "saved value".to_string(),
                help: Some("Where the engine listens.".to_string()),
                kind: SettingKindView::Url,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(
            html.contains(concat!(
                r#"<label class="setting-label" for="setting-engine.url">engine url</label>"#,
                r#"<span class="field-help" id="setting-help-engine.url">Where the engine listens.</span>"#,
                r#"<input type="url" name="engine.url" value="http://scanner.internal""#,
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"aria-describedby="setting-help-engine.url""#),
            "{html}"
        );
    }

    /// A network with no nodes that no store uses starts closed; one with
    /// nodes, or stores, is an open block with its store count.
    #[test]
    fn a_network_is_closed_until_it_has_nodes_or_stores() {
        let mut data = full_view(SettingsTab::Nodes);
        data.scanner_networks[0].tenant_count = 2;
        data.scanner_networks[2].rows.clear();
        let html = page(&data);
        assert!(
            html.contains(r#"<section class="node-network" data-network="mainnet" data-tenant-count="2" aria-labelledby="node-network-mainnet"><h3 id="node-network-mainnet">Mainnet</h3><p class="setting-source">Used by 2 stores.</p>"#),
            "{html}"
        );
        assert!(html.contains(r#"<details class="node-network" data-network="testnet" data-tenant-count="0"><summary>Add a node for testnet</summary>"#), "{html}");
        assert!(!html.contains("<textarea"), "no JSON box");
        assert!(
            !html.contains("<summary>Example</summary>"),
            "no JSON example"
        );

        // With stores but no node, it's open, so the admin sees the gap.
        data.scanner_networks[2].tenant_count = 1;
        let html = page(&data);
        assert!(html.contains(r#"<h3 id="node-network-testnet">Testnet</h3><p class="setting-source">Used by 1 store.</p>"#), "{html}");
    }

    #[test]
    fn node_rows_are_named_in_order_with_their_buttons_and_a_blank_row_to_add_one() {
        let mut data = full_view(SettingsTab::Nodes);
        data.scanner_networks[1].rows = vec![
            node_row_view("a.example:1"),
            node_row_view("b.example:2"),
            node_row_view("c.example:3"),
        ];
        data.scanner_networks[1].rows[1].row.ssl = true;
        data.scanner_networks[1].rows[1].row.self_signed = false;
        let html = page(&data);
        let block = &html[html.find(r#"data-network="stagenet""#).unwrap()
            ..html.find(r#"data-network="testnet""#).unwrap()];
        let legends: Vec<&str> = block
            .match_indices("<legend class=\"node-row-name\">")
            .map(|(at, m)| {
                &block[at + m.len()..at + m.len() + block[at + m.len()..].find('<').unwrap()]
            })
            .collect();
        assert_eq!(
            legends,
            ["Primary", "Fallback 1", "Fallback 2", "Add a node"]
        );
        assert!(block.contains("Fallbacks are tried in order when the one before fails."));
        // The fields, named by network and row.
        assert!(block.contains(r#"<input type="text" name="node_stagenet_1_address" id="node-stagenet-1-address" value="b.example:2""#), "{block}");
        assert!(block.contains(r#"<input type="checkbox" name="node_stagenet_1_ssl" id="node-stagenet-1-ssl" value="on" checked"#), "{block}");
        assert!(block.contains(r#"<input type="checkbox" name="node_stagenet_1_self_signed" id="node-stagenet-1-self_signed" value="on" aria-describedby"#), "{block}");
        assert!(
            block.contains(
                r#"name="node_stagenet_3_address" id="node-stagenet-3-address" value="""#
            ),
            "the blank row: {block}"
        );
        assert!(block.contains(r#"<input type="checkbox" name="node_stagenet_3_self_signed" id="node-stagenet-3-self_signed" value="on" checked"#), "self-signed is ticked by default: {block}");
        // No Move up on the first row, no Move down on the last, none on the blank one.
        let buttons: Vec<&str> = block
            .match_indices(r#"name="node_action" value=""#)
            .map(|(at, m)| {
                &block[at + m.len()..at + m.len() + block[at + m.len()..].find('"').unwrap()]
            })
            .collect();
        assert_eq!(
            buttons,
            [
                "down:stagenet:0",
                "remove:stagenet:0",
                "up:stagenet:1",
                "down:stagenet:1",
                "remove:stagenet:1",
                "up:stagenet:2",
                "remove:stagenet:2"
            ]
        );
        // Each field: its name, then what it's for, then the control.
        assert!(block.contains(r#"<label class="setting-label" for="node-stagenet-0-address">Address</label><span class="field-help" id="node-stagenet-0-address-help">The node's host and port, like <code>node.example.com:18089</code>."#), "{block}");
        // Enter in an address box saves, rather than pressing a row's button.
        let form = &html[html.find("<form ").unwrap()..];
        assert!(form.find(r#"<button type="submit" class="visually-hidden" tabindex="-1" aria-hidden="true">Save</button>"#).unwrap() < form.find(r#"name="node_action""#).unwrap());
    }

    #[test]
    fn a_nodes_status_is_said_in_words() {
        let mut data = full_view(SettingsTab::Nodes);
        let statuses = [
            NodeStatusView {
                height: Some(1_234_567),
                in_use: true,
                ..Default::default()
            },
            NodeStatusView {
                error: Some("connection refused".into()),
                resting: true,
                ..Default::default()
            },
            NodeStatusView {
                height: Some(10),
                wrong_network: Some("mainnet".into()),
                ..Default::default()
            },
        ];
        data.scanner_networks[1].rows = statuses
            .iter()
            .enumerate()
            .map(|(i, status)| NodeRowView {
                status: Some(status.clone()),
                ..node_row_view(&format!("n{i}.example:1"))
            })
            .collect();
        data.scanner_networks[1]
            .rows
            .push(node_row_view("new.example:1"));
        let html = page(&data);
        assert!(
            html.contains(r#"<p class="node-status">Reachable, height 1,234,567. In use.</p>"#),
            "{html}"
        );
        assert!(html.contains(r#"<p class="node-status is-problem">Not reachable: connection refused. Resting after failures.</p>"#), "{html}");
        assert!(
            html.contains(
                r#"<p class="node-status is-problem">Wrong network: this node is on mainnet.</p>"#
            ),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"class="node-status"#).count(),
            3,
            "nothing for a node with no status yet"
        );
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(1000), "1,000");
    }

    #[test]
    fn what_is_wrong_shows_where_it_is() {
        let mut data = full_view(SettingsTab::Nodes);
        data.scanner_networks[1].rows[0].row.address = "node.example.com".into();
        data.scanner_networks[1].rows[0].row.error =
            Some("Add the port, like node.example.com:18081.".into());
        data.scanner_networks[2].error =
            Some("node.example.com:18081 is on mainnet, not testnet.".into());
        let html = page(&data);
        assert!(
            html.contains(r#"value="node.example.com" aria-describedby="node-stagenet-0-address-help node-stagenet-0-error" aria-invalid="true""#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="setting-problem" id="node-stagenet-0-error">Add the port, like node.example.com:18081.</span>"#), "{html}");
        let testnet = &html[html.find(r#"data-network="testnet""#).unwrap()..];
        assert!(testnet.contains(r#"<p class="error" role="alert">node.example.com:18081 is on mainnet, not testnet.</p><p class="hint">Fallbacks"#), "at the top of its block: {testnet}");
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
        let backends = || SettingKindView::ChoiceList {
            choices: vec!["plain".into(), "socket".into()],
        };
        let page = |enabled: &str| {
            let data = AdminSettingsViewModel {
                tab: SettingsTab::Custody,
                scanner_configured: true,
                scanner_reachable: true,
                scanner_fields: vec![
                    field(
                        "key_custody.default_backend",
                        "plain",
                        SettingKindView::Choice {
                            choices: vec!["plain".into(), "socket".into()],
                        },
                    ),
                    field("key_custody.enabled_backends", enabled, backends()),
                    field(
                        "key_custody.socket_path",
                        "/run/kc.sock",
                        SettingKindView::Path,
                    ),
                ],
                ..Default::default()
            };
            admin_settings_page(&chrome(), &data).into_string()
        };

        let html = page("plain");
        // The backends are boxes to tick, after an empty value so ticking
        // none still says so; they come before the choice among them.
        assert!(
            html.contains(r#"<input type="hidden" name="key_custody.enabled_backends" value="">"#),
            "{html}"
        );
        assert!(html.contains(r#"<input type="checkbox" name="key_custody.enabled_backends" value="plain" checked>"#), "{html}");
        assert!(
            html.contains(
                r#"<input type="checkbox" name="key_custody.enabled_backends" value="socket">"#
            ),
            "{html}"
        );
        assert!(
            html.find(r#"name="key_custody.enabled_backends""#).unwrap()
                < html.find(r#"name="key_custody.default_backend""#).unwrap()
        );
        // The socket's path sits in the socket's own section, hidden while
        // socket is off; plain has nothing to set.
        let socket = html
            .find(r#"<section class="custody-backend" data-custody-backend="socket" hidden>"#)
            .expect(&html);
        assert!(
            html.find(r#"name="key_custody.socket_path""#).unwrap() > socket,
            "{html}"
        );
        assert!(
            html.find(r#"name="key_custody.default_backend""#).unwrap() < socket,
            "{html}"
        );
        assert!(html.contains(r#"<section class="custody-backend" data-custody-backend="plain"><h3>Key custody: plain</h3><p class="hint">Nothing to set up"#), "{html}");
        assert!(
            html.contains(r#"name !== "key_custody.enabled_backends""#),
            "shown as soon as it's ticked, with JavaScript"
        );

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
        let choice = scalar_field(&field(
            SettingKindView::Choice {
                choices: vec!["public".into(), "invite_only".into()],
            },
            "public",
        ))
        .into_string();
        assert!(
            choice.contains(r#"<option value="public" selected>"#),
            "{choice}"
        );
        let boolean = scalar_field(&field(SettingKindView::Bool, "false")).into_string();
        assert!(
            boolean.contains(r#"<option value="false" selected>"#),
            "{boolean}"
        );
        let secret = scalar_field(&field(
            SettingKindView::Secret,
            "\u{2022}\u{2022}\u{2022}\u{2022}",
        ))
        .into_string();
        assert!(
            secret.contains(r#"type="password""#) && secret.contains(r#"value="""#),
            "{secret}"
        );
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
        assert!(
            off.contains(r#"<option value="0" selected>Off</option>"#),
            "{off}"
        );
        assert!(
            off.contains(&format!(
                r#"<option value="{}">On for 1 hour</option>"#,
                now + 3600
            )),
            "{off}"
        );
        assert!(
            off.contains(&format!(
                r#"<option value="{}">On for 24 hours</option>"#,
                now + 86_400
            )),
            "{off}"
        );
        assert!(!off.contains("On until"), "{off}");

        let on = scalar_field(&field(&(now + 600).to_string())).into_string();
        assert!(
            on.contains(&format!(
                r#"<option value="{}" selected>On until 2026-09-21 14:23 UTC</option>"#,
                now + 600
            )),
            "{on}"
        );
        assert!(on.contains(r#"<option value="0">Off</option>"#), "{on}");

        let ended = scalar_field(&field(&(now - 1).to_string())).into_string();
        assert!(
            ended.contains(r#"<option value="0" selected>Off</option>"#),
            "a time already past is off: {ended}"
        );
    }

    #[test]
    fn notices_render_with_their_level_and_restart_only_fields_say_so() {
        let data = AdminSettingsViewModel {
            notices: vec![
                Notice::Error("2 stores use the stagenet network, which no longer has any reachable nodes.".into()),
                Notice::Warning("Saved. These settings take effect after the engine restarts: server.worker_threads.".into()),
                Notice::Info("Saved, but set by an environment variable.".into()),
            ],
            tab: SettingsTab::Server,
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
