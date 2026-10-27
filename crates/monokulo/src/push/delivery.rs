//! The push delivery worker: sends due pushes (`db::Db::due_push_deliveries`)
//! to connected apps' push targets, signed with this instance's VAPID key
//! and, for an `encrypted` target, encrypted afresh at each attempt
//! (RFC 8291 wants a fresh salt and key for every message), and records
//! each attempt.
//!
//! The schedule: up to `push.max_attempts` attempts (5), each allowed
//! `push.delivery_timeout_ms` (5 s), waiting 5, 20, 80 and 320 seconds
//! between them, or what the push service's `Retry-After` says, and never
//! past the push's TTL: a push whose TTL ends before its next attempt is
//! given up on, and one found due after it is given up on without a
//! request (`expired before it could be sent`). A push a minute late is
//! worth little; a sale's "seen" an hour late is worse than none.
//!
//! What an answer says about the target: `404` or `410` means the
//! subscription is gone, and the target is deleted at once;
//! `push.target_failure_limit` (20) failures in a row delete it too (a
//! `429` doesn't count: the push service is limiting this sender, not
//! refusing the device). The app registers again when it next runs.
//!
//! An endpoint is a capability: whoever has it can push to the device
//! (with its keys) or at least wake it. So it is never written to a log
//! line or a delivery's error, only its host.
//!
//! Requests go out as webhooks do (`crate::outbound`): no private
//! addresses unless `webhooks.allow_private_urls`, no redirects, no proxy,
//! never over Tor. Runs on its own task, so a slow push service delays
//! only its own pushes.

use std::sync::Arc;
use std::time::Duration;

use ::push::{
    encrypt, EncryptError, Message, Options, PlainJson, SendError, Sender, SubscriptionKeys, Ttl,
};

use crate::db::{
    Database, DbError, DuePush, PushAttempt, PushResult, PushTargetId, TargetAfter, TargetKind,
    EXPIRED_UNSENT,
};
use crate::outbound::{check_host, request_error_text, Outbound, Reach};
use crate::settings::PushConfig;
use crate::webhooks::delivery::{Clock, SystemClock};

/// Most pushes picked per batch, and most sent at once (one target's one
/// at a time).
pub const DELIVERY_BATCH: u32 = 50;
const DELIVERY_CONCURRENCY: usize = 16;
/// How often old pushes are deleted (`push.keep_days`).
const PRUNE_EVERY: Duration = Duration::from_secs(10 * 60);

/// Why a push with no attempts left is given up on: `push.max_attempts`
/// was lowered below what it had made.
pub const NO_ATTEMPTS_LEFT: &str = "no attempts left";

/// The wait after a failed attempt, `attempts_made` counting it: 5 seconds
/// after the first, four times longer each time, up to 320 seconds.
pub fn backoff_seconds(attempts_made: u32) -> i64 {
    5 * (1i64 << (2 * attempts_made.saturating_sub(1).min(3)))
}

/// What pushes are sent with: this instance's VAPID key, a sender for each
/// [`Reach`] over the outbound clients (each sender reuses its signed
/// tokens per push service), and the `sub` claim they carry.
pub struct PushClient {
    outbound: Outbound,
    vapid: Arc<::push::VapidKey>,
    contact: Option<::push::Contact>,
    public: Sender,
    any_address: Sender,
}

impl PushClient {
    /// Fails only if TLS can't be set up.
    pub fn build(
        vapid: Arc<::push::VapidKey>,
        contact: Option<::push::Contact>,
    ) -> reqwest::Result<Self> {
        let outbound = Outbound::build(concat!("monokulo-push/", env!("CARGO_PKG_VERSION")))?;
        let sender = |reach| {
            Sender::new(
                outbound.client(reach).clone(),
                Arc::clone(&vapid),
                contact.clone(),
            )
        };
        Ok(PushClient {
            public: sender(Reach::Public),
            any_address: sender(Reach::AnyAddress),
            outbound,
            vapid,
            contact,
        })
    }

