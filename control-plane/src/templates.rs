//! Minimal built-in HTML templates for the control-plane's browser-facing
//! signup/login pages (WBS 1.3.1, `http/dashboard.rs`).
//!
//! Loosely mirrors the engine's own `TemplateEngine`
//! (`../moneropay-core/src/templates.rs` at the repo root, which loads a
//! per-tenant custom checkout template from disk with an embedded
//! fallback) but is deliberately much simpler: control-plane has no
//! per-tenant customization concept for these pages, just two fixed,
//! `include_str!`-embedded templates, always the built-in ones.

use handlebars::Handlebars;
use serde::Serialize;

const STYLES_PARTIAL: &str = include_str!("../templates/_styles.html.hbs");
const NAV_PARTIAL: &str = include_str!("../templates/_nav.html.hbs");
const INTEGRATION_HELP_PARTIAL: &str = include_str!("../templates/_integration_help.html.hbs");

const LANDING_TEMPLATE: &str = include_str!("../templates/landing.html.hbs");
const SIGNUP_TEMPLATE: &str = include_str!("../templates/signup.html.hbs");
const LOGIN_TEMPLATE: &str = include_str!("../templates/login.html.hbs");
const CONNECT_TEMPLATE: &str = include_str!("../templates/connect.html.hbs");
const ORDERS_TEMPLATE: &str = include_str!("../templates/orders.html.hbs");
const ORDER_DETAIL_TEMPLATE: &str = include_str!("../templates/order_detail.html.hbs");
const WEBHOOKS_TEMPLATE: &str = include_str!("../templates/webhooks.html.hbs");
const CONNECT_PLATFORM_TEMPLATE: &str = include_str!("../templates/connect_platform.html.hbs");
const DASHBOARD_HOME_TEMPLATE: &str = include_str!("../templates/dashboard_home.html.hbs");
const NEW_STORE_PICKER_TEMPLATE: &str = include_str!("../templates/new_store_picker.html.hbs");
const WOOCOMMERCE_INSTRUCTIONS_TEMPLATE: &str = include_str!("../templates/woocommerce_instructions.html.hbs");
const STORE_DETAIL_TEMPLATE: &str = include_str!("../templates/store_detail.html.hbs");
const STATUS_TEMPLATE: &str = include_str!("../templates/status.html.hbs");
const CHECKOUT_TEMPLATE: &str = include_str!("../templates/checkout.html.hbs");
const CHECKOUT_NOT_FOUND_TEMPLATE: &str = include_str!("../templates/checkout_not_found.html.hbs");

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("failed to register template: {0}")]
    Register(#[from] handlebars::TemplateError),
    #[error("failed to render template: {0}")]
    Render(#[from] handlebars::RenderError),
}

/// The view model both the signup and login templates take: just an
/// optional, human-readable error message to display on re-render (a
/// duplicate email, or a wrong password/unknown email) - `None` on the
/// plain `GET` form.
#[derive(Debug, Default, Serialize)]
pub struct FormViewModel {
    pub error: Option<String>,
    /// Drives the nav bar's log-in-state links (`_nav.html.hbs`, inherited
    /// automatically by `{{> nav}}` since handlebars partials share their
    /// parent's context unless given an explicit one) - see
    /// `http::dashboard::render_signup`'s own doc comment for why this page
    /// always renders `false` regardless of any existing session.
    pub logged_in: bool,
}

/// The view model the login template takes (WBS 1.4.1 extends the plain
/// `FormViewModel` used elsewhere with `next`): `error` means the same thing
/// `FormViewModel::error` does. `next`, when present, is rendered as a
/// hidden form field so a successful login can redirect back to it (see
/// `dashboard::login_submit`) instead of the default inline confirmation -
/// this is the raw, caller-supplied query value, not yet validated as a safe
/// redirect target here (that validation happens in `login_submit`, right
/// before it's ever used as a redirect location, never here at render
/// time).
#[derive(Debug, Default, Serialize)]
pub struct LoginViewModel {
    pub error: Option<String>,
    pub next: Option<String>,
    /// Same purpose as [`FormViewModel::logged_in`] - always `false` here
    /// too, same reasoning.
    pub logged_in: bool,
}

/// The view model the wallet-connection template (WBS 1.3.2) takes: either
/// `error` is set (re-rendering the form after the engine rejected the
/// request) or `public_key` is set (a successful connection, showing the
/// confirmation view instead of the form) - never both, and plain `GET`
/// requests get neither.
#[derive(Debug, Default, Serialize)]
pub struct ConnectViewModel {
    pub error: Option<String>,
    pub public_key: Option<String>,
    /// The engine's real base URL (`EngineClient::base_url()`) - only ever
    /// populated (and only ever rendered) alongside `public_key` on a
    /// successful connection, so the integration-help partial shown here
    /// gets the store's real endpoint rather than a placeholder string. Left
    /// as the default empty string on the plain form render, where it's
    /// never used.
    pub endpoint: String,
    /// The submitted field values, echoed back into the re-rendered form on
    /// a validation error so a rejected submission doesn't throw away
    /// everything the merchant typed - including the two key fields, which
    /// are long, easy-to-mistype hex strings nobody wants to retype from
    /// scratch after one bad character. All empty strings (and the network
    /// flags below defaulting to mainnet) on a plain `GET` of the form.
    /// Never populated (and never rendered - see the template's own `{{#if
    /// public_key}}`) on a successful submission.
    pub site_url: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub allowed_origins: String,
    pub network_mainnet_selected: bool,
    pub network_stagenet_selected: bool,
    pub network_testnet_selected: bool,
    /// Always `true` - every caller of `render_connect` is already behind
    /// `AuthedUser` (`http/dashboard.rs`'s `connect_form`/`connect_submit`).
    pub logged_in: bool,
}

/// The view model the generic platform-connect confirm form (WBS 1.4.1,
/// `GET`/`POST /connect/{platform}`) takes - the same wallet-connection
/// fields the `/dashboard/connect` form (`ConnectViewModel`) has, plus the
/// three values that must survive the round trip as hidden fields
/// (`return_url`/`nonce`) or be shown to the merchant (`site_url`), and
/// `platform` so the form's own `action` can post back to the same
/// `/connect/{platform}` path it was reached at.
#[derive(Debug, Default, Serialize)]
pub struct PlatformConnectViewModel {
    pub platform: String,
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    pub error: Option<String>,
    /// Same re-fill purpose as [`ConnectViewModel`]'s own matching fields -
    /// see that struct's doc comment.
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub allowed_origins: String,
    pub network_mainnet_selected: bool,
    pub network_stagenet_selected: bool,
    pub network_testnet_selected: bool,
    /// Every store this user already has connected (any platform) - lets
    /// the confirm screen offer "use an existing store" instead of always
    /// forcing a brand-new tenant to be provisioned. Empty for a user with
    /// no stores yet, in which case the template shows only the
    /// create-a-new-store form (`{{#if existing_stores}}` is falsy for an
    /// empty vec, same convention `DashboardViewModel::has_stores` already
    /// relies on).
    pub existing_stores: Vec<ExistingStoreOption>,
    /// Always `true` - `GET`/`POST /connect/{platform}` only ever render
    /// this template once a session is already confirmed (see
    /// `http/connect.rs::start`'s own doc comment: no session redirects to
    /// `/dashboard/login` instead of rendering this at all).
    pub logged_in: bool,
}

/// One row of the "use an existing store" picker
/// (`connect_platform.html.hbs`) - just enough to identify and label a
/// choice, not the full [`crate::db::StoreConnectionRow`].
#[derive(Debug, Serialize)]
pub struct ExistingStoreOption {
    pub connection_id: String,
    pub display_name: String,
    pub platform: String,
}

/// The three `<option>` "selected" flags both connect forms' network
/// `<select>` need, derived from a submitted (or default) network value.
/// Handlebars-rust has no built-in string-equality helper - three plain
/// bools computed once here is simpler and more consistent with this
/// codebase's style than registering one. `network` not matching any known
/// value (shouldn't happen - the `<select>` only ever offers these three -
/// but a resubmitted form is still untrusted input) selects none of them,
/// same as an unrecognized value would render in a plain `<select>` anyway.
pub fn network_selected_flags(network: &str) -> (bool, bool, bool) {
    (network == "mainnet", network == "stagenet", network == "testnet")
}

/// One row of the orders list page (WBS 1.3.3) - just the fields the table
/// shows, not the full engine `OrderView`.
#[derive(Debug, Serialize)]
pub struct OrderRowViewModel {
    pub payment_id: String,
    pub status: String,
    pub fiat_amount: String,
    pub fiat_currency: String,
    pub created_at: i64,
}

/// The view model `GET /dashboard/connections/{id}/orders` takes.
#[derive(Debug, Serialize)]
pub struct OrdersViewModel {
    pub connection_id: String,
    pub orders: Vec<OrderRowViewModel>,
    /// Always `true` - every caller is behind `AuthedUser`.
    pub logged_in: bool,
}

/// A muted placeholder for a field with nothing to show - same
/// `<span class="muted">-</span>` convention the status page already uses
/// for "no value" (`height_display`, `_nav.html.hbs`'s own "Active" column),
/// applied here to every optional order/payment field so a merchant never
/// sees a bare, unexplained empty table cell.
const NO_VALUE: &str = "<span class=\"muted\">-</span>";

pub fn display_or_dash(value: Option<&str>) -> String {
    match value {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => NO_VALUE.to_string(),
    }
}

/// Compact, non-human-readable form (raw Unix seconds) - a deliberate,
/// user-requested reversion from an earlier human-readable-date attempt on
/// this page. Kept as its own function (rather than inlining `.to_string()`
/// at every call site) purely so the muted-dash-for-`None` behavior stays
/// centralized in one place - a plain number is still `Option`-aware here,
/// it's just no longer formatted as a calendar date.
pub fn display_timestamp_or_dash(value: Option<i64>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => NO_VALUE.to_string(),
    }
}

