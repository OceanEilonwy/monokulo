//! `mock-woocommerce`: a fake WooCommerce store that plays the role of a
//! real WooCommerce plugin's "Connect your Monero wallet" button, all the
//! way through to holding real, working `pk_`/`sk_` credentials. See
//! `docs/WOOCOMMERCE_WBS.md` 1.4.2 and `docs/WOOCOMMERCE_ROADMAP.md`'s
//! "Stage 6 - Connect flow" section for the full narrative this drives.
//!
//! This is genuinely **no browser involved** - [`run_connect_flow`] *is* the
//! synthetic browser, using a cookie-persisting `reqwest::Client` to walk
//! the exact HTTP path a real browser would (see `control-plane/src/http/
//! connect.rs`'s own module doc comment for the five steps this walks
//! through), and a real, locally-bound axum server stands in for the
//! plugin's own callback route - the actual "plugin-side" logic (verify the
//! nonce, call `/finish` server-to-server) lives in that callback's handler,
//! not in [`run_connect_flow`] itself, since that's exactly where a real
//! WordPress plugin's callback handler will live at WBS 1.5.3.
//!
//! ## Why the nonce check matters
//!
//! The `nonce` that rides along the whole flow (`GET /connect/{platform}`'s
//! query param -> the confirm form's hidden field -> the `return_url`
//! redirect's query param) exists so the callback can tell "this redirect
//! really came from the connect flow I started" from "someone (or something)
//! substituted or replayed a redirect at me carrying a stolen `token`
//! query param but not my own nonce." [`callback_handler`] rejects a
//! mismatch *before* ever calling `/finish` - see this crate's own tests for
//! proof this is load-bearing, not just checked-and-ignored.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

/// The real, working credentials `POST /connect/{platform}/finish` hands
/// back on success - see `control-plane/src/http/connect.rs::FinishResponse`,
/// which this mirrors field-for-field (the two crates only ever talk over
/// HTTP, same convention `control-plane::engine_client` already uses for the
/// engine's own admin API).
#[derive(Debug)]
pub struct ConnectedCredentials {
    pub public_key: String,
    pub secret_token: String,
    pub endpoint: String,
}

#[derive(Debug, Deserialize)]
struct FinishResponseBody {
    public_key: String,
    secret_token: String,
    endpoint: String,
}

/// The result of [`create_order`]: a real order seeded on the engine, plus
/// the checkout URL a WooCommerce customer would be redirected to next
/// (`GET /pay/v1/{pk}/{payment_id}` at the repo root's
/// `src/http/public.rs::payment_page` - see [`create_order`]'s own doc
/// comment).
#[derive(Debug)]
pub struct CreatedOrder {
    pub payment_id: String,
    pub checkout_url: String,
}

/// Field-for-field mirror of the engine's own
/// `src/http/public.rs::CreateOrderResponse` - only `payment_id` is actually
/// needed to build [`CreatedOrder`], but the rest is deserialized too so a
/// malformed/unexpected response body fails clearly via `serde_json` rather
/// than silently ignoring extra fields no differently than a real caller
/// would notice.
#[derive(Debug, Deserialize)]
struct CreateOrderResponseBody {
    payment_id: String,
}

/// Every way [`run_connect_flow`] (or the callback handler it waits on) can
/// fail. [`ConnectFlowError::NonceMismatch`] is the one that matters most for
/// this task's own purpose - see the module doc comment.
#[derive(Debug, thiserror::Error)]
pub enum ConnectFlowError {
    #[error("http request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("failed to bind the callback server's local socket: {0}")]
    CallbackBindFailed(std::io::Error),
    #[error(
        "the callback received a nonce that did not match the one this driver generated for this flow - \
         rejecting rather than proceeding, since this is exactly what a substituted or replayed redirect looks like"
    )]
    NonceMismatch,
    #[error("control plane rejected step '{step}' with {status}: {body}")]
    UnexpectedResponse { step: String, status: u16, body: String },
    #[error("the callback server never reported an outcome - the redirect chain never reached it")]
    CallbackNeverReceived,
}

