//! HTTP-layer integration tests for `/signup`, driven through the real
//! `Router` via `tower::ServiceExt::oneshot` — no bound socket needed, same
//! pattern as `moneropay_core::http::tests`.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::db::Db;

use super::{AppState, build_router};

fn test_app_state() -> AppState {
    AppState { db: Db::open_in_memory().unwrap().into_shared() }
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