/// Same as [`display_timestamp_or_dash`] but for a timestamp that's always
/// present (`created_at`/`expires_at`/`updated_at`) - never a dash.
pub fn display_timestamp(value: i64) -> String {
    value.to_string()
}

/// One payment row inside the order detail page's `payments` table -
/// mirrors the engine's own `PaymentView`, but with every timestamp/
/// optional field already rendered to a display string (`Option<i64>` ->
/// human-readable UTC or a muted dash) rather than left for the template to
/// interpret - the display strings are trusted HTML (`NO_VALUE` carries a
/// real `<span>`), so they're rendered with handlebars' triple-stash
/// (`{{{ }}}`) in the template, same as `network_selected_flags`-style
/// values elsewhere are computed in Rust rather than branched on in the
/// template.
#[derive(Debug, Serialize)]
pub struct PaymentRowViewModel {
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub first_seen_at_display: String,
    pub block_height_display: String,
    pub voided_at_display: String,
}

/// The full order detail shown by `GET
/// /dashboard/connections/{id}/orders/{payment_id}` on a successful lookup -
/// every `OrderView` field plus the `payments` list from
/// `OrderDetailResponse`, with timestamps/optional fields already rendered
/// to display strings - see [`PaymentRowViewModel`]'s own doc comment for
/// why.
#[derive(Debug, Serialize)]
pub struct OrderDetailData {
    pub payment_id: String,
    /// Raw, *not* pre-rendered to a trusted-HTML display string like the
    /// timestamp fields below - unlike a missing timestamp (always our own
    /// internally-generated value), a merchant order id is caller-supplied
    /// free text (the engine's public order-creation API accepts it as-is),
    /// so it must stay ordinary escaped template output
    /// (`{{order.merchant_order_id}}`), never `{{{ }}}` - see
    /// `order_detail.html.hbs`'s own `{{#if}}` handling of `None`.
    pub merchant_order_id: Option<String>,
    pub address: String,
    pub fiat_currency: String,
    pub fiat_amount: String,
    /// e.g. `"0.006700000000 XMR per 1 USD"`, or a muted dash for an order
    /// with no local fiat metadata (predates the feature, or was created
    /// directly against the engine rather than through control-plane).
    pub fiat_rate_display: String,
    /// Which provider (`"fixed"`/`"coingecko"`) quoted `fiat_rate_display` -
    /// `"unknown"` for a row that predates recording this at all (migration
    /// 0006), or a muted dash alongside `fiat_rate_display` for no metadata.
    pub fiat_rate_provider: String,
    pub xmr_amount_piconero: u64,
    pub amount_received_piconero: u64,
    pub status: String,
    pub confirmations: u64,
    pub double_spend_detected_at_display: String,
    /// Same caller-supplied-text caveat as `merchant_order_id` above (set
    /// via the engine's `set_refund_address` endpoint) - raw, escaped by
    /// the template, never triple-stashed.
    pub refund_address: Option<String>,
    pub created_at_display: String,
    pub expires_at_display: String,
    pub updated_at_display: String,
    pub payments: Vec<PaymentRowViewModel>,
}

