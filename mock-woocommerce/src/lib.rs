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

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

/// The real, working credentials `POST /connect/{platform}/finish` hands
/// back on success - see `control-plane/src/http/connect.rs::FinishResponse`,
/// which this mirrors field-for-field (the two crates only ever talk over
/// HTTP, same convention `control-plane::engine_client` already uses for the
/// engine's own admin API).
///
/// WBS 1.4.4 extends this with the real webhook this flow now always
/// registers: `webhook_signing_secret` (the engine's real per-webhook HMAC
/// key, echoed back by `/finish`) and `webhook_receiver` (this driver's own
/// long-lived receiver, still running - see [`WebhookReceiver`] - so a caller
/// can observe whatever real deliveries arrive after the connect flow itself
/// has finished).
#[derive(Debug)]
pub struct ConnectedCredentials {
    pub public_key: String,
    pub secret_token: String,
    pub endpoint: String,
    pub webhook_signing_secret: String,
    pub webhook_receiver: WebhookReceiver,
}

/// The plain fields `/finish` hands back once webhook registration has been
/// folded in - the oneshot channel's payload type between [`callback_handler`]
/// (which calls `/finish`) and [`run_connect_flow`] (which owns the
/// [`WebhookReceiver`] and combines it with these fields into the final
/// [`ConnectedCredentials`]). Kept separate from `ConnectedCredentials` itself
/// so the receiver - which must be spawned *before* the flow starts, since its
/// URL has to be handed to `/finish` - never needs to be moved through the
/// channel.
#[derive(Debug)]
struct FinishedCredentials {
    public_key: String,
    secret_token: String,
    endpoint: String,
    webhook_signing_secret: String,
}

#[derive(Debug, Deserialize)]
struct FinishResponseBody {
    public_key: String,
    secret_token: String,
    endpoint: String,
    // Required (not `Option`), unlike control-plane's own `FinishResponse` field of
    // the same name: this driver always supplies a `webhook_url` (see
    // `run_connect_flow`/`call_finish`), so a real engine always echoes this back on
    // success - a response missing it here would mean something is genuinely wrong,
    // not a caller-chosen omission.
    webhook_signing_secret: String,
}

/// One event this driver's webhook receiver verified and recorded - see
/// [`WebhookReceiver::events`]. Deliberately keeps the whole parsed payload
/// (`docs/DESIGN.md` §11's envelope plus whatever event-specific fields ride
/// along) rather than picking out only `event`/`event_id`, so a caller can
/// assert on anything the delivery actually carried.
#[derive(Debug, Clone)]
pub struct RecordedWebhookEvent {
    pub event: String,
    pub event_id: String,
    pub payload: serde_json::Value,
    /// The exact raw request body bytes, and the `X-MoneroPay-Signature` value
    /// presented alongside them - kept verbatim (not just "it verified, trust me")
    /// so a caller can independently re-run `shared::webhook_sign::verify_signature`
    /// itself against a secret obtained through a different channel (e.g. this
    /// crate's own forced-delivery test, which cross-checks against the
    /// `webhook_signing_secret` `/finish` returned) - a real, non-circular proof
    /// that this receiver's verification and the credentials the driver came away
    /// with agree on the same secret.
    pub raw_body: Vec<u8>,
    pub signature: String,
}

#[derive(Default)]
struct ReceiverState {
    /// Not known at bind time - see [`spawn_webhook_receiver`]'s doc comment - and
    /// set once `/finish` returns a real `signing_secret`. Every incoming request is
    /// rejected until this is set, since there is nothing correct to verify against
    /// before then.
    signing_secret: Option<String>,
    events: Vec<RecordedWebhookEvent>,
    seen_event_ids: HashSet<String>,
}

/// This driver's own real webhook receiver (WBS 1.4.4) - a genuine, network-bound
/// axum server standing in for the real WordPress plugin's own receiver endpoint
/// (WBS 1.5.4 will host equivalent PHP verification logic). Outlives the connect
/// flow that registered it (unlike [`CallbackServer`], which is deliberately
/// short-lived): a webhook can be delivered - or retried - long after `/finish` has
/// already returned, so this has to keep listening for as long as the caller holds
/// onto the [`ConnectedCredentials`] it came back on.
pub struct WebhookReceiver {
    /// The real local address this receiver is listening on - `POST
    /// {addr}/moneropay/webhook` is what gets registered as `webhook_url`.
    pub addr: SocketAddr,
    state: Arc<StdMutex<ReceiverState>>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for WebhookReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookReceiver").field("addr", &self.addr).finish_non_exhaustive()
    }
}

