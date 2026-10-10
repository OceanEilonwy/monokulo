//! The webhook delivery worker: sends due deliveries
//! (`db::Db::due_deliveries`) to stores' endpoints, signed
//! (`shared::webhook_sign`), and records each attempt.
//!
//! The schedule: up to `webhooks.max_attempts` attempts (8), each allowed
//! `webhooks.delivery_timeout_ms` (5 s), waiting 1, 2, 4, 8, 16, 32 and 64
//! minutes between them, so the last is about 2 h 7 min after the first. A
//! delivery whose last attempt fails is given up on: kept for the store
//! settings page, never tried again unless a merchant asks ("Send again",
//! "Retry failed").
//!
//! Nothing private: a URL whose host is, or resolves to, a loopback,
//! private, link-local or otherwise internal address is refused at every
//! attempt (DNS can change between attempts), redirects aren't followed,
//! and no proxy is used, unless `webhooks.allow_private_urls` is on.
//!
//! Runs on its own task, so a slow or hostile endpoint delays only its own
//! deliveries, never anything else monokulo does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use shared::webhook_sign::{is_disallowed_address, sign_payload};

use crate::db::{Attempt, AttemptOutcome, Database, DbError, DueDelivery};
use crate::settings::WebhookConfig;

/// What the worker tells the time by: the system's clock, or a test's.
pub trait Clock: Send + Sync {
    /// Seconds since the Unix epoch.
    fn now(&self) -> i64;
}

/// The system's clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        crate::now_unix()
    }
}

/// Most deliveries picked per batch, and most sent at once.
pub const DELIVERY_BATCH: u32 = 50;
const DELIVERY_CONCURRENCY: usize = 16;
/// Most deliveries one store gets in a batch, so one store's backlog or
/// slow endpoint can't hold the others up.
const DELIVERY_PER_STORE: u32 = 4;
/// How often old deliveries are deleted (`webhooks.keep_*_days`).
const PRUNE_EVERY: Duration = Duration::from_secs(10 * 60);
/// How much of an answer is kept: its first bytes.
pub const RESPONSE_EXCERPT_BYTES: usize = 512;

/// The wait after a failed attempt, `attempts_made` counting it: 1 minute
/// after the first, doubling up to 64 minutes.
pub fn backoff_seconds(attempts_made: u32) -> i64 {
    60 * (1i64 << attempts_made.saturating_sub(1).min(6))
}

/// The HTTP clients deliveries are sent with: one for each destination
/// policy, reused across deliveries (building one reads the system's
/// certificates). Each resolver is what makes the private-address check
/// hold: the addresses a name resolves to are checked by the very lookup
/// the connection is made from, so a name can't answer the check with one
/// address and the connection with another; a name with any private address
/// among its answers isn't connected to at all. Separate pools, so turning
/// `webhooks.allow_private_urls` off can't reuse a connection opened while
/// it was on.
#[derive(Clone)]
pub struct WebhookClient {
    client: reqwest::Client,
    private_client: reqwest::Client,
    allow_private: Arc<AtomicBool>,
}

impl WebhookClient {
    /// Redirects off (one to a private address must not be followed) and
    /// the checking resolver. Fails only if TLS can't be set up.
    pub fn build() -> reqwest::Result<Self> {
        let build = |allow| {
            let builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("monokulo-webhooks/", env!("CARGO_PKG_VERSION")))
                .dns_resolver(Arc::new(CheckingResolver {
                    allow_private: Arc::new(AtomicBool::new(allow)),
                }));
            // A proxy connects on our behalf, past the checking resolver.
            if allow { builder } else { builder.no_proxy() }.build()
        };
        Ok(Self {
            client: build(false)?,
            private_client: build(true)?,
            allow_private: Arc::new(AtomicBool::new(false)),
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
/// parses it first): [`check_ip_literal`] checks those.
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
            // Every answer has to pass: a name resolving to both a public
            // and a private address is the rebinding this defends against.
            if !allow_private && addrs.iter().any(|addr| is_disallowed_address(addr.ip())) {
                return Err(BoxError::from(DeliveryError::PrivateAddress));
            }
            Ok::<reqwest::dns::Addrs, BoxError>(Box::new(addrs.into_iter()))
        })
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("the URL's host could not be resolved: {0}")]
    UnresolvableHost(String),
    #[error("the URL is, or resolves to, a private or loopback address")]
    PrivateAddress,
}

