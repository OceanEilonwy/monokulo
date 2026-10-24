use super::store;
use crate::api::{self, prelude::*};
use crate::webhooks::test_delivery::{self, TestError};

api::resource!(
    "/api/v1/webhook-tests",
    rel = "webhook-tests",
    doc = "Where the store's server sends one of the store's webhooks a test delivery: a signed `webhook.test` event, to see its receiver take a real delivery without an order changing.\n\nA plugin's \"Test connection\" button and its smoke tests use it."
);

/// Which webhook to test.
#[api::input]
struct Input {
    /// The webhook to send it to (`wh_…`). Left out: the webhook the
    /// plugin registered when it connected (its `webhook_url`), or, when
    /// there's none, the store's only webhook.
    webhook_id: Option<String>,
}

/// How the test delivery went: what the webhook answered to its first
/// attempt.
///
/// Monokulo can't see whether the receiver checked the signature; a
/// receiver that follows the docs answers `2xx` only after it has, so
/// `delivered` is the closest sign that it did.
#[api::body]
struct Body {
    /// The webhook it was sent to.
    webhook_id: String,
    /// The test event's id, as sent in the body and in
    /// `X-Monokulo-Event-Id`: the receiver can match it with what it got.
    event_id: String,
    /// Whether the webhook answered with a `2xx`.
    delivered: bool,
    /// The HTTP status the webhook answered with; left out when nothing
    /// answered (it couldn't be reached, or took too long).
    status: Option<u16>,
    /// What went wrong, for a person (`answered 500`, `timed out`, `could
    /// not connect: …`); left out when it was delivered.
    error: Option<String>,
    /// How long the attempt took, in milliseconds.
    duration_ms: u64,
    /// The start of the answer: its status line, content type and first
    /// 512 bytes, as text. Left out when nothing answered.
    response: Option<String>,
    /// When Monokulo tries it again, in Unix seconds, as it does any
    /// delivery that fails: left out once delivered, or when it won't be.
    next_attempt_at: Option<i64>,
    store: Link<store::Path>,
}

/// Send a test webhook.
///
/// Monokulo sends the webhook a `webhook.test` delivery now, signed and
/// sent exactly as an order's events are, and answers once it has
/// answered (within `webhooks.delivery_timeout_ms`, 5 seconds unless the
/// admin changed it). It answers `200` whatever the webhook said:
/// `delivered` says whether it took it. The delivery is in the webhook's
/// deliveries on the store's settings page, marked as a test; one that
/// failed is tried again like any other.
///
/// At most 6 a minute for a store (the store's settings' "Send a test
/// webhook" included): past that, `429` `rate-limited`, with `retry_after`.
/// A `webhook_id` that isn't one of the store's webhooks, or a store with
/// no webhook to pick, is `404` `not-found`.
#[post(sk, form = "send-test-webhook", problems = [NotFound, RateLimited, Internal])]
async fn send(store: Store, instance: Instance, input: Input) -> api::Result<Body> {
    let state = instance.0;
    let (store_id, named) = (store.id.clone(), input.webhook_id);
    let picked = state
        .db
        .read(move |db| {
            let webhooks = db.list_webhooks(&store_id)?;
            let wanted = match named {
                Some(named) => Some(named),
                None => db.active_integration(&store_id)?.and_then(|i| i.webhook_id),
            };
            Ok::<_, crate::db::DbError>(match wanted {
                Some(wanted) => webhooks.into_iter().find(|w| w.id.as_str() == wanted),
                None if webhooks.len() == 1 => webhooks.into_iter().next(),
                None => None,
            })
        })
        .await;
    let webhook = match picked {
        Ok(Some(webhook)) => webhook,
        Ok(None) => {
            return Err(Problem::NotFound.detail(
                "There's no such webhook for this store. Name one of its webhooks in `webhook_id`, or connect the plugin again with a `webhook_url`.",
            ))
        }
        Err(_) => return Err(internal()),
    };
    match test_delivery::send(&state, &store, &webhook).await {
        Ok(delivery) => {
            tracing::info!(
                store.id = %store.id,
                webhook.id = %webhook.id,
                webhook.delivery = delivery.id,
                delivered = delivery.delivered_at.is_some(),
                "test webhook sent from the API"
            );
            Ok(Body {
                webhook_id: webhook.id.to_string(),
                event_id: delivery.event_id,
                delivered: delivery.delivered_at.is_some(),
                status: delivery.last_status_code,
                error: delivery.last_error,
                duration_ms: delivery.last_duration_ms.unwrap_or_default(),
                response: delivery.last_response,
                next_attempt_at: delivery.next_attempt_at,
                store: Link::new(store::Path {}),
            })
        }
        Err(TestError::RateLimited { retry_after }) => Err(Problem::RateLimited
            .detail(format!(
                "This store has sent {} test webhooks in the last minute; retry in {retry_after} s.",
                test_delivery::TESTS_PER_MINUTE
            ))
            .with("retry_after", serde_json::json!(retry_after))
            .header(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from(retry_after),
            )),
        Err(TestError::Failed(_)) => Err(internal()),
    }
}
