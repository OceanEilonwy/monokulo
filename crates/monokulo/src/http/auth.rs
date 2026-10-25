//! The sign-up, log-in, log-out and theme forms, on top of
//! `signup::create_account` and `login::authenticate` (which the first-run
//! admin setup shares). Logging in or signing up ends in the session
//! cookie [`super::AuthedUser`] reads back. Plus the redirect helpers every
//! form handler uses, and [`SafePath`], the check a `next` value passes
//! before anything redirects to it.

use axum::extract::{Form, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::db::Theme;
use crate::views;

use super::login::{self, LoginError};
use super::signup::{self, CreateAccountError};
use super::AppState;
use super::AuthedUser;
use crate::db::Database;

#[derive(Deserialize)]
pub struct SignupForm {
    pub email: String,
    pub password: String,
    /// The hidden field `views::auth::signup_page` always renders (see
    /// `views::auth::SignupViewModel::invite_token`) - empty in `"public"`
    /// mode, where it's submitted but ignored.
    #[serde(default)]
    pub invite: String,
    /// Where the visitor was going before signing up (a plugin's connect
    /// page): carried through wallet setup and back there.
    #[serde(default)]
    pub next: Option<String>,
    /// The theme picked on the form (`system`, `light` or `dark`), saved
    /// to the new account.
    #[serde(default)]
    pub theme: Option<String>,
}

#[derive(Deserialize)]
pub struct SignupQuery {
    /// `GET /signup?invite=<token>` - carried straight into the
    /// rendered form's hidden field, unvalidated (see
    /// `views::auth::SignupViewModel::invite_token`'s own doc comment on why validation
    /// only ever happens at submit time).
    #[serde(default)]
    pub invite: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub email: String,
    pub password: String,
    /// Carried through from `GET /login?next=...`'s hidden form
    /// field (see `LoginQuery`/`login_form` below) - `None` when there was
    /// no `next`. Never trusted as a redirect target as-is - see
    /// [`safe_redirect_path`].
    pub next: Option<String>,
}

/// Query parameters `GET /login` accepts - just the
/// optional `next` value the generic connect flow (`http/connect.rs`)
/// redirects here with when the browser has no session yet. Rendered
/// straight into the login page's hidden `next` field, unvalidated - see
/// [`safe_redirect_path`]'s own doc comment on why validating it only
/// matters, and only happens, at the point it's actually used as a redirect
/// location (`login_submit`), not here at render time.
#[derive(Deserialize)]
pub struct LoginQuery {
    pub next: Option<String>,
}

/// Validates that `next` is safe to use as an internal redirect target -
/// this is the one thing standing between `login_submit` and an
/// open-redirect vulnerability, since `next` is otherwise fully
/// attacker-controlled (anyone can link a victim straight to
/// `/login?next=<anything>`, not just the connect flow that
/// legitimately sets it).
///
/// Rather than guess at how a browser reads the value, `next` is parsed the
/// way a browser parses a `Location` (the WHATWG URL rules the `url` crate
/// implements), resolved against a placeholder origin, and kept only if it
/// is still on that origin. That refuses every way of leaving the site at
/// once: a protocol-relative `//evil.example.com`, an absolute URL with its
/// own scheme, a backslash browsers read as a slash (`/\evil.example.com`),
/// and tabs or newlines browsers strip before parsing (`/<TAB>/evil.example`
/// becomes `//evil.example`). What is kept is the parsed path, query and
/// fragment, so anything the parser would strip or percent-encode is
/// already gone or encoded in what we send. `next` must also start with
/// `/`, so a bare relative value isn't read against the page it came from.
///
/// A `next` that fails is not an error - the caller (`login_submit`) just
/// falls back to its existing default behavior, same as if `next` had
/// never been provided at all.
fn safe_redirect_path(next: &str) -> Option<String> {
    if !next.starts_with('/') {
        return None;
    }
    let placeholder = url::Url::parse("https://placeholder.invalid/").ok()?;
    let joined = placeholder.join(next).ok()?;
    if joined.origin() != placeholder.origin() {
        return None;
    }
    let mut path = joined.path().to_string();
    if let Some(query) = joined.query() {
        path.push('?');
        path.push_str(query);
    }
    if let Some(fragment) = joined.fragment() {
        path.push('#');
        path.push_str(fragment);
    }
    Some(path)
}

/// A path on this site, safe to send a browser to: made only by
/// [`SafePath::parse`], so a `next` value a request carried can't reach
/// [`redirect_to`] unchecked (an open redirect).
pub(crate) struct SafePath(String);

impl SafePath {
    /// The path [`safe_redirect_path`] makes of `next`, if it stays on this
    /// site.
    pub(crate) fn parse(next: &str) -> Option<Self> {
        safe_redirect_path(next).map(SafePath)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A `302` to a checked path that came from a request.
pub(crate) fn redirect_to(path: &SafePath) -> Response {
    crate::base::redirect(path.as_str())
}

/// `chrome.logged_in` is always `false` here, not a real per-request
/// session check - this page's whole purpose is establishing a *new*
/// session, so showing the sign-up/log-in links regardless of any existing
/// one is the reasonable default (see `views::auth`'s own doc comment).
async fn render_signup(
    state: &AppState,
    error: Option<&str>,
    invite_token: &str,
    next: Option<&str>,
    theme: Theme,
) -> Response {
    let invite_only = state.settings.signup_mode() == crate::settings::SignupMode::InviteOnly;
    let invite_required = invite_only && invite_token.trim().is_empty();
    let chrome = super::page_chrome(state, None, "").await;
    let data = views::auth::SignupViewModel {
        error: error.map(str::to_string),
        invite_required,
        invite_token: invite_token.to_string(),
        theme,
        next: next
            .and_then(SafePath::parse)
            .map(|p| p.as_str().to_owned()),
    };
    views::auth::signup_page(&chrome, &data).into_response()
}

/// Same `chrome.logged_in == false` reasoning as `render_signup` above.
async fn render_login(state: &AppState, error: Option<&str>, next: Option<&str>) -> Response {
    let chrome = super::page_chrome(state, None, "").await;
    let data = views::auth::LoginViewModel {
        error: error.map(str::to_string),
        connecting_site: super::connect::connecting_site(next),
        next: next.map(str::to_string),
    };
    views::auth::login_page(&chrome, &data).into_response()
}

/// A `302 Found` to a URL off this site (a plugin's return address);
/// a path on this site goes through `crate::base::redirect`.
pub(crate) fn redirect_away(url: &url::Url) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, url.as_str())]).into_response()
}

