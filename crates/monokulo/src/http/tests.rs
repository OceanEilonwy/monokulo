//! HTTP-layer integration tests for `/signup`, driven through the real
//! `Router` via `tower::ServiceExt::oneshot` — no bound socket needed, same
//! pattern as `scanner::http::tests`.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::db::Db;
use crate::engine_client::EngineClient;

use super::{AppState, build_router};

/// A dummy, never-dialed engine URL — these tests exercise `/signup`,
/// `/login`, `/logout`, and the `AuthedUser` extractor, none of which ever
/// reach the engine client. See `connections.rs`'s own tests for the
/// `/connections` handler, which does need a real spawned engine.
/// See `AppState`'s own doc comment on `exchange_rate`.
fn test_exchange_rate_provider() -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
    std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only())
}

pub(super) fn test_app_state() -> AppState {
    AppState {
        db: { let db = Db::open_in_memory().unwrap(); db.seed_test_admin(); db.into_shared() },
        engine_client: EngineClient::new("http://127.0.0.1:1"),
        encryption_key: [7u8; 32],
        status_cache: crate::http::status_page::new_status_cache(),
        exchange_rate: test_exchange_rate_provider(),
        rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
        event_streams: Default::default(),
        dns: std::sync::Arc::new(crate::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
    }
}

fn test_router() -> Router {
    build_router(test_app_state())
}

fn signup_request(email: &str, password: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/signup")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
        .unwrap()
}

fn login_request(email: &str, password: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
        .unwrap()
}

fn whoami_request(bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri("/_test/whoami");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).unwrap()
}