/// Refuses a URL whose host is an IP literal of a private, loopback or
/// link-local address (unless `allow_private`): the half of the check a
/// resolver never sees.
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
        return Err(DeliveryError::PrivateAddress);
    }
    Ok(())
}

/// What a failed request is recorded as: the error and its causes, without
/// the URL (whose query string may hold a merchant's token).
fn request_error_text(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut text = if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else {
        e.to_string()
    };
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Headers monokulo sets itself, which a webhook's extra headers can't.
pub fn is_reserved_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("x-monokulo-")
        || [
            "host",
            "content-length",
            "content-type",
            "transfer-encoding",
            "connection",
            "user-agent",
        ]
        .contains(&name.as_str())
}

/// How one attempt went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptResult {
    pub delivered: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
    /// The start of the answer: its status line, content type and first
    /// [`RESPONSE_EXCERPT_BYTES`] bytes.
    pub response: Option<String>,
    pub signature: String,
    pub duration: Duration,
}

/// One attempt at `delivery`, signed with `secret` at `now`, within
/// `timeout` (resolving the host included).
pub async fn attempt_delivery(
    client: &WebhookClient,
    delivery: &DueDelivery,
    secret: &str,
    timeout: Duration,
    now: i64,
) -> AttemptResult {
    let signature = sign_payload(secret, now, delivery.body.as_bytes());
    let started = std::time::Instant::now();
    let sent =
        tokio::time::timeout(timeout, send(client, delivery, signature.clone(), timeout)).await;
    let duration = started.elapsed();
    let (status, error, response) = match sent {
        Ok(Ok((status, response))) => (Some(status), None, Some(response)),
        Ok(Err(error)) => (None, Some(error), None),
        Err(_) => (None, Some(format!("no answer within {timeout:?}")), None),
    };
    let delivered = status.is_some_and(|s| (200..300).contains(&s));
    let error = match (delivered, status, error) {
        (false, Some(status), None) => Some(format!("answered {status}")),
        (_, _, error) => error,
    };
    AttemptResult {
        delivered,
        status,
        error,
        response,
        signature,
        duration,
    }
}

/// Sends the request: the status and the start of the answer, or why
/// nothing answered.
async fn send(
    client: &WebhookClient,
    delivery: &DueDelivery,
    signature: String,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let url = url::Url::parse(&delivery.url).map_err(|e| format!("not a valid URL: {e}"))?;
    let allow_private = client.allows_private();
    check_ip_literal(&url, allow_private).map_err(|e| e.to_string())?;
    let http = if allow_private {
        &client.private_client
    } else {
        &client.client
    };
    let mut request = http
        .post(url)
        .timeout(timeout)
        .header("Content-Type", "application/json")
        .header("X-Monokulo-Signature", signature)
        .header("X-Monokulo-Event", &delivery.event_type)
        .header("X-Monokulo-Event-Id", &delivery.event_id)
        .body(delivery.body.clone());
    // Our trace, so a merchant who logs it can match their side to ours.
    if let Some(traceparent) = telemetry::trace::current_traceparent() {
        request = request.header(telemetry::trace::TRACEPARENT, traceparent);
    }
    if let Ok(serde_json::Value::Object(headers)) =
        serde_json::from_str::<serde_json::Value>(&delivery.extra_headers)
    {
        for (name, value) in headers {
            if let (false, Some(value)) = (is_reserved_header(&name), value.as_str()) {
                request = request.header(name, value);
            }
        }
    }
    let mut response = request.send().await.map_err(request_error_text)?;
    let status = response.status();
    let mut excerpt = format!(
        "HTTP/1.1 {} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    if let Some(content_type) = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        excerpt.push_str(&format!("\ncontent-type: {content_type}"));
    }
    // Only the first bytes: an endpoint can't make monokulo read (or
    // keep) more than that.
    let mut body = Vec::new();
    while body.len() < RESPONSE_EXCERPT_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) | Err(_) => break,
        }
    }
    body.truncate(RESPONSE_EXCERPT_BYTES);
    if !body.is_empty() {
        excerpt.push_str("\n\n");
        excerpt.push_str(&String::from_utf8_lossy(&body));
    }
    Ok((status.as_u16(), excerpt))
}

