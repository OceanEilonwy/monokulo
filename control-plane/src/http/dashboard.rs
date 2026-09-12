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

use crate::templates::{ConnectViewModel, FormViewModel};

use super::AppState;
use super::AuthedUser;
use super::connections::{self, CreateConnectionError, CreateConnectionFields};
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

/// `POST /dashboard/connect`'s form fields (WBS 1.3.2) - the browser
/// equivalent of `POST /connections`'s JSON body, minus `platform` (hardcoded
/// to `"woocommerce"` below - a real "choose a platform" UI is a later, fuller
/// dashboard concern) and the three fields the JSON API already treats as
/// optional (`confirmations_required`/`zero_conf_max_piconero`/
/// `order_expiry_seconds`), left `None` here so the engine's own defaults
/// apply. `allowed_origins` arrives as one comma-separated text input rather
/// than a JSON array, since an HTML form has no native array field - split
/// into a `Vec<String>` in `connect_submit` below.
#[derive(Deserialize)]
pub struct ConnectForm {
    pub site_url: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: String,
    pub allowed_origins: String,
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

fn render_connect_form(state: &AppState, error: Option<&str>) -> Response {
    let html = state
        .templates
        .render_connect(&ConnectViewModel { error: error.map(str::to_string), public_key: None })
        .expect("the built-in connect template must always render");
    Html(html).into_response()
}

fn render_connect_success(state: &AppState, public_key: &str) -> Response {
    let html = state
        .templates
        .render_connect(&ConnectViewModel { error: None, public_key: Some(public_key.to_string()) })
        .expect("the built-in connect template must always render");
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

/// `GET /dashboard/connect` (WBS 1.3.2) - behind [`AuthedUser`]. A missing or
/// invalid session gets the exact same `401` `AuthedUser` already returns for
/// every other protected route in this crate (the JSON API's own
/// `/connections` included) - no redirect-on-401 behavior exists anywhere in
/// the dashboard yet, so a bare `401` here is the consistent choice rather
/// than inventing new behavior for just this one route.
pub async fn connect_form(State(state): State<AppState>, AuthedUser(_user, _token_hash): AuthedUser) -> Response {
    render_connect_form(&state, None)
}

/// `POST /dashboard/connect` (WBS 1.3.2) - the form equivalent of
/// `POST /connections`, calling the exact same
/// [`connections::create_connection_for_user`] both surfaces share.
/// `platform` isn't a visible field yet (hardcoded to `"woocommerce"` below -
/// a real "choose a platform" UI is a later, fuller dashboard concern per the
/// WBS's own Stage 4/dashboard notes); `confirmations_required`/
/// `zero_conf_max_piconero`/`order_expiry_seconds` aren't visible fields
/// either and are left `None` so the engine's own defaults apply, consistent
/// with the JSON API already treating them as optional.
pub async fn connect_submit(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
    Form(form): Form<ConnectForm>,
) -> Response {
    // Same split an admin-API/CLI caller would do for a comma-separated
    // list: trim whitespace around each entry, drop empty entries (so a
    // blank field submits an empty list rather than `[""]`).
    let allowed_origins: Vec<String> =
        form.allowed_origins.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();

    let fields = CreateConnectionFields {
        platform: "woocommerce".to_string(),
        site_url: form.site_url,
        view_key_hex: form.view_key_hex,
        spend_pubkey_hex: form.spend_pubkey_hex,
        network: Some(form.network),
        allowed_origins,
        confirmations_required: None,
        zero_conf_max_piconero: None,
        order_expiry_seconds: None,
    };

    match connections::create_connection_for_user(&state, &user, fields).await {
        Ok(outcome) => render_connect_success(&state, &outcome.public_key),
        Err(CreateConnectionError::BadRequest(message)) => render_connect_form(&state, Some(&message)),
        Err(CreateConnectionError::Internal) => {
            render_connect_form(&state, Some("Something went wrong. Please try again."))
        }
    }
}
