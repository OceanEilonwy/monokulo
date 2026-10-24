//! Test deliveries: a `webhook.test` event ([`super::body::test_body`])
//! sent to one of a store's webhooks when the merchant asks ("Send a test
//! webhook" in the store's settings) or the store's plugin does (the API's
//! `send-test-webhook` form, `crate::api::v1::webhook_tests`), so either can
//! see a real signed delivery arrive without an order changing.
//!
//! A test delivery is a delivery like any other: the same headers and
//! signature, sent by the same client, recorded in the webhook's
//! deliveries, and tried again on the same schedule when it fails. The
//! only difference is its first attempt: made at once, while the caller
//! waits, so the caller learns how it went.
//!
//! At most [`TESTS_PER_MINUTE`] a minute for a store, from the dashboard and
//! the API together: each is a request to a URL the merchant chose, made by
//! this Monokulo.

use std::collections::HashMap;

use parking_lot::Mutex;

use super::body::{self, LinkBases, TEST_EVENT};
use super::delivery::{attempt_delivery, backoff_seconds, log_attempt};
use crate::db::{
    Attempt, AttemptOutcome, ConnectionId, DbError, DeliveryRow, DueDelivery, NewTestDelivery,
    StoreConnectionRow, WebhookRow,
};

/// Most test deliveries a store may send in a minute.
pub const TESTS_PER_MINUTE: u32 = 6;

/// How many test deliveries each store has sent in the current minute.
#[derive(Default)]
pub struct Limiter {
    /// Each store's minute (Unix seconds / 60) and its count in it.
    minutes: Mutex<HashMap<ConnectionId, (i64, u32)>>,
}

impl Limiter {
    /// Counts one test delivery for `store` at `now`, or, when it has sent
    /// [`TESTS_PER_MINUTE`] this minute already, the seconds until the next.
    pub fn take(&self, store: &ConnectionId, now: i64) -> Result<(), u64> {
        let minute = now.div_euclid(60);
        let mut minutes = self.minutes.lock();
        // Only this minute's counts matter: drop the rest as they pile up.
        if minutes.len() > 1024 {
            minutes.retain(|_, (at, _)| *at == minute);
        }
        let (at, count) = minutes.entry(store.clone()).or_insert((minute, 0));
        if *at != minute {
            (*at, *count) = (minute, 0);
        }
        if *count >= TESTS_PER_MINUTE {
            return Err(u64::try_from(60 - now.rem_euclid(60)).unwrap_or(60));
        }
        *count += 1;
        Ok(())
    }
}

/// Why a test delivery wasn't sent.
#[derive(Debug, thiserror::Error)]
pub enum TestError {
    /// The store sent [`TESTS_PER_MINUTE`] this minute already.
    #[error(
        "this store has sent {TESTS_PER_MINUTE} test webhooks this minute; wait {retry_after} s"
    )]
    RateLimited { retry_after: u64 },
    /// It couldn't be sent (no HTTP client, an unreadable signing secret)
    /// or recorded: logged, nothing sent or nothing kept.
    #[error("{0}")]
    Failed(String),
}

impl From<DbError> for TestError {
    fn from(e: DbError) -> Self {
        TestError::Failed(e.to_string())
    }
}

/// Sends a test delivery to `store`'s `webhook` now and records it: the
/// delivery as recorded, with its first attempt.
pub async fn send(
    state: &crate::http::AppState,
    store: &StoreConnectionRow,
    webhook: &WebhookRow,
) -> Result<DeliveryRow, TestError> {
    // The abuse protection's clock: stopped where the API's examples are
    // recorded, so the wait they show is the same every time.
    state
        .webhooks
        .tests
        .take(&store.id, state.abuse.now())
        .map_err(|retry_after| TestError::RateLimited { retry_after })?;
    let config = state.settings.webhooks.load();
    let client = state.webhooks.client().await.map_err(|e| {
        tracing::error!(error = %e, "could not build the webhook HTTP client");
        TestError::Failed(e.to_string())
    })?;
    client.set_allow_private(config.allow_private_urls);
    let secret = crate::crypto::decrypt(
        &state.encryption_key,
        crate::crypto::Binding::WebhookSecret(webhook.id.as_str()),
        &webhook.signing_secret_encrypted,
    )
    .map_err(|e| {
        tracing::error!(webhook.id = %webhook.id, error = %e, "a webhook's signing secret can't be read; no test sent");
        TestError::Failed(e.to_string())
    })?;
    let at = crate::now_unix();
    let event_id = format!("evt_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
    let bases = LinkBases {
        web: state.settings.public_uri(),
        onion: state.onion_uri(),
    };
    let due = DueDelivery {
        delivery_id: 0,
        webhook_id: webhook.id.clone(),
        store_id: store.id.clone(),
        order_id: None,
        body: body::test_body(&event_id, at, store, webhook.id.as_str(), &bases),
        event_id,
        event_type: TEST_EVENT.to_owned(),
        attempt_count: 0,
        url: webhook.url.clone(),
        extra_headers: webhook.extra_headers.clone(),
        signing_secret_encrypted: webhook.signing_secret_encrypted.clone(),
    };
    let span = tracing::info_span!(
        "webhook delivery",
        otel.kind = "client",
        store.id = %store.id,
        webhook.id = %webhook.id,
        webhook.event = TEST_EVENT,
        webhook.event_id = %due.event_id,
        attempt = 1,
    );
    let result = tracing::Instrument::instrument(
        async {
            let result = attempt_delivery(client, &due, &secret, config.delivery_timeout, at).await;
            log_attempt(&due, &result, config.max_attempts);
            result
        },
        span,
    )
    .await;
    let outcome = AttemptOutcome {
        attempt: Attempt {
            n: 1,
            at,
            status: result.status,
            error: result.error,
            ms: u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX),
            signature: result.signature,
        },
        delivered: result.delivered,
        response: result.response,
        // Tried again like any delivery: a receiver that was down for the
        // test still gets it once it's back.
        next_attempt_at: (config.max_attempts > 1)
            .then(|| crate::now_unix().saturating_add(backoff_seconds(1))),
    };
    let (webhook_id, store_id) = (webhook.id.clone(), store.id.clone());
    let delivery = state
        .db
        .write(move |db| {
            let id = db.record_test_delivery(&NewTestDelivery {
                webhook_id: &webhook_id,
                event_id: &due.event_id,
                body: &due.body,
                at,
                outcome: &outcome,
            })?;
            db.get_delivery(&store_id, &webhook_id, id)
        })
        .await
        .map_err(|e| {
            tracing::error!(webhook.id = %webhook.id, error = %e, "a test webhook delivery could not be recorded");
            TestError::from(e)
        })?;
    // Deleted in between (the webhook, by another tab): sent, not kept.
    delivery.ok_or_else(|| TestError::Failed("the webhook was deleted".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Six a minute for a store, the seventh told how long until the next
    /// minute; another store has its own six; a new minute starts over.
    #[test]
    fn a_store_sends_six_tests_a_minute() {
        let limiter = Limiter::default();
        let (bakery, florist) = (ConnectionId::new("bakery"), ConnectionId::new("florist"));
        let minute = 1_767_225_600;
        for _ in 0..TESTS_PER_MINUTE {
            assert_eq!(limiter.take(&bakery, minute + 15), Ok(()));
        }
        assert_eq!(limiter.take(&bakery, minute + 15), Err(45));
        assert_eq!(limiter.take(&bakery, minute + 59), Err(1));
        assert_eq!(limiter.take(&florist, minute + 59), Ok(()));
        assert_eq!(limiter.take(&bakery, minute + 60), Ok(()));
    }
}
