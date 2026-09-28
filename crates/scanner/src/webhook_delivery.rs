//! The webhook HTTP delivery worker: claims due rows from `webhook_deliveries` and
//! performs the actual outbound request, using the signing (`webhook_sign::sign_payload`)
//! and SSRF address-classification (`webhook_sign::is_disallowed_address`) logic
//! already built and tested in isolation. See `docs/DESIGN.md` §11.
//!
//! Runs as a loop separate from the writer/scanner, exactly so a slow or hostile
//! merchant endpoint can never stall order-state commits (§DESIGN.md §9).

use std::time::Duration;

use serde_json::Value;

use crate::store::{DueDelivery, SharedStore};
use crate::webhook_sign::{is_disallowed_address, sign_payload};

/// Fallback attempt ceiling, matching `WebhooksConfig::default()`. Only used by
/// callers that have no configuration to consult (the tests below); `main` passes
/// `webhooks.max_attempts` through instead. Kept in sync with that default on
/// purpose - the two disagreeing would make the tests here prove something about a
/// number production never uses.
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

/// Resolves `url`'s host and rejects it if *any* resolved address is
/// private/loopback/link-local, unless `allow_private` is set (a self-hoster
/// testing against their own LAN - see `docs/DESIGN.md` §13's
/// `webhooks.allow_private_urls`). Done immediately before dispatch, not only at
/// webhook registration time, since DNS can change in between.
///
/// Returns the one address the request must actually be sent to. Validating and then
/// handing the *hostname* back to `reqwest` - which resolves it again, independently -
/// is not a check at all against an attacker who controls the DNS for that name: a
/// zero-TTL record, or a round-robin alternating between a public and a private
/// address, simply answers the validating lookup with the public one and the
/// connecting lookup with `127.0.0.1`. The caller pins the connection to this exact
/// address instead of re-resolving.
async fn resolve_and_validate(
    url: &url::Url,
    allow_private: bool,
) -> Result<Option<std::net::SocketAddr>, DeliveryError> {
    if allow_private {
        return Ok(None);
    }
    let host = url.host_str().ok_or_else(|| DeliveryError::UnresolvableHost("no host".into()))?;
    // `Url::host_str` hands back an IPv6 literal in its URL form, brackets and all
    // (`[::1]`), which is not something a resolver accepts: it parses as neither an
    // IP address nor a DNS name, so *every* IPv6-literal webhook URL failed with
    // "unresolvable host" on every attempt until it hit the retry ceiling. Stripping
    // the brackets puts the literal back through the same `lookup_host` (and
    // therefore the same `is_disallowed_address`) path a hostname takes, rather than
    // leaving a whole address family permanently undeliverable.
    let lookup_host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((lookup_host, port))
        .await
        .map_err(|e| DeliveryError::UnresolvableHost(e.to_string()))?
        .collect();
    if addrs.is_empty() {
        return Err(DeliveryError::UnresolvableHost(host.to_string()));
    }
    // Every returned address has to pass, not just the one that gets used - a name
    // resolving to both a public and a private address is exactly the rebinding
    // pattern this is defending against, not a partially-acceptable target.
    for addr in &addrs {
        if is_disallowed_address(addr.ip()) {
            return Err(DeliveryError::SsrfBlocked);
        }
    }
    Ok(Some(addrs[0]))
}

/// A short-lived client that will only ever connect to `addr` for `host`, whatever
/// DNS says by the time the request goes out. `reqwest`'s `resolve` override applies
/// solely to the address selection: the request still carries the original `Host`
/// header and, over TLS, the original SNI name, so the destination server sees an
/// ordinary request for the hostname the merchant registered.
fn pinned_client(host: &str, addr: std::net::SocketAddr) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        // Matching the shared client's policy: a redirect to a private address must
        // not be followed blindly (§DESIGN.md §11), and a per-request client that
        // quietly reinstated the default policy would reopen exactly that hole.
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, addr)
        .build()
}

