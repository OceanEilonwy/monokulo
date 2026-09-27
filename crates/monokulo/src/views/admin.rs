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
                button type="submit" { "Create admin account" }
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
                    button type="submit" { "Request an invite" }
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
    pub created_at_display: String,
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
                    form method="post" action=(format!("/dashboard/admin/invites/{}/delete?page={}", row.id, page)) class="inline-form" {
                        button type="submit" { "delete" }
                    }
                }
            }
        }
    }
}

pub fn admin_invites_page(chrome: &PageChrome, data: &AdminInvitesViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Invites" }
            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }
            @if let Some(success) = &data.success {
                p class="success" { (success) }
            }

            form method="post" action="/dashboard/admin/invites/create-link" {
                button type="submit" { "Create invite link" }
            }
            @if let Some(link) = &data.created_link {
                p { "Share this link - it works once:" }
                pre { (link) }
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
                        a href=(format!("/dashboard/admin/invites?page={}", data.previous_page)) { "Previous" }
                    }
                    span { "Page " (data.page) " of " (data.total_pages) }
                    @if data.has_next {
                        a href=(format!("/dashboard/admin/invites?page={}", data.next_page)) { "Next" }
                    }
                }
                form method="post" action="/dashboard/admin/invites/delete-all" {
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

#[derive(Default)]
pub struct AdminSettingsViewModel {
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

fn scalar_input(field: &AdminScalarFieldView) -> Markup {
    let name = field.key.as_str();
    match &field.kind {
        SettingKindView::Integer { min, max } => html! {
            input type="number" name=(name) value=(field.value) min=[min] max=[max] step="1";
        },
        SettingKindView::Bool => html! {
            select name=(name) {
                option value="true" selected[field.value == "true"] { "true" }
                option value="false" selected[field.value == "false"] { "false" }
            }
        },
        SettingKindView::Choice { choices } => html! {
            select name=(name) {
                @for choice in choices {
                    option value=(choice) selected[&field.value == choice] { (choice) }
                }
            }
        },
        SettingKindView::Url => html! { input type="url" name=(name) value=(field.value); },
        // Never echoed back: left empty means "keep the current one".
        SettingKindView::Secret => html! {
            input type="password" name=(name) value="" autocomplete="off"
                placeholder=(if field.value.is_empty() { "not set" } else { "set - leave empty to keep it" });
            @if !field.value.is_empty() {
                label class="inline" {
                    input type="checkbox" name=(format!("clear:{name}")) value="on";
                    " Clear it"
                }
            }
        },
        SettingKindView::Json => html! { textarea name=(name) rows="4" { (field.value) } },
        _ => html! { input type="text" name=(name) value=(field.value); },
    }
}

fn scalar_field(field: &AdminScalarFieldView) -> Markup {
    html! {
        div class="setting-field" {
            label {
                (field.label) " "
                (scalar_input(field))
            }
            @if let Some(help) = &field.help {
                span class="field-help" { (help) }
            }
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
/// and the red banner after the save says what happened.
const CONFIRM_CLEARED_NETWORK_SCRIPT: &str = r#"(function () {
  var form = document.getElementById("scanner-settings-form");
  if (!form) return;
  form.addEventListener("submit", function (event) {
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
  });
})();"#;

/// Settings shown under "Abuse protection" (`crate::abuse`).
fn is_abuse_field(key: &str) -> bool {
    key.starts_with("abuse.") || key.starts_with("rate_limit.")
}

/// The engine's settings, grouped (task 4.7).
fn engine_group(key: &str) -> &'static str {
    match key.split('.').next().unwrap_or("") {
        "key_custody" => "Key custody",
        "payment" => "Payments",
        "server" => "Server",
        "webhooks" => "Webhooks",
        _ => "Other",
    }
}

pub fn admin_settings_page(chrome: &PageChrome, data: &AdminSettingsViewModel) -> Markup {
    let groups = ["Key custody", "Payments", "Server", "Webhooks", "Other"];
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Admin settings" }
            @if let Some(error) = &data.error {
                p class="error" role="alert" { (error) }
            }
            @if let Some(success) = &data.success {
                p class="success" { (success) }
            }
            (notices(&data.notices))

            h2 { "Monokulo" }
            p class="hint" { "Saved settings apply straight away. An environment variable, where set, always wins over the value saved here - saving still works, it just won't take effect until that variable is unset." }
            form method="post" action="/dashboard/admin/settings" {
                @for field in data.monokulo_fields.iter().filter(|f| !is_abuse_field(&f.key)) {
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
                button type="submit" { "Save monokulo settings" }
            }

            h2 { "Engine" }
            @if !data.scanner_configured {
                p { "Set " code { "engine.url" } " and " code { "engine.admin_token" } " above and save to manage this instance's engine settings from here." }
            } @else if data.scanner_reachable {
                form method="post" action="/dashboard/admin/scanner-settings" id="scanner-settings-form" {
                    h3 { "Monero nodes" }
                    @for network in &data.scanner_networks {
                        div class="setting-field" {
                            label {
                                "Monero node (" (network.network) ") "
                                textarea name=(format!("monero_node_{}", network.network)) rows="4"
                                    data-network=(network.network) data-tenant-count=(network.tenant_count) { (network.value_json) }
                            }
                            @if let Some(description) = &network.description {
                                span class="field-help" { (description) }
                            }
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
                        @if data.scanner_fields.iter().any(|f| engine_group(&f.key) == group) {
                            h3 { (group) }
                            @for field in data.scanner_fields.iter().filter(|f| engine_group(&f.key) == group) {
                                (scalar_field(field))
                            }
                        }
                    }
                    button type="submit" { "Save engine settings" }
                }
                script { (maud::PreEscaped(CONFIRM_CLEARED_NETWORK_SCRIPT)) }
            } @else {
                p class="error" role="alert" {
                    "Could not reach the configured engine: "
                    @if let Some(scanner_error) = &data.scanner_error { (scanner_error) }
                }
            }
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
            created_at_display: "just now".to_string(),
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
        assert!(html.contains(r#"<span class="field-help">Where the engine listens.</span>"#));
        assert!(html.contains("Abuse protection"));
        assert!(html.contains(r#"type="url" name="engine.url" value="http://scanner.internal""#), "{html}");
        assert!(html.contains("payment confirmations required"));
        assert!(html.contains(r#"type="number" name="payment.confirmations_required" value="10" min="0" max="720""#), "{html}");
        assert!(html.contains("Example: <code>10</code>"));
        assert!(html.contains("Monero node (mainnet)"));
        assert!(html.contains(r#"name="monero_node_mainnet""#));
        assert!(html.contains(r#"data-tenant-count="2""#));
        assert!(html.contains("Used by 2 stores."));
        assert!(html.contains("fallbacks"), "the node field explains its shape");
        assert!(html.contains("window.confirm"), "confirms before clearing a network in use");
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