pub async fn signup_form(
    State(state): State<AppState>,
    Query(query): Query<SignupQuery>,
) -> Response {
    render_signup(
        &state,
        None,
        query.invite.as_deref().unwrap_or(""),
        query.next.as_deref(),
        Theme::System,
    )
    .await
}

/// The theme a sign-up form chose: System unless it names another.
fn signup_theme(chosen: Option<&str>) -> Theme {
    match chosen {
        Some("light") => Theme::Light,
        Some("dark") => Theme::Dark,
        _ => Theme::System,
    }
}

pub async fn signup_submit(
    State(state): State<AppState>,
    on: crate::network::OnNetwork,
    Form(form): Form<SignupForm>,
) -> Response {
    let theme = signup_theme(form.theme.as_deref());
    let (error, invite) = match signup::create_account(
        &state,
        &form.email,
        &form.password,
        false,
        Some(&form.invite),
    )
    .await
    {
        // Logged in straight away, and on to setting up a store (`/setup`),
        // or back to where they were going (a plugin's connect page, which
        // sets one up too).
        Ok(user_id) => {
            let user_id = crate::db::UserId::new(user_id);
            if theme != Theme::System {
                let id = user_id.clone();
                if let Err(e) = state
                    .db
                    .write(move |db| db.update_user_theme(&id, theme))
                    .await
                {
                    // The account works without it: the theme is System.
                    tracing::error!(error = %e, "saving the theme chosen at sign-up failed");
                }
            }
            let Ok(raw_token) = login::start_session(&state, &user_id).await else {
                return crate::base::redirect("/login");
            };
            let cookie = super::session_cookie(on.secure, raw_token.expose().to_string());
            let jar = CookieJar::new().add(cookie);
            let setup = match form.next.as_deref().and_then(SafePath::parse) {
                Some(next) => next.as_str().to_owned(),
                None => "/setup".to_owned(),
            };
            return (jar, crate::base::redirect(&setup)).into_response();
        }
        Err(CreateAccountError::DuplicateEmail) => (
            Some("That email is already registered. Try logging in instead.".to_string()),
            form.invite.as_str(),
        ),
        Err(CreateAccountError::Internal) => (
            Some("Something went wrong. Please try again.".to_string()),
            form.invite.as_str(),
        ),
        Err(CreateAccountError::InviteRequired) => (None, ""),
        Err(CreateAccountError::InvalidOrUsedInvite) => (
            Some(
                "That invite link is invalid or has already been used. Please request a new one."
                    .to_string(),
            ),
            "",
        ),
        Err(CreateAccountError::WeakPassword) => (
            Some(format!(
                "The password must be at least {} characters.",
                signup::MIN_PASSWORD_LEN
            )),
            form.invite.as_str(),
        ),
        Err(CreateAccountError::InvalidEmail) => (
            Some("That is not an email address.".to_string()),
            form.invite.as_str(),
        ),
        // Not a path a non-admin signup takes.
        Err(CreateAccountError::AlreadySetUp) => return crate::base::redirect("/login"),
    };
    render_signup(
        &state,
        error.as_deref(),
        invite,
        form.next.as_deref(),
        theme,
    )
    .await
}