/// The event id to advertise in `X-Monokulo-Event-Id`, read back out of the signed
/// payload rather than generated here, so the header can never disagree with the body
/// the merchant actually verifies. Falls back to the delivery row's own id for any
/// row enqueued before events carried one (the queue survives restarts and upgrades).
fn event_id_of(delivery: &DueDelivery) -> String {
    serde_json::from_str::<Value>(&delivery.payload_json)
        .ok()
        .and_then(|v| v.get("event_id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| delivery.delivery_id.to_string())
}

pub struct DeliveryOutcome {
    pub delivered: bool,
    pub response_status: Option<u16>,
    pub error: Option<String>,
}

/// Performs one delivery attempt: SSRF-validates the resolved address, signs the
/// payload, sends the request with redirects disabled (a redirect to a private
/// address must not be followed blindly - §DESIGN.md §11) and a bounded timeout.
pub async fn attempt_delivery(
    client: &reqwest::Client,
    delivery: &DueDelivery,
    allow_private: bool,
    timeout: Duration,
) -> DeliveryOutcome {
    let parsed_url = match url::Url::parse(&delivery.url) {
        Ok(u) => u,
        Err(e) => {
            return DeliveryOutcome { delivered: false, response_status: None, error: Some(format!("invalid url: {e}")) };
        }
    };

    let validated_addr = match resolve_and_validate(&parsed_url, allow_private).await {
        Ok(addr) => addr,
        Err(e) => return DeliveryOutcome { delivered: false, response_status: None, error: Some(e.to_string()) },
    };

    // Pin the connection to the address that was just validated. Without this the
    // validation above is advisory only, since `reqwest` would resolve the hostname
    // a second time and could get a different answer.
    let pinned;
    let client = match validated_addr {
        Some(addr) => {
            let host = parsed_url.host_str().unwrap_or_default().to_string();
            match pinned_client(&host, addr) {
                Ok(c) => {
                    pinned = c;
                    &pinned
                }
                Err(e) => {
                    return DeliveryOutcome {
                        delivered: false,
                        response_status: None,
                        error: Some(format!("could not build a pinned HTTP client: {e}")),
                    };
                }
            }
        }
        None => client,
    };

    let signature = sign_payload(&delivery.signing_secret, delivery.payload_json.as_bytes());
    let extra_headers: Value = serde_json::from_str(&delivery.extra_headers_json).unwrap_or(Value::Null);

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
                error: if status.is_success() { None } else { Some(format!("non-success status {status}")) },
            }
        }
        Err(e) => DeliveryOutcome { delivered: false, response_status: None, error: Some(e.to_string()) },
    }
}

/// One line per attempt, inside its `webhook delivery` span.
fn log_outcome(delivery: &DueDelivery, outcome: &DeliveryOutcome, max_attempts: u32) {
    let status = outcome.response_status;
    if outcome.delivered {
        tracing::info!(http.response.status_code = status, "webhook delivered");
    } else if delivery.attempt_count + 1 >= max_attempts {
        tracing::warn!(http.response.status_code = status, error = outcome.error.as_deref(), "webhook delivery failed; no more attempts");
    } else {
        tracing::info!(http.response.status_code = status, error = outcome.error.as_deref(), "webhook delivery failed; will retry");
    }
}

/// One pass: claims every currently-due delivery and attempts it once. A row that
/// fails is rescheduled with backoff up to `max_attempts`, after which it's left
/// alone (visible via the admin API for debugging, never silently retried forever
/// or silently dropped). Returns the number of rows processed this tick.
///
/// `max_attempts` is a parameter rather than the module constant it used to be
/// because `webhooks.max_attempts` is a real, range-validated configuration knob:
/// `Config::validate_bounds` rejects `0` for it with a message explaining that no
/// webhook would ever be delivered, which promises the operator it is load-bearing.
/// It wasn't - the value was parsed, validated, and then never read by anything, so
/// a self-hoster who set `max_attempts = 24` got 8. A setting that is checked but
/// ignored is worse than one that doesn't exist.
///
/// Takes the *shared* store (`Arc<Mutex<Store>>`), not a bare `&Store`, and
/// deliberately locks it only around the brief synchronous calls before and after
/// each delivery attempt - never across `attempt_delivery`'s `.await`, which does
/// real outbound network I/O and can take the full `timeout` to resolve. Holding the
/// lock across that would block every other request touching the store (including
/// unrelated HTTP handlers, since they share the same `Store`) for as long as a
/// slow or hanging merchant endpoint takes to respond - exactly the kind of
/// lock-across-await mistake caught once already in `http::admin::delete_own_tenant`
/// during development.
/// Most deliveries picked per tick, and most sent at once.
pub const DELIVERY_BATCH: u32 = 50;
const DELIVERY_CONCURRENCY: usize = 16;
/// Most deliveries one store gets in a batch, so one store's backlog or slow
/// endpoint can't hold up the others.
const DELIVERY_PER_TENANT: u32 = 4;