/// The view model `GET /dashboard/connections/{id}/orders/{payment_id}`
/// takes - `order` is `None` for an unknown `payment_id` (or one belonging
/// to a different tenant), rendering a "not found" state instead of the
/// detail table.
#[derive(Debug, Serialize)]
pub struct OrderDetailViewModel {
    pub connection_id: String,
    pub order: Option<OrderDetailData>,
    /// Always `true` - every caller is behind `AuthedUser`.
    pub logged_in: bool,
}

/// One row of the webhooks list page (WBS 1.3.3) - mirrors the engine's own
/// `WebhookView` field-for-field.
#[derive(Debug, Serialize)]
pub struct WebhookRowViewModel {
    pub webhook_id: String,
    pub url: String,
    pub enabled: bool,
    pub created_at: i64,
}

/// The view model `GET /dashboard/connections/{id}/webhooks` (and the
/// create/delete handlers, which all re-render this same page rather than
/// redirect) takes.
#[derive(Debug, Default, Serialize)]
pub struct WebhooksViewModel {
    pub connection_id: String,
    pub webhooks: Vec<WebhookRowViewModel>,
    pub error: Option<String>,
    /// Set only immediately after a successful `POST .../webhooks` - the
    /// engine's own `CreateWebhookResponse` hands back a real signing secret
    /// exactly once, at creation time; its `WebhookView` (what every later
    /// `GET .../webhooks` list call returns, confirmed by reading that
    /// struct directly - `src/http/admin.rs` at the repo root) has no
    /// `signing_secret` field at all, so there is no way to ever fetch it
    /// again after this moment - same one-time-reveal shape as a tenant's
    /// own `sk_...` at connect time. Never populated on a plain `GET`, and
    /// gone again the moment the page is reloaded.
    pub created_webhook_signing_secret: Option<String>,
    /// Always `true` - every caller is behind `AuthedUser`.
    pub logged_in: bool,
}

/// One connected store as shown on the dashboard home page - a much smaller
/// projection of [`crate::db::StoreConnectionRow`] than the full order/
/// webhook views need, plus two fields no engine or database call directly
/// hands back: `display_name` (there is no user-facing name column on
/// `store_connections` - see `Db::list_store_connections_for_user`'s own
/// doc comment for why this derives one from `site_url` instead of adding
/// one) and `health` (`"ok"` if `EngineClient::get_tenant` on this
/// connection's decrypted `sk_...` succeeded, `"error"` otherwise - the only
/// signal actually available without this service also probing the
/// merchant's own `site_url`, which nothing here does).
#[derive(Debug, Serialize)]
pub struct DashboardStoreRow {
    pub connection_id: String,
    pub display_name: String,
    pub platform: String,
    pub site_url: String,
    pub public_key: String,
    pub health: String,
    pub health_label: String,
}

/// One order row on the dashboard home page - like [`OrderRowViewModel`] but
/// also carrying which store it belongs to, since the dashboard shows
/// orders across every connected store, not just one.
#[derive(Debug, Serialize)]
pub struct DashboardOrderRow {
    pub connection_id: String,
    pub display_name: String,
    pub payment_id: String,
    pub status: String,
    pub fiat_amount: String,
    pub fiat_currency: String,
    pub created_at: i64,
}