/// Sends one batch of due deliveries, concurrently, picked fairly across
/// stores, and records each attempt as it finishes. Returns how many were
/// picked: a full batch means more may be due now.
pub async fn run_delivery_tick(
    db: &Database,
    client: &WebhookClient,
    key: &crate::crypto::AtRestKey,
    config: &WebhookConfig,
    clock: &dyn Clock,
) -> Result<usize, DbError> {
    use futures_util::stream::{self, StreamExt as _};

    client.set_allow_private(config.allow_private_urls);
    let now = clock.now();
    let due = db
        .read(move |db| db.due_deliveries(now, DELIVERY_PER_STORE, DELIVERY_BATCH))
        .await?;
    let count = due.len();
    let max_attempts = config.max_attempts;
    let timeout = config.delivery_timeout;

    let mut attempts = stream::iter(due)
        .map(|delivery| {
            let span = tracing::info_span!(
                "webhook delivery",
                otel.kind = "client",
                store.id = %delivery.store_id,
                webhook.id = %delivery.webhook_id,
                order.id = %delivery.order_id,
                webhook.event = %delivery.event_type,
                webhook.event_id = %delivery.event_id,
                attempt = delivery.attempt_count.saturating_add(1),
            );
            tracing::Instrument::instrument(
                async move {
                    if delivery.attempt_count >= max_attempts {
                        // The most attempts allowed was lowered below what
                        // this one already had.
                        return (delivery, None);
                    }
                    let secret = match crate::crypto::decrypt(
                        key,
                        crate::crypto::Binding::WebhookSecret(delivery.webhook_id.as_str()),
                        &delivery.signing_secret_encrypted,
                    ) {
                        Ok(secret) => secret,
                        Err(e) => {
                            tracing::error!(error = %e, "a webhook's signing secret can't be read; giving up on its delivery");
                            return (delivery, None);
                        }
                    };
                    let at = clock.now();
                    let result = attempt_delivery(client, &delivery, &secret, timeout, at).await;
                    log_attempt(&delivery, &result, max_attempts);
                    (delivery, Some((result, at)))
                },
                span,
            )
        })
        .buffer_unordered(DELIVERY_CONCURRENCY);

    // Each attempt is recorded as soon as it's done: one slow endpoint in
    // the batch doesn't hold the others' outcomes back, and a later write
    // failing doesn't lose the earlier ones.
    let mut first_error = None;
    while let Some((delivery, attempted)) = attempts.next().await {
        let id = delivery.delivery_id;
        let written = match attempted {
            None => {
                let at = clock.now();
                db.write(move |db| db.give_up_delivery(id, at)).await
            }
            Some((result, at)) => {
                let made = delivery.attempt_count.saturating_add(1);
                let outcome = AttemptOutcome {
                    attempt: Attempt {
                        n: made,
                        at,
                        status: result.status,
                        error: result.error,
                        ms: u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX),
                        signature: result.signature,
                    },
                    delivered: result.delivered,
                    response: result.response,
                    // Timed from when the attempt was made, not from the
                    // start of a slow batch.
                    next_attempt_at: (made < max_attempts)
                        .then(|| clock.now().saturating_add(backoff_seconds(made))),
                };
                db.write(move |db| db.record_delivery_attempt(id, &outcome))
                    .await
            }
        };
        if let Err(e) = written {
            tracing::warn!(webhook.id = %delivery.webhook_id, error = %e, "a webhook delivery's attempt could not be recorded");
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(count),
    }
}

/// Deletes deliveries delivered longer ago than `webhooks.keep_delivered_days`
/// and ones given up on longer ago than `webhooks.keep_given_up_days`.
/// Returns how many of each.
pub async fn prune_deliveries(
    db: &Database,
    config: &WebhookConfig,
    now: i64,
) -> Result<(u64, u64), DbError> {
    let delivered_before = now.saturating_sub(config.keep_delivered_secs);
    let gave_up_before = now.saturating_sub(config.keep_given_up_secs);
    let (delivered, gave_up) = db
        .write(move |db| db.prune_deliveries(delivered_before, gave_up_before))
        .await?;
    if delivered + gave_up > 0 {
        tracing::info!(
            webhook.pruned_delivered = delivered,
            webhook.pruned_gave_up = gave_up,
            "old webhook deliveries deleted"
        );
    }
    Ok((delivered, gave_up))
}

