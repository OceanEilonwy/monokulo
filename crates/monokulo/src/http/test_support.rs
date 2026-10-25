//! Helpers shared by the HTTP modules' tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The response body as JSON.
pub async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// The response body as text.
pub async fn body_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Checks `response` is the problem `slug`, as every machine client is
/// answered (`crate::api::runtime::problem`): `application/problem+json`,
/// its documented `type`, `title` and `status` (which the response's own
/// status matches) and a `detail`. Returns the body.
pub async fn assert_problem(response: axum::response::Response, slug: &str) -> serde_json::Value {
    use crate::api::runtime::problem::{Problem, PROBLEM_JSON};
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = body_text(response).await;
    assert_eq!(content_type, PROBLEM_JSON, "{status}: {body}");
    let body: serde_json::Value = serde_json::from_str(&body).expect("a problem is JSON");
    let problem = Problem::ALL
        .iter()
        .find(|p| p.slug() == slug)
        .unwrap_or_else(|| panic!("{slug} isn't a documented problem"));
    assert_eq!(body["type"], problem.type_uri(), "{body}");
    assert_eq!(body["title"], problem.title(), "{body}");
    assert_eq!(body["status"], status.as_u16(), "{body}");
    assert_eq!(status, problem.status(), "{body}");
    assert!(
        body["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "{body}"
    );
    assert!(body.get("error").is_none(), "{body}");
    body
}

/// `s` encoded for a form body or query string.
pub fn urlencoding_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Signs up a fresh user against `router` through the sign-up form (which
/// logs them in), returning their session token: the `session` cookie's
/// value. Send it as `cookie: session=<token>`.
pub async fn signed_up_and_logged_in_session_token(
    router: &Router,
    email: &str,
    password: &str,
) -> String {
    let signup = router
        .clone()
        .oneshot(form_post(
            "/signup",
            None,
            &[("email", email), ("password", password)],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND, "signing up {email}");
    let set_cookie = signup
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("session="))
        .expect("signing up logs in")
        .to_owned();
    set_cookie
        .split(';')
        .next()
        .and_then(|pair| pair.strip_prefix("session="))
        .unwrap()
        .to_owned()
}

/// The test wallet's keys: the fixed-scalar pair the engine's own tests
/// use, which pass its real wallet-material validation.
pub const TEST_VIEW_KEY_HEX: &str =
    "0707070707070707070707070707070707070707070707070707070707070707";
pub const TEST_SPEND_PUBKEY_HEX: &str =
    "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

/// A store made through store setup for a test.
pub struct TestStore {
    pub id: String,
    pub public_key: String,
    /// Its secret key, as the store's plugin is given it.
    pub secret_key: String,
}

/// Makes a store for the account behind `session_token` on the test
/// wallet, as `/setup` does (the account's wallet is reused for its second
/// store): `site` may be empty, `base_currency` is its code.
pub async fn store_with_keys(
    router: &Router,
    state: &crate::http::AppState,
    session_token: &str,
    site: &str,
    base_currency: &str,
) -> TestStore {
    let cookie = format!("session={session_token}");
    let user = state
        .db
        .lock()
        .find_session(
            &credentials::auth::RawToken::presented(session_token).hash(),
            crate::now_unix(),
        )
        .unwrap()
        .expect("a live session")
        .user_id;
    let wallet = state
        .db
        .lock()
        .list_wallets(&user)
        .unwrap()
        .into_iter()
        .next();
    let name = if site.is_empty() { "Till" } else { site };
    let made = match wallet {
        Some(wallet) => router
            .clone()
            .oneshot(form_post(
                "/setup/wallet/existing",
                Some(&cookie),
                &[
                    ("store_name", name),
                    ("store_site", site),
                    ("base_currency", base_currency),
                    ("wallet_id", wallet.wallet.id.as_str()),
                ],
            ))
            .await
            .unwrap(),
        None => {
            set_up_store_with_keys(
                router,
                &cookie,
                &[
                    ("store_name", name),
                    ("store_site", site),
                    ("base_currency", base_currency),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("name", "Shop wallet"),
                ],
            )
            .await
        }
    };
    let id = store_made(&made);
    let row = state
        .db
        .lock()
        .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(id.clone()))
        .unwrap()
        .unwrap();
    let secret_key = crate::crypto::decrypt(
        &state.encryption_key,
        crate::crypto::Binding::StoreSecret(row.id.as_str()),
        &row.tenant_secret_token_encrypted,
    )
    .unwrap();
    TestStore {
        id,
        public_key: row.tenant_public_key,
        secret_key,
    }
}

/// A JSON request to the API, with `key` as its bearer token.
pub fn api_request(
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// Logs in as the harness-seeded admin account (`Db::seed_test_admin`) and
/// returns its session cookie's `name=value` pair, ready to attach as a
/// `cookie` header.
pub async fn admin_session_cookie(router: &Router) -> String {
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
                .uri("/login")
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

/// A form post, as a browser sends one, with `cookie` (`session=...`).
pub fn form_post(uri: &str, cookie: Option<&str>, fields: &[(&str, &str)]) -> Request<Body> {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::from(body)).unwrap()
}

/// Sets up a store through `/setup` as a browser without JavaScript does:
/// the store step, then a wallet brought in with its keys, which makes the
/// store. `fields` are the store step's and the keys form's (`kind`,
/// `store_name`, `store_site`, `view_key_hex`, ...). The keys form's
/// response: a `303` to `/setup/done/{id}` once the store is made; the
/// store step's response when it refused the answers.
pub async fn set_up_store_with_keys(
    router: &Router,
    cookie: &str,
    fields: &[(&str, &str)],
) -> axum::response::Response {
    let store = router
        .clone()
        .oneshot(form_post("/setup", Some(cookie), fields))
        .await
        .unwrap();
    if store.status() != StatusCode::SEE_OTHER {
        return store;
    }
    router
        .clone()
        .oneshot(form_post("/setup/wallet/keys", Some(cookie), fields))
        .await
        .unwrap()
}

/// Sets up a store (with no site when `site` is empty)
/// on one of the account's wallets: the response to
/// `POST /setup/wallet/existing`.
pub async fn set_up_store_on_wallet(
    router: &Router,
    cookie: &str,
    name: &str,
    site: &str,
    wallet_id: &str,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(form_post(
            "/setup/wallet/existing",
            Some(cookie),
            &[
                ("store_name", name),
                ("store_site", site),
                ("wallet_id", wallet_id),
            ],
        ))
        .await
        .unwrap()
}

/// The store a `/setup` response made: the id in its `/setup/done/{id}`
/// location (under the path Monokulo is served at, if any).
pub fn store_made(response: &axum::response::Response) -> String {
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "expected the store to be made"
    );
    let location = response.headers()["location"].to_str().unwrap();
    location
        .split_once("/setup/done/")
        .map(|(_, id)| id)
        .unwrap_or_else(|| panic!("not the Done page: {location}"))
        .split('?')
        .next()
        .unwrap()
        .to_owned()
}