/// The view model `GET /dashboard` takes (WBS follow-up to 1.3.3 - the
/// actual dashboard-home page that task's own `dashboard.rs` doc comment
/// notes doesn't exist yet). `has_stores` is redundant with
/// `!stores.is_empty()` but kept as an explicit field rather than computed
/// in the template - handlebars' `{{#if}}` on an array already means
/// "non-empty", so this is really just documentation of that fact at the
/// call site rather than a second source of truth to drift.
#[derive(Debug, Serialize)]
pub struct DashboardViewModel {
    pub has_stores: bool,
    pub stores: Vec<DashboardStoreRow>,
    pub recent_orders: Vec<DashboardOrderRow>,
    /// Sum of every order's `amount_received_piconero` across every
    /// connected store - "total received XMR that has been detected",
    /// deliberately the *detected* amount (what the scanner actually
    /// matched on-chain) rather than only orders in a `paid`/`overpaid`
    /// status, since a partially-paid or still-confirming order has still
    /// genuinely had funds detected for it.
    pub total_received_xmr: String,
    /// Always `true` - every caller is behind `AuthedUser`.
    pub logged_in: bool,
}

/// The view model the integration-help partial (`_integration_help.html.hbs`)
/// takes - shared verbatim by the post-connect success page
/// (`connect.html.hbs`) and the store detail page (`store_detail.html.hbs`),
/// so the two can never show different instructions for the same store.
#[derive(Debug, Serialize)]
pub struct IntegrationHelpViewModel {
    pub public_key: String,
    pub endpoint: String,
}

/// The view model `GET /dashboard/connections/{id}` (the store detail page)
/// takes. `store` is `None` for an unknown/not-owned connection id (same
/// enumeration-defense convention as [`OrderDetailViewModel`] and
/// `http/orders.rs`'s own `load_owned_connection` doc comment - a missing
/// row and someone else's row render identically).
#[derive(Debug, Serialize)]
pub struct StoreDetailViewModel {
    pub store: Option<StoreDetailData>,
    /// Always `true` - every caller is behind `AuthedUser`.
    pub logged_in: bool,
}

#[derive(Debug, Serialize)]
pub struct StoreDetailData {
    pub connection_id: String,
    pub display_name: String,
    pub platform: String,
    pub site_url: String,
    pub public_key: String,
    pub endpoint: String,
    pub health: String,
    pub health_label: String,
    pub created_at: i64,
    pub recent_orders: Vec<OrderRowViewModel>,
    /// Drives which half of `_integration_help.html.hbs` renders -
    /// handlebars-rust has no built-in string-equality helper (same reason
    /// `network_selected_flags` exists), so this is `platform ==
    /// "woocommerce"` computed once here rather than in the template.
    pub is_woocommerce: bool,
    /// Set only when the "create a test order" form on this page (see
    /// `http/orders.rs::create_order`) was just rejected - the engine's own
    /// validation error (an unsupported currency, an unparseable amount),
    /// surfaced verbatim, same convention `WebhooksViewModel::error` already
    /// applies to webhook creation. `None` on a plain page load.
    pub order_creation_error: Option<String>,
    /// The tenant's current confirmation threshold, as last fetched from
    /// the engine (`TenantView::confirmations_required`) - `0` when the
    /// engine is currently unreachable (`health == "error"`), same
    /// "degrade honestly, show *something* real-ish rather than fail the
    /// whole page" approach `recent_orders` already takes for that case.
    pub confirmations_required: u64,
    /// Set only when the "update settings" form on this page (see
    /// `http/orders.rs::update_confirmations_required`) was just rejected -
    /// the engine's own validation error, surfaced verbatim. Shared by
    /// every settings sub-form on this page (just confirmations for now),
    /// since only one can ever be submitted at a time. `None` on a plain
    /// page load.
    pub settings_error: Option<String>,
}

/// One Monero node's row on the status page - mirrors
/// `engine_client::NodeStatus` but with presentation already done
/// (`height_display`/`is_reachable`) since the control-plane is the one
/// place that logic belongs now (see `http/status_page.rs`'s own doc
/// comment on why the engine's own `/status` deliberately stays JSON-only).
/// Handlebars-rust's `{{#if}}` treats the number `0` as falsy exactly like
/// JS, so a genuine height of 0 must never be branched on directly in the
/// template - `height_display` is always a pre-formatted string, same fix
/// as `network_selected_flags`.
#[derive(Debug, Serialize)]
pub struct StatusNodeView {
    pub label: String,
    pub is_active: bool,
    pub is_reachable: bool,
    pub height_display: String,
    pub error: Option<String>,
}