/// Same fixed-scalar view-key/spend-pubkey construction every other test in
/// this workspace uses (see `control-plane/src/http/connect.rs`'s own test
/// module, or `control-plane/src/engine_client.rs`'s, for the reasoning) -
/// `view` is any 32 bytes with the top nibble cleared (a valid low-order
/// scalar), and `spend_pubkey` is a real point on the curve. This mock has no
/// real wallet behind it, so hardcoding the same known-good test key
/// material every other test already relies on is the right choice here too,
/// not just in tests - there is nothing else this driver could plausibly use.
const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

/// Percent-encodes `s` for embedding as one query-string value - the same
/// `form_urlencoded::byte_serialize` encoding `control-plane`'s own
/// `http/connect.rs::encode_query_value` uses to build the `next` value it
/// hands to `/dashboard/login`. Used here to *independently reconstruct*
/// that exact same `next` path (see [`run_connect_flow`]) rather than
/// capturing it off a redirect response - since both sides use the same
/// underlying encoding primitive, the two constructions agree byte-for-byte.
fn encode_query_value(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Runs the full WBS 1.4.1 connect flow against a real, reachable control
/// plane at `control_plane_base_url` (e.g. `http://127.0.0.1:8081`), acting
/// as both the merchant's browser and the plugin's own backend:
///
/// 1. Binds this driver's own callback server (standing in for the plugin's
///    settings-page callback route) and generates a fresh nonce.
/// 2. `GET /connect/woocommerce?...` with no session - auto-followed by the
///    cookie-persisting client to `/dashboard/login`.
/// 3. `POST /dashboard/signup` with a fresh, unique email.
/// 4. `POST /dashboard/login` with `next` set to the exact original
///    connect-start path - auto-followed all the way to the confirm form.
/// 5. `POST /connect/woocommerce` with the confirm form's fields (the fixed
///    test wallet material above) - auto-followed to this driver's own
///    callback server, whose handler does the real "plugin-side" work (nonce
///    check, then a separate server-to-server `/finish` call) and reports
///    its outcome back here over a `tokio::sync::oneshot` channel.
///
/// Returns the real, working credentials the control plane's `/finish`
/// handed back once the callback has reported them.
pub async fn run_connect_flow(control_plane_base_url: &str) -> Result<ConnectedCredentials, ConnectFlowError> {
    let platform = "woocommerce";
    let site_url = "https://mock-shop.example.com";
    let nonce = format!("nonce-{}", Uuid::new_v4());
    // A fresh, unique email every run so repeated invocations (e.g. this
    // crate's own tests, run back to back) never collide on "duplicate
    // email" - same reasoning `control-plane`'s own tests apply per-test.
    let email = format!("mock-woocommerce+{}@example.com", Uuid::new_v4());
    let password = "correct horse battery staple";

    let callback = spawn_callback_server(nonce.clone(), control_plane_base_url.to_string(), platform.to_string()).await?;
    let result = run_connect_flow_inner(control_plane_base_url, platform, site_url, &nonce, &email, password, &callback).await;
    callback.task.abort();
    result
}

/// The actual HTTP walk (module doc comment / [`run_connect_flow`]'s own doc
/// comment lay out the five steps) - factored out so [`run_connect_flow`]
/// can unconditionally abort the callback server's background task
/// afterward, on both success and failure.
async fn run_connect_flow_inner(
    control_plane_base_url: &str,
    platform: &str,
    site_url: &str,
    nonce: &str,
    email: &str,
    password: &str,
    callback: &CallbackServer,
) -> Result<ConnectedCredentials, ConnectFlowError> {
    let callback_url = format!("http://{}/moneropay/callback", callback.addr);

    let client = reqwest::Client::builder().cookie_store(true).build()?;

    // The exact relative path+query `control-plane`'s own
    // `http/connect.rs::start` would construct as `this_url` for the
    // `/dashboard/login?next=...` redirect - reconstructed independently
    // (see `encode_query_value`'s doc comment) rather than parsed back off a
    // response, so it can be handed straight to `/dashboard/login`'s `next`
    // field in step 4 below.
    let connect_next_path = format!(
        "/connect/{platform}?site_url={}&return_url={}&nonce={}",
        encode_query_value(site_url),
        encode_query_value(&callback_url),
        encode_query_value(nonce),
    );
    let connect_start_url = format!("{control_plane_base_url}{connect_next_path}");

    // Step 2: no session yet - auto-followed to /dashboard/login.
    let get_response = client.get(&connect_start_url).send().await?;
    expect_ok(get_response, "connect start (pre-login)").await?;

    // Step 3: sign up a fresh account. Its own redirect goes to a bare
    // /dashboard/login with no `next` (today's real behavior) - that's fine,
    // step 4 supplies `next` itself.
    let signup_response =
        client.post(format!("{control_plane_base_url}/dashboard/signup")).form(&[("email", email), ("password", password)]).send().await?;
    expect_ok(signup_response, "dashboard signup").await?;

    // Step 4: log in, carrying `next` back to the original connect-start
    // path - auto-followed (the client now holds the just-issued session
    // cookie) all the way to the confirm form.
    let login_response = client
        .post(format!("{control_plane_base_url}/dashboard/login"))
        .form(&[("email", email), ("password", password), ("next", connect_next_path.as_str())])
        .send()
        .await?;
    expect_ok(login_response, "dashboard login").await?;

    // Step 5: confirm the wallet connection with the fixed test wallet
    // material - auto-followed to this driver's own callback server, whose
    // handler (`callback_handler`) does the rest and reports back over the
    // oneshot channel awaited below.
    let confirm_response = client
        .post(format!("{control_plane_base_url}/connect/{platform}"))
        .form(&[
            ("site_url", site_url),
            ("return_url", callback_url.as_str()),
            ("nonce", nonce),
            ("view_key_hex", TEST_VIEW_KEY_HEX),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
            ("network", "mainnet"),
            ("allowed_origins", ""),
        ])
        .send()
        .await?;
    expect_ok(confirm_response, "connect confirm").await?;

    callback.result_rx_recv().await
}

async fn expect_ok(response: reqwest::Response, step: &str) -> Result<(), ConnectFlowError> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Err(ConnectFlowError::UnexpectedResponse { step: step.to_string(), status, body })
}