pub async fn login_form(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Response {
    render_login(&state, None, query.next.as_deref()).await
}

/// `POST /logout` - the nav's "Log out" button (a small same-origin form,
/// not a bare `<a href>`, since it changes state). Behind [`AuthedUser`]
/// like every other signed-in page. Deletes the session, clears the cookie
/// (an empty value with `max_age(0)`) so the browser stops presenting it,
/// and redirects to the login page.
pub async fn logout_submit(
    State(db): State<Database>,
    State(revocations): State<std::sync::Arc<crate::revocation::Revocations>>,
    on: crate::network::OnNetwork,
    AuthedUser(_user, token_hash): AuthedUser,
) -> Response {
    // The cookie goes either way; a session row that stays is logged, as
    // it would still work for whoever has the token.
    let session = token_hash.clone();
    match db.write(move |db| db.delete_session(&session)).await {
        // What the session has open ends with it.
        Ok(_) => revocations.revoke(&token_hash),
        Err(e) => tracing::error!(error = %e, "logging out failed to delete the session"),
    }
    let mut cookie = super::session_cookie(on.secure, String::new());
    cookie.set_max_age(time::Duration::ZERO);
    let jar = CookieJar::new().add(cookie);
    (jar, crate::base::redirect("/login")).into_response()
}

#[derive(Deserialize)]
pub struct ThemeForm {
    pub next: Option<String>,
    pub theme: Option<String>,
}

fn selected_theme(current: Theme, submitted: Option<&str>) -> Theme {
    match submitted {
        Some("light") => Theme::Light,
        Some("system") => Theme::System,
        Some("dark") => Theme::Dark,
        _ => current.next(),
    }
}

/// `POST /theme` - the account menu's no-JS theme switch. Named
/// submit buttons select a theme directly; a form without a choice cycles.
/// Persists the result against the authenticated user (not a cookie - see
/// `migrations/0017_user_theme.sql` for why this is server-side per-user
/// state rather than client storage), then redirects back to wherever the
/// form was submitted from - the same validated-`next` pattern
/// `login_submit` uses (see [`safe_redirect_path`]), falling back to `/`
/// for anything that doesn't validate. No CSRF token needed beyond what
/// `logout_submit` above relies on (`SameSite=Lax`, `http::csrf`).
pub async fn theme_submit(
    State(db): State<Database>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<ThemeForm>,
) -> Response {
    let next_theme = selected_theme(user.theme, form.theme.as_deref());
    let user_id = user.id.clone();
    db.write(move |db| db.update_user_theme(&user_id, next_theme))
        .await
        .ok();
    match form.next.as_deref().and_then(SafePath::parse) {
        Some(next) => redirect_to(&next),
        None => crate::base::redirect("/"),
    }
}

/// `POST /login`. If a *validated* `next` was carried through the form
/// (see [`safe_redirect_path`]), a successful login redirects there -
/// this is what lets the connect flow (`http/connect.rs`) send a browser
/// with no session to log in and land back where it started. Otherwise
/// (no `next`, or one that fails validation) it goes to the home page.
pub async fn login_submit(
    State(state): State<AppState>,
    on: crate::network::OnNetwork,
    Form(form): Form<LoginForm>,
) -> Response {
    match login::authenticate(&state, &form.email, &form.password).await {
        Ok((_user, raw_token)) => {
            // See `http::session_cookie` for the cookie's attributes.
            let cookie = super::session_cookie(on.secure, raw_token.expose().to_string());
            let jar = CookieJar::new().add(cookie);

            // A validated `next` wins over the home page - see this
            // function's own doc comment and [`safe_redirect_path`].
            if let Some(next) = form.next.as_deref().and_then(SafePath::parse) {
                return (jar, redirect_to(&next)).into_response();
            }

            // A plain login lands on the home page (`http/home.rs`).
            (jar, crate::base::redirect("/")).into_response()
        }
        Err(LoginError::Unauthorized) => {
            render_login(
                &state,
                Some("Invalid email or password."),
                form.next.as_deref(),
            )
            .await
        }
        Err(LoginError::Internal) => {
            render_login(
                &state,
                Some("Something went wrong. Please try again."),
                form.next.as_deref(),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SafePath;

    /// A redirect target from a request exists only once checked: a path
    /// on this site, never anything a browser could take elsewhere. These
    /// are what keep `login_submit` from following an attacker's `next`
    /// after a real login.
    #[test]
    fn a_safe_path_is_made_only_from_a_path_on_this_site() {
        let made = |next: &str| SafePath::parse(next).map(|p| p.as_str().to_string());
        for accepted in [
            "/plugins/connect/woocommerce",
            "/plugins/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&nonce=abc",
            "/account/wallets",
            "/account/wallets#keys",
        ] {
            assert_eq!(made(accepted).as_deref(), Some(accepted));
        }
        // Still on this site, as the path a browser would read: a colon in
        // the path is only a path, and a space is encoded rather than left
        // for a browser to read.
        for (next, path) in [
            ("/javascript:alert(1)", "/javascript:alert(1)"),
            ("/ /evil", "/%20/evil"),
        ] {
            assert_eq!(made(next).as_deref(), Some(path), "{next}");
        }
        for refused in [
            // Protocol-relative.
            "//evil.example.com",
            "//evil.example.com/path",
            // Absolute.
            "https://evil.example.com",
            "http://evil.example.com/account",
            // Some browsers read a leading backslash as a slash, which
            // makes this `//evil.example.com`.
            "/\\evil.example.com",
            // Browsers strip tabs and newlines from a URL before parsing
            // it, which makes each of these `//evil.example`.
            "/\t/evil.example",
            "/\n/evil.example",
            "/\r\n/evil.example",
            "/\t\t/evil.example",
            // No leading slash.
            "evil.example.com",
            "account/wallets",
            " //evil.example",
        ] {
            assert!(made(refused).is_none(), "{refused:?}");
        }
    }
}