/// One network's scanner-loop row on the status page - mirrors
/// `engine_client::ScannerStatusView` plus a single overall `status_label`
/// ("healthy" / "stale" / "tick failing" / "has not been scanned yet") so
/// the template renders one tag instead of re-deriving it from three bools.
#[derive(Debug, Serialize)]
pub struct StatusScannerView {
    pub ever_ticked: bool,
    pub status_label: String,
    pub status_tag_class: String,
    pub last_tick_display: String,
    pub tick_count: u64,
    pub tenants_scanned: usize,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StatusNetworkView {
    pub network: String,
    pub nodes: Vec<StatusNodeView>,
    pub scanner: StatusScannerView,
}

/// The view model `GET /status` takes. `engine_error`, when set, means the
/// engine itself couldn't be reached at all (a genuinely different, more
/// serious case than any one node or scanner being unhealthy) - the
/// template shows a plain error banner instead of the networks table in
/// that case, same "degrade honestly, don't fabricate data" approach the
/// engine's own daemon fallback uses.
#[derive(Debug, Default, Serialize)]
pub struct StatusPageViewModel {
    pub engine_error: Option<String>,
    pub networks: Vec<StatusNetworkView>,
    pub poll_interval_secs: u64,
    pub generated_at_display: String,
    /// Unlike every other view model's `logged_in` (always a fixed
    /// literal, since those pages are always/never behind `AuthedUser`),
    /// this one is a genuine, per-request lookup - `/status` is
    /// unauthenticated, so whether the nav shows "log out" here reflects
    /// whatever session (if any) the visitor actually presented. See
    /// `status_page`'s own handler.
    pub logged_in: bool,
}

/// The view model for a page whose only dynamic content is the nav bar's
/// own log-in-state links - `landing`/`new_store_picker`/
/// `woocommerce_instructions` used to render with `&()` (no context at
/// all); each needs exactly this one field now.
#[derive(Debug, Serialize)]
pub struct NavOnlyViewModel {
    pub logged_in: bool,
}

/// One payment row on the checkout page's payments table
/// (`docs/fx_refactor.md` Phase 2.2) - mirrors the engine's own (soon-
/// removed) `PaymentViewModel` field-for-field.
#[derive(Debug, Serialize)]
pub struct CheckoutPaymentViewModel {
    pub txid_short: String,
    pub amount_xmr: String,
    pub confirmations: u64,
    pub is_zero_conf: bool,
}

/// The view model `GET /pay/{pk}/orders/{payment_id}` takes - mirrors the
/// engine's own (soon-removed) `CheckoutViewModel` field-for-field, with
/// one addition (`pk`, needed by the page's own polling script to build
/// its status-check URL) and no `logged_in` at all (this page carries no
/// site nav - see `http::checkout`'s own module doc comment for why).
#[derive(Debug, Serialize)]
pub struct CheckoutViewModel {
    pub payment_id: String,
    pub status: String,
    pub status_label: String,
    pub status_class: String,
    pub address: String,
    pub qr_code_svg: String,
    pub xmr_amount: String,
    pub amount_received_xmr: String,
    pub fiat_amount: String,
    pub fiat_currency: String,
    pub confirmations: u64,
    pub confirmations_required: u64,
    pub is_terminal: bool,
    pub double_spend_detected_at: Option<i64>,
    pub expires_at: i64,
    pub merchant_order_id: Option<String>,
    pub pk: String,
    pub payments: Vec<CheckoutPaymentViewModel>,
}

pub struct TemplateEngine {
    handlebars: Handlebars<'static>,
}

impl TemplateEngine {
    pub fn new() -> Result<Self, TemplateError> {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(true);
        // Partials shared by every page below - see their own files'
        // comments. Registered under names with no leading underscore
        // (handlebars-rust has no notion of "partial vs. template", a
        // registered template is callable as `{{> name}}` by whatever name
        // it's registered under) so `{{> styles}}`/`{{> nav}}`/
        // `{{> integration_help}}` read cleanly from every page.
        handlebars.register_template_string("styles", STYLES_PARTIAL)?;
        handlebars.register_template_string("nav", NAV_PARTIAL)?;
        handlebars.register_template_string("integration_help", INTEGRATION_HELP_PARTIAL)?;

        handlebars.register_template_string("landing", LANDING_TEMPLATE)?;
        handlebars.register_template_string("signup", SIGNUP_TEMPLATE)?;
        handlebars.register_template_string("login", LOGIN_TEMPLATE)?;
        handlebars.register_template_string("connect", CONNECT_TEMPLATE)?;
        handlebars.register_template_string("orders", ORDERS_TEMPLATE)?;
        handlebars.register_template_string("order_detail", ORDER_DETAIL_TEMPLATE)?;
        handlebars.register_template_string("webhooks", WEBHOOKS_TEMPLATE)?;
        handlebars.register_template_string("connect_platform", CONNECT_PLATFORM_TEMPLATE)?;
        handlebars.register_template_string("dashboard_home", DASHBOARD_HOME_TEMPLATE)?;
        handlebars.register_template_string("new_store_picker", NEW_STORE_PICKER_TEMPLATE)?;
        handlebars.register_template_string("woocommerce_instructions", WOOCOMMERCE_INSTRUCTIONS_TEMPLATE)?;
        handlebars.register_template_string("store_detail", STORE_DETAIL_TEMPLATE)?;
        handlebars.register_template_string("status", STATUS_TEMPLATE)?;
        handlebars.register_template_string("checkout", CHECKOUT_TEMPLATE)?;
        handlebars.register_template_string("checkout_not_found", CHECKOUT_NOT_FOUND_TEMPLATE)?;
        Ok(TemplateEngine { handlebars })
    }

    pub fn render_signup(&self, data: &FormViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("signup", data)?)
    }

