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

const SIGNUP_TEMPLATE: &str = include_str!("../templates/signup.html.hbs");
const LOGIN_TEMPLATE: &str = include_str!("../templates/login.html.hbs");
const CONNECT_TEMPLATE: &str = include_str!("../templates/connect.html.hbs");
const ORDERS_TEMPLATE: &str = include_str!("../templates/orders.html.hbs");
const ORDER_DETAIL_TEMPLATE: &str = include_str!("../templates/order_detail.html.hbs");
const WEBHOOKS_TEMPLATE: &str = include_str!("../templates/webhooks.html.hbs");
const CONNECT_PLATFORM_TEMPLATE: &str = include_str!("../templates/connect_platform.html.hbs");

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
}

/// One payment row inside the order detail page's `payments` table -
/// mirrors the engine's own `PaymentView` field-for-field.
#[derive(Debug, Serialize)]
pub struct PaymentRowViewModel {
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub first_seen_at: i64,
    pub block_height: Option<i64>,
    pub voided_at: Option<i64>,
}

/// The full order detail shown by `GET
/// /dashboard/connections/{id}/orders/{payment_id}` on a successful lookup -
/// every `OrderView` field plus the `payments` list from
/// `OrderDetailResponse`.
#[derive(Debug, Serialize)]
pub struct OrderDetailData {
    pub payment_id: String,
    pub merchant_order_id: Option<String>,
    pub address: String,
    pub fiat_currency: String,
    pub fiat_amount: String,
    pub xmr_amount_piconero: u64,
    pub amount_received_piconero: u64,
    pub status: String,
    pub confirmations: u64,
    pub double_spend_detected_at: Option<i64>,
    pub refund_address: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
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

/// The view model `GET /dashboard/connections/{id}/webhooks` takes.
#[derive(Debug, Serialize)]
pub struct WebhooksViewModel {
    pub connection_id: String,
    pub webhooks: Vec<WebhookRowViewModel>,
}

pub struct TemplateEngine {
    handlebars: Handlebars<'static>,
}

impl TemplateEngine {
    pub fn new() -> Result<Self, TemplateError> {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(true);
        handlebars.register_template_string("signup", SIGNUP_TEMPLATE)?;
        handlebars.register_template_string("login", LOGIN_TEMPLATE)?;
        handlebars.register_template_string("connect", CONNECT_TEMPLATE)?;
        handlebars.register_template_string("orders", ORDERS_TEMPLATE)?;
        handlebars.register_template_string("order_detail", ORDER_DETAIL_TEMPLATE)?;
        handlebars.register_template_string("webhooks", WEBHOOKS_TEMPLATE)?;
        handlebars.register_template_string("connect_platform", CONNECT_PLATFORM_TEMPLATE)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .render_signup(&FormViewModel { error: Some("that email is already registered".to_string()) })
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
            .render_login(&LoginViewModel { error: Some("invalid email or password".to_string()), next: None })
            .unwrap();
        assert!(html.contains("invalid email or password"));
    }

    #[test]
    fn login_template_includes_a_hidden_next_field_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_login(&LoginViewModel { error: None, next: Some("/connect/woocommerce?nonce=abc".to_string()) })
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
            })
            .unwrap();
        assert!(html.contains("bad view key hex"));
        assert!(html.contains("<form"), "the form must still be present on error");
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
            .render_connect(&ConnectViewModel { error: Some("bad view key hex".to_string()), public_key: None })
            .unwrap();
        assert!(html.contains("bad view key hex"));
        assert!(html.contains("<form"), "the form must still be present on error");
    }

    #[test]
    fn connect_template_shows_the_public_key_instead_of_the_form_on_success() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine
            .render_connect(&ConnectViewModel { error: None, public_key: Some("pk_deadbeef".to_string()) })
            .unwrap();
        assert!(html.contains("pk_deadbeef"));
        assert!(!html.contains("<form"), "the confirmation view should not still show the form");
    }
}
