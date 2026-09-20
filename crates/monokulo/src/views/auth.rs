//! `GET`/`POST /dashboard/login` and `/dashboard/signup` -
//! `http::dashboard::render_login`/`render_signup`.
//!
//! Both pages always render with `chrome.logged_in == false` regardless of
//! any real session (see each struct's own doc comment below) - the nav's
//! theme toggle only ever shows once logged in, so `chrome.current_path` is
//! never actually read here; callers pass an empty string rather than
//! bothering to extract the real request URI for a value that can't affect
//! the output.

use maud::{html, Markup};

use super::{layout, PageChrome};

/// `error`, when present, means the same thing on both pages: the previous
/// submission was rejected (a duplicate email, an invalid invite, wrong
/// credentials) and is shown, re-rendering the same form. `logged_in`/
/// `is_admin` used to live on this struct (and `LoginViewModel` below) - see
/// the old `templates::FormViewModel::logged_in`'s doc comment for why they
/// were always a fixed `false` literal - but that was only ever needed to
/// feed the nav partial; now that the nav comes from `PageChrome`, supplied
/// separately by the caller, there's nothing left for this struct to carry
/// beyond the page's own real data.
pub struct SignupViewModel {
    pub error: Option<String>,
    /// `true` when this instance's `signup.mode` is `"invite_only"` and no
    /// valid-looking invite token is in play - the page shows a "you need
    /// an invite" message and a link to `/request-invite` *instead of* the
    /// email/password form entirely. Always `false` in `"public"` mode.
    pub invite_required: bool,
    /// Carried through as a hidden form field (`GET
    /// /dashboard/signup?invite=...`'s query param, echoed back on a
    /// rejected `POST` the same way every other field on this form already
    /// is) - the raw, not-yet-validated token; real validation happens at
    /// submit time (`Db::redeem_invite_and_create_user`), never here.
    pub invite_token: String,
}

pub struct LoginViewModel {
    pub error: Option<String>,
    /// When present, rendered as a hidden form field so a successful login
    /// can redirect back to it (see `dashboard::login_submit`) instead of
    /// the default inline confirmation - the raw, caller-supplied query
    /// value, not yet validated as a safe redirect target here (that
    /// happens in `login_submit`, right before it's ever used as a
    /// redirect location, never here at render time).
    pub next: Option<String>,
}

pub fn signup_page(chrome: &PageChrome, data: &SignupViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Sign up" }
            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }
            @if data.invite_required {
                p {
                    "This instance is invite-only. "
                    a href="/request-invite" { "Request an invite" }
                    " to join."
                }
            } @else {
                form method="post" action="/dashboard/signup" {
                    input type="hidden" name="invite" value=(data.invite_token);
                    label { "Email " input type="email" name="email" required; }
                    label { "Password " input type="password" name="password" required; }
                    button type="submit" { "Sign up" }
                }
            }
            p { "Already have an account? " a href="/dashboard/login" { "Log in" } }
        }
    };
    layout(chrome, "Sign up - Monokulo", body)
}

pub fn login_page(chrome: &PageChrome, data: &LoginViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Log in" }
            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }
            form method="post" action="/dashboard/login" {
                @if let Some(next) = &data.next {
                    input type="hidden" name="next" value=(next);
                }
                label { "Email " input type="email" name="email" required; }
                label { "Password " input type="password" name="password" required; }
                button type="submit" { "Log in" }
            }
            p { "Need an account? " a href="/dashboard/signup" { "Sign up" } }
        }
    };
    layout(chrome, "Log in - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "")
    }

    #[test]
    fn signup_page_renders_with_no_error() {
        let html = signup_page(&chrome(), &SignupViewModel { error: None, invite_required: false, invite_token: String::new() }).into_string();
        assert!(html.contains("<form"));
        assert!(html.to_lowercase().contains("sign up"));
    }

    #[test]
    fn signup_page_shows_the_error_when_present() {
        let html = signup_page(
            &chrome(),
            &SignupViewModel {
                error: Some("that email is already registered".to_string()),
                invite_required: false,
                invite_token: String::new(),
            },
        )
        .into_string();
        assert!(html.contains("that email is already registered"));
    }

    #[test]
    fn signup_page_shows_the_invite_required_message_and_hides_the_form() {
        let html =
            signup_page(&chrome(), &SignupViewModel { error: None, invite_required: true, invite_token: String::new() }).into_string();
        assert!(!html.contains("<form"), "an invite-only instance with no token must not show the signup form");
        assert!(html.contains("/request-invite"));
    }

    #[test]
    fn login_page_renders_with_no_error() {
        let html = login_page(&chrome(), &LoginViewModel { error: None, next: None }).into_string();
        assert!(html.contains("<form"));
        assert!(html.to_lowercase().contains("log in"));
    }

    #[test]
    fn login_page_shows_the_error_when_present() {
        let html =
            login_page(&chrome(), &LoginViewModel { error: Some("invalid email or password".to_string()), next: None }).into_string();
        assert!(html.contains("invalid email or password"));
    }

    #[test]
    fn login_page_includes_a_hidden_next_field_when_present() {
        let html =
            login_page(&chrome(), &LoginViewModel { error: None, next: Some("/connect/woocommerce?nonce=abc".to_string()) })
                .into_string();
        assert!(html.contains(r#"type="hidden" name="next""#), "expected a hidden next field, got: {html}");
        assert!(html.contains("/connect/woocommerce?nonce"), "expected the next value's content present, got: {html}");
        assert!(html.contains("abc"), "expected the next value's content present, got: {html}");
    }

    #[test]
    fn login_page_escapes_special_characters_in_next_rather_than_injecting_them_raw() {
        let html = login_page(&chrome(), &LoginViewModel { error: None, next: Some("/connect/woocommerce?a=1&b=2".to_string()) })
            .into_string();
        assert!(html.contains("&amp;"), "expected the & in next to be HTML-escaped, got: {html}");
        assert!(!html.contains("a=1&b=2"), "a raw, unescaped & would be a template-injection smell, got: {html}");
    }

    #[test]
    fn login_page_has_no_hidden_next_field_when_absent() {
        let html = login_page(&chrome(), &LoginViewModel { error: None, next: None }).into_string();
        assert!(!html.contains(r#"name="next""#), "expected no hidden next field, got: {html}");
    }
}