fn logout_request(bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri("/logout");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_valid_signup_succeeds_and_does_not_return_the_password_or_hash() {
    let router = test_router();

    let response = router.oneshot(signup_request("alice@example.com", "correct horse battery staple")).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let body = body_json(response).await;
    let obj = body.as_object().unwrap();
    assert!(obj.get("user_id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()));
    // Nothing password-shaped leaked into the response.
    assert!(!obj.contains_key("password"));
    assert!(!obj.contains_key("password_hash"));
    let rendered = body.to_string();
    assert!(!rendered.contains("correct horse battery staple"));
    assert!(!rendered.contains("$argon2"));
}

#[tokio::test]
async fn signing_up_the_same_email_twice_returns_conflict_on_the_second_attempt() {
    let router = test_router();

    let first = router.clone().oneshot(signup_request("bob@example.com", "first password")).await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = router.oneshot(signup_request("bob@example.com", "a different password")).await.unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_stored_password_hash_is_a_real_argon2_hash_not_the_plaintext_password() {
    let state = test_app_state();
    let router = build_router(state.clone());

    let plaintext = "correct horse battery staple";
    let response = router.oneshot(signup_request("carol@example.com", plaintext)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let row = state.db.lock().unwrap().get_user_by_email("carol@example.com").unwrap().unwrap();
    assert_ne!(row.password_hash, plaintext, "the plaintext password must never be stored as-is");
    assert!(
        row.password_hash.starts_with("$argon2"),
        "expected a PHC-format Argon2 hash, got: {}",
        row.password_hash
    );
    assert!(shared::password::verify_password(plaintext, &row.password_hash));
}

#[tokio::test]
async fn a_correct_login_returns_a_non_empty_session_token() {
    let router = test_router();

    let signup = router.clone().oneshot(signup_request("dave@example.com", "hunter2hunter2")).await.unwrap();
    assert_eq!(signup.status(), StatusCode::CREATED);

    let response = router.oneshot(login_request("dave@example.com", "hunter2hunter2")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    let token = body.as_object().unwrap().get("session_token").and_then(|v| v.as_str());
    assert!(token.is_some_and(|t| !t.is_empty()));
}

#[tokio::test]
async fn a_wrong_password_returns_unauthorized() {
    let router = test_router();

    let signup = router.clone().oneshot(signup_request("erin@example.com", "the real password")).await.unwrap();
    assert_eq!(signup.status(), StatusCode::CREATED);

    let response = router.oneshot(login_request("erin@example.com", "not the real password")).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_unknown_email_returns_the_same_unauthorized_response_as_a_wrong_password() {
    let router = test_router();

    let unknown_email_response = router
        .clone()
        .oneshot(login_request("nobody-has-this-account@example.com", "whatever"))
        .await
        .unwrap();
    assert_eq!(unknown_email_response.status(), StatusCode::UNAUTHORIZED);
    let unknown_email_body = body_json(unknown_email_response).await;

    let signup = router.clone().oneshot(signup_request("frank@example.com", "the real password")).await.unwrap();
    assert_eq!(signup.status(), StatusCode::CREATED);
    let wrong_password_response =
        router.oneshot(login_request("frank@example.com", "not the real password")).await.unwrap();
    assert_eq!(wrong_password_response.status(), StatusCode::UNAUTHORIZED);
    let wrong_password_body = body_json(wrong_password_response).await;

    // Same status *and* same body shape - a client must not be able to
    // distinguish "no such account" from "wrong password".
    assert_eq!(unknown_email_body, wrong_password_body);
}

#[tokio::test]
async fn a_valid_session_token_reaches_the_protected_test_route_as_the_right_user() {
    let router = test_router();

    let signup = router.clone().oneshot(signup_request("grace@example.com", "correct horse battery staple")).await.unwrap();
    assert_eq!(signup.status(), StatusCode::CREATED);
    let user_id = body_json(signup).await.as_object().unwrap().get("user_id").unwrap().as_str().unwrap().to_string();

    let login =
        router.clone().oneshot(login_request("grace@example.com", "correct horse battery staple")).await.unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let session_token =
        body_json(login).await.as_object().unwrap().get("session_token").unwrap().as_str().unwrap().to_string();

    let response = router.oneshot(whoami_request(Some(&session_token))).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body.as_object().unwrap().get("user_id").unwrap().as_str().unwrap(), user_id);
}

#[tokio::test]
async fn the_protected_test_route_rejects_a_missing_authorization_header() {
    let router = test_router();
    let response = router.oneshot(whoami_request(None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_protected_test_route_rejects_an_unknown_bearer_token() {
    let router = test_router();
    let response = router.oneshot(whoami_request(Some("garbage-token-nobody-issued"))).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logging_out_a_valid_session_returns_no_content() {
    let router = test_router();

    let signup =
        router.clone().oneshot(signup_request("henry@example.com", "correct horse battery staple")).await.unwrap();
    assert_eq!(signup.status(), StatusCode::CREATED);

    let login =
        router.clone().oneshot(login_request("henry@example.com", "correct horse battery staple")).await.unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let session_token =
        body_json(login).await.as_object().unwrap().get("session_token").unwrap().as_str().unwrap().to_string();

    let response = router.oneshot(logout_request(Some(&session_token))).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn after_logging_out_the_same_session_token_no_longer_reaches_a_protected_route() {
    let router = test_router();

    let signup =
        router.clone().oneshot(signup_request("iris@example.com", "correct horse battery staple")).await.unwrap();
    assert_eq!(signup.status(), StatusCode::CREATED);

    let login =
        router.clone().oneshot(login_request("iris@example.com", "correct horse battery staple")).await.unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let session_token =
        body_json(login).await.as_object().unwrap().get("session_token").unwrap().as_str().unwrap().to_string();

    // Prove the session actually works before logout, so the post-logout
    // 401 below demonstrates revocation rather than a token that never
    // worked in the first place.
    let before = router.clone().oneshot(whoami_request(Some(&session_token))).await.unwrap();
    assert_eq!(before.status(), StatusCode::OK);

    let logout_response = router.clone().oneshot(logout_request(Some(&session_token))).await.unwrap();
    assert_eq!(logout_response.status(), StatusCode::NO_CONTENT);

    // The same token, reused against the same protected route, must now be
    // rejected - the session was genuinely deleted, not just "the logout
    // call returned 204".
    let after = router.oneshot(whoami_request(Some(&session_token))).await.unwrap();
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_rejects_a_missing_authorization_header() {
    let router = test_router();
    let response = router.oneshot(logout_request(None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_rejects_an_unknown_bearer_token() {
    let router = test_router();
    let response = router.oneshot(logout_request(Some("garbage-token-nobody-issued"))).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// -- WBS 1.3.1: browser-facing signup/login pages ---------------------------

fn form_request(uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

/// Minimal `application/x-www-form-urlencoded` percent-encoding for test
/// fixtures only - real clients (browsers) do this themselves; there's no
/// reason to pull in a whole crate just to build a test request body.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn get_dashboard_signup_returns_html() {
    let router = test_router();
    let request = Request::builder().method("GET").uri("/dashboard/signup").body(Body::empty()).unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(content_type.contains("text/html"), "expected text/html, got {content_type}");
    let html = body_text(response).await;
    assert!(html.contains("<form"));
    assert!(html.contains("/dashboard/signup"));
}

#[tokio::test]
async fn get_dashboard_login_returns_html() {
    let router = test_router();
    let request = Request::builder().method("GET").uri("/dashboard/login").body(Body::empty()).unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(content_type.contains("text/html"), "expected text/html, got {content_type}");
    let html = body_text(response).await;
    assert!(html.contains("<form"));
    assert!(html.contains("/dashboard/login"));
}

#[tokio::test]
async fn posting_valid_form_encoded_signup_data_redirects_to_the_login_page() {
    let router = test_router();
    let response = router
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "form-signup@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND, "expected a 302 redirect");
    let location = response.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/dashboard/login");
}

#[tokio::test]
async fn posting_a_duplicate_email_to_dashboard_signup_rerenders_the_form_with_an_error() {
    let router = test_router();

    let first = router
        .clone()
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "dupe-form@example.com"), ("password", "first password here")],
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::FOUND);

    let second = router
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "dupe-form@example.com"), ("password", "a different password")],
        ))
        .await
        .unwrap();
    // Still a 200 re-render of the form, not a redirect and not a bare JSON 409.
    assert_eq!(second.status(), StatusCode::OK);
    let html = body_text(second).await;
    assert!(html.contains("already registered"), "expected a visible duplicate-email error, got: {html}");
    assert!(html.contains("<form"), "the signup form must still be present: {html}");
}

#[tokio::test]
async fn posting_valid_form_encoded_login_data_sets_a_session_cookie() {
    let router = test_router();

    let signup = router
        .clone()
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "form-login@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    let response = router
        .oneshot(form_request(
            "/dashboard/login",
            &[("email", "form-login@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    // A plain login with no `next` redirects straight to the real dashboard
    // home page (`http/home.rs`) now that one exists - see
    // `dashboard::login_submit`'s own doc comment.
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers().get("location").unwrap(), "/dashboard");
    let set_cookie = response.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(set_cookie.starts_with("session="), "expected a `session` cookie, got: {set_cookie}");
    assert!(set_cookie.to_lowercase().contains("httponly"), "expected HttpOnly, got: {set_cookie}");
    assert!(set_cookie.to_lowercase().contains("samesite=lax"), "expected SameSite=Lax, got: {set_cookie}");
}

#[tokio::test]
async fn the_session_cookie_from_dashboard_login_authenticates_against_a_protected_route() {
    let router = test_router();

    let signup = router
        .clone()
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "cookie-auth@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    let login = router
        .clone()
        .oneshot(form_request(
            "/dashboard/login",
            &[("email", "cookie-auth@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::FOUND);
    let set_cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    // Extract just `session=<value>` from the full `Set-Cookie` line (which
    // also carries `; HttpOnly; SameSite=Lax; Path=/`) - that's what a
    // browser would send back in a `Cookie` request header.
    let session_pair = set_cookie.split(';').next().unwrap().to_string();
    assert!(session_pair.starts_with("session="));

    // Prove the cookie path through `AuthedUser` is real, not just present
    // in the code: use it (as a `Cookie` header, no `Authorization` header
    // at all) against the existing bearer-only protected test route.
    let whoami_request = Request::builder()
        .method("GET")
        .uri("/_test/whoami")
        .header("cookie", session_pair)
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(whoami_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "the session cookie must authenticate via AuthedUser");
    let body = body_json(response).await;
    assert!(body.as_object().unwrap().get("user_id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn the_landing_page_shows_log_out_instead_of_log_in_once_a_session_cookie_is_presented() {
    let router = test_router();

    let no_session = router
        .clone()
        .oneshot(Request::builder().method("GET").uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let html = body_text(no_session).await;
    assert!(html.contains(r#"href="/dashboard/login""#), "expected a log-in link with no session, got: {html}");
    assert!(!html.contains("log out"), "expected no log-out link with no session, got: {html}");

    let signup = router
        .clone()
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "nav-auth-state@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);
    let login = router
        .clone()
        .oneshot(form_request(
            "/dashboard/login",
            &[("email", "nav-auth-state@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    let set_cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    let session_pair = set_cookie.split(';').next().unwrap().to_string();

    let with_session = router
        .oneshot(Request::builder().method("GET").uri("/").header("cookie", session_pair).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let html = body_text(with_session).await;
    // The landing page's own body has separate, always-shown marketing CTAs
    // ("Sign up - it's free...", "Log in", both capitalized, with a `btn`
    // class) that are deliberately unaffected by login state - only the nav
    // bar's own lowercase, unstyled "log in"/"sign up" links are checked
    // here, matched by their exact nav markup rather than a generic `href`
    // substring that would also match those body CTAs.
    assert!(html.contains(r#">log out<"#), "expected a log-out link once a real session cookie is presented, got: {html}");
    assert!(!html.contains(r#"href="/dashboard/login">log in<"#), "the nav's log-in link must be gone once logged in, got: {html}");
    assert!(!html.contains(r#"href="/dashboard/signup">sign up<"#), "the nav's sign-up link must be gone once logged in, got: {html}");
}

#[tokio::test]
async fn logging_out_via_the_dashboard_nav_form_clears_the_session_and_redirects_home() {
    let router = test_router();

    let signup = router
        .clone()
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "dashboard-logout@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);
    let login = router
        .clone()
        .oneshot(form_request(
            "/dashboard/login",
            &[("email", "dashboard-logout@example.com"), ("password", "correct horse battery staple")],
        ))
        .await
        .unwrap();
    let set_cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    let session_pair = set_cookie.split(';').next().unwrap().to_string();

    let logout = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/logout")
                .header("cookie", session_pair.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::FOUND, "expected a redirect after logging out");
    assert_eq!(logout.headers().get("location").unwrap(), "/");

    // The same, now-deleted session cookie must no longer authenticate.
    let whoami_after = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/_test/whoami")
                .header("cookie", session_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(whoami_after.status(), StatusCode::UNAUTHORIZED, "the logged-out session must no longer authenticate");
}

#[tokio::test]
async fn posting_a_wrong_password_to_dashboard_login_rerenders_the_form_with_a_generic_error() {
    let router = test_router();

    let signup = router
        .clone()
        .oneshot(form_request(
            "/dashboard/signup",
            &[("email", "wrong-pw-form@example.com"), ("password", "the real password")],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    let response = router
        .oneshot(form_request(
            "/dashboard/login",
            &[("email", "wrong-pw-form@example.com"), ("password", "not the real password")],
        ))
        .await
        .unwrap();
    // Still a 200 re-render, not a redirect and not a bare JSON 401.
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("set-cookie").is_none(), "no session cookie on a failed login");
    let html = body_text(response).await;
    assert!(html.contains("Invalid email or password"), "expected a generic login error, got: {html}");
}

#[tokio::test]
async fn an_unknown_email_at_dashboard_login_gets_the_same_generic_error_as_a_wrong_password() {
    let router = test_router();
    let response = router
        .oneshot(form_request(
            "/dashboard/login",
            &[("email", "nobody-has-this-account@example.com"), ("password", "whatever")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("Invalid email or password"), "expected the same generic login error, got: {html}");
}

// -- WBS 1.3.2: browser-facing wallet-connection form ------------------------
//
// Unlike the rest of this file, these tests need a *real* spawned engine
// (same reason as `connections.rs`'s own tests: `/dashboard/connect` really
// provisions a tenant) - so they get their own `AppState` helper instead of
// `test_app_state()`'s dummy, never-dialed engine URL.

/// Same fixed-scalar construction `connections.rs`'s and `engine_client.rs`'s
/// own tests use - see those modules for why these particular values pass
/// the engine's real wallet-material validation.
const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

async fn test_state_with_real_engine() -> (AppState, scanner_test_support::TestEngineHandle) {
    let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let engine_client = EngineClient::new(format!("http://{}", engine.addr));
    let state = AppState {
        db: { let db = Db::open_in_memory().unwrap(); db.seed_test_admin(); db.into_shared() },
        engine_client,
        encryption_key: [7u8; 32],
        status_cache: crate::http::status_page::new_status_cache(),
        exchange_rate: test_exchange_rate_provider(),
        rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
        event_streams: Default::default(),
        dns: std::sync::Arc::new(crate::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
    };
    (state, engine)
}

fn connect_get_request(cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri("/dashboard/connect");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::empty()).unwrap()
}

fn connect_post_request(cookie: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method("POST")
        .uri("/dashboard/connect")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", cookie)
        .body(Body::from(body))
        .unwrap()
}

/// Signs up and logs in a fresh user through the browser form flow, returning
/// the `session=<value>` pair (no `HttpOnly`/`SameSite`/`Path` attributes) a
/// browser would send back as a `Cookie` header - same extraction
/// `the_session_cookie_from_dashboard_login_authenticates_against_a_protected_route`
/// above uses.
async fn signed_up_and_logged_in_session_cookie(router: &Router, email: &str, password: &str) -> String {
    let signup = router
        .clone()
        .oneshot(form_request("/dashboard/signup", &[("email", email), ("password", password)]))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    let login = router
        .clone()
        .oneshot(form_request("/dashboard/login", &[("email", email), ("password", password)]))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::FOUND);
    let set_cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    set_cookie.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn get_dashboard_connect_without_a_session_is_rejected() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let response = router.oneshot(connect_get_request(None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_logged_in_user_submitting_valid_wallet_fields_gets_a_confirmation_page_and_a_real_store_connections_row() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());

    let cookie =
        signed_up_and_logged_in_session_cookie(&router, "connect-form@example.com", "correct horse battery staple")
            .await;

    let response = router
        .oneshot(connect_post_request(
            &cookie,
            &[
                ("site_url", "https://shop.example.com"),
                ("view_key_hex", TEST_VIEW_KEY_HEX),
                ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ("network", "mainnet"),
                ("allowed_origins", "https://shop.example.com, https://admin.example.com"),
                ("base_currency", "XMR"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;

    let public_key_start = html.find("pk_").expect("expected a real pk_ value in the confirmation page");
    let public_key: String =
        html[public_key_start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    assert!(public_key.len() > 3, "expected a real pk_... value, got: {public_key}");

    // The secret token must never be shown on the confirmation page.
    assert!(!html.contains("sk_"), "the confirmation page must never contain the secret token");

    // Confirm the row that actually landed in `store_connections`. The
    // browser form flow never hands the connection id back to the caller
    // (unlike the JSON API's response), so look it up by the public key
    // shown on the confirmation page instead - see
    // `Db::get_store_connection_by_public_key`'s doc comment for why that
    // lookup exists.
    let user = state
        .db
        .lock()
        .unwrap()
        .get_user_by_email("connect-form@example.com")
        .unwrap()
        .expect("the signed-up user should exist");
    let row = state
        .db
        .lock()
        .unwrap()
        .get_store_connection_by_public_key(&public_key)
        .unwrap()
        .expect("a store_connections row for this public key must exist");
    assert_eq!(row.user_id, user.id);
    // "custom", not "woocommerce" - this is the advanced/direct-API form,
    // never routed through a WooCommerce plugin. Real user-reported bug:
    // this was wrongly hardcoded to "woocommerce" for every advanced-form
    // connection.
    assert_eq!(row.platform, "custom");
    assert_eq!(row.site_url, "https://shop.example.com");
}

#[tokio::test]
async fn submitting_an_invalid_view_key_rerenders_the_form_with_a_visible_error() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let cookie =
        signed_up_and_logged_in_session_cookie(&router, "bad-view-key@example.com", "correct horse battery staple")
            .await;

    let response = router
        .oneshot(connect_post_request(
            &cookie,
            &[
                ("site_url", "https://shop.example.com"),
                // Wrong length - not valid hex for a 32-byte view key.
                ("view_key_hex", "0707"),
                ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ("network", "mainnet"),
                ("allowed_origins", ""),
                ("base_currency", "XMR"),
            ],
        ))
        .await
        .unwrap();

    // A visible, re-rendered form - not a raw 500 and not a panic.
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("<form"), "expected the connect form to be re-rendered, got: {html}");
    assert!(html.contains("class=\"error\""), "expected a visible error message, got: {html}");
    assert!(!html.contains("pk_"), "a rejected submission must not show a public key");
}

/// The real UX bug: losing every field on a rejected submission - a
/// merchant who mistyped one character of a 64-char hex key shouldn't have
/// to retype the whole form, including the *other*, perfectly valid key.
/// Drives the real endpoint end to end (not just the template layer, which
/// `templates.rs`'s own `connect_template_re_fills_...` test already
/// covers) with a genuinely invalid spend key, so this exercises the real
/// `connect_submit` -> `render_connect_form(..., Some(&form))` path.
#[tokio::test]
async fn a_rejected_connect_submission_re_fills_every_field_the_merchant_typed() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let cookie =
        signed_up_and_logged_in_session_cookie(&router, "keep-my-inputs@example.com", "correct horse battery staple")
            .await;

    let response = router
        .oneshot(connect_post_request(
            &cookie,
            &[
                ("site_url", "https://my-real-shop.example.com"),
                ("view_key_hex", TEST_VIEW_KEY_HEX),
                // 64 hex chars, well-formed, but not a real curve point -
                // real, verified invalid input (see admin.rs's own
                // regression tests at the repo root), not a length/hex
                // mistake this form's client-side pattern= would already
                // catch before ever reaching the server.
                ("spend_pubkey_hex", &"ff".repeat(32)),
                ("network", "stagenet"),
                ("allowed_origins", "https://my-real-shop.example.com, https://admin.example.com"),
                ("base_currency", "XMR"),
            ],
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("class=\"error\""), "expected a visible error, got: {html}");

    assert!(
        html.contains(r#"value="https://my-real-shop.example.com""#),
        "expected site_url re-filled, got: {html}"
    );
    assert!(html.contains(&format!(r#"value="{TEST_VIEW_KEY_HEX}""#)), "expected the valid view key kept, got: {html}");
    assert!(
        html.contains(&format!(r#"value="{}""#, "ff".repeat(32))),
        "expected the rejected spend key re-filled too, so the merchant can see and fix exactly it, got: {html}"
    );
    assert!(
        html.contains(r#"value="https://my-real-shop.example.com, https://admin.example.com""#),
        "expected allowed_origins re-filled, got: {html}"
    );
    assert!(html.contains(r#"value="stagenet" selected"#), "expected stagenet to stay selected, got: {html}");
    assert!(!html.contains(r#"value="mainnet" selected"#), "mainnet must not silently reappear as selected, got: {html}");
}

#[tokio::test]
async fn an_unknown_base_currency_on_the_dashboard_connect_form_is_rejected_before_provisioning_a_tenant() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let cookie = signed_up_and_logged_in_session_cookie(&router, "bad-currency-form@example.com", "correct horse battery staple").await;

    let response = router
        .oneshot(connect_post_request(
            &cookie,
            &[
                ("site_url", "https://shop.example.com"),
                ("view_key_hex", TEST_VIEW_KEY_HEX),
                ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ("network", "mainnet"),
                ("allowed_origins", ""),
                ("base_currency", "NOTREAL"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "a rejected submission re-renders the form, not a redirect");
    let html = body_text(response).await;
    assert!(html.contains("class=\"error\""), "expected a visible error, got: {html}");
    assert!(!html.contains("pk_"), "no tenant should have been provisioned for a rejected submission");
}

// -- WBS 1.4.1: `next`-redirect support on dashboard::login_submit ----------
//
// The pure open-redirect-rejection proof lives in `dashboard.rs`'s own
// `#[cfg(test)] mod tests` (`is_safe_redirect_path`'s direct unit tests) -
// these are the HTTP-level complement: a real login request carrying a
// malicious `next` must not redirect anywhere attacker-controlled, and a
// real login with no `next` at all must behave exactly as it always has
// (the existing tests above already cover that implicitly, since none of
// them ever send a `next` field and all still pass unchanged).

#[tokio::test]
async fn a_successful_login_with_a_malicious_next_falls_back_to_the_default_confirmation_not_a_redirect() {
    let router = test_router();

    let email = "malicious-next@example.com";
    let password = "correct horse battery staple";
    let signup = router.clone().oneshot(form_request("/dashboard/signup", &[("email", email), ("password", password)])).await.unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    for malicious_next in ["//evil.example.com", "https://evil.example.com", "/\\evil.example.com"] {
        let response = router
            .clone()
            .oneshot(form_request("/dashboard/login", &[("email", email), ("password", password), ("next", malicious_next)]))
            .await
            .unwrap();
        // A malicious `next` must be silently ignored, falling back to the
        // exact same `/dashboard` redirect a `next`-less login gets - never
        // the attacker-controlled value. The response *is* a redirect
        // either way now that a real dashboard exists (see
        // `dashboard::login_submit`'s doc comment) - the load-bearing check
        // is the `Location` value, not whether a redirect happened at all.
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get("location").unwrap(),
            "/dashboard",
            "a malicious next ({malicious_next}) must never appear in Location - only the safe default"
        );
    }
}

fn invite_token_signup_request(email: &str, password: &str, invite_token: Option<&str>) -> Request<Body> {
    let mut body = serde_json::json!({ "email": email, "password": password });
    if let Some(token) = invite_token {
        body["invite_token"] = serde_json::Value::String(token.to_string());
    }
    Request::builder().method("POST").uri("/signup").header("content-type", "application/json").body(Body::from(body.to_string())).unwrap()
}

/// `signup.mode` gating for the JSON `POST /signup` API - the browser-facing
/// `/dashboard/signup` form's own equivalent behavior (including the real
/// single-use guarantee) is covered end to end in `http::invites::tests`,
/// which drives it through a real `/request-invite` + admin "create invite
/// link" flow; these tests exercise the JSON surface specifically, since
/// `signup::create_account` is shared by both and must not drift between
/// them.
#[tokio::test]
async fn public_mode_signup_needs_no_invite_token_at_all() {
    // `test_app_state`'s own `seed_test_admin` already defaults to
    // `"public"` - this is the harness default every other test in this
    // file already relies on, asserted explicitly here as documentation.
    let router = test_router();
    let response = router.oneshot(invite_token_signup_request("nobody-needs-an-invite@example.com", "correct horse battery staple", None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn invite_only_mode_rejects_a_signup_with_no_token() {
    let state = test_app_state();
    let db = state.db.clone();
    let router = build_router(state);
    db.lock().unwrap().set_setting("signup.mode", "invite_only").unwrap();

    let response = router.oneshot(invite_token_signup_request("hopeful@example.com", "correct horse battery staple", None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(body["error"].as_str().unwrap().contains("invite"), "expected a clear invite-required error, got: {body}");
}

#[tokio::test]
async fn invite_only_mode_accepts_a_valid_token_exactly_once() {
    let state = test_app_state();
    let db = state.db.clone();
    let router = build_router(state);
    db.lock().unwrap().set_setting("signup.mode", "invite_only").unwrap();
    let raw_token = shared::auth::generate_invite_token();
    db.lock().unwrap().create_invite_link("link-1", &shared::auth::hash_secret_token(&raw_token), None, None, crate::now_unix()).unwrap();

    let first = router
        .clone()
        .oneshot(invite_token_signup_request("first@example.com", "correct horse battery staple", Some(&raw_token)))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);

    // The literal ask: "more than one account cannot be registered using
    // the same link".
    let second = router
        .oneshot(invite_token_signup_request("second@example.com", "correct horse battery staple", Some(&raw_token)))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    let body = body_json(second).await;
    assert!(body["error"].as_str().unwrap().contains("already been used"), "expected a clear already-used error, got: {body}");
}

/// An embed works on any site - clearnet or `.onion` - so the public
/// `/pay/...` routes answer CORS for any origin, preflight included, and
/// never allow credentials. The dashboard stays same-origin only.
#[tokio::test]
async fn public_embed_routes_allow_any_origin_and_the_dashboard_does_not() {
    let router = test_router();
    let onion = "http://shopexampleabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion";

    let preflight = router
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/pay/pk_test/orders")
                .header("origin", onion)
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .header("access-control-request-private-network", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(preflight.status().is_success(), "got {}", preflight.status());
    let headers = preflight.headers();
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert!(headers["access-control-allow-methods"].to_str().unwrap().contains("POST"));
    assert!(headers["access-control-allow-headers"].to_str().unwrap().contains("content-type"));
    assert_eq!(headers["access-control-allow-private-network"], "true");
    assert!(!headers.contains_key("access-control-allow-credentials"));

    // Error responses carry it too, so a caller can read why it failed.
    for uri in ["/pay/pk_test/orders/order_x/status", "/pay/pk_test/orders/order_x/events", "/static/monokulo-client.js"] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).header("origin", "https://shop.example").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap()), Some("*"), "{uri}");
    }

    let dashboard = router
        .oneshot(Request::builder().uri("/dashboard").header("origin", "https://shop.example").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(!dashboard.headers().contains_key("access-control-allow-origin"));
}
