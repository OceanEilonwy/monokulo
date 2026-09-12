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
