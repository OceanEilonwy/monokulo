//! The webhook HTTP delivery worker.
//!
//! It claims due rows from `webhook_deliveries` and performs the actual
//! outbound request, using the signing (`webhook_sign::sign_payload`) and SSRF
//! address-classification (`webhook_sign::is_disallowed_address`) logic already
//! built and tested in isolation.
//!
//! See `docs/DESIGN.md` §11.
//!
//! Runs as a loop separate from the writer/scanner, exactly so a slow or hostile
//! merchant endpoint can never stall order-state commits (§DESIGN.md §9).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::store::{DueDelivery, SharedStore};
use crate::webhook_sign::{is_disallowed_address, sign_payload};

/// The one HTTP client webhooks are sent with.
///
/// Its resolver is what makes the SSRF check hold: the addresses a name
/// resolves to are checked (`is_disallowed_address`) by the lookup the
/// connection is made from, not by a separate lookup an attacker's DNS
/// could answer differently; a name with any private address among its
/// answers isn't connected to at all. One client, not one per delivery:
/// building a client loads and parses the system's CA store with blocking
/// file I/O, and a new client has no connection to reuse.
///
/// `allow_private` is live (`webhooks.allow_private_urls`, read each tick):
/// a self-hoster testing against their own LAN turns the check off.
#[derive(Clone)]
pub struct WebhookClient {
    client: reqwest::Client,
    allow_private: Arc<AtomicBool>,
}

impl WebhookClient {
    /// A client with redirects off (a redirect to a private address must
    /// not be followed blindly, `docs/DESIGN.md` §11) and the checking
    /// resolver. Fails only if the TLS backend can't initialise.
    pub fn build() -> reqwest::Result<Self> {
        let allow_private = Arc::new(AtomicBool::new(false));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(CheckingResolver {
                allow_private: Arc::clone(&allow_private),
            }))
            .build()?;
        Ok(Self {
            client,
            allow_private,
        })
    }

    /// Whether private, loopback and link-local destinations are allowed.
    pub fn set_allow_private(&self, allow: bool) {
        self.allow_private.store(allow, Ordering::Relaxed);
    }

    fn allows_private(&self) -> bool {
        self.allow_private.load(Ordering::Relaxed)
    }
}

/// The system resolver, with every answer checked before it is connected
/// to. An IP literal in a URL never reaches a resolver (the connector
/// parses it first), so `attempt_delivery` checks those itself.
struct CheckingResolver {
    allow_private: Arc<AtomicBool>,
}

impl reqwest::dns::Resolve for CheckingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_private = self.allow_private.load(Ordering::Relaxed);
        Box::pin(async move {
            // Port 0: the connector sets the URL's port on each address.
            let addrs: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if addrs.is_empty() {
                return Err(BoxError::from(DeliveryError::UnresolvableHost(
                    name.as_str().to_owned(),
                )));
            }
            // Every answer has to pass, not just the one that gets used: a
            // name resolving to both a public and a private address is the
            // rebinding pattern this defends against, not a partly
            // acceptable target.
            if !allow_private && addrs.iter().any(|addr| is_disallowed_address(addr.ip())) {
                return Err(BoxError::from(DeliveryError::SsrfBlocked));
            }
            Ok::<reqwest::dns::Addrs, BoxError>(Box::new(addrs.into_iter()))
        })
    }
}

/// The error a `reqwest` resolver answers with.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Fallback attempt ceiling, matching `WebhooksConfig::default()`.
///
/// Only used by callers that have no configuration to consult (the tests below);
/// `main` passes `webhooks.max_attempts` through instead. Kept in sync with that
/// default on purpose - the two disagreeing would make the tests here prove
/// something about a number production never uses.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 8;

/// Exponential backoff, capped: 1m, 2m, 4m, ... up to 1h.
fn backoff_seconds(attempt_count: u32) -> i64 {
    let capped_exponent = attempt_count.min(6); // 2^6 * 60 = 3840s, close enough to "up to an hour"
    60 * (1i64 << capped_exponent)
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("URL could not be resolved: {0}")]
    UnresolvableHost(String),
    #[error("URL resolves to a disallowed (private/loopback/link-local) address")]
    SsrfBlocked,
    #[error("request failed: {0}")]
    RequestFailed(String),
}

