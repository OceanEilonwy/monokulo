//! `GET`/`POST /admin/setup` - the first-run admin setup wizard. A fresh
//! monokulo install has no user at all, let alone an admin one, and no
//! `AuthedUser`-style credential could ever bootstrap itself - this route is
//! necessarily unauthenticated, the same way `/signup` is.
//!
//! Gated on `Db::is_setup_complete` (`crate::db`), not on "does an admin user
//! exist" - a dedicated `settings` row, set once this wizard's `POST`
//! succeeds, per the explicit "based upon a flag in database" requirement.
//! `home::landing` (`GET /`) redirects here whenever setup isn't complete -
//! that's the only gate: every other route stays reachable exactly as before
//! (in particular, the plain merchant `/signup` flow is untouched - this
//! wizard only ever concerns the one instance-wide admin account).
//!
//! A successful submission creates the one admin account
//! (`signup::create_account(.., is_admin: true)`), marks setup complete, logs
//! the new admin straight in (the same session-cookie mechanics
//! `dashboard::login_submit` uses), and redirects to the admin settings page
//! - so completing the wizard is the entire "first start" experience the
//! product spec calls for, not a dead end that then demands a second manual
//! login.

use axum::extract::{Form, State};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::Deserialize;

use crate::views;
use crate::views::admin::SetupViewModel;

use super::dashboard::redirect_302;
use super::login;
use super::signup::{self, CreateAccountError};
use super::AppState;

/// Minimum password length enforced here - deliberately longer than
/// whatever (if any) minimum the plain merchant signup form enforces: this
/// one account can reconfigure every setting on the instance, scanner
/// settings included, so it gets its own, stricter floor rather than
/// inheriting a merchant-signup default that was never chosen with that in
/// mind.
const MIN_ADMIN_PASSWORD_LEN: usize = 12;

#[derive(Deserialize)]
pub struct SetupForm {
    pub email: String,
    pub password: String,
    pub confirm_password: String,
}

fn render_setup_form(state: &AppState, error: Option<&str>, email: &str) -> Response {
    let chrome = super::page_chrome(state, None, "/admin/setup");
    let data = SetupViewModel { error: error.map(str::to_string), email: email.to_string() };
    views::admin::setup_page(&chrome, &data).into_response()
}

/// `GET /admin/setup`. Once setup is already complete this is no longer a
/// meaningful page to show (and must never re-run - it would let a visitor
/// overwrite the one admin account) - redirect home instead of erroring, the
/// same "just take me somewhere sensible" behavior a stale bookmark to this
/// URL deserves.
pub async fn setup_form(State(state): State<AppState>) -> Response {
    if state.db.lock().unwrap().is_setup_complete().unwrap_or(true) {
        return redirect_302("/");
    }
    render_setup_form(&state, None, "")
}

