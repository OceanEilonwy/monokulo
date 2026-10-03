//! Helpers shared by the HTTP modules' tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The response body as JSON.
pub(crate) async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// The response body as text.
pub(crate) async fn body_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// `s` encoded for a form body or query string.
pub(crate) fn urlencoding_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Signs up and logs in a fresh user against `router` through the JSON
/// API, returning their session (bearer) token.
pub(crate) async fn signed_up_and_logged_in_session_token(
    router: &Router,
    email: &str,
    password: &str,
) -> String {
    let request = |uri: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "email": email, "password": password }).to_string(),
            ))
            .unwrap()
    };
    let signup = router.clone().oneshot(request("/signup")).await.unwrap();
    assert_eq!(signup.status(), StatusCode::CREATED);
    let login = router.clone().oneshot(request("/login")).await.unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    body_json(login).await["session_token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Logs in as the harness-seeded admin account (`Db::seed_test_admin`) and
/// returns its session cookie's `name=value` pair, ready to attach as a
/// `cookie` header.
pub(crate) async fn admin_session_cookie(router: &Router) -> String {
    let body = format!(
        "email={}&password={}",
        urlencoding_encode(crate::db::TEST_ADMIN_EMAIL),
        urlencoding_encode(crate::db::TEST_ADMIN_PASSWORD)
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .expect("expected a session cookie from a correct admin login")
        .to_str()
        .unwrap();
    set_cookie.split(';').next().unwrap().to_string()
}