    /// Signs with `contact` from now on, when it isn't what is signed with
    /// already: `server.public_uri` changed. Tokens signed before are
    /// dropped with the senders that kept them.
    pub fn set_contact(&mut self, contact: Option<::push::Contact>) {
        if contact == self.contact {
            return;
        }
        for (reach, sender) in [
            (Reach::Public, &mut self.public),
            (Reach::AnyAddress, &mut self.any_address),
        ] {
            *sender = Sender::new(
                self.outbound.client(reach).clone(),
                Arc::clone(&self.vapid),
                contact.clone(),
            );
        }
        self.contact = contact;
    }

    fn sender(&self, reach: Reach) -> &Sender {
        match reach {
            Reach::Public => &self.public,
            Reach::AnyAddress => &self.any_address,
        }
    }
}

/// The `sub` claim a push carries (RFC 8292 §2.1): this Monokulo's web
/// address, how a push service's operator reaches whoever runs it; none
/// while it has none, or one that isn't `https` (which the claim takes).
pub fn contact(settings: &crate::settings::MonokuloSettings) -> Option<::push::Contact> {
    settings
        .public_uri()
        .and_then(|uri| ::push::Contact::parse(&uri).ok())
}

/// What became of one due push.
enum Done {
    /// Given up on before any request: why.
    GaveUp(String),
    /// Attempted.
    Attempted(PushAttempt),
    /// Its push service said the subscription is gone.
    Gone,
}

