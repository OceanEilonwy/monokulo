//! The generic, platform-agnostic connect flow (WBS 1.4.1,
//! `docs/WOOCOMMERCE_ROADMAP.md` Stage 6) - "OAuth-style one-click install"
//! for any platform's plugin, written once and shared by every platform
//! (WooCommerce first, per the WBS; nothing here is WooCommerce-specific -
//! `platform` is just a path parameter threaded straight through into
//! `store_connections.platform`, exactly like `connections::create_connection_for_user`
//! already treats it for the JSON/dashboard-form surfaces).
//!
//! The five steps (see the roadmap doc for the full narrative):
//! 1. `GET /connect/{platform}?site_url=...&return_url=...&nonce=...` - the
//!    plugin sends the merchant's browser here.
//! 2. No session yet: redirect to `/dashboard/login?next=<this same URL>` -
//!    `next` is validated as a safe, same-origin relative path before ever
//!    being used as a redirect target (`dashboard::is_safe_redirect_path`);
//!    a real session logging in there redirects back here automatically
//!    (`dashboard::login_submit`).
//! 3. A valid session: render a confirm screen with the same
//!    wallet-connection fields `/dashboard/connect` has, as a
//!    `POST /connect/{platform}` form (same path, not a `/confirm`
//!    sub-path) - `return_url`/`nonce` ride along as hidden fields.
//! 4. `POST /connect/{platform}` (behind [`AuthedUser`]): calls the exact
//!    same [`connections::create_connection_for_user`] every other surface
//!    uses, mints a single-use connect token, and redirects to `return_url`
//!    with `token`/`nonce` appended as query parameters (via the `url`
//!    crate, so an existing query string on `return_url` is preserved
//!    correctly rather than string-concatenated).
//! 5. `POST /connect/{platform}/finish` - deliberately *not* behind
//!    [`AuthedUser`]: the plugin calls this server-to-server, with no
//!    control-plane session at all. Redeems the token exactly once (see
//!    [`crate::db::Db::consume_connect_token`]'s atomicity doc comment) and
//!    returns `{public_key, secret_token, endpoint}` plus, as of WBS 1.4.4,
//!    a `webhook_signing_secret` when the request carried a `webhook_url` -
//!    see [`finish`]'s own doc comment for the registration/failure policy.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::crypto;
use crate::now_unix;
use crate::templates::PlatformConnectViewModel;

use super::connections::{self, CreateConnectionError, CreateConnectionFields};
use super::dashboard::redirect_302;
use super::{AppState, AuthedUser};

/// How long a connect token remains redeemable after issuance. It only
/// needs to survive the redirect round trip out to the plugin's
/// `return_url` and the plugin's own immediate server-to-server `/finish`
/// call right after - a few minutes comfortably covers real network
/// latency/retries without leaving a redeemable (even if single-use) token
/// valid for long if it somehow ends up somewhere it shouldn't (a proxy
/// log, browser history) before being redeemed.
const CONNECT_TOKEN_TTL_SECONDS: i64 = 10 * 60;

#[derive(Deserialize)]
pub struct ConnectQuery {
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
}

fn render_confirm_form(
    state: &AppState,
    platform: &str,
    site_url: &str,
    return_url: &str,
    nonce: &str,
    error: Option<&str>,
) -> Response {
    let html = state
        .templates
        .render_platform_connect(&PlatformConnectViewModel {
            platform: platform.to_string(),
            site_url: site_url.to_string(),
            return_url: return_url.to_string(),
            nonce: nonce.to_string(),
            error: error.map(str::to_string),
        })
        .expect("the built-in platform-connect template must always render");
    axum::response::Html(html).into_response()
}

