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

/// One editable field on the admin settings page - either one of monokulo's
/// own settings or one of the *proxied* scanner settings, fetched live over
/// HTTP from whichever scanner instance is configured.
pub struct AdminScalarFieldView {
    /// The stable settings-table key (also the form field's `name`).
    pub key: String,
    pub label: String,
    /// The field's current *effective* value - what wins under
    /// `env > database > default`.
    pub value: String,
    /// `"environment variable"`, `"saved value"`, or `"default"`.
    pub source_label: String,
}

/// One `monero_node.<network>` entry on the scanner-settings half of the
/// admin page - shown/edited as a single JSON text field.
pub struct AdminNetworkFieldView {
    pub network: String,
    /// Empty when this network has no node configured yet.
    pub value_json: String,
}

#[derive(Default)]
pub struct AdminSettingsViewModel {
    pub error: Option<String>,
    pub success: Option<String>,
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

fn scalar_field(field: &AdminScalarFieldView) -> Markup {
    html! {
        label { (field.label) " " input type="text" name=(field.key) value=(field.value); }
        span class="setting-source" { "(" (field.source_label) ")" }
    }
}

pub fn admin_settings_page(chrome: &PageChrome, data: &AdminSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Admin settings" }
            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }
            @if let Some(success) = &data.success {
                p class="success" { (success) }
            }

            h2 { "Monokulo" }
            p { "An environment variable, where set, always wins over the value saved here - saving still works, it just won't take effect until that variable is unset." }
            form method="post" action="/dashboard/admin/settings" {
                @for field in &data.monokulo_fields {
                    (scalar_field(field))
                }
                button type="submit" { "Save monokulo settings" }
            }

            h2 { "Scanner" }
            @if !data.scanner_configured {
                p { "Set " code { "engine.url" } " and " code { "engine.admin_token" } " above and save to manage this instance's scanner settings from here." }
            } @else if data.scanner_reachable {
                form method="post" action="/dashboard/admin/scanner-settings" {
                    @for field in &data.scanner_fields {
                        (scalar_field(field))
                    }
                    @for network in &data.scanner_networks {
                        label {
                            "Monero node (" (network.network) ") "
                            textarea name=(format!("monero_node_{}", network.network)) rows="4" { (network.value_json) }
                        }
                        span class="setting-source" { "JSON, or leave empty to leave this network unconfigured" }
                    }
                    button type="submit" { "Save scanner settings" }
                }
            } @else {
                p class="error" {
                    "Could not reach the configured scanner: "
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
        assert!(html.contains("Could not reach the configured scanner"));
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
            }],
            scanner_configured: true,
            scanner_reachable: true,
            scanner_fields: vec![AdminScalarFieldView {
                key: "payment.confirmations_required".to_string(),
                label: "payment confirmations required".to_string(),
                value: "10".to_string(),
                source_label: "default".to_string(),
            }],
            scanner_networks: vec![AdminNetworkFieldView { network: "mainnet".to_string(), value_json: "{}".to_string() }],
            ..Default::default()
        };
        let html = admin_settings_page(&chrome(), &data).into_string();
        assert!(html.contains("engine url"));
        assert!(html.contains(r#"value="http://scanner.internal""#));
        assert!(html.contains("payment confirmations required"));
        assert!(html.contains(r#"value="10""#));
        assert!(html.contains("Monero node (mainnet)"));
        assert!(html.contains(r#"name="monero_node_mainnet""#));
    }
}