    pub fn render_login(&self, data: &LoginViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("login", data)?)
    }

    pub fn render_platform_connect(&self, data: &PlatformConnectViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("connect_platform", data)?)
    }

    pub fn render_connect(&self, data: &ConnectViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("connect", data)?)
    }

    pub fn render_orders(&self, data: &OrdersViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("orders", data)?)
    }

    pub fn render_order_detail(&self, data: &OrderDetailViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("order_detail", data)?)
    }

    pub fn render_webhooks(&self, data: &WebhooksViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("webhooks", data)?)
    }

    pub fn render_landing(&self, logged_in: bool) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("landing", &NavOnlyViewModel { logged_in })?)
    }

    pub fn render_dashboard_home(&self, data: &DashboardViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("dashboard_home", data)?)
    }

    pub fn render_new_store_picker(&self, logged_in: bool) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("new_store_picker", &NavOnlyViewModel { logged_in })?)
    }

    pub fn render_woocommerce_instructions(&self, logged_in: bool) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("woocommerce_instructions", &NavOnlyViewModel { logged_in })?)
    }

    pub fn render_store_detail(&self, data: &StoreDetailViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("store_detail", data)?)
    }

    pub fn render_status(&self, data: &StatusPageViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("status", data)?)
    }

    pub fn render_checkout(&self, data: &CheckoutViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("checkout", data)?)
    }

    pub fn render_checkout_not_found(&self) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("checkout_not_found", &())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_timestamp_or_dash_shows_a_raw_compact_number_not_a_human_readable_date() {
        assert_eq!(display_timestamp_or_dash(Some(1_700_000_000)), "1700000000");
        assert_eq!(display_timestamp_or_dash(None), NO_VALUE);
        assert_eq!(display_timestamp(1_700_000_000), "1700000000");
    }

    #[test]
    fn display_or_dash_shows_the_muted_placeholder_for_none_or_empty() {
        assert_eq!(display_or_dash(Some("real value")), "real value");
        assert_eq!(display_or_dash(None), NO_VALUE);
        assert_eq!(display_or_dash(Some("")), NO_VALUE, "an empty string is not a real value either");
    }

    #[test]
    fn signup_template_renders_with_no_error() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_signup(&FormViewModel::default()).unwrap();
        assert!(html.contains("<form"));
        assert!(html.to_lowercase().contains("sign up"));
    }

    #[test]
    fn signup_template_shows_the_error_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_signup(&FormViewModel { error: Some("that email is already registered".to_string()), logged_in: false })
            .unwrap();
        assert!(html.contains("that email is already registered"));
    }

    #[test]
    fn login_template_renders_with_no_error() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_login(&LoginViewModel::default()).unwrap();
        assert!(html.contains("<form"));
        assert!(html.to_lowercase().contains("log in"));
    }

    #[test]
    fn login_template_shows_the_error_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_login(&LoginViewModel { error: Some("invalid email or password".to_string()), next: None, logged_in: false })
            .unwrap();
        assert!(html.contains("invalid email or password"));
    }

    #[test]
    fn login_template_includes_a_hidden_next_field_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_login(&LoginViewModel { error: None, next: Some("/connect/woocommerce?nonce=abc".to_string()), logged_in: false })
            .unwrap();
        assert!(html.contains(r#"type="hidden" name="next""#), "expected a hidden next field, got: {html}");
        // Handlebars auto-escapes HTML-significant characters (including
        // `=`, as `&#x3D;`) in attribute values by default - correct, safe
        // behavior (never `{{{next}}}`/triple-stash - see
        // `connect.html.hbs`'s own precedent), so check for the value's
        // *content* surviving, not a byte-for-byte unescaped match.
        assert!(html.contains("/connect/woocommerce?nonce"), "expected the next value's content present, got: {html}");
        assert!(html.contains("abc"), "expected the next value's content present, got: {html}");
    }

    #[test]
    fn login_template_escapes_special_characters_in_next_rather_than_injecting_them_raw() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_login(&LoginViewModel {
                error: None,
                next: Some("/connect/woocommerce?a=1&b=2".to_string()),
                logged_in: false,
            })
            .unwrap();
        assert!(html.contains("&amp;"), "expected the & in next to be HTML-escaped, got: {html}");
        assert!(!html.contains("a=1&b=2"), "a raw, unescaped & would be a template-injection smell, got: {html}");
    }

    #[test]
    fn login_template_has_no_hidden_next_field_when_absent() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_login(&LoginViewModel::default()).unwrap();
        assert!(!html.contains(r#"name="next""#), "expected no hidden next field, got: {html}");
    }

    #[test]
    fn connect_platform_template_renders_the_confirm_form() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_platform_connect(&PlatformConnectViewModel {
                platform: "woocommerce".to_string(),
                site_url: "https://shop.example.com".to_string(),
                return_url: "https://shop.example.com/settings".to_string(),
                nonce: "nonce-abc".to_string(),
                error: None,
                ..Default::default()
            })
            .unwrap();
        assert!(html.contains("https://shop.example.com"));
        assert!(html.contains(r#"action="/connect/woocommerce""#));
        assert!(html.contains(r#"name="return_url" value="https://shop.example.com/settings""#));
        assert!(html.contains(r#"name="nonce" value="nonce-abc""#));
        assert!(html.contains("view_key_hex"));
    }

    #[test]
    fn connect_platform_template_shows_the_error_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_platform_connect(&PlatformConnectViewModel {
                platform: "woocommerce".to_string(),
                site_url: "https://shop.example.com".to_string(),
                return_url: "https://shop.example.com/settings".to_string(),
                nonce: "nonce-abc".to_string(),
                error: Some("bad view key hex".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert!(html.contains("bad view key hex"));
        assert!(html.contains("<form"), "the form must still be present on error");
    }

    #[test]
    fn connect_platform_template_re_fills_every_submitted_field_when_re_rendered_after_a_rejected_submission() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_platform_connect(&PlatformConnectViewModel {
                platform: "woocommerce".to_string(),
                site_url: "https://shop.example.com".to_string(),
                return_url: "https://shop.example.com/settings".to_string(),
                nonce: "nonce-abc".to_string(),
                error: Some("bad view key hex".to_string()),
                view_key_hex: "0707070707070707070707070707070707070707070707070707070707070707".to_string(),
                spend_pubkey_hex: "deadbeef".to_string(),
                allowed_origins: "https://shop.example.com".to_string(),
                network_stagenet_selected: true,
                ..Default::default()
            })
            .unwrap();
        assert!(
            html.contains(r#"value="0707070707070707070707070707070707070707070707070707070707070707""#),
            "expected view_key_hex echoed back, got: {html}"
        );
        assert!(html.contains(r#"value="deadbeef""#), "expected spend_pubkey_hex echoed back, got: {html}");
        assert!(
            html.contains(r#"name="allowed_origins" value="https://shop.example.com""#),
            "expected allowed_origins echoed back, got: {html}"
        );
        assert!(html.contains(r#"value="stagenet" selected"#), "expected the stagenet option marked selected, got: {html}");
    }

    #[test]
    fn connect_template_renders_the_form_with_no_error_or_public_key() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_connect(&ConnectViewModel::default()).unwrap();
        assert!(html.contains("<form"));
        assert!(html.contains("view_key_hex"));
    }

    #[test]
    fn connect_template_shows_the_error_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_connect(&ConnectViewModel { error: Some("bad view key hex".to_string()), ..Default::default() })
            .unwrap();
        assert!(html.contains("bad view key hex"));
        assert!(html.contains("<form"), "the form must still be present on error");
    }

    /// The actual bug being fixed: a rejected submission used to lose every
    /// field the merchant typed, forcing them to retype two long hex keys
    /// from scratch. `error` being set must not mean these are gone.
    #[test]
    fn connect_template_re_fills_every_submitted_field_when_re_rendered_after_a_rejected_submission() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_connect(&ConnectViewModel {
                error: Some("bad view key hex".to_string()),
                site_url: "https://shop.example.com".to_string(),
                view_key_hex: "0707070707070707070707070707070707070707070707070707070707070707".to_string(),
                spend_pubkey_hex: "deadbeef".to_string(),
                allowed_origins: "https://shop.example.com, https://admin.example.com".to_string(),
                network_stagenet_selected: true,
                ..Default::default()
            })
            .unwrap();
        assert!(html.contains(r#"value="https://shop.example.com""#), "expected site_url echoed back, got: {html}");
        assert!(
            html.contains(r#"value="0707070707070707070707070707070707070707070707070707070707070707""#),
            "expected view_key_hex echoed back, got: {html}"
        );
        assert!(html.contains(r#"value="deadbeef""#), "expected spend_pubkey_hex echoed back, got: {html}");
        assert!(
            html.contains(r#"value="https://shop.example.com, https://admin.example.com""#),
            "expected allowed_origins echoed back, got: {html}"
        );
        assert!(
            html.contains(r#"value="stagenet" selected"#),
            "expected the stagenet option marked selected, got: {html}"
        );
        assert!(
            !html.contains(r#"value="mainnet" selected"#),
            "mainnet must not stay marked selected once stagenet was actually submitted, got: {html}"
        );
    }

    #[test]
    fn connect_template_shows_the_public_key_instead_of_the_form_on_success() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_connect(&ConnectViewModel {
                error: None,
                public_key: Some("pk_deadbeef".to_string()),
                endpoint: "http://127.0.0.1:8080".to_string(),
                ..Default::default()
            })
            .unwrap();
        assert!(html.contains("pk_deadbeef"));
        assert!(!html.contains("<form"), "the confirmation view should not still show the form");
    }

    #[test]
    fn landing_page_renders_with_a_signup_cta() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_landing(false).unwrap();
        assert!(html.contains(r#"href="/dashboard/signup""#));
        assert!(html.to_lowercase().contains("monero"));
    }

    #[test]
    fn dashboard_home_shows_the_add_store_cta_when_the_user_has_no_stores() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_dashboard_home(&DashboardViewModel {
                has_stores: false,
                stores: vec![],
                recent_orders: vec![],
                total_received_xmr: "0".to_string(),
                logged_in: true,
            })
            .unwrap();
        assert!(html.contains(r#"href="/dashboard/connections/new""#));
        assert!(!html.contains("<table"), "an empty dashboard shouldn't render a store table at all");
    }

    #[test]
    fn dashboard_home_lists_stores_and_recent_orders_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_dashboard_home(&DashboardViewModel {
                has_stores: true,
                stores: vec![DashboardStoreRow {
                    connection_id: "conn_1".to_string(),
                    display_name: "shop.example.com".to_string(),
                    platform: "woocommerce".to_string(),
                    site_url: "https://shop.example.com".to_string(),
                    public_key: "pk_abc123".to_string(),
                    health: "ok".to_string(),
                    health_label: "healthy".to_string(),
                }],
                recent_orders: vec![DashboardOrderRow {
                    connection_id: "conn_1".to_string(),
                    display_name: "shop.example.com".to_string(),
                    payment_id: "pay_xyz".to_string(),
                    status: "paid".to_string(),
                    fiat_amount: "25.00".to_string(),
                    fiat_currency: "USD".to_string(),
                    created_at: 1000,
                }],
                total_received_xmr: "1.234567890123".to_string(),
                logged_in: true,
            })
            .unwrap();
        assert!(html.contains("pk_abc123"));
        assert!(html.contains("1.234567890123"));
        assert!(html.contains("tag-ok"));
        assert!(html.contains(r#"href="/dashboard/connections/conn_1""#));
        assert!(html.contains("pay_xyz"));
        assert!(html.contains(r#"href="/dashboard/connections/conn_1/orders/pay_xyz""#));
    }

    #[test]
    fn new_store_picker_links_to_both_flows() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_new_store_picker(true).unwrap();
        assert!(html.contains(r#"href="/dashboard/connections/new/woocommerce""#));
        assert!(html.contains(r#"href="/dashboard/connect""#));
    }

    #[test]
    fn woocommerce_instructions_page_renders() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_woocommerce_instructions(true).unwrap();
        assert!(html.to_lowercase().contains("woocommerce"));
        assert!(html.contains(r#"href="/dashboard/connect""#));
    }

    #[test]
    fn store_detail_renders_not_found_state_when_store_is_none() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_store_detail(&StoreDetailViewModel { store: None, logged_in: true }).unwrap();
        assert!(html.to_lowercase().contains("not found"));
    }

    #[test]
    fn store_detail_renders_integration_help_with_the_right_public_key_via_the_shared_partial() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_store_detail(&StoreDetailViewModel {
                store: Some(StoreDetailData {
                    connection_id: "conn_1".to_string(),
                    display_name: "shop.example.com".to_string(),
                    platform: "woocommerce".to_string(),
                    site_url: "https://shop.example.com".to_string(),
                    public_key: "pk_abc123".to_string(),
                    endpoint: "http://127.0.0.1:8080".to_string(),
                    health: "error".to_string(),
                    health_label: "unreachable".to_string(),
                    created_at: 1000,
                    recent_orders: vec![],
                    is_woocommerce: true,
                    order_creation_error: None,
                    confirmations_required: 10,
                    settings_error: None,
                }),
                logged_in: true,
            })
            .unwrap();
        // Proves the integration_help partial actually received this
        // store's own public_key/endpoint via its explicit hash params
        // (`{{> integration_help public_key=store.public_key ...}}`), not
        // some stale or empty context - the exact same partial the
        // post-connect success page uses (see
        // `connect_template_shows_the_public_key_instead_of_the_form_on_success`),
        // so the two can never drift on what "integrate this store" means.
        assert!(html.contains("pk_abc123"));
        assert!(html.contains("http://127.0.0.1:8080"));
        assert!(html.contains("tag-error"));
        assert!(html.contains("Integrate this store"));
        // is_woocommerce: true must render the "already connected" copy,
        // not the "install the plugin" onboarding steps - real bug: this
        // used to always show WooCommerce onboarding instructions even for
        // stores connected through the advanced/custom form.
        assert!(html.contains("already connected via the WooCommerce plugin"));
        assert!(!html.contains("Install the"), "should not show plugin-install instructions for an already-connected store");
    }

    /// The other half of the same real bug: a store connected via the
    /// advanced (custom) form must show generic direct-API instructions,
    /// never the WooCommerce-specific onboarding steps - it was never
    /// connected through the plugin at all.
    #[test]
    fn store_detail_shows_generic_integration_help_for_a_non_woocommerce_store() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_store_detail(&StoreDetailViewModel {
                store: Some(StoreDetailData {
                    connection_id: "conn_1".to_string(),
                    display_name: "shop.example.com".to_string(),
                    platform: "custom".to_string(),
                    site_url: "https://shop.example.com".to_string(),
                    public_key: "pk_abc123".to_string(),
                    endpoint: "http://127.0.0.1:8080".to_string(),
                    health: "ok".to_string(),
                    health_label: "healthy".to_string(),
                    created_at: 1000,
                    recent_orders: vec![],
                    is_woocommerce: false,
                    order_creation_error: None,
                    confirmations_required: 10,
                    settings_error: None,
                }),
                logged_in: true,
            })
            .unwrap();
        assert!(html.contains("Install the"), "expected the WooCommerce onboarding steps to still be offered, got: {html}");
        assert!(!html.contains("already connected via the WooCommerce plugin"));
    }

    #[test]
    fn store_detail_shows_the_create_order_error_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_store_detail(&StoreDetailViewModel {
                store: Some(StoreDetailData {
                    connection_id: "conn_1".to_string(),
                    display_name: "shop.example.com".to_string(),
                    platform: "custom".to_string(),
                    site_url: "https://shop.example.com".to_string(),
                    public_key: "pk_abc123".to_string(),
                    endpoint: "http://127.0.0.1:8080".to_string(),
                    health: "ok".to_string(),
                    health_label: "healthy".to_string(),
                    created_at: 1000,
                    recent_orders: vec![],
                    is_woocommerce: false,
                    order_creation_error: Some("unsupported currency: XYZ".to_string()),
                    confirmations_required: 10,
                    settings_error: None,
                }),
                logged_in: true,
            })
            .unwrap();
        assert!(html.contains("unsupported currency: XYZ"), "expected the real error surfaced, got: {html}");
        assert!(html.contains("<form"), "the create-order form must still be present on error");
    }
}