/// One line per attempt, inside its `webhook delivery` span.
fn log_attempt(delivery: &DueDelivery, result: &AttemptResult, max_attempts: u32) {
    let status = result.status;
    let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);
    if result.delivered {
        tracing::info!(
            http.response.status_code = status,
            duration_ms,
            "webhook delivered"
        );
    } else if delivery.attempt_count.saturating_add(1) >= max_attempts {
        tracing::warn!(
            http.response.status_code = status,
            duration_ms,
            error = result.error.as_deref(),
            "webhook delivery failed; no more attempts"
        );
    } else {
        tracing::info!(
            http.response.status_code = status,
            duration_ms,
            error = result.error.as_deref(),
            "webhook delivery failed; will retry"
        );
    }
}

/// Delivers due webhooks for as long as monokulo runs: at once when woken
/// (an event queued, a merchant's "Send again"), and otherwise every few
/// seconds, as retries fall due.
#[expect(
    clippy::infinite_loop,
    reason = "a supervised loop: `shared::supervise` restarts one that returns"
)]
pub async fn run_delivery_loop(
    db: Database,
    key: crate::crypto::AtRestKey,
    settings: Arc<crate::settings::MonokuloSettings>,
    webhooks: Arc<super::Webhooks>,
) {
    let client = loop {
        match WebhookClient::build() {
            Ok(client) => break client,
            Err(e) => {
                tracing::error!(error = %e, "could not build the webhook HTTP client; trying again in 30 s");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };
    let mut pruned_at: Option<tokio::time::Instant> = None;
    loop {
        // Read each batch: a saved change applies to the next attempt.
        let config = settings.webhooks.load();
        if pruned_at.is_none_or(|at| at.elapsed() >= PRUNE_EVERY) {
            pruned_at = Some(tokio::time::Instant::now());
            if let Err(e) = prune_deliveries(&db, &config, crate::now_unix()).await {
                tracing::warn!(error = %e, "old webhook deliveries could not be deleted");
            }
        }
        let picked = match run_delivery_tick(&db, &client, &key, &config, &SystemClock).await {
            Ok(picked) => picked,
            Err(e) => {
                tracing::warn!(error = %e, "a webhook delivery batch failed");
                0
            }
        };
        // A full batch: more may be waiting, so straight on.
        if picked < DELIVERY_BATCH as usize {
            let _ = tokio::time::timeout(Duration::from_secs(5), webhooks.woken()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check is the resolver's: the addresses a name resolves to are
    /// what the connection is made to, and a name with a private address
    /// among its answers is refused before anything connects. Checking
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
        assert!(refused.contains("private or loopback"), "{refused}");

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
    /// its own: a loopback literal of either family is refused, a public
    /// one passes, and a name is left to the resolver.
    #[test]
    fn an_ip_literal_url_is_classified_before_the_request() {
        let url = |u: &str| url::Url::parse(u).unwrap();
        for private in [
            "http://[::1]:9999/hook",
            "http://127.0.0.1/hook",
            "http://169.254.169.254/latest",
            "http://10.0.0.1/hook",
            "http://[::ffff:127.0.0.1]/hook",
        ] {
            assert!(
                matches!(
                    check_ip_literal(&url(private), false),
                    Err(DeliveryError::PrivateAddress)
                ),
                "{private}"
            );
        }
        check_ip_literal(&url("https://[2606:4700:4700::1111]/hook"), false).unwrap();
        check_ip_literal(&url("https://1.1.1.1/hook"), false).unwrap();
        check_ip_literal(&url("https://shop.example/hook"), false).unwrap();
        check_ip_literal(&url("http://[::1]/hook"), true).unwrap();
    }

    /// The waits between attempts: 1 minute after the first failure,
    /// doubling to 64 minutes, and no further.
    #[test]
    fn the_wait_doubles_from_a_minute_to_64_minutes() {
        let minutes: Vec<i64> = (1..=9).map(|made| backoff_seconds(made) / 60).collect();
        assert_eq!(minutes, vec![1, 2, 4, 8, 16, 32, 64, 64, 64]);
    }

    #[test]
    fn monokulos_own_headers_are_reserved() {
        for name in [
            "X-Monokulo-Signature",
            "x-monokulo-event-id",
            "Content-Type",
            "Host",
            "User-Agent",
        ] {
            assert!(is_reserved_header(name), "{name}");
        }
        assert!(!is_reserved_header("Authorization"));
        assert!(!is_reserved_header("X-Api-Key"));
    }
}