/// `POST /admin/setup`. Re-checks `is_setup_complete` again right before
/// creating the account - not just trusting that `GET` already gated this -
/// since two concurrent submissions (or a replayed form post after setup
/// already completed elsewhere) must never be able to create a second admin
/// account.
pub async fn setup_submit(State(state): State<AppState>, Form(form): Form<SetupForm>) -> Response {
    if state.db.lock().unwrap().is_setup_complete().unwrap_or(true) {
        return redirect_302("/");
    }

    if form.password != form.confirm_password {
        return render_setup_form(&state, Some("Passwords do not match."), &form.email);
    }
    if form.password.len() < MIN_ADMIN_PASSWORD_LEN {
        return render_setup_form(
            &state,
            Some(&format!("Password must be at least {MIN_ADMIN_PASSWORD_LEN} characters.")),
            &form.email,
        );
    }

    match signup::create_account(&state, &form.email, &form.password, true, None) {
        Ok(_user_id) => {
            // The account row and the `setup_complete` flag are two separate
            // writes (`Db` has no cross-statement transaction API today) -
            // if marking complete somehow failed, the account still exists,
            // so this genuinely is the fallback the module doc comment on
            // `is_setup_complete` describes: `settings` explicitly (a
            // dedicated flag), not "any admin user exists" - `.ok()` here
            // means the very next request just re-runs the (idempotent for
            // this purpose) `mark_setup_complete` if this instance ever hits
            // that path.
            state.db.lock().unwrap().mark_setup_complete().ok();

            match login::authenticate(&state, &form.email, &form.password) {
                Ok((_user, raw_token)) => {
                    let cookie = Cookie::build((super::SESSION_COOKIE_NAME, raw_token))
                        .http_only(true)
                        .same_site(SameSite::Lax)
                        .path("/")
                        .build();
                    let jar = CookieJar::new().add(cookie);
                    (jar, redirect_302("/dashboard/admin/settings")).into_response()
                }
                // The account was just created with this exact password, so
                // this is unreachable in practice - falling back to a plain
                // login redirect rather than `.expect()` is simply the
                // cheapest safe response to a case that should never occur.
                Err(_) => redirect_302("/dashboard/login"),
            }
        }
        Err(CreateAccountError::DuplicateEmail) => {
            render_setup_form(&state, Some("That email is already registered."), &form.email)
        }
        // `is_admin: true` above skips the invite check outright
        // (`signup::create_account`'s own doc comment) - these two variants
        // are genuinely unreachable from this call site, kept as a plain
        // fallback rather than `unreachable!()` since "something went wrong,
        // try again" is still a perfectly safe response if that ever
        // somehow changed.
        Err(CreateAccountError::Internal)
        | Err(CreateAccountError::InviteRequired)
        | Err(CreateAccountError::InvalidOrUsedInvite) => {
            render_setup_form(&state, Some("Something went wrong. Please try again."), &form.email)
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::db::Db;
    use crate::engine_client::EngineClient;
    use crate::http::{build_router, AppState};

    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    /// A fresh, *unseeded* instance - deliberately not `Db::seed_test_admin`
    /// (every other test fixture in this crate does seed it, precisely so
    /// the wizard *doesn't* trigger there) - this module exists specifically
    /// to prove the wizard *does* trigger, and works, on a genuinely fresh
    /// install.
    fn fresh_router() -> Router {
        let state = AppState {
            db: Db::open_in_memory().unwrap().into_shared(),
            engine_client: EngineClient::new("http://127.0.0.1:1"),
            encryption_key: TEST_ENCRYPTION_KEY,
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only()),
            rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
            event_streams: Default::default(),
        };
        build_router(state)
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn setup_form_request(email: &str, password: &str, confirm_password: &str) -> Request<Body> {
        let body = format!(
            "email={}&password={}&confirm_password={}",
            urlencoding_encode(email),
            urlencoding_encode(password),
            urlencoding_encode(confirm_password),
        );
        Request::builder()
            .method("POST")
            .uri("/admin/setup")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    }

    fn urlencoding_encode(s: &str) -> String {
        url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
    }

    /// The real point of this whole feature: a fresh install's front door
    /// (`GET /`) must not show the landing page at all - it redirects
    /// straight to the wizard.
    #[tokio::test]
    async fn a_fresh_install_redirects_the_landing_page_to_the_setup_wizard() {
        let router = fresh_router();
        let response = router.oneshot(Request::builder().method("GET").uri("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers().get("location").unwrap(), "/admin/setup");
    }

    #[tokio::test]
    async fn the_setup_wizard_form_itself_is_reachable_on_a_fresh_install() {
        let router = fresh_router();
        let response =
            router.oneshot(Request::builder().method("GET").uri("/admin/setup").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("Set up your admin account"), "expected the wizard form, got: {html}");
    }

    /// Once setup is complete, revisiting the wizard (e.g. a stale bookmark)
    /// must never let a second submission overwrite or add another admin -
    /// it just bounces away.
    #[tokio::test]
    async fn revisiting_the_wizard_after_setup_is_complete_redirects_away_instead_of_re_rendering() {
        let router = fresh_router();
        let submit = router
            .clone()
            .oneshot(setup_form_request("admin@example.com", "a very long admin password", "a very long admin password"))
            .await
            .unwrap();
        assert_eq!(submit.status(), StatusCode::FOUND, "the first submission should succeed and redirect");

        let second_get =
            router.clone().oneshot(Request::builder().method("GET").uri("/admin/setup").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(second_get.status(), StatusCode::FOUND);
        assert_eq!(second_get.headers().get("location").unwrap(), "/");

        let second_post = router
            .oneshot(setup_form_request("attacker@example.com", "another long password here", "another long password here"))
            .await
            .unwrap();
        assert_eq!(second_post.status(), StatusCode::FOUND);
        assert_eq!(second_post.headers().get("location").unwrap(), "/");
    }

    #[tokio::test]
    async fn a_successful_submission_creates_an_admin_marks_setup_complete_logs_in_and_lands_on_the_settings_page() {
        let router = fresh_router();

        let response = router
            .clone()
            .oneshot(setup_form_request("owner@example.com", "a very long admin password", "a very long admin password"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers().get("location").unwrap(), "/dashboard/admin/settings");
        let set_cookie = response.headers().get("set-cookie").expect("expected a session cookie to be set").to_str().unwrap();
        assert!(set_cookie.starts_with("session="), "expected a real session cookie, got: {set_cookie}");

        // The wizard's own gate on `/` must now be gone - setup is complete.
        let landing = router
            .oneshot(Request::builder().method("GET").uri("/").header("cookie", set_cookie.split(';').next().unwrap()).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(landing.status(), StatusCode::OK);
        let html = body_text(landing).await;
        assert!(html.contains("log out"), "the freshly created admin should already be logged in, got: {html}");
    }

    #[tokio::test]
    async fn mismatched_passwords_are_rejected_and_no_account_is_created() {
        let router = fresh_router();
        let response = router
            .clone()
            .oneshot(setup_form_request("owner@example.com", "a very long admin password", "does not match at all"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a rejected submission re-renders the form, not a redirect");
        let html = body_text(response).await;
        assert!(html.contains("do not match"), "expected a clear error, got: {html}");

        // Setup must still be incomplete - the wizard still gates `/`.
        let landing = router.oneshot(Request::builder().method("GET").uri("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(landing.status(), StatusCode::FOUND);
        assert_eq!(landing.headers().get("location").unwrap(), "/admin/setup");
    }

    #[tokio::test]
    async fn a_too_short_password_is_rejected() {
        let router = fresh_router();
        let response =
            router.oneshot(setup_form_request("owner@example.com", "short1", "short1")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("at least"), "expected a clear minimum-length error, got: {html}");
    }
}
