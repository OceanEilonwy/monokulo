//! Browser-facing signup/login pages (WBS 1.3.1) - a separate, human-facing
//! surface layered on top of the same account-creation/login logic the JSON
//! `POST /signup`/`POST /login` API uses (`signup::create_account`,
//! `login::authenticate` - factored out of those handlers for exactly this
//! reuse, see their own doc comments). The JSON API stays exactly as-is;
//! nothing here changes its behavior, request/response shape, or routes.
//!
//! Ends in a session cookie rather than a bearer token a human would have
//! to copy-paste out of a JSON body, per the WBS's own design decision for
//! this task. The cookie holds the exact same raw session token
//! `POST /login`'s JSON response returns as `session_token` - just delivered
//! as `Set-Cookie` instead. [`super::AuthedUser`] accepts either form (see
//! its doc comment), so this is genuinely one auth system with two ways in,
//! not two parallel ones.
//!
//! A plain HTML `<form>` posts `application/x-www-form-urlencoded`, not
//! JSON - hence `axum::extract::Form` here instead of `axum::extract::Json`.

use axum::extract::{Form, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::Deserialize;

use crate::templates::FormViewModel;

use super::AppState;
use super::login::{self, LoginError};
use super::signup::{self, CreateAccountError};

#[derive(Deserialize)]
pub struct SignupForm {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub email: String,
    pub password: String,
}

fn render_signup(state: &AppState, error: Option<&str>) -> Response {
    let html = state
        .templates
        .render_signup(&FormViewModel { error: error.map(str::to_string) })
        .expect("the built-in signup template must always render");
    Html(html).into_response()
}

fn render_login(state: &AppState, error: Option<&str>) -> Response {
    let html = state
        .templates
        .render_login(&FormViewModel { error: error.map(str::to_string) })
        .expect("the built-in login template must always render");
    Html(html).into_response()
}

/// `303`-free, deliberate `302 Found` redirect (axum's own `Redirect::to`
/// issues `303 See Other` instead - see its doc comment - and the WBS spec
/// for this task calls out `302` specifically).
fn redirect_302(location: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

pub async fn signup_form(State(state): State<AppState>) -> Response {
    render_signup(&state, None)
}

pub async fn signup_submit(State(state): State<AppState>, Form(form): Form<SignupForm>) -> Response {
    match signup::create_account(&state, &form.email, &form.password) {
        // Simplest reasonable post-signup behavior: send the new user to the
        // login page rather than also logging them in here - it reuses
        // `login_submit`'s own cookie-setting path instead of duplicating it,
        // at the cost of one extra form submission for the user.
        Ok(_user_id) => redirect_302("/dashboard/login"),
        Err(CreateAccountError::DuplicateEmail) => {
            render_signup(&state, Some("That email is already registered. Try logging in instead."))
        }
        Err(CreateAccountError::Internal) => render_signup(&state, Some("Something went wrong. Please try again.")),
    }
}

pub async fn login_form(State(state): State<AppState>) -> Response {
    render_login(&state, None)
}

pub async fn login_submit(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    match login::authenticate(&state, &form.email, &form.password) {
        Ok((_user, raw_token)) => {
            // `HttpOnly` - never readable from page JS, so an XSS can't
            // exfiltrate the session token. `SameSite=Lax` - sent on
            // top-level navigation but not on cross-site subrequests/embeds,
            // a reasonable default CSRF mitigation for a cookie-authenticated
            // browser flow with no separate CSRF token yet. `Path=/` so it's
            // sent back to every control-plane route `AuthedUser` might
            // guard, not just `/dashboard/*`. Not marked `Secure`: this is a
            // local/dev deployment with no TLS terminated in front of it yet
            // (see `main.rs`'s own placeholder-config notes) - marking it
            // `Secure` now would silently break the cookie over plain HTTP
            // before real deployment wiring exists.
            let cookie = Cookie::build((super::SESSION_COOKIE_NAME, raw_token))
                .http_only(true)
                .same_site(SameSite::Lax)
                .path("/")
                .build();
            let jar = CookieJar::new().add(cookie);

            // No real dashboard content page exists yet (WBS 1.3.3, a later
            // task), so a redirect to one would land on a 404. Rendering a
            // minimal inline confirmation here instead is the honest
            // option - it doesn't promise a destination that doesn't exist -
            // and 1.3.3 can freely turn this into a redirect once there's
            // somewhere real to go.
            let body = Html(
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
                 <title>Logged in - MoneroPay Cloud</title></head><body>\
                 <h1>You're logged in</h1><p>Your session is active.</p></body></html>",
            );
            (jar, body).into_response()
        }
        Err(LoginError::Unauthorized) => render_login(&state, Some("Invalid email or password.")),
        Err(LoginError::Internal) => render_login(&state, Some("Something went wrong. Please try again.")),
    }
}