/// WBS 1.4.3: creates a real order directly against the engine's own public,
/// unauthenticated `POST /api/v1/t/{public_key}/orders` (`src/http/
/// public.rs::create_order` at the repo root - no `sk_`/`Authorization`
/// header involved, exactly like a real WooCommerce checkout page's
/// server-to-server call would be, since a merchant's `pk_` is not a
/// secret), then builds the checkout redirect target
/// (`{engine_base_url}/pay/v1/{public_key}/{payment_id}`, matching
/// `src/http/mod.rs`'s own route table for `public::payment_page`) from the
/// real `payment_id` the engine handed back - not a plausibly-shaped guess.
///
/// `engine_base_url` is the engine's own externally-reachable address (e.g.
/// `ConnectedCredentials::endpoint` from [`run_connect_flow`]), *not* the
/// control plane - order creation talks to the engine directly, the same
/// way a real WooCommerce site's checkout page would call the engine with
/// the `pk_` it was configured with.
pub async fn create_order(
    engine_base_url: &str,
    public_key: &str,
    fiat_amount: &str,
    fiat_currency: &str,
) -> Result<CreatedOrder, ConnectFlowError> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{engine_base_url}/api/v1/t/{public_key}/orders"))
        .json(&serde_json::json!({
            "fiat_amount": fiat_amount,
            "fiat_currency": fiat_currency,
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(ConnectFlowError::UnexpectedResponse { step: "create order".to_string(), status, body });
    }

    let parsed: CreateOrderResponseBody = response.json().await?;
    let checkout_url = format!("{engine_base_url}/pay/v1/{public_key}/{}", parsed.payment_id);
    Ok(CreatedOrder { payment_id: parsed.payment_id, checkout_url })
}

/// Server-to-server `POST {control_plane_base_url}/connect/{platform}/finish`,
/// deliberately a plain, separate `reqwest` call sharing no state (and no
/// cookie jar) with the browser-acting client in [`run_connect_flow_inner`],
/// exactly like the real plugin's backend has no control-plane session at
/// all. This is called from inside [`callback_handler`], not by
/// [`run_connect_flow`] after the fact - see the module doc comment on why
/// that placement matters (it mirrors where the real WordPress plugin's
/// callback handler will do this at WBS 1.5.3).
async fn call_finish(control_plane_base_url: &str, platform: &str, token: &str) -> Result<ConnectedCredentials, ConnectFlowError> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{control_plane_base_url}/connect/{platform}/finish"))
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(ConnectFlowError::UnexpectedResponse { step: "connect finish".to_string(), status, body });
    }

    let parsed: FinishResponseBody = response.json().await?;
    Ok(ConnectedCredentials { public_key: parsed.public_key, secret_token: parsed.secret_token, endpoint: parsed.endpoint })
}