/// Sends one batch of due pushes, concurrently but one target's one at a
/// time in queue order, and records each as it finishes. `reach` is what `webhooks.allow_private_urls` allows. Returns
/// how many were picked: a full batch means more may be due now.
pub async fn run_delivery_tick(
    db: &Database,
    client: &PushClient,
    config: &PushConfig,
    reach: Reach,
    clock: &dyn Clock,
) -> Result<usize, DbError> {
    use futures_util::stream::{self, StreamExt as _};

    let now = clock.now();
    let due = db
        .read(move |db| db.due_push_deliveries(now, DELIVERY_BATCH))
        .await?;
    let count = due.len();
    let mut first_error = None;
    let mut readable = Vec::with_capacity(count);
    for due in due {
        match due {
            Ok(push) => readable.push(push),
            Err(unreadable) => {
                tracing::error!(
                    push_target.id = %unreadable.target_id,
                    error = %unreadable.error,
                    "a push can't be read; giving up on it"
                );
                let at = clock.now();
                let id = unreadable.id;
                let error = format!("unreadable: {}", unreadable.error);
                if let Err(e) = db.write(move |db| db.give_up_push(id, at, &error)).await {
                    first_error.get_or_insert(e);
                }
            }
        }
    }

    // One target's pushes go one after another, as they were queued, and
    // different targets' at once: a Live Activity shows whichever state
    // lands last, so a till's newer sale must land after its older one.
    let mut attempts = stream::iter(by_target(readable))
        .map(|pushes| {
            Box::pin(stream::iter(pushes).then(move |push| {
                let span = tracing::info_span!(
                    "push delivery",
                    otel.kind = "client",
                    store.id = %push.target.store_id,
                    push_target.id = %push.target.id,
                    push_target.kind = push.target.kind.name().as_str(),
                    push.event = push.event.as_str(),
                    push.host = push.target.endpoint.host(),
                    attempt = push.attempt_count.saturating_add(1),
                );
                tracing::Instrument::instrument(
                    async move {
                        let done = deliver(client, &push, config, reach, clock).await;
                        (push, done)
                    },
                    span,
                )
            }))
        })
        .flatten_unordered(DELIVERY_CONCURRENCY);

    // Each push is recorded as soon as it's done: one slow push service in
    // the batch doesn't hold the others' outcomes back, and a later write
    // failing doesn't lose the earlier ones.
    while let Some((push, done)) = attempts.next().await {
        let (id, target_id) = (push.id, push.target.id.clone());
        let written = match done {
            Done::GaveUp(reason) => {
                let at = clock.now();
                db.write(move |db| db.give_up_push(id, at, &reason)).await
            }
            Done::Gone => {
                tracing::info!(
                    push_target.id = %target_id,
                    push.host = push.target.endpoint.host(),
                    "the push service says the subscription is gone; push target deleted"
                );
                db.write(move |db| db.delete_dead_push_target(&target_id).map(|_| ()))
                    .await
            }
            Done::Attempted(attempt) => {
                let after = db
                    .write(move |db| db.record_push_attempt(id, &target_id, &attempt))
                    .await;
                if let Ok(TargetAfter::Deleted) = after {
                    tracing::warn!(
                        push_target.id = %push.target.id,
                        push.host = push.target.endpoint.host(),
                        failures = config.target_failure_limit,
                        "a push target failed too many times in a row; deleted"
                    );
                }
                after.map(|_| ())
            }
        };
        if let Err(e) = written {
            tracing::warn!(push_target.id = %push.target.id, error = %e, "a push's attempt could not be recorded");
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(count),
    }
}

/// `due` by target, each target's in the order they were queued, the
/// targets in the order their first push came due.
fn by_target(due: Vec<DuePush>) -> Vec<Vec<DuePush>> {
    let mut groups: Vec<Vec<DuePush>> = Vec::new();
    let mut index: std::collections::HashMap<PushTargetId, usize> =
        std::collections::HashMap::new();
    for push in due {
        match index.entry(push.target.id.clone()) {
            std::collections::hash_map::Entry::Occupied(at) => groups[*at.get()].push(push),
            std::collections::hash_map::Entry::Vacant(at) => {
                at.insert(groups.len());
                groups.push(vec![push]);
            }
        }
    }
    for pushes in &mut groups {
        pushes.sort_by_key(|push| push.id);
    }
    groups
}

/// One due push: given up on, or attempted once.
async fn deliver(
    client: &PushClient,
    push: &DuePush,
    config: &PushConfig,
    reach: Reach,
    clock: &dyn Clock,
) -> Done {
    if push.attempt_count >= config.max_attempts {
        return Done::GaveUp(NO_ATTEMPTS_LEFT.to_owned());
    }
    let at = clock.now();
    let Some(ttl) = remaining(push.expires_at, at) else {
        tracing::info!("a push expired before it could be sent; given up");
        return Done::GaveUp(EXPIRED_UNSENT.to_owned());
    };
    let message = match message(push) {
        Ok(message) => message,
        // Every payload is made to fit (`push::payload`): one that doesn't
        // is a bug, and sending it again won't help.
        Err(error) => {
            tracing::error!(error = %error, "a push can't be sent; given up");
            return Done::GaveUp(error);
        }
    };
    let options = Options {
        urgency: push.urgency,
        ttl,
        topic: Some(push.topic.clone()),
    };
    let started = std::time::Instant::now();
    let sent = match check_host(push.target.endpoint.as_url(), reach) {
        Err(refused) => Err(Failure::counted(refused.to_string())),
        Ok(()) => match tokio::time::timeout(
            config.delivery_timeout,
            client
                .sender(reach)
                .send(&push.target.endpoint, &message, &options, at),
        )
        .await
        {
            Ok(Ok(sent)) => Ok(sent.status.as_u16()),
            Ok(Err(SendError::Gone)) => return Done::Gone,
            Ok(Err(error)) => Err(Failure::of(error)),
            Err(_) => Err(Failure::counted(format!(
                "timed out: no answer within {:?}",
                config.delivery_timeout
            ))),
        },
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let made = push.attempt_count.saturating_add(1);
    let attempt = match sent {
        Ok(status) => {
            tracing::info!(
                http.response.status_code = status,
                duration_ms,
                "push delivered"
            );
            PushAttempt {
                at,
                status: Some(status),
                result: PushResult::Delivered,
            }
        }
        Err(failure) => {
            let retry_at = match failure.retry {
                Retry::Never => None,
                Retry::After(wait) => Some(wait),
                Retry::Backoff => Some(backoff_seconds(made)),
            }
            .filter(|_| made < config.max_attempts)
            // Timed from when the attempt was made, never past the TTL.
            .map(|wait| clock.now().saturating_add(wait))
            .filter(|next| *next < push.expires_at);
            if failure.retry == Retry::Never {
                tracing::error!(
                    http.response.status_code = failure.status,
                    duration_ms,
                    error = %failure.error,
                    "push refused for good; given up"
                );
            } else if retry_at.is_none() {
                tracing::warn!(
                    http.response.status_code = failure.status,
                    duration_ms,
                    error = %failure.error,
                    "push failed; no more attempts"
                );
            } else {
                tracing::info!(
                    http.response.status_code = failure.status,
                    duration_ms,
                    error = %failure.error,
                    "push failed; will retry"
                );
            }
            PushAttempt {
                at,
                status: failure.status,
                result: PushResult::Failed {
                    error: failure.error,
                    retry_at,
                    failure_limit: failure.counts.then_some(config.target_failure_limit),
                },
            }
        }
    };
    Done::Attempted(attempt)
}

/// What is left of a push's TTL at `now`; `None` once it has run out.
fn remaining(expires_at: i64, now: i64) -> Option<Ttl> {
    let left = expires_at.saturating_sub(now);
    (left > 0).then(|| Ttl::seconds(u32::try_from(left).unwrap_or(u32::MAX)))
}

/// What is sent to `push`'s target: its payload encrypted to the device
/// afresh, or as it is for a Live Activity. The error says why it can't be.
fn message(push: &DuePush) -> Result<Message, String> {
    match &push.target.kind {
        TargetKind::Encrypted { p256dh, auth } => {
            let keys = SubscriptionKeys {
                p256dh: *p256dh,
                auth: auth.clone(),
            };
            encrypt(&keys, push.payload.as_bytes())
                .map(Message::Encrypted)
                .map_err(|EncryptError::TooLarge(bytes)| {
                    format!("its payload is too large to encrypt ({bytes} bytes)")
                })
        }
        TargetKind::LiveActivity { .. } => PlainJson::from_slice(push.payload.as_bytes())
            .map(Message::PlainJson)
            .map_err(|e| format!("its payload can't be sent: {e}")),
    }
}

/// When a failed push is tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// Never: the push service won't ever take it.
    Never,
    /// After this many seconds, as the push service said.
    After(i64),
    /// After the schedule's wait.
    Backoff,
}

/// A failed attempt: why, the status if anything answered, when to try
/// again, and whether it counts towards the target's run of failures.
struct Failure {
    error: String,
    status: Option<u16>,
    retry: Retry,
    counts: bool,
}

impl Failure {
    /// A failure of the target's own (no answer, a refused address),
    /// tried again on the schedule.
    fn counted(error: String) -> Self {
        Failure {
            error,
            status: None,
            retry: Retry::Backoff,
            counts: true,
        }
    }

    fn of(error: SendError) -> Self {
        let wait = |after: Option<Duration>| {
            after.map_or(Retry::Backoff, |after| {
                Retry::After(i64::try_from(after.as_secs()).unwrap_or(i64::MAX))
            })
        };
        let text = error.to_string();
        match error {
            // Handled before: the target is deleted.
            SendError::Gone => Failure::counted(text),
            SendError::TooLarge => Failure {
                error: text,
                status: Some(413),
                retry: Retry::Never,
                counts: false,
            },
            SendError::RateLimited { retry_after } => Failure {
                error: text,
                status: Some(429),
                retry: wait(retry_after),
                counts: false,
            },
            SendError::Server {
                status,
                retry_after,
            } => Failure {
                error: text,
                status: Some(status.as_u16()),
                retry: wait(retry_after),
                counts: true,
            },
            SendError::Refused { status } => Failure {
                error: text,
                status: Some(status.as_u16()),
                retry: Retry::Backoff,
                counts: true,
            },
            SendError::Request(error) => Failure::counted(request_error_text(error)),
            // This instance's own key failing to sign: nothing about the
            // target, and nothing a retry would change.
            SendError::Vapid(_) => Failure {
                error: text,
                status: None,
                retry: Retry::Never,
                counts: false,
            },
        }
    }
}

/// Deletes pushes delivered, given up on or superseded longer ago than
/// `push.keep_days`. Returns how many.
pub async fn prune_deliveries(
    db: &Database,
    config: &PushConfig,
    now: i64,
) -> Result<u64, DbError> {
    let before = now.saturating_sub(config.keep_secs);
    let pruned = db.write(move |db| db.prune_push_deliveries(before)).await?;
    if pruned > 0 {
        tracing::info!(push.pruned = pruned, "old pushes deleted");
    }
    Ok(pruned)
}

/// Delivers due pushes for as long as monokulo runs: at once when woken
/// (a sale event queued), and otherwise every few seconds, as retries fall
/// due.
#[expect(
    clippy::infinite_loop,
    reason = "a supervised loop: `service_runtime::supervise` restarts one that returns"
)]
pub async fn run_delivery_loop(
    db: Database,
    settings: Arc<crate::settings::MonokuloSettings>,
    push: Arc<super::Push>,
) {
    let mut client = loop {
        match PushClient::build(push.vapid(), contact(&settings)) {
            Ok(client) => break client,
            Err(e) => {
                tracing::error!(error = %e, "could not build the push HTTP client; trying again in 30 s");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };
    let mut pruned_at: Option<tokio::time::Instant> = None;
    loop {
        // Read each batch: a saved change applies to the next attempt.
        let config = settings.push.load();
        let reach = Reach::allowing_private(settings.webhooks.load().allow_private_urls);
        client.set_contact(contact(&settings));
        if pruned_at.is_none_or(|at| at.elapsed() >= PRUNE_EVERY) {
            pruned_at = Some(tokio::time::Instant::now());
            if let Err(e) = prune_deliveries(&db, &config, crate::now_unix()).await {
                tracing::warn!(error = %e, "old pushes could not be deleted");
            }
        }
        let picked = match run_delivery_tick(&db, &client, &config, reach, &SystemClock).await {
            Ok(picked) => picked,
            Err(e) => {
                tracing::warn!(error = %e, "a push delivery batch failed");
                0
            }
        };
        // A full batch: more may be waiting, so straight on.
        if picked < DELIVERY_BATCH as usize {
            let _ = tokio::time::timeout(Duration::from_secs(5), push.woken()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The waits between attempts: 5 seconds after the first failure, four
    /// times longer each time, to 320 seconds, and no further.
    #[test]
    fn the_wait_grows_from_5_to_320_seconds() {
        let waits: Vec<i64> = (1..=6).map(backoff_seconds).collect();
        assert_eq!(waits, vec![5, 20, 80, 320, 320, 320]);
        assert_eq!(backoff_seconds(0), 5);
        assert_eq!(backoff_seconds(u32::MAX), 320);
    }

    #[test]
    fn what_is_left_of_a_ttl() {
        assert_eq!(remaining(100, 40), Some(Ttl::seconds(60)));
        assert_eq!(remaining(100, 99), Some(Ttl::seconds(1)));
        assert_eq!(remaining(100, 100), None);
        assert_eq!(remaining(100, 1_000), None);
        assert_eq!(remaining(i64::MAX, 0), Some(Ttl::seconds(u32::MAX)));
    }

    /// What each answer means for the push and its target.
    #[test]
    fn each_answer_is_retried_or_not_and_counted_or_not() {
        let status = |code| reqwest::StatusCode::from_u16(code).unwrap();
        let cases = [
            (SendError::TooLarge, Retry::Never, false),
            (
                SendError::RateLimited {
                    retry_after: Some(Duration::from_secs(7)),
                },
                Retry::After(7),
                false,
            ),
            (
                SendError::RateLimited { retry_after: None },
                Retry::Backoff,
                false,
            ),
            (
                SendError::Server {
                    status: status(503),
                    retry_after: Some(Duration::from_secs(30)),
                },
                Retry::After(30),
                true,
            ),
            (
                SendError::Server {
                    status: status(500),
                    retry_after: None,
                },
                Retry::Backoff,
                true,
            ),
            (
                SendError::Refused {
                    status: status(401),
                },
                Retry::Backoff,
                true,
            ),
        ];
        for (error, retry, counts) in cases {
            let what = error.to_string();
            let failure = Failure::of(error);
            assert_eq!((failure.retry, failure.counts), (retry, counts), "{what}");
        }
    }
}