/// Refuses a URL whose host is an IP literal of a private, loopback or
/// link-local address (unless `allow_private`). A literal never reaches
/// the client's resolver, which checks every name; this is the other half
/// of the same check, made just before the request.
fn check_ip_literal(url: &url::Url, allow_private: bool) -> Result<(), DeliveryError> {
    if allow_private {
        return Ok(());
    }
    let ip = match url.host() {
        None => return Err(DeliveryError::UnresolvableHost("no host".into())),
        Some(url::Host::Ipv4(ip)) => std::net::IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) => std::net::IpAddr::V6(ip),
        Some(url::Host::Domain(_)) => return Ok(()),
    };
    if is_disallowed_address(ip) {
        return Err(DeliveryError::SsrfBlocked);
    }
    Ok(())
}

/// The event id to advertise in `X-Monokulo-Event-Id`, read back out of the signed
/// payload rather than generated here, so the header can never disagree with the body
/// the merchant actually verifies. Falls back to the delivery row's own id for any
/// row enqueued before events carried one (the queue survives restarts and upgrades).
fn event_id_of(delivery: &DueDelivery) -> String {
    serde_json::from_str::<Value>(&delivery.payload_json)
        .ok()
        .and_then(|v| v.get("event_id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| delivery.delivery_id.to_string())
}

/// What a failed request is recorded as: the error and its causes (the
/// resolver's refusal, a connection reset), without the URL. `reqwest`'s
/// own message carries the URL whole, query string (a merchant's token,
/// say) included, and this is stored and logged on every failed attempt;
/// the webhook row names the URL for anyone who needs it.
fn request_error_text(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

pub struct DeliveryOutcome {
    pub delivered: bool,
    pub response_status: Option<u16>,
    pub error: Option<String>,
}

/// Performs one delivery attempt.
///
/// SSRF-validates the resolved address, signs the payload, sends the request
/// with redirects disabled (a redirect to a private address must not be
/// followed blindly - §DESIGN.md §11) and a bounded timeout.
pub async fn attempt_delivery(
    client: &WebhookClient,
    delivery: &DueDelivery,
    timeout: Duration,
) -> DeliveryOutcome {
    // This covers DNS validation as well as the HTTP exchange. A request-level
    // timeout alone starts too late: a stalled resolver could hold the worker
    // indefinitely before `send` was even called.
    match tokio::time::timeout(timeout, attempt_delivery_inner(client, delivery, timeout)).await {
        Ok(outcome) => outcome,
        Err(_) => DeliveryOutcome {
            delivered: false,
            response_status: None,
            error: Some(format!("delivery did not finish within {timeout:?}")),
        },
    }
}

async fn attempt_delivery_inner(
    client: &WebhookClient,
    delivery: &DueDelivery,
    timeout: Duration,
) -> DeliveryOutcome {
    let parsed_url = match url::Url::parse(&delivery.url) {
        Ok(u) => u,
        Err(e) => {
            return DeliveryOutcome {
                delivered: false,
                response_status: None,
                error: Some(format!("invalid url: {e}")),
            };
        }
    };

    if let Err(e) = check_ip_literal(&parsed_url, client.allows_private()) {
        return DeliveryOutcome {
            delivered: false,
            response_status: None,
            error: Some(e.to_string()),
        };
    }
    let client = &client.client;

    // Signed at the moment of sending, so the receiver's tolerance window
    // is measured from this attempt, not from when the event happened.
    let signature = sign_payload(
        delivery.signing_secret.expose(),
        shared::time::now_unix(),
        delivery.payload_json.as_bytes(),
    );
    let extra_headers: Value =
        serde_json::from_str(&delivery.extra_headers_json).unwrap_or(Value::Null);

    let mut request = client
        .post(parsed_url)
        .timeout(timeout)
        .header("X-Monokulo-Signature", signature)
        .header("X-Monokulo-Event", &delivery.event_type)
        .header("X-Monokulo-Event-Id", event_id_of(delivery))
        .header("Content-Type", "application/json")
        .body(delivery.payload_json.clone());
    // Our trace, so a merchant who logs it can match their side to ours
    // (structured_logging.md 2.3). Set before the merchant's own extra
    // headers, which win.
    if let Some(traceparent) = telemetry::trace::current_traceparent() {
        request = request.header(telemetry::trace::TRACEPARENT, traceparent);
    }

    if let Value::Object(map) = extra_headers {
        for (k, v) in map {
            if let Some(v) = v.as_str() {
                request = request.header(k, v);
            }
        }
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            DeliveryOutcome {
                delivered: status.is_success(),
                response_status: Some(status.as_u16()),
                error: if status.is_success() {
                    None
                } else {
                    Some(format!("non-success status {status}"))
                },
            }
        }
        Err(e) => DeliveryOutcome {
            delivered: false,
            response_status: None,
            error: Some(request_error_text(e)),
        },
    }
}

/// One line per attempt, inside its `webhook delivery` span.
fn log_outcome(delivery: &DueDelivery, outcome: &DeliveryOutcome, max_attempts: u32) {
    let status = outcome.response_status;
    if outcome.delivered {
        tracing::info!(http.response.status_code = status, "webhook delivered");
    } else if delivery.attempt_count + 1 >= max_attempts {
        tracing::warn!(
            http.response.status_code = status,
            error = outcome.error.as_deref(),
            "webhook delivery failed; no more attempts"
        );
    } else {
        tracing::info!(
            http.response.status_code = status,
            error = outcome.error.as_deref(),
            "webhook delivery failed; will retry"
        );
    }
}

/// Most deliveries picked per tick, and most sent at once.
pub const DELIVERY_BATCH: u32 = 50;
const DELIVERY_CONCURRENCY: usize = 16;
/// Most deliveries one store gets in a batch, so one store's backlog or slow
/// endpoint can't hold up the others.
const DELIVERY_PER_TENANT: u32 = 4;

/// Sends one batch of due deliveries, concurrently, picked fairly across
/// stores (`Store::due_webhook_deliveries_fair`), and records each outcome.
///
/// A delivery that fails is rescheduled with backoff up to `max_attempts`
/// (`webhooks.max_attempts`), after which it is given up: visible through
/// the admin API, never retried forever or dropped in silence. Returns how
/// many were attempted; a full batch means more may be due now.
///
/// `now` picks what is due. Each outcome is recorded at `now` plus the time
/// elapsed since the tick began, so a retry after a slow batch is scheduled
/// from when its attempt actually happened.
pub async fn run_delivery_tick(
    store: &SharedStore,
    client: &WebhookClient,
    timeout: Duration,
    max_attempts: u32,
    now: i64,
) -> Result<usize, crate::store::StoreError> {
    let db = crate::store::Db::over_shared(Arc::clone(store));
    run_delivery_tick_on(&db, client, timeout, max_attempts, now).await
}

/// `run_delivery_tick` through the database worker: the production path.
pub async fn run_delivery_tick_on(
    db: &crate::store::Db,
    client: &WebhookClient,
    timeout: Duration,
    max_attempts: u32,
    now: i64,
) -> Result<usize, crate::store::StoreError> {
    use crate::store::db::Class;
    use futures_util::stream::{self, StreamExt as _};

    let due = db
        .run(Class::Webhook, move |s| {
            s.due_webhook_deliveries_fair(now, DELIVERY_PER_TENANT, DELIVERY_BATCH)
        })
        .await?;
    let count = due.len();
    let started = std::time::Instant::now();

    let mut outcomes = stream::iter(due)
        .map(|delivery| {
            let span = tracing::info_span!(
                "webhook delivery",
                otel.kind = "client",
                webhook.id = %delivery.webhook_id,
                order.id = %delivery.order_id,
                webhook.event = %delivery.event_type,
                attempt = delivery.attempt_count + 1,
            );
            tracing::Instrument::instrument(
                async move {
                    let outcome = attempt_delivery(client, &delivery, timeout).await;
                    let attempted_at = now + started.elapsed().as_secs() as i64;
                    log_outcome(&delivery, &outcome, max_attempts);
                    (delivery, outcome, attempted_at)
                },
                span,
            )
        })
        .buffer_unordered(DELIVERY_CONCURRENCY);

    // Persist each completed outcome before awaiting any more I/O. Otherwise
    // cancelling one slow delivery loses every already-completed outcome in
    // the batch. Still record later outcomes if an earlier write fails.
    let mut first_error = None;
    while let Some((delivery, outcome, at)) = outcomes.next().await {
        let written = db
            .run(Class::Webhook, move |store| {
                if outcome.delivered {
                    store.mark_webhook_delivered(
                        delivery.delivery_id,
                        outcome.response_status.unwrap_or(0),
                        at,
                    )
                } else if delivery.attempt_count + 1 >= max_attempts {
                    // Give up: the row stays for inspection, never retried -
                    // see docs/DESIGN.md §11.
                    store.give_up_webhook_delivery(
                        delivery.delivery_id,
                        outcome.response_status,
                        outcome.error.as_deref(),
                        at,
                    )
                } else {
                    store.schedule_webhook_retry(
                        delivery.delivery_id,
                        at + backoff_seconds(delivery.attempt_count),
                        outcome.response_status,
                        outcome.error.as_deref(),
                        at,
                    )
                }
            })
            .await;
        if let Err(e) = written {
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(count),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::store::Store;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse as _;
    use axum::routing::post;
    use axum::Router;

    /// A client that may (or may not) reach the test servers on loopback.
    fn test_client_allowing_private(allow: bool) -> WebhookClient {
        let client = WebhookClient::build().unwrap();
        client.set_allow_private(allow);
        client
    }

    fn test_client() -> WebhookClient {
        test_client_allowing_private(true)
    }

    /// Spins up a real local HTTP server (no mocking library needed) whose handler
    /// receives the request's headers and decides the response - lets tests both
    /// script failure/success sequences and inspect exactly what a delivery sent.
    async fn spawn_test_server<F>(handler: F) -> String
    where
        F: Fn(HeaderMap) -> axum::response::Response + Send + Sync + 'static,
    {
        #[derive(Clone)]
        struct Shared(Arc<dyn Fn(HeaderMap) -> axum::response::Response + Send + Sync>);
        async fn hook(
            State(shared): State<Shared>,
            headers: HeaderMap,
            _body: String,
        ) -> axum::response::Response {
            (shared.0)(headers)
        }
        let shared = Shared(Arc::new(handler));

        let app = Router::new().route("/hook", post(hook)).with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/hook")
    }

    fn due_delivery(url: &str, signing_secret: &str) -> DueDelivery {
        DueDelivery {
            delivery_id: 1,
            webhook_id: "wh_1".into(),
            order_id: "pay_1".into(),
            event_type: "order.paid".into(),
            payload_json: "{\"event\":\"order.paid\"}".into(),
            attempt_count: 0,
            url: url.to_owned(),
            extra_headers_json: "{}".into(),
            signing_secret: live_settings::Secret::new(signing_secret),
        }
    }

    #[tokio::test]
    async fn successful_delivery_carries_a_verifiable_signature() {
        use axum::http::StatusCode;
        let captured_signature: Arc<parking_lot::Mutex<Option<String>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let captured_clone = Arc::clone(&captured_signature);
        let url = spawn_test_server(move |headers| {
            let sig = headers
                .get("X-Monokulo-Signature")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            *captured_clone.lock() = sig;
            StatusCode::OK.into_response()
        })
        .await;

        let delivery = due_delivery(&url, "whsec_test");

        let outcome = attempt_delivery(&test_client(), &delivery, Duration::from_secs(2)).await;
        assert!(outcome.delivered);
        assert_eq!(outcome.response_status, Some(200));
        let signature = captured_signature.lock().clone().expect("signed");
        assert!(crate::webhook_sign::verify_signature(
            "whsec_test",
            delivery.payload_json.as_bytes(),
            &signature,
            shared::time::now_unix(),
        ));
        // Signed with the time of sending.
        let signed_at: i64 = signature[2..signature.find(',').unwrap()].parse().unwrap();
        assert!(shared::time::now_unix().abs_diff(signed_at) <= 5);
    }

    #[tokio::test]
    async fn delivery_advertises_the_signed_payloads_own_event_id_as_a_header() {
        use axum::http::StatusCode;
        let captured: Arc<parking_lot::Mutex<Option<String>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let captured_clone = Arc::clone(&captured);
        let url = spawn_test_server(move |headers| {
            *captured_clone.lock() = headers
                .get("X-Monokulo-Event-Id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            StatusCode::OK.into_response()
        })
        .await;

        let mut delivery = due_delivery(&url, "whsec_test");
        delivery.payload_json =
            r#"{"order_id":"pay_1","status":"paid","event_id":"evt_abc123","event":"order.paid","created_at":1700000000}"#
                .into();

        let outcome = attempt_delivery(&test_client(), &delivery, Duration::from_secs(2)).await;
        assert!(outcome.delivered);
        assert_eq!(
            *captured.lock(),
            Some("evt_abc123".to_owned()),
            "the header must repeat the id from the signed body, never a separately-generated one"
        );
    }

    /// The check is the resolver's: the addresses a name resolves to are
    /// what the connection is made to, and a name with a private address
    /// among its answers is refused before anything connects. Validating
    /// with one lookup and connecting with another would be no check
    /// against whoever controls the name's DNS (a zero-TTL record, or a
    /// round-robin between a public and a private address).
    #[tokio::test]
    async fn the_resolver_refuses_a_name_with_a_private_address_and_answers_otherwise() {
        use reqwest::dns::Resolve as _;
        let allow_private = Arc::new(AtomicBool::new(false));
        let resolver = CheckingResolver {
            allow_private: Arc::clone(&allow_private),
        };
        let name = |host: &str| host.parse::<reqwest::dns::Name>().unwrap();
        let refused = match resolver.resolve(name("localhost")).await {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a loopback answer must be refused"),
        };
        assert!(refused.contains("disallowed"), "{refused}");

        allow_private.store(true, Ordering::Relaxed);
        let addrs: Vec<_> = resolver.resolve(name("localhost")).await.unwrap().collect();
        assert!(
            addrs.iter().all(|addr| addr.ip().is_loopback()),
            "{addrs:?}"
        );
        assert!(
            addrs.iter().all(|addr| addr.port() == 0),
            "the connector sets the URL's port: {addrs:?}"
        );
    }

    /// An IP literal never reaches the resolver, so it is classified on
    /// its own: a loopback literal of either family is blocked, a public
    /// one passes, and a name is left to the resolver.
    #[test]
    fn an_ip_literal_url_is_classified_before_the_request() {
        let url = |u: &str| url::Url::parse(u).unwrap();
        assert!(matches!(
            check_ip_literal(&url("http://[::1]:9999/hook"), false),
            Err(DeliveryError::SsrfBlocked)
        ));
        assert!(matches!(
            check_ip_literal(&url("http://127.0.0.1/hook"), false),
            Err(DeliveryError::SsrfBlocked)
        ));
        check_ip_literal(&url("https://[2606:4700:4700::1111]/hook"), false).unwrap();
        check_ip_literal(&url("https://1.1.1.1/hook"), false).unwrap();
        check_ip_literal(&url("https://shop.example/hook"), false).unwrap();
        check_ip_literal(&url("http://[::1]/hook"), true).unwrap();
    }

    #[tokio::test]
    async fn failing_endpoint_is_rescheduled_with_backoff_and_incremented_attempt_count() {
        use axum::http::StatusCode;
        let url =
            spawn_test_server(|_headers| StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;

        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                &crate::store::NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let order = store
            .create_order(&crate::store::NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: 1,
                address: "sub1".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at: 2000,
            })
            .unwrap();
        let webhook = store
            .create_webhook(&tenant.tenant.id, &url, "{}", "whsec_test", 1000)
            .unwrap();
        let delivery_id = store
            .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{\"a\":1}", 1000)
            .unwrap();
        let store = store.into_shared();

        let processed = run_delivery_tick(
            &store,
            &test_client(),
            Duration::from_secs(2),
            DEFAULT_MAX_ATTEMPTS,
            1000,
        )
        .await
        .unwrap();
        assert_eq!(processed, 1);

        let due_immediately = store
            .lock()
            .due_webhook_deliveries_for_test(1001, 10)
            .unwrap();
        assert!(
            due_immediately.is_empty(),
            "must not be immediately due again - backoff must push it out"
        );

        let due_after_backoff = store
            .lock()
            .due_webhook_deliveries_for_test(1000 + 61, 10)
            .unwrap();
        assert_eq!(due_after_backoff.len(), 1);
        assert_eq!(due_after_backoff[0].attempt_count, 1);
        assert_eq!(due_after_backoff[0].delivery_id, delivery_id);
    }

    #[tokio::test]
    async fn ssrf_check_blocks_loopback_by_default_but_allows_it_when_explicitly_opted_in() {
        use axum::http::StatusCode;
        let url = spawn_test_server(|_headers| StatusCode::OK.into_response()).await;
        let delivery = due_delivery(&url, "whsec_test");

        // The test server's URL is an IP literal: checked before the request.
        let client = test_client_allowing_private(false);
        let blocked = attempt_delivery(&client, &delivery, Duration::from_secs(2)).await;
        assert!(!blocked.delivered);
        assert!(blocked.error.unwrap().contains("disallowed"));

        // The same server by name: the resolver refuses the address it
        // answers with, and the request is never made.
        let by_name = due_delivery(&url.replace("127.0.0.1", "localhost"), "whsec_test");
        let blocked = attempt_delivery(&client, &by_name, Duration::from_secs(2)).await;
        assert!(!blocked.delivered);
        assert!(blocked.error.unwrap().contains("disallowed"));

        client.set_allow_private(true);
        let allowed = attempt_delivery(&client, &delivery, Duration::from_secs(2)).await;
        assert!(allowed.delivered);
        let allowed = attempt_delivery(&client, &by_name, Duration::from_secs(2)).await;
        assert!(allowed.delivered);
    }

    #[tokio::test]
    async fn giving_up_after_max_attempts_stops_scheduling_further_retries_soon() {
        use axum::http::StatusCode;
        let url =
            spawn_test_server(|_headers| StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;

        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                &crate::store::NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let order = store
            .create_order(&crate::store::NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: 1,
                address: "sub1".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at: 2000,
            })
            .unwrap();
        let webhook = store
            .create_webhook(&tenant.tenant.id, &url, "{}", "whsec_test", 1000)
            .unwrap();
        store
            .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", 1000)
            .unwrap();
        let store = store.into_shared();

        let mut now = 1000i64;
        for _ in 0..DEFAULT_MAX_ATTEMPTS {
            run_delivery_tick(
                &store,
                &test_client(),
                Duration::from_secs(2),
                DEFAULT_MAX_ATTEMPTS,
                now,
            )
            .await
            .unwrap();
            now += 100_000; // comfortably past any backoff window
        }

        // One more tick, far in the future, should find nothing due - it gave up.
        let processed = run_delivery_tick(
            &store,
            &test_client(),
            Duration::from_secs(2),
            DEFAULT_MAX_ATTEMPTS,
            now + 10_000_000,
        )
        .await
        .unwrap();
        assert_eq!(processed, 0);
    }

    #[tokio::test]
    async fn the_configured_attempt_ceiling_is_what_actually_stops_retries() {
        // `webhooks.max_attempts` is parsed and range-validated by
        // `Config::validate_bounds`, whose error message tells the operator that `0`
        // would mean no webhook is ever delivered - a promise that the value is read
        // by something. It wasn't: `run_delivery_tick` compared against a hardcoded
        // module constant, so a self-hoster who raised the ceiling to survive a
        // longer endpoint outage silently kept the default 8 and lost the
        // notification anyway.
        //
        // Driving the tick with a ceiling that is *not* the default is the only way
        // to tell "the config is wired up" apart from "the config happens to equal
        // the constant".
        use axum::http::StatusCode;
        let url =
            spawn_test_server(|_headers| StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;

        let configured_ceiling = 3u32;
        assert_ne!(
            configured_ceiling, DEFAULT_MAX_ATTEMPTS,
            "this test only proves anything while the configured value differs from the fallback"
        );

        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                &crate::store::NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let order = store
            .create_order(&crate::store::NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: 1,
                address: "sub1".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at: 2000,
            })
            .unwrap();
        let webhook = store
            .create_webhook(&tenant.tenant.id, &url, "{}", "whsec_test", 1000)
            .unwrap();
        store
            .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", 1000)
            .unwrap();
        let store = store.into_shared();

        // Every attempt short of the ceiling must still reschedule.
        let mut now = 1000i64;
        for attempt in 1..configured_ceiling {
            let processed = run_delivery_tick(
                &store,
                &test_client(),
                Duration::from_secs(2),
                configured_ceiling,
                now,
            )
            .await
            .unwrap();
            assert_eq!(
                processed, 1,
                "attempt {attempt} is below the ceiling and must still be retried"
            );
            now += 100_000;
        }

        // The attempt that reaches the ceiling is the last one.
        assert_eq!(
            run_delivery_tick(
                &store,
                &test_client(),
                Duration::from_secs(2),
                configured_ceiling,
                now
            )
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            run_delivery_tick(
                &store,
                &test_client(),
                Duration::from_secs(2),
                configured_ceiling,
                now + 10_000_000
            )
            .await
            .unwrap(),
            0,
            "retries must stop at the *configured* ceiling, not at the module's fallback constant"
        );
    }

    // -- Fair, concurrent delivery (admin_settings_v2.md task 7.8) ------------

    /// A local endpoint that waits `delay` then answers `status`, counting hits.
    async fn spawn_endpoint(
        delay: Duration,
        status: u16,
    ) -> (String, Arc<std::sync::atomic::AtomicU64>) {
        let hits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&hits);
        let app = Router::new().route(
            "/hook",
            post(move |_body: String| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    axum::http::StatusCode::from_u16(status).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/hook"), hits)
    }

    /// A store with a webhook at `url` and `orders` orders, each with one due
    /// delivery. Returns the webhook id and the order ids.
    fn store_with_deliveries(
        store: &Store,
        url: &str,
        orders: usize,
        due_at: i64,
    ) -> (crate::store::WebhookId, Vec<crate::store::OrderId>) {
        use crate::store::{NewOrder, NewTenant};
        let tenant = store
            .create_tenant(
                &NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1,
            )
            .unwrap()
            .tenant;
        let webhook = store
            .create_webhook(&tenant.id, url, "{}", "whsec", 1)
            .unwrap();
        let mut order_ids = vec![];
        for _ in 0..orders {
            let index = store.allocate_minor_index(&tenant.id).unwrap();
            let order = store
                .create_order(&NewOrder {
                    idempotency_key: None,
                    confirmations_required_override: None,
                    tenant_id: tenant.id.clone(),
                    merchant_order_id: None,
                    minor_index: index,
                    address: format!("a{index}"),
                    xmr_amount_piconero: 1,
                    description: None,
                    created_at: 1,
                    expires_at: 10_000_000_000,
                })
                .unwrap();
            store
                .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", due_at)
                .unwrap();
            order_ids.push(order.id);
        }
        (webhook.id, order_ids)
    }

    fn pending_for(store: &SharedStore, webhook_id: &crate::store::WebhookId) -> usize {
        store
            .lock()
            .due_webhook_deliveries_for_test(i64::MAX / 2, 10_000)
            .unwrap()
            .iter()
            .filter(|d| &d.webhook_id == webhook_id)
            .count()
    }

    /// The fair picker's rules, at the store: one delivery per order at a
    /// time (the oldest enqueued, due or not), a store's share counted
    /// after that, and a given-up delivery holding nothing back.
    #[test]
    fn deliveries_are_picked_one_per_order_oldest_first_within_a_stores_share() {
        let store = Store::open_in_memory().unwrap();
        let (webhook, orders) = store_with_deliveries(&store, "https://a.example/hook", 4, 100);
        // Order 0 has four more events queued behind its first.
        for n in 1..=4 {
            store
                .enqueue_webhook_delivery(&webhook, &orders[0], "order.confirming", "{}", 100 + n)
                .unwrap();
        }
        let picked = store
            .due_webhook_deliveries_fair(1000, DELIVERY_PER_TENANT, DELIVERY_BATCH)
            .unwrap();
        let picked_orders: Vec<_> = picked.iter().map(|d| d.order_id.clone()).collect();
        assert_eq!(
            picked_orders, orders,
            "one per order, and order 0's backlog does not use up the store's share"
        );

        // Order 0's first event fails and waits out its backoff; a later
        // event for it is enqueued meanwhile and is due before the retry.
        let first = picked[0].delivery_id;
        store
            .schedule_webhook_retry(first, 5_000, Some(500), Some("boom"), 1000)
            .unwrap();
        store
            .enqueue_webhook_delivery(&webhook, &orders[0], "order.paid", "{}", 1001)
            .unwrap();
        let picked = store
            .due_webhook_deliveries_fair(2000, DELIVERY_PER_TENANT, DELIVERY_BATCH)
            .unwrap();
        assert!(
            !picked.iter().any(|d| d.order_id == orders[0]),
            "nothing for order 0 until its first event is delivered or abandoned: {picked:?}"
        );
        let picked = store
            .due_webhook_deliveries_fair(5_000, DELIVERY_PER_TENANT, DELIVERY_BATCH)
            .unwrap();
        assert_eq!(
            picked
                .iter()
                .find(|d| d.order_id == orders[0])
                .map(|d| d.delivery_id),
            Some(first),
            "the retry goes first, when due"
        );

        // Given up on: out of the way, and out of the backlog count.
        store
            .give_up_webhook_delivery(first, Some(500), Some("boom"), 5_000)
            .unwrap();
        let picked = store
            .due_webhook_deliveries_fair(5_000, DELIVERY_PER_TENANT, DELIVERY_BATCH)
            .unwrap();
        let next = picked.iter().find(|d| d.order_id == orders[0]).unwrap();
        assert_eq!(next.event_type, "order.confirming");
        assert_ne!(next.delivery_id, first);
        let (backlog, _) = store.webhook_backlog(10_000).unwrap();
        assert_eq!(
            backlog,
            4 + 4,
            "eight undelivered, the given-up one not counted"
        );
    }

    #[tokio::test]
    async fn one_stores_slow_endpoint_and_backlog_do_not_hold_up_another_store() {
        let (slow_url, slow_hits) = spawn_endpoint(Duration::from_millis(800), 200).await;
        let (fast_url, _) = spawn_endpoint(Duration::ZERO, 200).await;
        let store = Store::open_in_memory().unwrap();
        let (slow_webhook, _) = store_with_deliveries(&store, &slow_url, 40, 100);
        // Queued after all of the slow store's, so plain oldest-first order
        // would put it behind them.
        let (fast_webhook, _) = store_with_deliveries(&store, &fast_url, 1, 101);
        let store = store.into_shared();

        let started = std::time::Instant::now();
        run_delivery_tick(&store, &test_client(), Duration::from_secs(5), 8, 1000)
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "sent concurrently, not one after another: {:?}",
            started.elapsed()
        );

        assert_eq!(
            pending_for(&store, &fast_webhook),
            0,
            "the other store's webhook went out in the first tick"
        );
        assert_eq!(
            slow_hits.load(Ordering::SeqCst),
            DELIVERY_PER_TENANT as u64,
            "the busy store got its fair share, not the whole batch"
        );
        assert_eq!(
            pending_for(&store, &slow_webhook),
            40 - DELIVERY_PER_TENANT as usize
        );
    }

    #[tokio::test]
    async fn cancelling_a_batch_keeps_outcomes_that_already_completed() {
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = Arc::clone(&started);
        let app = Router::new().route(
            "/hook",
            post(move || {
                let signal = Arc::clone(&signal);
                async move {
                    signal.notify_one();
                    std::future::pending::<axum::http::StatusCode>().await
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = Store::open_in_memory().unwrap();
        // URL validation finishes immediately; the other endpoint stays pending.
        let (fast, _) = store_with_deliveries(&store, "invalid URL", 1, 100);
        store_with_deliveries(&store, &url, 1, 101);
        let store = store.into_shared();
        let client = test_client();
        {
            let tick = run_delivery_tick(&store, &client, Duration::from_secs(60), 8, 1000);
            tokio::pin!(tick);
            tokio::select! {
                result = &mut tick => panic!("batch unexpectedly completed: {result:?}"),
                () = started.notified() => {}
            }
        }
        server.abort();
        let deliveries = store
            .lock()
            .due_webhook_deliveries_for_test(i64::MAX / 2, 100)
            .unwrap();
        let completed = deliveries.iter().find(|d| d.webhook_id == fast).unwrap();
        assert_eq!(
            completed.attempt_count, 1,
            "completed outcomes must survive cancellation of another delivery"
        );
    }

    #[tokio::test]
    async fn many_stores_are_all_served_within_a_couple_of_ticks() {
        let (url, _) = spawn_endpoint(Duration::ZERO, 200).await;
        let store = Store::open_in_memory().unwrap();
        let webhooks: Vec<crate::store::WebhookId> = (0..60)
            .map(|i| store_with_deliveries(&store, &url, 1, 100 + i).0)
            .collect();
        let store = store.into_shared();

        let first = run_delivery_tick(&store, &test_client(), Duration::from_secs(5), 8, 1000)
            .await
            .unwrap();
        assert_eq!(
            first, DELIVERY_BATCH as usize,
            "a full batch, so the loop goes again straight away"
        );
        run_delivery_tick(&store, &test_client(), Duration::from_secs(5), 8, 1000)
            .await
            .unwrap();
        for webhook in &webhooks {
            assert_eq!(
                pending_for(&store, &shared::ids::WebhookId::new(webhook.to_string())),
                0
            );
        }
    }

    #[tokio::test]
    async fn two_events_for_one_order_are_never_in_flight_together() {
        let (url, hits) = spawn_endpoint(Duration::ZERO, 200).await;
        let store = Store::open_in_memory().unwrap();
        let (webhook, orders) = store_with_deliveries(&store, &url, 1, 100);
        store
            .enqueue_webhook_delivery(
                &shared::ids::WebhookId::new(webhook.to_string()),
                &shared::ids::OrderId::new(orders[0].to_string()),
                "order.confirming",
                "{}",
                101,
            )
            .unwrap();
        let store = store.into_shared();

        run_delivery_tick(&store, &test_client(), Duration::from_secs(5), 8, 1000)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1, "the older one first, alone");
        run_delivery_tick(&store, &test_client(), Duration::from_secs(5), 8, 1000)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(pending_for(&store, &webhook), 0);
    }

    /// The endpoint takes over 2 s to answer (real time: the request is
    /// real I/O, which a paused clock would run ahead of). The retry is
    /// due 60 s after the attempt ended, so not at 61 s from the tick's
    /// start; how much later depends on the machine, and is only bounded.
    #[tokio::test]
    async fn a_retry_is_scheduled_from_when_its_attempt_happened() {
        let (url, _) = spawn_endpoint(Duration::from_millis(2100), 500).await;
        let store = Store::open_in_memory().unwrap();
        let (webhook, _) = store_with_deliveries(&store, &url, 1, 100);
        let store = store.into_shared();

        run_delivery_tick(&store, &test_client(), Duration::from_secs(5), 8, 1000)
            .await
            .unwrap();
        // The attempt took over 2s; its first retry comes 60s after that.
        let due_at = |t: i64| {
            store
                .lock()
                .due_webhook_deliveries_for_test(t, 10)
                .unwrap()
                .iter()
                .filter(|d| d.webhook_id == webhook)
                .count()
        };
        assert_eq!(
            due_at(1000 + 60 + 1),
            0,
            "not 60s from the start of the tick"
        );
        assert_eq!(
            due_at(1000 + 60 + 30),
            1,
            "60s after an attempt that took a few seconds"
        );
    }
}
