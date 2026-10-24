//! The log-in and sign-up pages (`/login`, `/signup`), for
//! `http::auth::render_login`/`render_signup`.
//!
//! Both pages always render with `chrome.logged_in == false` regardless of
//! any real session (see each struct's own doc comment below) - the nav's
//! account menu (with its theme switch) only shows once logged in, so
//! `chrome.current_path` is never actually read here; callers pass an empty
//! string rather than bothering to extract the real request URI for a
//! value that can't affect the output.

use maud::{html, Markup};

use super::PageChrome;
use ui::{Alert, Button, ButtonType, ButtonVariant, Field, InputType, Tone};

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
    /// /signup?invite=...`'s query param, echoed back on a
    /// rejected `POST` the same way every other field on this form already
    /// is) - the raw, not-yet-validated token; real validation happens at
    /// submit time (`Db::redeem_invite_and_create_user`), never here.
    pub invite_token: String,
    /// Where the visitor was going (a plugin's connect page), already
    /// checked to be a path on this site: kept through sign-up and wallet
    /// setup.
    pub next: Option<String>,
    /// The theme the form has picked: System, or what a refused sign-up
    /// sent.
    pub theme: crate::db::Theme,
}

pub struct LoginViewModel {
    pub error: Option<String>,
    /// When present, rendered as a hidden form field so a successful login
    /// can redirect back to it (see `auth::login_submit`) instead of
    /// the default inline confirmation - the raw, caller-supplied query
    /// value, not yet validated as a safe redirect target here (that
    /// happens in `login_submit`, right before it's ever used as a
    /// redirect location, never here at render time).
    pub next: Option<String>,
    /// What the plugin is called and its shop's host, when `next` is a
    /// plugin's connect page.
    pub connecting_site: Option<crate::plugins::ConnectingPlugin>,
}

fn with_next(path: &str, next: Option<&str>) -> String {
    match next {
        Some(next) => crate::base::url(format!(
            "{path}?next={}",
            url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>()
        )),
        None => crate::base::url(path),
    }
}

pub fn signup_page(chrome: &PageChrome, data: &SignupViewModel) -> Markup {
    let header = ui::PageHeader::new("Sign up");
    let body = html! {
        @if let Some(error) = &data.error {
            (Alert::new(error.as_str(), Tone::Error))
        }
        @if data.invite_required {
            p {
                "This instance is invite-only. "
                a href=(crate::base::url("/request-invite")) { "Request an invite" }
                " to join."
            }
        } @else {
            form method="post" action=(crate::base::url("/signup")) class="signup-form mk-stack" {
                input type="hidden" name="invite" value=(data.invite_token);
                @if let Some(next) = &data.next {
                    input type="hidden" name="next" value=(next);
                }
                (Field::new("Email", "signup-email", "email").kind(InputType::Email).required(true).autocomplete("email"))
                (Field::new("Password", "signup-password", "password")
                    .kind(InputType::Password)
                    .required(true)
                    .autocomplete("new-password")
                    .attr("minlength", crate::http::MIN_PASSWORD_LEN.to_string())
                    .hint(format!("At least {} characters.", crate::http::MIN_PASSWORD_LEN)))
                (super::account::theme_choice(data.theme, None, "You can change it any time from the account menu."))
                (Button::new("Sign up").variant(ButtonVariant::Primary).kind(ButtonType::Submit))
            }
        }
        p { "Already have an account? " a href=(with_next("/login", data.next.as_deref())) { "Log in" } }
    };
    super::Page::new("Sign up - Monokulo", header).render(chrome, body)
}

pub fn login_page(chrome: &PageChrome, data: &LoginViewModel) -> Markup {
    let header = ui::PageHeader::new("Log in");
    let body = html! {
        @if let Some(crate::plugins::ConnectingPlugin { plugin, site }) = &data.connecting_site {
            (Alert::new(html! {
                p { strong { (site) " wants to connect to Monokulo." } }
                p {
                    "Its " (plugin) " plugin sent you here. Log in to choose which wallet its payments go to. "
                    "New to Monokulo? Sign up instead: you'll set up a wallet, then come straight back to this."
                }
            }, Tone::Notice).block(true).quiet(true))
        }
        @if let Some(error) = &data.error {
            (Alert::new(error.as_str(), Tone::Error))
        }
        form method="post" action=(crate::base::url("/login")) class="mk-stack" {
            @if let Some(next) = &data.next {
                input type="hidden" name="next" value=(next);
            }
            (Field::new("Email", "login-email", "email").kind(InputType::Email).required(true).autocomplete("email"))
            (Field::new("Password", "login-password", "password")
                .kind(InputType::Password)
                .required(true)
                .autocomplete("current-password"))
            (Button::new("Log in").variant(ButtonVariant::Primary).kind(ButtonType::Submit))
        }
        p { "Need an account? " a href=(with_next("/signup", data.next.as_deref())) { "Sign up" } }
    };
    super::Page::new("Log in - Monokulo", header).render(chrome, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "")
    }

    #[test]
    fn the_login_page_carries_next_only_when_given_and_escaped() {
        let html = login_page(
            &chrome(),
            &LoginViewModel {
                connecting_site: None,
                error: None,
                next: Some("/plugins/connect/acme-cart?a=1&b=2".to_string()),
            },
        )
        .into_string();
        assert!(
            html.contains("&amp;"),
            "expected the & in next to be HTML-escaped, got: {html}"
        );
        assert!(
            !html.contains("a=1&b=2"),
            "a raw, unescaped & would be a template-injection smell, got: {html}"
        );

        // Login page has no hidden next field when absent.
        {
            let html = login_page(
                &chrome(),
                &LoginViewModel {
                    connecting_site: None,
                    error: None,
                    next: None,
                },
            )
            .into_string();
            assert!(
                !html.contains(r#"name="next""#),
                "expected no hidden next field, got: {html}"
            );
        }

        // Login page includes a hidden next field when present.
        {
            let html = login_page(
                &chrome(),
                &LoginViewModel {
                    connecting_site: None,
                    error: None,
                    next: Some("/plugins/connect/acme-cart?nonce=abc".to_string()),
                },
            )
            .into_string();
            assert!(
                html.contains(r#"type="hidden" name="next""#),
                "expected a hidden next field, got: {html}"
            );
            assert!(
                html.contains("/plugins/connect/acme-cart?nonce"),
                "expected the next value's content present, got: {html}"
            );
            assert!(
                html.contains("abc"),
                "expected the next value's content present, got: {html}"
            );
        }
    }
}