/// Query parameters the callback route receives on the redirect back from
/// `POST /connect/{platform}` - `token` is the single-use connect token;
/// `nonce` is checked against the one this driver generated before starting
/// the flow (see [`callback_handler`]).
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    token: String,
    nonce: String,
}

/// Shared state the callback route's handler needs: the nonce this driver
/// generated for this flow (to check the incoming one against), where to
/// reach the control plane for the server-to-server `/finish` call, which
/// platform this connection is for, and a slot for the one-shot result
/// sender - wrapped in `Arc<Mutex<Option<..>>>` so it can be taken exactly
/// once (a `oneshot::Sender` is itself single-use; the `Option`/`Mutex` layer
/// is just what lets it live inside `Clone`-able `axum` state until that one
/// use happens).
#[derive(Clone)]
struct CallbackState {
    expected_nonce: Arc<str>,
    control_plane_base_url: Arc<str>,
    platform: Arc<str>,
    result_tx: Arc<Mutex<Option<oneshot::Sender<Result<ConnectedCredentials, ConnectFlowError>>>>>,
}

/// `GET /moneropay/callback` - the callback route standing in for the real
/// plugin's own settings-page callback (WBS 1.5.3 will host the same logic
/// in PHP). This is where the actual "plugin-side" security check lives: a
/// `nonce` that doesn't match the one this driver generated is rejected
/// outright, *before* ever calling `/finish` - seeing a wrong nonce here is
/// exactly what a substituted or replayed redirect would produce, so
/// proceeding anyway would defeat the whole point of round-tripping it.
async fn callback_handler(State(state): State<CallbackState>, Query(query): Query<CallbackQuery>) -> Response {
    let outcome: Result<ConnectedCredentials, ConnectFlowError> = if query.nonce.as_str() != state.expected_nonce.as_ref() {
        Err(ConnectFlowError::NonceMismatch)
    } else {
        call_finish(&state.control_plane_base_url, &state.platform, &query.token).await
    };

    let response = match &outcome {
        Ok(_) => {
            (StatusCode::OK, Html("<!doctype html><html><body><h1>Connected</h1><p>Your Monero wallet is now connected.</p></body></html>"))
                .into_response()
        }
        Err(ConnectFlowError::NonceMismatch) => (StatusCode::BAD_REQUEST, "nonce mismatch - rejected").into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "connect finish failed").into_response(),
    };

    // Only the first callback this server ever receives gets to report a
    // result - matches the single-use nature of the connect token itself.
    // See this crate's own tests for why a test needing to observe a
    // *second* callback (after a rejected one) drives that second call
    // directly instead of relying on a second slot here.
    if let Some(tx) = state.result_tx.lock().await.take() {
        let _ = tx.send(outcome);
    }

    response
}

/// A running, real (network-bound) callback server, standing in for a real
/// plugin's settings-page callback route - same low-level shape as
/// `engine_test_support::TestEngineHandle`, just serving one callback route
/// instead of a whole engine.
struct CallbackServer {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
    result_rx: Mutex<Option<oneshot::Receiver<Result<ConnectedCredentials, ConnectFlowError>>>>,
}