impl WebhookReceiver {
    /// Every event verified and recorded so far, in delivery order. Cloned out from
    /// behind the lock rather than borrowed, so a caller can poll this in a loop
    /// (see this crate's own forced-delivery test) without holding a lock across a
    /// `sleep`.
    pub fn events(&self) -> Vec<RecordedWebhookEvent> {
        self.state.lock().unwrap().events.clone()
    }
}

impl Drop for WebhookReceiver {
    /// Same reasoning as `engine_test_support::TestEngineHandle`/[`CallbackServer`]:
    /// a hard `abort()` is simple and sufficient - each test gets its own ephemeral
    /// port and task, and there is nothing worth gracefully draining.
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `POST /moneropay/webhook` - the real receiver route. Takes the raw request body
/// as [`Bytes`], deliberately *not* a pre-parsed `Json<T>` extractor: signature
/// verification (`shared::webhook_sign::verify_signature`) must run against the
/// exact bytes the sender signed, before any JSON parsing/re-serialization ever
/// touches them, or it will never match (see that module's own doc comment on why
/// this is the single easiest thing to get subtly wrong here). Only once the
/// signature verifies is the body parsed as JSON at all.
///
/// Rejects (without recording anything) a request with a missing/malformed
/// signature header, or one that fails to verify, or one arriving before this
/// receiver has been told its own `signing_secret` (see [`ReceiverState`]) - all of
/// these are indistinguishable `401`s, since none of them are this receiver's own
/// fault to explain.
async fn webhook_handler(State(state): State<Arc<StdMutex<ReceiverState>>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let Some(presented_signature) = headers.get("X-MoneroPay-Signature").and_then(|v| v.to_str().ok()) else {
        return StatusCode::UNAUTHORIZED;
    };

    let signing_secret = { state.lock().unwrap().signing_secret.clone() };
    let Some(signing_secret) = signing_secret else {
        return StatusCode::UNAUTHORIZED;
    };

    if !shared::webhook_sign::verify_signature(&signing_secret, &body, presented_signature) {
        return StatusCode::UNAUTHORIZED;
    }

    // Only reached once the raw bytes are proven genuine - parsing/interpreting an
    // unverified body at all would be acting on data an attacker (or a merely
    // misconfigured sender) could have fabricated.
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let event = parsed.get("event").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let event_id = parsed.get("event_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    // Webhook delivery is at-least-once (`docs/DESIGN.md` §11) - a real retry of an
    // already-recorded delivery is expected, not a bug, and dedupes on `event_id`
    // exactly as the design doc says a receiver should.
    let mut guard = state.lock().unwrap();
    if guard.seen_event_ids.insert(event_id.clone()) {
        guard.events.push(RecordedWebhookEvent {
            event,
            event_id,
            payload: parsed,
            raw_body: body.to_vec(),
            signature: presented_signature.to_string(),
        });
    }

    StatusCode::OK
}

/// Binds this receiver's own real `127.0.0.1:0` socket and serves
/// `POST /moneropay/webhook` in a background task - same low-level pattern
/// [`spawn_callback_server`]/`engine_test_support::TestEngineConfig::spawn` already
/// use. Its `signing_secret` isn't known yet at this point (the receiver's URL has
/// to exist *before* `/finish` is called, since it's an input to that call, while
/// the secret is part of that call's response) - [`webhook_handler`] simply rejects
/// everything until [`CallbackState`]'s `webhook_state` (the same `Arc` this
/// receiver holds) is updated once `/finish` succeeds.
async fn spawn_webhook_receiver() -> Result<WebhookReceiver, ConnectFlowError> {
    let state = Arc::new(StdMutex::new(ReceiverState::default()));
    let router: Router = Router::new().route("/moneropay/webhook", post(webhook_handler)).with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(ConnectFlowError::BindFailed)?;
    let addr = listener.local_addr().map_err(ConnectFlowError::BindFailed)?;

    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    Ok(WebhookReceiver { addr, state, task })
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
    #[error("failed to bind a local socket: {0}")]
    BindFailed(std::io::Error),
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
/// handed back once the callback has reported them, plus (WBS 1.4.4) the
/// real webhook signing secret and this driver's own still-running
/// [`WebhookReceiver`] - see [`ConnectedCredentials`]'s doc comment.
pub async fn run_connect_flow(control_plane_base_url: &str) -> Result<ConnectedCredentials, ConnectFlowError> {
    run_connect_flow_with(control_plane_base_url, None).await
}

/// Same as [`run_connect_flow`], but provisions the tenant with a specific
/// `order_expiry_seconds` instead of the engine's own default (via
/// `ConfirmForm::order_expiry_seconds` - see `control-plane/src/http/
/// connect.rs`). Exists for a caller that needs to force a real,
/// non-payment-dependent order status transition quickly (WBS 1.4.4's own
/// forced-delivery test: an order past a very short `expires_at` reaches
/// `expired` purely from wall-clock time, see `docs/DESIGN.md` §7.6) without
/// waiting on the engine's real default expiry window.
pub async fn run_connect_flow_with_order_expiry_seconds(
    control_plane_base_url: &str,
    order_expiry_seconds: i64,
) -> Result<ConnectedCredentials, ConnectFlowError> {
    run_connect_flow_with(control_plane_base_url, Some(order_expiry_seconds)).await
}

/// The shared implementation behind [`run_connect_flow`]/
/// [`run_connect_flow_with_order_expiry_seconds`] - a builder-style config
/// parameter here would be overkill for one optional knob, so this is just a
/// plain `Option`, following the same "generalize rather than add a
/// near-duplicate function" judgment `engine_test_support::TestEngineConfig`
/// already applied to a similar situation.
async fn run_connect_flow_with(
    control_plane_base_url: &str,
    order_expiry_seconds: Option<i64>,
) -> Result<ConnectedCredentials, ConnectFlowError> {
    let platform = "woocommerce";
    let site_url = "https://mock-shop.example.com";
    let nonce = format!("nonce-{}", Uuid::new_v4());
    // A fresh, unique email every run so repeated invocations (e.g. this
    // crate's own tests, run back to back) never collide on "duplicate
    // email" - same reasoning `control-plane`'s own tests apply per-test.
    let email = format!("mock-woocommerce+{}@example.com", Uuid::new_v4());
    let password = "correct horse battery staple";

    // Spawned *before* the flow starts and deliberately outlives it (unlike the
    // callback server below): its address has to exist so it can be handed to
    // `/finish` as `webhook_url`, and it must keep running afterward to receive
    // whatever real deliveries arrive later - see `WebhookReceiver`'s own doc
    // comment.
    let webhook_receiver = spawn_webhook_receiver().await?;
    let webhook_url = format!("http://{}/moneropay/webhook", webhook_receiver.addr);

    let callback = spawn_callback_server(
        nonce.clone(),
        control_plane_base_url.to_string(),
        platform.to_string(),
        webhook_url,
        webhook_receiver.state.clone(),
    )
    .await?;
    let result =
        run_connect_flow_inner(control_plane_base_url, platform, site_url, &nonce, &email, password, order_expiry_seconds, &callback)
            .await;
    callback.task.abort();

    result.map(|finished| ConnectedCredentials {
        public_key: finished.public_key,
        secret_token: finished.secret_token,
        endpoint: finished.endpoint,
        webhook_signing_secret: finished.webhook_signing_secret,
        webhook_receiver,
    })
}

/// The actual HTTP walk (module doc comment / [`run_connect_flow`]'s own doc
/// comment lay out the five steps) - factored out so [`run_connect_flow_with`]
/// can unconditionally abort the callback server's background task
/// afterward, on both success and failure.
#[allow(clippy::too_many_arguments)]
async fn run_connect_flow_inner(
    control_plane_base_url: &str,
    platform: &str,
    site_url: &str,
    nonce: &str,
    email: &str,
    password: &str,
    order_expiry_seconds: Option<i64>,
    callback: &CallbackServer,
) -> Result<FinishedCredentials, ConnectFlowError> {
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
    let order_expiry_seconds_string = order_expiry_seconds.map(|s| s.to_string());
    let mut confirm_fields: Vec<(&str, &str)> = vec![
        ("site_url", site_url),
        ("return_url", callback_url.as_str()),
        ("nonce", nonce),
        ("view_key_hex", TEST_VIEW_KEY_HEX),
        ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
        ("network", "mainnet"),
        ("allowed_origins", ""),
    ];
    if let Some(s) = &order_expiry_seconds_string {
        confirm_fields.push(("order_expiry_seconds", s.as_str()));
    }
    let confirm_response = client.post(format!("{control_plane_base_url}/connect/{platform}")).form(&confirm_fields).send().await?;
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
///
/// `webhook_url` (WBS 1.4.4) is this driver's own [`WebhookReceiver`]'s
/// address - always supplied, unlike control-plane's own optional field of
/// the same name, since this driver always wants a real webhook registered.
async fn call_finish(control_plane_base_url: &str, platform: &str, token: &str, webhook_url: &str) -> Result<FinishedCredentials, ConnectFlowError> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{control_plane_base_url}/connect/{platform}/finish"))
        .json(&serde_json::json!({ "token": token, "webhook_url": webhook_url }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(ConnectFlowError::UnexpectedResponse { step: "connect finish".to_string(), status, body });
    }

    let parsed: FinishResponseBody = response.json().await?;
    Ok(FinishedCredentials {
        public_key: parsed.public_key,
        secret_token: parsed.secret_token,
        endpoint: parsed.endpoint,
        webhook_signing_secret: parsed.webhook_signing_secret,
    })
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
/// platform this connection is for, this driver's own webhook receiver's
/// URL (passed to `/finish` as `webhook_url`, WBS 1.4.4) and a handle to that
/// same receiver's shared state (to set its `signing_secret` once `/finish`
/// returns one - see [`ReceiverState`]), and a slot for the one-shot result
/// sender - wrapped in `Arc<Mutex<Option<..>>>` so it can be taken exactly
/// once (a `oneshot::Sender` is itself single-use; the `Option`/`Mutex` layer
/// is just what lets it live inside `Clone`-able `axum` state until that one
/// use happens).
#[derive(Clone)]
struct CallbackState {
    expected_nonce: Arc<str>,
    control_plane_base_url: Arc<str>,
    platform: Arc<str>,
    webhook_url: Arc<str>,
    webhook_state: Arc<StdMutex<ReceiverState>>,
    result_tx: Arc<Mutex<Option<oneshot::Sender<Result<FinishedCredentials, ConnectFlowError>>>>>,
}

/// `GET /moneropay/callback` - the callback route standing in for the real
/// plugin's own settings-page callback (WBS 1.5.3 will host the same logic
/// in PHP). This is where the actual "plugin-side" security check lives: a
/// `nonce` that doesn't match the one this driver generated is rejected
/// outright, *before* ever calling `/finish` - seeing a wrong nonce here is
/// exactly what a substituted or replayed redirect would produce, so
/// proceeding anyway would defeat the whole point of round-tripping it.
async fn callback_handler(State(state): State<CallbackState>, Query(query): Query<CallbackQuery>) -> Response {
    let outcome: Result<FinishedCredentials, ConnectFlowError> = if query.nonce.as_str() != state.expected_nonce.as_ref() {
        Err(ConnectFlowError::NonceMismatch)
    } else {
        call_finish(&state.control_plane_base_url, &state.platform, &query.token, &state.webhook_url).await
    };

    // Tell this driver's own webhook receiver its real signing secret, now that
    // `/finish` has handed one back - see `ReceiverState::signing_secret`'s own doc
    // comment for why the receiver couldn't have known this any earlier.
    if let Ok(finished) = &outcome {
        state.webhook_state.lock().unwrap().signing_secret = Some(finished.webhook_signing_secret.clone());
    }

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
    result_rx: Mutex<Option<oneshot::Receiver<Result<FinishedCredentials, ConnectFlowError>>>>,
}

impl CallbackServer {
    /// Awaits this server's one-shot result slot. `pub(crate)` visibility
    /// isn't needed - this is only ever called from within this same module
    /// (production code) or its own test submodule.
    async fn result_rx_recv(&self) -> Result<FinishedCredentials, ConnectFlowError> {
        let rx = self.result_rx.lock().await.take().expect("result_rx_recv called more than once on the same CallbackServer");
        rx.await.map_err(|_| ConnectFlowError::CallbackNeverReceived)?
    }
}

/// Binds a real `127.0.0.1:0` socket and serves the callback route in a
/// background task - same low-level pattern
/// `engine_test_support::TestEngineConfig::spawn` already uses for the
/// engine itself, just for one callback route rather than a whole app.
/// `webhook_url`/`webhook_state` (WBS 1.4.4) are threaded straight into
/// [`CallbackState`] - see that type's own doc comment.
async fn spawn_callback_server(
    expected_nonce: String,
    control_plane_base_url: String,
    platform: String,
    webhook_url: String,
    webhook_state: Arc<StdMutex<ReceiverState>>,
) -> Result<CallbackServer, ConnectFlowError> {
    let (tx, rx) = oneshot::channel();
    let state = CallbackState {
        expected_nonce: Arc::from(expected_nonce.as_str()),
        control_plane_base_url: Arc::from(control_plane_base_url.as_str()),
        platform: Arc::from(platform.as_str()),
        webhook_url: Arc::from(webhook_url.as_str()),
        webhook_state,
        result_tx: Arc::new(Mutex::new(Some(tx))),
    };

    let router: Router = Router::new().route("/moneropay/callback", get(callback_handler)).with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(ConnectFlowError::BindFailed)?;
    let addr = listener.local_addr().map_err(ConnectFlowError::BindFailed)?;

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

        // WBS 1.4.4: this flow always registers a real webhook now, at this
        // driver's own receiver - not just a plausibly-shaped secret in the
        // response.
        assert!(!credentials.webhook_signing_secret.is_empty());
        let webhooks = engine_client
            .list_webhooks(&credentials.secret_token)
            .await
            .expect("list_webhooks against the real engine should succeed");
        assert_eq!(webhooks.len(), 1);
        assert_eq!(webhooks[0].url, format!("http://{}/moneropay/webhook", credentials.webhook_receiver.addr));
        assert!(credentials.webhook_receiver.events().is_empty(), "no event has been delivered yet");
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
        // only) with the *correct* nonce as what it expects. The webhook_url/
        // webhook_state below are never exercised by this test (the mismatched
        // nonce is rejected before `/finish` - and therefore before any webhook
        // registration - is ever reached), so a throwaway address and a fresh,
        // unshared receiver state are enough.
        let callback = spawn_callback_server(
            correct_nonce.clone(),
            control_plane_base_url.clone(),
            platform.to_string(),
            "http://127.0.0.1:1/moneropay/webhook".to_string(),
            Arc::new(StdMutex::new(ReceiverState::default())),
        )
        .await
        .unwrap();

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

    // --- WBS 1.4.4: webhook receiver -------------------------------------------

    /// The same known secret/payload/signature triple
    /// `shared::webhook_sign::tests::known_vector_for_cross_language_php_verification`
    /// documents specifically for reuse by another verifier (its own doc comment:
    /// "must reproduce byte-for-byte to prove its HMAC-SHA256 signing matches this
    /// Rust implementation") - reused here rather than invented fresh, per that
    /// module's own instruction. The constants there are private to `shared`, so the
    /// values are reproduced literally rather than imported.
    const KNOWN_VECTOR_SECRET: &str = "known_vector_secret_for_php_crosscheck";
    const KNOWN_VECTOR_PAYLOAD: &[u8] = br#"{"event":"order.paid","order_id":"12345","amount_piconero":"1000000000000"}"#;
    const KNOWN_VECTOR_SIGNATURE_HEX: &str = "436a60c6f66d20b611c7e4a3f78ab13167fb26680a65d8b2e5a114c182de80f1";

    /// A direct unit test of signature verification (WBS 1.4.4's own explicit ask),
    /// decoupled from any HTTP round trip - this is exactly the check
    /// [`webhook_handler`] runs against the raw request body before ever parsing it
    /// as JSON.
    #[test]
    fn signature_verification_matches_the_known_cross_language_vector() {
        assert!(shared::webhook_sign::verify_signature(KNOWN_VECTOR_SECRET, KNOWN_VECTOR_PAYLOAD, KNOWN_VECTOR_SIGNATURE_HEX));

        // A tampered payload (one byte flipped in the trailing amount) must not
        // verify against the same signature - the sanity check that this isn't
        // trivially true for any input.
        let tampered = br#"{"event":"order.paid","order_id":"12345","amount_piconero":"1000000000000"}"#
            .iter()
            .copied()
            .map(|b| if b == b'1' { b'2' } else { b })
            .collect::<Vec<u8>>();
        assert!(!shared::webhook_sign::verify_signature(KNOWN_VECTOR_SECRET, &tampered, KNOWN_VECTOR_SIGNATURE_HEX));
    }

    /// Directly sets a spawned [`WebhookReceiver`]'s `signing_secret` - same-module
    /// access to the private `state` field, standing in for what
    /// `callback_handler` does once `/finish` returns a real one.
    fn set_receiver_signing_secret(receiver: &WebhookReceiver, secret: &str) {
        receiver.state.lock().unwrap().signing_secret = Some(secret.to_string());
    }

    #[tokio::test]
    async fn webhook_receiver_records_a_validly_signed_delivery_and_dedupes_a_retried_event_id() {
        let receiver = spawn_webhook_receiver().await.unwrap();
        let secret = "whsec_receiver_test";
        set_receiver_signing_secret(&receiver, secret);

        let payload = serde_json::json!({
            "event": "order.expired",
            "event_id": "evt_dedupe_test",
            "created_at": 1_700_000_000,
            "payment_id": "pay_dedupe_test",
            "status": "expired",
        });
        let body = payload.to_string();
        let signature = shared::webhook_sign::sign_payload(secret, body.as_bytes());

        let client = reqwest::Client::new();
        let url = format!("http://{}/moneropay/webhook", receiver.addr);

        // A real delivery, then its retry (at-least-once delivery, `docs/DESIGN.md`
        // §11) - the exact same body and signature both times.
        for _ in 0..2 {
            let response = client.post(&url).header("X-MoneroPay-Signature", &signature).body(body.clone()).send().await.unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }

        let events = receiver.events();
        assert_eq!(events.len(), 1, "the retried event_id must not be recorded twice");
        assert_eq!(events[0].event, "order.expired");
        assert_eq!(events[0].event_id, "evt_dedupe_test");
        assert_eq!(events[0].payload["payment_id"], serde_json::json!("pay_dedupe_test"));
    }

    #[tokio::test]
    async fn webhook_receiver_rejects_an_invalid_signature_and_records_nothing() {
        let receiver = spawn_webhook_receiver().await.unwrap();
        set_receiver_signing_secret(&receiver, "whsec_receiver_test");

        let body = serde_json::json!({ "event": "order.expired", "event_id": "evt_should_never_be_recorded" }).to_string();
        let client = reqwest::Client::new();
        let url = format!("http://{}/moneropay/webhook", receiver.addr);

        // Wrong secret entirely - genuinely invalid, not just a near miss.
        let wrong_signature = shared::webhook_sign::sign_payload("not-the-real-secret", body.as_bytes());
        let rejected = client.post(&url).header("X-MoneroPay-Signature", &wrong_signature).body(body.clone()).send().await.unwrap();
        assert_eq!(rejected.status(), reqwest::StatusCode::UNAUTHORIZED);

        // No signature header at all.
        let missing_header = client.post(&url).body(body.clone()).send().await.unwrap();
        assert_eq!(missing_header.status(), reqwest::StatusCode::UNAUTHORIZED);

        assert!(receiver.events().is_empty(), "an unverified request must never be recorded");
    }

    #[tokio::test]
    async fn webhook_receiver_rejects_everything_before_its_signing_secret_is_known() {
        // No `set_receiver_signing_secret` call - mirrors the real window between
        // this receiver being spawned (so its URL exists to hand to `/finish`) and
        // `/finish` actually returning a `signing_secret` for `callback_handler` to
        // set. There is nothing correct to verify against in that window, so
        // everything must be rejected, not merely unverified-and-accepted.
        let receiver = spawn_webhook_receiver().await.unwrap();
        let body = serde_json::json!({ "event": "order.expired", "event_id": "evt_too_early" }).to_string();
        // Signed with a secret this receiver could not possibly know yet - the
        // point is that even a "plausible" signature is rejected before the real
        // secret exists to check it against.
        let signature = shared::webhook_sign::sign_payload("whsec_guessed_early", body.as_bytes());

        let response = reqwest::Client::new()
            .post(format!("http://{}/moneropay/webhook", receiver.addr))
            .header("X-MoneroPay-Signature", &signature)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert!(receiver.events().is_empty());
    }

    // --- WBS 1.4.4: a genuine, forced end-to-end delivery -----------------------

    /// The centerpiece test: forces a *real* webhook delivery - a live outbound HTTP
    /// call from the engine's own background delivery worker, signed with a real
    /// per-webhook secret, verified by this crate's own receiver route - with no
    /// directly-inserted-row shortcut anywhere in the path.
    ///
    /// How the event is forced: `run_connect_flow_with_order_expiry_seconds`
    /// provisions the tenant with `order_expiry_seconds: 1` (confirmed against the
    /// real code, not assumed - see `docs/DESIGN.md` §7.6 and
    /// `src/scanner.rs::an_unpaid_order_past_its_deadline_becomes_expired_on_a_tick_that_matches_nothing`,
    /// and `engine_test_support::TestEngineConfig::with_background_loops`'s own doc
    /// comment for why the recompute sweep that reaches `expired` needs neither a
    /// real payment nor a non-trivial `MoneroDaemonClient`). An order is then created
    /// against that tenant with no payment ever made; once more than a second of
    /// real wall-clock time has passed, the engine's own background scanner tick
    /// (spawned via `with_background_loops` on the test engine below) recomputes the
    /// order to `expired` and enqueues a real, signed `order.expired` webhook, which
    /// the engine's own background delivery tick then genuinely POSTs to this
    /// crate's receiver.
    ///
    /// This is provably not a shortcut: nothing in this test ever touches the
    /// engine's or control plane's databases directly, mints an event/delivery row
    /// itself, or calls any scanner/delivery function by hand - every step is a real
    /// HTTP call (`run_connect_flow_with_order_expiry_seconds`, `create_order`) or a
    /// real background loop (`with_background_loops`) already proven to work on its
    /// own in `engine_test_support`'s own test suite. The only thing this test does
    /// that production code doesn't is poll for the effect rather than wait
    /// indefinitely for it.
    #[tokio::test]
    async fn a_genuinely_forced_order_expired_webhook_is_delivered_and_verified() {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_rate(TEST_CURRENCY, TEST_RATE_PICONERO_PER_UNIT)
            .with_background_loops()
            .spawn()
            .await;
        let control_plane = spawn_test_control_plane(engine.addr).await;
        let control_plane_base_url = format!("http://{}", control_plane.addr);

        let credentials = run_connect_flow_with_order_expiry_seconds(&control_plane_base_url, 1)
            .await
            .expect("the connect flow should succeed end to end against a real engine + control plane");

        let order = create_order(&credentials.endpoint, &credentials.public_key, "1.00", TEST_CURRENCY)
            .await
            .expect("order creation should succeed against a real engine with a configured rate");

        // Poll rather than a fixed sleep: the background loops tick every ~150ms
        // (`engine_test_support::BACKGROUND_LOOP_INTERVAL`), and this only needs to
        // wait for the first tick after this order's 1-second `expires_at` has
        // actually passed and the delivery worker has had one more tick to send it.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let matched = loop {
            let found = credentials
                .webhook_receiver
                .events()
                .into_iter()
                .find(|e| e.payload.get("payment_id").and_then(|v| v.as_str()) == Some(order.payment_id.as_str()));
            if let Some(found) = found {
                break Some(found);
            }
            if tokio::time::Instant::now() >= deadline {
                break None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };

        let event = matched.expect("expected a real order.expired webhook delivery to arrive within the deadline");
        assert_eq!(event.event, "order.expired");
        assert_eq!(event.payload["status"], serde_json::json!("expired"));
        assert!(event.event_id.starts_with("evt_"));

        // The receiver only ever records a request whose signature it already
        // verified (against whatever secret its own internal state held at the
        // time - see `webhook_handler`), so a recorded event at all is already
        // strong evidence. This goes one step further and is genuinely
        // non-circular: it independently re-verifies the *exact* raw bytes and
        // signature the delivery actually carried (`event.raw_body`/
        // `event.signature`, captured verbatim by the receiver, not reconstructed)
        // against `credentials.webhook_signing_secret` - the value this test
        // obtained through a completely different channel (parsed straight out of
        // `/finish`'s own JSON response, never read from the receiver's internal
        // state). The two agreeing proves the receiver that verified this delivery
        // and the credentials this driver came away with are talking about the
        // same real secret, not two coincidentally-successful checks.
        assert!(shared::webhook_sign::verify_signature(&credentials.webhook_signing_secret, &event.raw_body, &event.signature));
        // And a specificity check: an arbitrary wrong secret must not verify the
        // same real bytes/signature - ruling out a `verify_signature` that
        // accidentally always returns `true`.
        assert!(!shared::webhook_sign::verify_signature("definitely-the-wrong-secret", &event.raw_body, &event.signature));
    }
}