/// Percent-encodes `s` for safe embedding as one query-string value - the
/// same encoding a browser's own `application/x-www-form-urlencoded`
/// submission uses. Used here to build the `next` URL handed to
/// `/dashboard/login`, not to build `return_url`'s own query string (that
/// one goes through the `url` crate's `query_pairs_mut`, which encodes
/// correctly on its own).
fn encode_query_value(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// `GET /connect/{platform}` (WBS 1.4.1, step 1-3). No valid session:
/// redirect to `/dashboard/login` carrying a `next` that reconstructs this
/// exact URL (see `dashboard::login_submit`/`is_safe_redirect_path` - this
/// value is always a same-origin relative path built entirely from this
/// route's own known shape, so it always passes that validation on the way
/// back). A valid session: render the confirm form.
pub async fn start(
    State(state): State<AppState>,
    Path(platform): Path<String>,
    Query(query): Query<ConnectQuery>,
    headers: HeaderMap,
) -> Response {
    if super::resolve_authed_user(&state, &headers).is_none() {
        let this_url = format!(
            "/connect/{}?site_url={}&return_url={}&nonce={}",
            platform,
            encode_query_value(&query.site_url),
            encode_query_value(&query.return_url),
            encode_query_value(&query.nonce),
        );
        return redirect_302(&format!("/dashboard/login?next={}", encode_query_value(&this_url)));
    }

    render_confirm_form(&state, &platform, &query.site_url, &query.return_url, &query.nonce, None)
}

/// `POST /connect/{platform}`'s form fields (WBS 1.4.1, step 4) - the same
/// wallet-connection fields `dashboard::ConnectForm` has, plus `site_url`
/// (shown, not editable, on the confirm screen) and the `return_url`/`nonce`
/// hidden fields carried through from `GET /connect/{platform}`'s query
/// string so they survive the round trip.
#[derive(Deserialize)]
pub struct ConfirmForm {
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: String,
    pub allowed_origins: String,
    /// Not shown on the confirm screen (no UI field for it yet) — carried purely so
    /// a caller who needs a non-default tenant `order_expiry_seconds` (e.g. WBS
    /// 1.4.4's forced-expiry test) has a real way to set it through this flow rather
    /// than only via the JSON `POST /connections` surface. `#[serde(default)]` keeps
    /// every existing form submission (none of which send this field) parsing
    /// exactly as before, defaulting to the engine's own default.
    #[serde(default)]
    pub order_expiry_seconds: Option<i64>,
}

/// `POST /connect/{platform}` (behind [`AuthedUser`], WBS 1.4.1 step 4): the
/// confirm-form submission. Provisions the tenant via the exact same
/// [`connections::create_connection_for_user`] every other surface uses; on
/// success, mints a single-use connect token and redirects to `return_url`
/// with `token`/`nonce` appended (parsed and re-serialized via the `url`
/// crate, so a `return_url` that already carries its own query string is
/// handled correctly - never a naive string-concatenated `?`). On an engine
/// rejection or internal error, re-renders the confirm form with a visible
/// error - same pattern as `dashboard::connect_submit`.
pub async fn confirm_submit(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
    Path(platform): Path<String>,
    Form(form): Form<ConfirmForm>,
) -> Response {
    // Same comma-separated-list split every other wallet-connection form in
    // this crate uses (`dashboard::connect_submit`'s own comment explains
    // the reasoning).
    let allowed_origins: Vec<String> =
        form.allowed_origins.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();

    let fields = CreateConnectionFields {
        platform: platform.clone(),
        site_url: form.site_url.clone(),
        view_key_hex: form.view_key_hex,
        spend_pubkey_hex: form.spend_pubkey_hex,
        network: Some(form.network),
        allowed_origins,
        confirmations_required: None,
        zero_conf_max_piconero: None,
        order_expiry_seconds: form.order_expiry_seconds,
    };

    let outcome = match connections::create_connection_for_user(&state, &user, fields).await {
        Ok(outcome) => outcome,
        Err(CreateConnectionError::BadRequest(message)) => {
            return render_confirm_form(&state, &platform, &form.site_url, &form.return_url, &form.nonce, Some(&message));
        }
        Err(CreateConnectionError::Internal) => {
            return render_confirm_form(
                &state,
                &platform,
                &form.site_url,
                &form.return_url,
                &form.nonce,
                Some("Something went wrong. Please try again."),
            );
        }
    };

    let raw_token = shared::auth::generate_connect_token();
    let token_hash = shared::auth::hash_secret_token(&raw_token);
    let stored = state.db.lock().unwrap().create_connect_token(&token_hash, &outcome.connection_id, &form.nonce, now_unix());
    if stored.is_err() {
        return render_confirm_form(
            &state,
            &platform,
            &form.site_url,
            &form.return_url,
            &form.nonce,
            Some("Something went wrong. Please try again."),
        );
    }

    let mut redirect_url = match Url::parse(&form.return_url) {
        Ok(url) => url,
        Err(_) => {
            return render_confirm_form(
                &state,
                &platform,
                &form.site_url,
                &form.return_url,
                &form.nonce,
                Some("Invalid return_url."),
            );
        }
    };
    // `query_pairs_mut` appends to whatever query string `return_url`
    // already has (parsing it properly first, per the `url` crate's own
    // model) rather than string-concatenating a `?`/`&`, which would
    // produce a broken URL for a `return_url` that already has its own
    // query parameters.
    redirect_url.query_pairs_mut().append_pair("token", &raw_token).append_pair("nonce", &form.nonce);

    redirect_302(redirect_url.as_str())
}

#[derive(Deserialize)]
pub struct FinishRequest {
    pub token: String,
    /// The plugin's own webhook receiver URL (WBS 1.4.4) — it can only be known once
    /// the plugin holds real credentials, hence this arriving here rather than at
    /// the earlier confirm step. `Option` for backward compatibility: a caller that
    /// omits it (or an older plugin build) simply gets no webhook registered, and
    /// `FinishResponse::webhook_signing_secret` is absent from the response, exactly
    /// as it always has been for every caller before this field existed.
    #[serde(default)]
    pub webhook_url: Option<String>,
}

#[derive(Serialize)]
pub struct FinishResponse {
    pub public_key: String,
    pub secret_token: String,
    pub endpoint: String,
    /// Present only when `webhook_url` was supplied and registration succeeded.
    /// `skip_serializing_if` keeps the wire shape for a caller with no webhook
    /// exactly what it always was — a bare `{public_key, secret_token, endpoint}` —
    /// rather than growing a permanent `null` field for every existing caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_signing_secret: Option<String>,
}

/// `POST /connect/{platform}/finish` (WBS 1.4.1 step 5, extended by 1.4.4) -
/// deliberately *not* behind [`AuthedUser`]: this is a server-to-server call
/// from the plugin, which has no control-plane session at all (it's the
/// browser, not the plugin's backend, that ever holds one). Redeems the
/// connect token exactly once (see [`crate::db::Db::consume_connect_token`])
/// and returns the real credentials - plus, when `webhook_url` was supplied,
/// registers a real webhook against the engine (via
/// [`crate::engine_client::EngineClient::create_webhook`]) and returns its
/// `signing_secret`.
///
/// Every failure mode - unknown token, already-consumed token, expired
/// token, a connection/decrypt failure that should never actually happen for
/// a row this service itself wrote, or (new in 1.4.4) a failed webhook
/// registration - collapses to a bare `401`. That's deliberate for the first
/// three, same enumeration-defense principle used everywhere else in this
/// crate; for webhook registration specifically it is a considered policy
/// choice, not just "reuse the existing pattern" - see the doc comment
/// immediately above the webhook-registration branch below for the tradeoff
/// this accepts and why.
pub async fn finish(State(state): State<AppState>, Json(req): Json<FinishRequest>) -> Response {
    let token_hash = shared::auth::hash_secret_token(&req.token);

    let connection_id = {
        let db = state.db.lock().unwrap();
        match db.consume_connect_token(&token_hash, now_unix(), CONNECT_TOKEN_TTL_SECONDS) {
            Ok(Some(id)) => id,
            Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
            Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
        }
    };

    let row = {
        let db = state.db.lock().unwrap();
        match db.get_store_connection_by_id(&connection_id) {
            Ok(Some(row)) => row,
            // The token pointed at a connection that no longer exists -
            // shouldn't happen (nothing deletes `store_connections` rows),
            // but this is this service's own problem, not a credential the
            // caller could have gotten right some other way.
            Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
            Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
        }
    };

    let secret_token = match crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted) {
        Ok(v) => v,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    // Webhook registration (WBS 1.4.4): only attempted when the caller supplied a
    // `webhook_url`. On failure this collapses the *entire* `/finish` call to `401`,
    // exactly like every other failure mode above - deliberately, per this task's own
    // spec, even though the token has by this point already been irreversibly
    // consumed (see `consume_connect_token`'s atomicity doc comment) and a
    // registration failure here is `EngineClientError`-shaped, not enumeration-shaped
    // (unlike the branches above, this one has nothing to hide from a legitimate
    // caller - a network blip or a rejected URL isn't a secret). The accepted
    // tradeoff: a plugin whose *webhook* registration fails (a transient network
    // issue between the control plane and the engine, say, with credential retrieval
    // itself having fully succeeded) gets no credentials at all and cannot retry with
    // the same token - it must restart the whole connect flow from `GET
    // /connect/{platform}` to mint a fresh one. This was judged the safer default
    // over the alternative (return credentials anyway, with no webhook and no way to
    // signal that clearly in a shape existing callers already parse) rather than
    // because the collapse-to-401 pattern was merely convenient to reuse - flagged
    // here explicitly in case a real deployment prefers "credentials now, webhook
    // registration retried separately" instead.
    let webhook_signing_secret = match &req.webhook_url {
        Some(url) => match state.engine_client.create_webhook(&secret_token, url).await {
            Ok((_webhook_id, signing_secret)) => Some(signing_secret),
            Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
        },
        None => None,
    };

    Json(FinishResponse {
        public_key: row.tenant_public_key,
        secret_token,
        endpoint: state.engine_client.base_url().to_string(),
        webhook_signing_secret,
    })
    .into_response()
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

    use super::super::{AppState, build_router};

    /// Same fixed-scalar construction `connections.rs`'s and
    /// `engine_client.rs`'s own tests use.
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            db: Db::open_in_memory().unwrap().into_shared(),
            engine_client,
            encryption_key: TEST_ENCRYPTION_KEY,
            templates: std::sync::Arc::new(crate::templates::TemplateEngine::new().unwrap()),
        };
        (state, engine)
    }

    fn form_body(fields: &[(&str, &str)]) -> String {
        fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&")
    }

    /// Minimal `application/x-www-form-urlencoded` percent-encoding for test
    /// fixtures only - same helper `http/tests.rs` already defines for its
    /// own form-based tests, duplicated here rather than made `pub(crate)`
    /// purely for a test helper (real clients do this themselves).
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

    fn form_request(uri: &str, cookie: Option<&str>, fields: &[(&str, &str)]) -> Request<Body> {
        let mut builder =
            Request::builder().method("POST").uri(uri).header("content-type", "application/x-www-form-urlencoded");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::from(form_body(fields))).unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Signs up and logs in a fresh user through the browser form flow,
    /// returning the `session=<value>` pair a browser would send back as a
    /// `Cookie` header - same helper `http/tests.rs` uses for its own
    /// WBS 1.3.2 tests.
    async fn signed_up_and_logged_in_session_cookie(router: &Router, email: &str, password: &str) -> String {
        let signup =
            router.clone().oneshot(form_request("/dashboard/signup", None, &[("email", email), ("password", password)])).await.unwrap();
        assert_eq!(signup.status(), StatusCode::FOUND);

        let login =
            router.clone().oneshot(form_request("/dashboard/login", None, &[("email", email), ("password", password)])).await.unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let set_cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
        set_cookie.split(';').next().unwrap().to_string()
    }

    fn parse_query_params(url: &str) -> std::collections::HashMap<String, String> {
        let parsed = url::Url::parse(url).unwrap();
        parsed.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[tokio::test]
    async fn full_round_trip_confirm_then_redirect_then_finish_yields_real_working_credentials() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "connect-flow@example.com", "correct horse battery staple")
                .await;

        // Step 1-3: GET the connect start URL with a valid session - expect
        // the confirm form, not a login redirect.
        let get_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fsettings%3Fpage%3Dmonero&nonce=nonce-xyz")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_response.status(), StatusCode::OK, "expected the confirm form, not a redirect");
        let html = body_text(get_response).await;
        assert!(html.contains("shop.example.com"), "expected the site_url shown on the confirm page, got: {html}");
        assert!(html.contains(r#"action="/connect/woocommerce""#), "expected the confirm form to post back to /connect/woocommerce, got: {html}");

        // Step 4: POST the confirm form with valid wallet fields.
        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings?page=monero"),
                    ("nonce", "nonce-xyz"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND, "expected a 302 redirect to return_url");
        let location = post_response.headers().get("location").unwrap().to_str().unwrap().to_string();

        // The existing `page=monero` query param must survive alongside the
        // newly appended ones - proof the redirect is built by parsing and
        // re-serializing `return_url`, not by naively concatenating a `?`.
        let params = parse_query_params(&location);
        assert_eq!(params.get("page").map(String::as_str), Some("monero"));
        let token = params.get("token").expect("expected a token query param").clone();
        assert!(!token.is_empty());
        assert_eq!(params.get("nonce").map(String::as_str), Some("nonce-xyz"), "the nonce must round-trip unchanged");

        // Step 5: POST the token to /finish - server-to-server, no session
        // at all.
        let finish_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "token": token }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_response.status(), StatusCode::OK, "expected the first finish call to succeed");
        let finish_body = body_json(finish_response).await;
        let obj = finish_body.as_object().unwrap();
        let public_key = obj.get("public_key").unwrap().as_str().unwrap();
        assert!(public_key.starts_with("pk_"));
        let secret_token = obj.get("secret_token").unwrap().as_str().unwrap();
        assert!(secret_token.starts_with("sk_"));
        let endpoint = obj.get("endpoint").unwrap().as_str().unwrap();
        assert!(!endpoint.is_empty());
        // No webhook signing secret yet (WBS 1.4.4, not this task).
        assert!(!obj.contains_key("webhook_secret"));
        assert!(!obj.contains_key("signing_secret"));

        // Strong proof: `secret_token` is the tenant's real, working `sk_`
        // credential, not just a string that happens to start with `sk_` -
        // same pattern 1.2.3/1.3.2 already established.
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let tenant_view = engine_client
            .get_tenant(secret_token)
            .await
            .expect("the returned secret_token should be the tenant's genuine, functioning sk_ credential");
        assert_eq!(tenant_view.public_key, public_key);
    }

    #[tokio::test]
    async fn finishing_the_same_token_twice_only_succeeds_once() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "connect-single-use@example.com",
            "correct horse battery staple",
        )
        .await;

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-single-use"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let location = post_response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let token = parse_query_params(&location).get("token").unwrap().clone();

        let finish_request = || {
            Request::builder()
                .method("POST")
                .uri("/connect/woocommerce/finish")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({ "token": token }).to_string()))
                .unwrap()
        };

        // Prove the first call actually succeeds before asserting the
        // second fails - otherwise a second-call 401 could just mean the
        // token never worked at all.
        let first = router.clone().oneshot(finish_request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK, "the first finish call must actually succeed");

        let second = router.oneshot(finish_request()).await.unwrap();
        assert_eq!(second.status(), StatusCode::UNAUTHORIZED, "reusing an already-consumed token must fail");
    }

    #[tokio::test]
    async fn finish_with_a_garbage_token_returns_unauthorized() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "token": "conn_nobody_ever_issued_this" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// WBS 1.4.4: a `webhook_url` supplied to `/finish` genuinely registers a
    /// webhook against the real engine - not just a plausibly-shaped response.
    /// Also exercises this task's other addition, `ConfirmForm::order_expiry_seconds`
    /// (threaded all the way to `CreateTenantRequest`), in the same round trip: the
    /// created tenant's `order_expiry_seconds` is fetched back via `get_tenant` and
    /// must match exactly what the confirm form sent, proving the field genuinely
    /// reaches the engine rather than being silently dropped.
    #[tokio::test]
    async fn finish_with_a_webhook_url_registers_a_real_webhook_and_carries_the_signing_secret() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(&router, "webhook-register@example.com", "correct horse battery staple").await;

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-webhook"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                    ("order_expiry_seconds", "1"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let location = post_response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let token = parse_query_params(&location).get("token").unwrap().clone();

        let finish_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": token, "webhook_url": "https://merchant.example/hook" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_response.status(), StatusCode::OK);
        let body = body_json(finish_response).await;
        let obj = body.as_object().unwrap();
        let secret_token = obj.get("secret_token").unwrap().as_str().unwrap().to_string();
        let signing_secret =
            obj.get("webhook_signing_secret").expect("expected webhook_signing_secret in the response").as_str().unwrap();
        assert!(!signing_secret.is_empty());

        // Strong proof, not just a well-shaped response: the webhook genuinely
        // exists on the real engine, under this tenant, with the exact URL
        // submitted - and the tenant's order_expiry_seconds genuinely reached the
        // engine too.
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let webhooks = engine_client.list_webhooks(&secret_token).await.expect("list_webhooks against the real engine should succeed");
        assert_eq!(webhooks.len(), 1);
        assert_eq!(webhooks[0].url, "https://merchant.example/hook");

        let tenant_view = engine_client.get_tenant(&secret_token).await.expect("get_tenant against the real engine should succeed");
        assert_eq!(tenant_view.order_expiry_seconds, 1, "order_expiry_seconds must have reached the engine's real tenant record");
    }

    /// A `webhook_url` the engine rejects (WBS 1.4.4's collapse-to-401 policy, see
    /// `finish`'s own doc comment) fails the *entire* `/finish` call, not just the
    /// webhook part - the caller never sees `public_key`/`secret_token` at all, and
    /// (since the token was already consumed by this point) can't simply retry the
    /// same token once it supplies a valid URL.
    #[tokio::test]
    async fn finish_with_a_rejected_webhook_url_fails_the_whole_call() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "webhook-reject@example.com", "correct horse battery staple").await;

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-webhook-reject"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let location = post_response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let token = parse_query_params(&location).get("token").unwrap().clone();

        // `ftp://` is neither `http` nor `https` - the engine's own
        // `create_webhook` rejects it with a real `400`, which `EngineClient`
        // surfaces as an `Err`, which this handler collapses to `401`.
        let finish_response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "token": token, "webhook_url": "ftp://not-http.example/hook" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_unauthenticated_connect_start_redirects_through_login_and_back_to_the_original_url() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let original_uri = "/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fsettings&nonce=detour-nonce";

        let get_response = router
            .clone()
            .oneshot(Request::builder().method("GET").uri(original_uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(get_response.status(), StatusCode::FOUND, "expected a redirect to the login page");
        let login_location = get_response.headers().get("location").unwrap().to_str().unwrap().to_string();
        assert!(login_location.starts_with("/dashboard/login?next="), "expected a next-carrying login redirect, got: {login_location}");

        // Extract the (still percent-encoded) `next` value exactly as a
        // browser would receive it in the `Location` header, then sign up
        // and log in, submitting that same value back as the login form's
        // hidden `next` field - the real end-to-end detour, not just a call
        // to the validator function in isolation.
        let next_value = login_location.strip_prefix("/dashboard/login?next=").unwrap();
        let decoded_next = url::form_urlencoded::parse(format!("x={next_value}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(decoded_next, original_uri, "the next value must reconstruct the exact original connect URL");

        let email = "connect-detour@example.com";
        let password = "correct horse battery staple";
        let signup = router.clone().oneshot(form_request("/dashboard/signup", None, &[("email", email), ("password", password)])).await.unwrap();
        assert_eq!(signup.status(), StatusCode::FOUND);

        let login_response = router
            .clone()
            .oneshot(form_request("/dashboard/login", None, &[("email", email), ("password", password), ("next", &decoded_next)]))
            .await
            .unwrap();
        assert_eq!(login_response.status(), StatusCode::FOUND, "a successful login with a valid next must redirect");
        let final_location = login_response.headers().get("location").unwrap().to_str().unwrap().to_string();
        assert_eq!(final_location, original_uri, "must land back on the exact original connect URL, query params intact");
    }
}