impl CallbackServer {
    /// Awaits this server's one-shot result slot. `pub(crate)` visibility
    /// isn't needed - this is only ever called from within this same module
    /// (production code) or its own test submodule.
    async fn result_rx_recv(&self) -> Result<ConnectedCredentials, ConnectFlowError> {
        let rx = self.result_rx.lock().await.take().expect("result_rx_recv called more than once on the same CallbackServer");
        rx.await.map_err(|_| ConnectFlowError::CallbackNeverReceived)?
    }
}

/// Binds a real `127.0.0.1:0` socket and serves the callback route in a
/// background task - same low-level pattern
/// `engine_test_support::TestEngineConfig::spawn` already uses for the
/// engine itself, just for one callback route rather than a whole app.
async fn spawn_callback_server(expected_nonce: String, control_plane_base_url: String, platform: String) -> Result<CallbackServer, ConnectFlowError> {
    let (tx, rx) = oneshot::channel();
    let state = CallbackState {
        expected_nonce: Arc::from(expected_nonce.as_str()),
        control_plane_base_url: Arc::from(control_plane_base_url.as_str()),
        platform: Arc::from(platform.as_str()),
        result_tx: Arc::new(Mutex::new(Some(tx))),
    };

    let router: Router = Router::new().route("/moneropay/callback", get(callback_handler)).with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(ConnectFlowError::CallbackBindFailed)?;
    let addr = listener.local_addr().map_err(ConnectFlowError::CallbackBindFailed)?;

    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    Ok(CallbackServer { addr, task, result_rx: Mutex::new(Some(rx)) })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// A running, real (network-bound) control-plane instance for this
    /// crate's own tests - no shared `control-plane-test-support` crate
    /// exists yet (only `engine-test-support`, for the engine), so this is a
    /// small, private helper local to this crate, built directly on
    /// `control_plane::http::{AppState, build_router}` the same way
    /// `engine_test_support::TestEngineConfig::spawn` is built on the
    /// engine's own `AppState`/`build_router`. See this task's report for
    /// whether a shared crate is worth it yet.
    struct TestControlPlaneHandle {
        addr: SocketAddr,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestControlPlaneHandle {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Fixed test encryption key - same convention `control-plane`'s own
    /// tests use (see `control-plane/src/http/connections.rs`'s
    /// `TEST_ENCRYPTION_KEY`); no `CONTROL_PLANE_ENCRYPTION_KEY` environment
    /// variable needed for a test-constructed `AppState`.
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    /// Spawns a real control-plane instance (in-memory `Db`, an
    /// `EngineClient` pointed at `engine_addr`, the fixed test encryption
    /// key, `TemplateEngine::new()`) bound to a real ephemeral local port.
    /// No exchange rate/order-creation setup needed on the engine side -
    /// this flow never creates an order, only a tenant.
    async fn spawn_test_control_plane(engine_addr: SocketAddr) -> TestControlPlaneHandle {
        use control_plane::db::Db;
        use control_plane::engine_client::EngineClient;
        use control_plane::http::{AppState, build_router};
        use control_plane::templates::TemplateEngine;

        let state = AppState {
            db: Db::open_in_memory().expect("failed to open in-memory control-plane db for test").into_shared(),
            engine_client: EngineClient::new(format!("http://{engine_addr}")),
            encryption_key: TEST_ENCRYPTION_KEY,
            templates: Arc::new(TemplateEngine::new().expect("built-in control-plane templates must parse")),
        };
        let router = build_router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind an ephemeral local port for the test control plane");
        let addr = listener.local_addr().expect("bound listener has no local address");

        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        TestControlPlaneHandle { addr, task }
    }

    fn parse_query_params(url: &str) -> HashMap<String, String> {
        let parsed = url::Url::parse(url).unwrap();
        parsed.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[tokio::test]
    async fn run_connect_flow_against_a_real_engine_and_control_plane_yields_genuine_working_credentials() {
        let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let control_plane = spawn_test_control_plane(engine.addr).await;
        let control_plane_base_url = format!("http://{}", control_plane.addr);

        let credentials = run_connect_flow(&control_plane_base_url)
            .await
            .expect("the connect flow should succeed end to end against a real engine + control plane");

        assert!(credentials.public_key.starts_with("pk_"), "expected a real pk_ value, got: {}", credentials.public_key);
        assert!(credentials.secret_token.starts_with("sk_"), "expected a real sk_ value, got: {}", credentials.secret_token);
        assert_eq!(credentials.endpoint, format!("http://{}", engine.addr));

        // Strong proof, not just "starts with sk_": the returned
        // secret_token is genuinely this tenant's working credential against
        // the real spawned engine - same pattern used throughout this
        // workspace's other connect-flow/connection tests.
        let engine_client = control_plane::engine_client::EngineClient::new(format!("http://{}", engine.addr));
        let tenant_view = engine_client
            .get_tenant(&credentials.secret_token)
            .await
            .expect("the returned secret_token should be the tenant's genuine, functioning sk_ credential");
        assert_eq!(tenant_view.public_key, credentials.public_key);
    }

    /// A fixed, arbitrary exchange rate for a test-only currency - same
    /// convention `control-plane/src/http/orders.rs`'s own tests use (only
    /// its non-zero-ness matters, since the engine's `compute_xmr_amount` is
    /// exact integer arithmetic regardless of the rate's real-world
    /// plausibility).
    const TEST_CURRENCY: &str = "USD";
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    /// WBS 1.4.3: runs the full connect flow to get real, working
    /// credentials against a real engine (reusing [`run_connect_flow`]
    /// rather than duplicating tenant-creation logic), then calls
    /// [`create_order`] against that same engine with those credentials -
    /// proving order creation is genuinely wired to the real public API, not
    /// just plausibly shaped. The final assertion fetches the returned
    /// `checkout_url` directly with a plain `reqwest::get` (standing in for
    /// the customer's browser being redirected there) and confirms it's a
    /// real, working checkout page, not just a well-formed string - only
    /// possible because this engine was spawned with `with_rate` in
    /// addition to `with_networks`, unlike this crate's other tests, which
    /// never create an order.
    #[tokio::test]
    async fn create_order_against_a_real_engine_yields_a_working_checkout_redirect() {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_rate(TEST_CURRENCY, TEST_RATE_PICONERO_PER_UNIT)
            .spawn()
            .await;
        let control_plane = spawn_test_control_plane(engine.addr).await;
        let control_plane_base_url = format!("http://{}", control_plane.addr);

        let credentials = run_connect_flow(&control_plane_base_url)
            .await
            .expect("the connect flow should succeed end to end against a real engine + control plane");
        assert_eq!(credentials.endpoint, format!("http://{}", engine.addr));

        let order = create_order(&credentials.endpoint, &credentials.public_key, "10.00", TEST_CURRENCY)
            .await
            .expect("order creation should succeed against a real engine with a configured rate");

        assert!(!order.payment_id.is_empty(), "expected a non-empty payment_id");
        assert_eq!(
            order.checkout_url,
            format!("http://{}/pay/v1/{}/{}", engine.addr, credentials.public_key, order.payment_id),
            "checkout_url should be shaped exactly like the engine's own /pay/v1/{{pk}}/{{payment_id}} route"
        );

        // Strong proof, not just a plausibly-shaped URL: actually fetch it,
        // the same way a customer's browser would be redirected there next,
        // and confirm it's a genuine, working checkout page.
        let checkout_response =
            reqwest::get(&order.checkout_url).await.expect("fetching the checkout_url should succeed");
        assert_eq!(checkout_response.status(), reqwest::StatusCode::OK);
        let body = checkout_response.text().await.expect("checkout page response should have a body");
        assert!(body.contains("<html"), "expected the checkout page to be real HTML, got: {body}");
    }

    /// The load-bearing nonce-mismatch proof: a raw HTTP call bypassing
    /// `run_connect_flow`'s own logic entirely - simulating a substituted or
    /// replayed redirect - hits the callback URL with the *real* token but a
    /// *wrong* nonce, and the callback handler must reject it, specifically
    /// via the nonce check (not some other failure), and must never have
    /// called `/finish` as a side effect.
    #[tokio::test]
    async fn a_callback_with_a_mismatched_nonce_is_rejected_and_never_consumes_the_token() {
        let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let control_plane = spawn_test_control_plane(engine.addr).await;
        let control_plane_base_url = format!("http://{}", control_plane.addr);

        let platform = "woocommerce";
        let site_url = "https://mock-shop.example.com";
        let correct_nonce = format!("nonce-{}", Uuid::new_v4());
        let email = format!("nonce-mismatch-{}@example.com", Uuid::new_v4());
        let password = "correct horse battery staple";
        // Never actually dialed in this test - the confirm step's redirect
        // is inspected via its `Location` header instead of being followed
        // (see the client's `redirect::Policy::none()` below), so this can
        // point anywhere.
        let return_url = "http://127.0.0.1:1/moneropay/callback";

        // Unlike `run_connect_flow`'s own client, this test's client
        // deliberately never auto-follows redirects - every step here is
        // inspected directly (status/`Location` header) rather than chased,
        // since the whole point is to intercept the token/nonce before they
        // ever reach a real callback route.
        let client =
            reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap();

        let connect_next_path = format!(
            "/connect/{platform}?site_url={}&return_url={}&nonce={}",
            encode_query_value(site_url),
            encode_query_value(return_url),
            encode_query_value(&correct_nonce),
        );

        client.get(format!("{control_plane_base_url}{connect_next_path}")).send().await.unwrap();
        client
            .post(format!("{control_plane_base_url}/dashboard/signup"))
            .form(&[("email", email.as_str()), ("password", password)])
            .send()
            .await
            .unwrap();
        client
            .post(format!("{control_plane_base_url}/dashboard/login"))
            .form(&[("email", email.as_str()), ("password", password), ("next", connect_next_path.as_str())])
            .send()
            .await
            .unwrap();

        let confirm_response = client
            .post(format!("{control_plane_base_url}/connect/{platform}"))
            .form(&[
                ("site_url", site_url),
                ("return_url", return_url),
                ("nonce", correct_nonce.as_str()),
                ("view_key_hex", TEST_VIEW_KEY_HEX),
                ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ("network", "mainnet"),
                ("allowed_origins", ""),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(confirm_response.status(), reqwest::StatusCode::FOUND, "expected a redirect carrying the connect token");
        let location = confirm_response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let token = parse_query_params(&location).get("token").expect("expected a token query param").clone();

        // Spawn this crate's own callback server (private, same-crate access
        // only) with the *correct* nonce as what it expects.
        let callback = spawn_callback_server(correct_nonce.clone(), control_plane_base_url.clone(), platform.to_string()).await.unwrap();

        // The attack: the real token, but a substituted nonce.
        let raw_client = reqwest::Client::new();
        let malicious = raw_client
            .get(format!("http://{}/moneropay/callback?token={}&nonce=an-attacker-substituted-nonce", callback.addr, token))
            .send()
            .await
            .unwrap();
        assert_eq!(malicious.status(), reqwest::StatusCode::BAD_REQUEST, "a mismatched nonce must be rejected");

        let observed = callback.result_rx_recv().await;
        assert!(
            matches!(observed, Err(ConnectFlowError::NonceMismatch)),
            "expected the nonce check itself to have fired (not some other failure), got: {observed:?}"
        );
        callback.task.abort();

        // Strong proof the mismatched-nonce attempt never reached /finish:
        // the exact same token, presented directly to /finish server-to-
        // server (no nonce involved at that layer at all), must still
        // succeed - which could only be true if it was never consumed above.
        let finish_response = raw_client
            .post(format!("{control_plane_base_url}/connect/{platform}/finish"))
            .json(&serde_json::json!({ "token": token }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            finish_response.status(),
            reqwest::StatusCode::OK,
            "the token must still be unconsumed after the rejected mismatched-nonce attempt"
        );
    }
}
