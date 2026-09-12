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
        Ok(TemplateEngine { handlebars })
    }

    pub fn render_signup(&self, data: &FormViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("signup", data)?)
    }

    pub fn render_login(&self, data: &FormViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("login", data)?)
    }

    pub fn render_connect(&self, data: &ConnectViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("connect", data)?)
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
        let html = engine.render_login(&FormViewModel::default()).unwrap();
        assert!(html.contains("<form"));
        assert!(html.to_lowercase().contains("log in"));
    }

    #[test]
    fn login_template_shows_the_error_when_present() {
        let engine = TemplateEngine::new().unwrap();
        let html =
            engine.render_login(&FormViewModel { error: Some("invalid email or password".to_string()) }).unwrap();
        assert!(html.contains("invalid email or password"));
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