/// Sends one batch of due deliveries, concurrently, picked fairly across
/// stores (`Store::due_webhook_deliveries_fair`), and records each outcome.
/// Returns how many were attempted; a full batch means more may be due now.
///
/// `now` picks what is due. Each outcome is recorded at `now` plus the time
/// elapsed since the tick began, so a retry after a slow batch is scheduled
/// from when its attempt actually happened.
pub async fn run_delivery_tick(
    store: &SharedStore,
    client: &reqwest::Client,
    allow_private: bool,
    timeout: Duration,
    max_attempts: u32,
    now: i64,
) -> Result<usize, crate::store::StoreError> {
    use futures_util::stream::{self, StreamExt};

    let due = store.lock().due_webhook_deliveries_fair(now, DELIVERY_PER_TENANT, DELIVERY_BATCH)?;
    let count = due.len();
    let started = std::time::Instant::now();

    let outcomes: Vec<(DueDelivery, DeliveryOutcome, i64)> = stream::iter(due)
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
                    let outcome = attempt_delivery(client, &delivery, allow_private, timeout).await;
                    let attempted_at = now + started.elapsed().as_secs() as i64;
                    log_outcome(&delivery, &outcome, max_attempts);
                    (delivery, outcome, attempted_at)
                },
                span,
            )
        })
        .buffer_unordered(DELIVERY_CONCURRENCY)
        .collect()
        .await;

    // Every outcome is recorded even if an earlier one fails to write: a
    // delivery that went out but wasn't marked would be sent again.
    let mut first_error = None;
    for (delivery, outcome, at) in outcomes {
        let store = store.lock();
        let written = if outcome.delivered {
            store.mark_webhook_delivered(delivery.delivery_id, outcome.response_status.unwrap_or(0), at)
        } else if delivery.attempt_count + 1 >= max_attempts {
            // Give up: record the final failure but stop scheduling retries by
            // pushing next_attempt_at far into the future rather than leaving it
            // due forever. The row itself is never deleted - see docs/DESIGN.md §11.
            store.schedule_webhook_retry(
                delivery.delivery_id,
                at + 100 * 365 * 24 * 60 * 60, // effectively "never again"
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
        };
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
mod tests {
    use super::*;
    use crate::store::Store;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use std::sync::Arc;

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap()
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
        let shared = Shared(Arc::new(handler));

        async fn hook(State(shared): State<Shared>, headers: HeaderMap, _body: String) -> axum::response::Response {
            (shared.0)(headers)
        }

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
            url: url.to_string(),
            extra_headers_json: "{}".into(),
            signing_secret: signing_secret.to_string(),
        }
    }

    #[tokio::test]
    async fn successful_delivery_carries_a_verifiable_signature() {
        use axum::http::StatusCode;
        let captured_signature: Arc<parking_lot::Mutex<Option<String>>> = Arc::new(parking_lot::Mutex::new(None));
        let captured_clone = captured_signature.clone();
        let url = spawn_test_server(move |headers| {
            let sig = headers.get("X-Monokulo-Signature").and_then(|v| v.to_str().ok()).map(str::to_string);
            *captured_clone.lock() = sig;
            StatusCode::OK.into_response()
        })
        .await;

        let delivery = due_delivery(&url, "whsec_test");
        let expected_signature = sign_payload("whsec_test", delivery.payload_json.as_bytes());

        let outcome = attempt_delivery(&test_client(), &delivery, true, Duration::from_secs(2)).await;
        assert!(outcome.delivered);
        assert_eq!(outcome.response_status, Some(200));
        assert_eq!(*captured_signature.lock(), Some(expected_signature));
    }

    #[tokio::test]
    async fn delivery_advertises_the_signed_payloads_own_event_id_as_a_header() {
        use axum::http::StatusCode;
        let captured: Arc<parking_lot::Mutex<Option<String>>> = Arc::new(parking_lot::Mutex::new(None));
        let captured_clone = captured.clone();
        let url = spawn_test_server(move |headers| {
            *captured_clone.lock() =
                headers.get("X-Monokulo-Event-Id").and_then(|v| v.to_str().ok()).map(str::to_string);
            StatusCode::OK.into_response()
        })
        .await;

        let mut delivery = due_delivery(&url, "whsec_test");
        delivery.payload_json =
            r#"{"order_id":"pay_1","status":"paid","event_id":"evt_abc123","event":"order.paid","created_at":1700000000}"#
                .into();

        let outcome = attempt_delivery(&test_client(), &delivery, true, Duration::from_secs(2)).await;
        assert!(outcome.delivered);
        assert_eq!(
            *captured.lock(),
            Some("evt_abc123".to_string()),
            "the header must repeat the id from the signed body, never a separately-generated one"
        );
    }

    #[tokio::test]
    async fn a_pinned_client_connects_to_the_validated_address_and_still_sends_the_original_host() {
        // The mechanism behind the DNS-rebinding fix. Validating a hostname's
        // resolved addresses and then handing `reqwest` the *hostname* is no check at
        // all against whoever controls that name's DNS: a zero-TTL record, or a
        // round-robin between a public and a private address, answers the validating
        // lookup and the connecting lookup differently. Pinning the connection to the
        // address that was actually validated is what closes that, and it has to do
        // so without disturbing the Host header (and, over TLS, the SNI name) the
        // merchant's server expects to see.
        use axum::http::StatusCode;
        let captured_host: Arc<parking_lot::Mutex<Option<String>>> = Arc::new(parking_lot::Mutex::new(None));
        let captured_clone = captured_host.clone();
        let url = spawn_test_server(move |headers| {
            *captured_clone.lock() = headers.get("host").and_then(|v| v.to_str().ok()).map(str::to_string);
            StatusCode::OK.into_response()
        })
        .await;
        let server_addr: std::net::SocketAddr = url
            .trim_start_matches("http://")
            .trim_end_matches("/hook")
            .parse()
            .unwrap();

        // A hostname that resolves to nothing at all, so the request can only
        // possibly arrive if the pinning - not DNS - decided where it went.
        let client = pinned_client("webhook.invalid", server_addr).unwrap();
        let response = client.post("http://webhook.invalid/hook").body("{}").send().await.unwrap();

        assert!(response.status().is_success());
        assert_eq!(
            *captured_host.lock(),
            Some("webhook.invalid".to_string()),
            "the destination must still see the hostname it was registered under, not the pinned IP"
        );
    }

    #[tokio::test]
    async fn an_ipv6_literal_url_is_classified_rather_than_dismissed_as_unresolvable() {
        // `Url::host_str` returns an IPv6 literal in URL form - brackets included -
        // and `[::1]` parses as neither an IP address nor a DNS name, so handing it
        // straight to the resolver made *every* IPv6-literal webhook fail with
        // "unresolvable host" on every attempt until it burned through the retry
        // ceiling: an entire address family permanently undeliverable, and the
        // loopback case not actually blocked so much as accidentally never reached.
        let loopback = url::Url::parse("http://[::1]:9999/hook").unwrap();
        assert!(
            matches!(resolve_and_validate(&loopback, false).await, Err(DeliveryError::SsrfBlocked)),
            "an IPv6 loopback literal must be blocked by the address classifier, not by failing to resolve"
        );

        let public = url::Url::parse("https://[2606:4700:4700::1111]/hook").unwrap();
        let addr = resolve_and_validate(&public, false)
            .await
            .expect("a public IPv6 literal must validate")
            .expect("and must be pinned to a concrete address");
        assert_eq!(addr.ip(), "2606:4700:4700::1111".parse::<std::net::IpAddr>().unwrap());
        assert_eq!(addr.port(), 443, "the scheme's default port, since the URL named none");
    }

    #[tokio::test]
    async fn failing_endpoint_is_rescheduled_with_backoff_and_incremented_attempt_count() {
        use axum::http::StatusCode;
        let url = spawn_test_server(|_headers| StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;

        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                crate::store::NewTenant {
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
            .create_order(crate::store::NewOrder {
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
        let webhook = store.create_webhook(&tenant.tenant.id, &url, "{}", "whsec_test", 1000).unwrap();
        let delivery_id = store
            .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{\"a\":1}", 1000)
            .unwrap();
        let store = store.into_shared();

        let processed = run_delivery_tick(&store, &test_client(), true, Duration::from_secs(2), DEFAULT_MAX_ATTEMPTS, 1000).await.unwrap();
        assert_eq!(processed, 1);

        let due_immediately = store.lock().due_webhook_deliveries(1001, 10).unwrap();
        assert!(due_immediately.is_empty(), "must not be immediately due again - backoff must push it out");

        let due_after_backoff = store.lock().due_webhook_deliveries(1000 + 61, 10).unwrap();
        assert_eq!(due_after_backoff.len(), 1);
        assert_eq!(due_after_backoff[0].attempt_count, 1);
        assert_eq!(due_after_backoff[0].delivery_id, delivery_id);
    }

    #[tokio::test]
    async fn ssrf_check_blocks_loopback_by_default_but_allows_it_when_explicitly_opted_in() {
        use axum::http::StatusCode;
        let url = spawn_test_server(|_headers| StatusCode::OK.into_response()).await;
        let delivery = due_delivery(&url, "whsec_test");

        let blocked = attempt_delivery(&test_client(), &delivery, false, Duration::from_secs(2)).await;
        assert!(!blocked.delivered);
        assert!(blocked.error.unwrap().contains("disallowed"));

        let allowed = attempt_delivery(&test_client(), &delivery, true, Duration::from_secs(2)).await;
        assert!(allowed.delivered);
    }

    #[tokio::test]
    async fn giving_up_after_max_attempts_stops_scheduling_further_retries_soon() {
        use axum::http::StatusCode;
        let url = spawn_test_server(|_headers| StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;

        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                crate::store::NewTenant {
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
            .create_order(crate::store::NewOrder {
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
        let webhook = store.create_webhook(&tenant.tenant.id, &url, "{}", "whsec_test", 1000).unwrap();
        store.enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", 1000).unwrap();
        let store = store.into_shared();

        let mut now = 1000i64;
        for _ in 0..DEFAULT_MAX_ATTEMPTS {
            run_delivery_tick(&store, &test_client(), true, Duration::from_secs(2), DEFAULT_MAX_ATTEMPTS, now).await.unwrap();
            now += 100_000; // comfortably past any backoff window
        }

        // One more tick, far in the future, should find nothing due - it gave up.
        let processed = run_delivery_tick(&store, &test_client(), true, Duration::from_secs(2), DEFAULT_MAX_ATTEMPTS, now + 10_000_000).await.unwrap();
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
        let url = spawn_test_server(|_headers| StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;

        let configured_ceiling = 3u32;
        assert_ne!(
            configured_ceiling, DEFAULT_MAX_ATTEMPTS,
            "this test only proves anything while the configured value differs from the fallback"
        );

        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                crate::store::NewTenant {
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
            .create_order(crate::store::NewOrder {
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
        let webhook = store.create_webhook(&tenant.tenant.id, &url, "{}", "whsec_test", 1000).unwrap();
        store.enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", 1000).unwrap();
        let store = store.into_shared();

        // Every attempt short of the ceiling must still reschedule.
        let mut now = 1000i64;
        for attempt in 1..configured_ceiling {
            let processed =
                run_delivery_tick(&store, &test_client(), true, Duration::from_secs(2), configured_ceiling, now)
                    .await
                    .unwrap();
            assert_eq!(processed, 1, "attempt {attempt} is below the ceiling and must still be retried");
            now += 100_000;
        }

        // The attempt that reaches the ceiling is the last one.
        assert_eq!(
            run_delivery_tick(&store, &test_client(), true, Duration::from_secs(2), configured_ceiling, now)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            run_delivery_tick(
                &store,
                &test_client(),
                true,
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
    async fn spawn_endpoint(delay: Duration, status: u16) -> (String, Arc<std::sync::atomic::AtomicU64>) {
        let hits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = hits.clone();
        let app = Router::new().route(
            "/hook",
            post(move |_body: String| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    fn store_with_deliveries(store: &Store, url: &str, orders: usize, due_at: i64) -> (String, Vec<String>) {
        use crate::store::{NewOrder, NewTenant};
        let tenant = store
            .create_tenant(
                NewTenant {
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
        let webhook = store.create_webhook(&tenant.id, url, "{}", "whsec", 1).unwrap();
        let mut order_ids = vec![];
        for _ in 0..orders {
            let index = store.allocate_minor_index(&tenant.id).unwrap();
            let order = store
                .create_order(NewOrder {
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
            store.enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", due_at).unwrap();
            order_ids.push(order.id);
        }
        (webhook.id, order_ids)
    }

    fn pending_for(store: &SharedStore, webhook_id: &str) -> usize {
        store.lock().due_webhook_deliveries(i64::MAX / 2, 10_000).unwrap().iter().filter(|d| d.webhook_id == webhook_id).count()
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
        run_delivery_tick(&store, &test_client(), true, Duration::from_secs(5), 8, 1000).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(4), "sent concurrently, not one after another: {:?}", started.elapsed());

        assert_eq!(pending_for(&store, &fast_webhook), 0, "the other store's webhook went out in the first tick");
        assert_eq!(
            slow_hits.load(std::sync::atomic::Ordering::SeqCst),
            DELIVERY_PER_TENANT as u64,
            "the busy store got its fair share, not the whole batch"
        );
        assert_eq!(pending_for(&store, &slow_webhook), 40 - DELIVERY_PER_TENANT as usize);
    }

    #[tokio::test]
    async fn many_stores_are_all_served_within_a_couple_of_ticks() {
        let (url, _) = spawn_endpoint(Duration::ZERO, 200).await;
        let store = Store::open_in_memory().unwrap();
        let webhooks: Vec<String> = (0..60).map(|i| store_with_deliveries(&store, &url, 1, 100 + i).0).collect();
        let store = store.into_shared();

        let first = run_delivery_tick(&store, &test_client(), true, Duration::from_secs(5), 8, 1000).await.unwrap();
        assert_eq!(first, DELIVERY_BATCH as usize, "a full batch, so the loop goes again straight away");
        run_delivery_tick(&store, &test_client(), true, Duration::from_secs(5), 8, 1000).await.unwrap();
        for webhook in &webhooks {
            assert_eq!(pending_for(&store, webhook), 0);
        }
    }

    #[tokio::test]
    async fn two_events_for_one_order_are_never_in_flight_together() {
        let (url, hits) = spawn_endpoint(Duration::ZERO, 200).await;
        let store = Store::open_in_memory().unwrap();
        let (webhook, orders) = store_with_deliveries(&store, &url, 1, 100);
        store.enqueue_webhook_delivery(&webhook, &orders[0], "order.confirming", "{}", 101).unwrap();
        let store = store.into_shared();

        run_delivery_tick(&store, &test_client(), true, Duration::from_secs(5), 8, 1000).await.unwrap();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "the older one first, alone");
        run_delivery_tick(&store, &test_client(), true, Duration::from_secs(5), 8, 1000).await.unwrap();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(pending_for(&store, &webhook), 0);
    }

    #[tokio::test]
    async fn a_retry_is_scheduled_from_when_its_attempt_happened() {
        let (url, _) = spawn_endpoint(Duration::from_millis(2100), 500).await;
        let store = Store::open_in_memory().unwrap();
        let (webhook, _) = store_with_deliveries(&store, &url, 1, 100);
        let store = store.into_shared();

        run_delivery_tick(&store, &test_client(), true, Duration::from_secs(5), 8, 1000).await.unwrap();
        // The attempt took over 2s; its first retry comes 60s after that.
        let due_at = |t: i64| store.lock().due_webhook_deliveries(t, 10).unwrap().iter().filter(|d| d.webhook_id == webhook).count();
        assert_eq!(due_at(1000 + 60 + 1), 0, "not 60s from the start of the tick");
        assert_eq!(due_at(1000 + 2 + 60), 1);
    }
}
