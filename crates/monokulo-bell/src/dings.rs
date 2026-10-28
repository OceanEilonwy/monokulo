//! The ding: an empty-of-meaning Web Push that tells a till to ask Square
//! for new payments now (`docs/design.md` §3). It carries no sale data, so
//! a lost, late, repeated or forged one costs the till one request to
//! Square at most; the till's timer is the backstop.
//!
//! Every ding is `{"wake":"square"}` encrypted to the till's subscription
//! (RFC 8291), signed with the Bell's VAPID key (RFC 8292), `Urgency:
//! high`, `TTL: 60` (a ding a minute late is worth nothing) and `Topic:
//! wake`, so a push service holding several for an offline till hands
//! over one. It is sent once: there is no queue and no retry.
//!
//! Each merchant's tills are sent at most
//! `bell.dings_per_merchant_per_minute` dings a minute, the rest dropped:
//! every ding is signed with the Bell's one key, which a push relay counts
//! as one sender, so without a share of its own one merchant could spend
//! the relay's budget for every merchant behind the Bell.
//!
//! A payment rings once: Square sends `payment.updated` of a payment that
//! stays `COMPLETED` (its processing fee filled in, a refund), and on an
//! iPhone or iPad a ding is an alert the till can't take back. The ids of
//! the payments rung are kept in memory only, for [`REMEMBERED_FOR`]
//! seconds and [`REMEMBERED_AT_MOST`] of them; a restart forgets them,
//! which costs one extra ding at most.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use outbound::Reach;
use push::{Message, Options, SendError, Sender, Topic, Ttl, Urgency};
use rate_limit::RateLimiter;
use tokio::sync::watch;

use crate::registrations::{Failed, Registration, RegistrationStore};
use crate::square::{LocationId, MerchantId, PaymentId};

/// What a ding says, before it is encrypted.
pub const WAKE: &[u8] = br#"{"wake":"square"}"#;

/// A ding's time to live.
pub const TTL: Ttl = Ttl::seconds(60);

/// A ding's topic.
pub const TOPIC: &str = "wake";

/// How long, in seconds, a payment rung is remembered: past the minute or
/// so in which Square sends a payment's later updates.
pub const REMEMBERED_FOR: i64 = 10 * 60;

/// The most payments remembered at once, the oldest forgotten first: a
/// bound on the memory a burst of payments takes.
pub const REMEMBERED_AT_MOST: usize = 10_000;

/// Sends dings to registrations, and keeps their failures.
pub struct Dings {
    sender: Sender,
    store: RegistrationStore,
    reach: Reach,
    failure_limit: u32,
    options: Options,
    /// Each merchant's dings this minute, against
    /// `bell.dings_per_merchant_per_minute`.
    per_merchant: RateLimiter<MerchantId>,
    rung: Mutex<Rung>,
    /// How many rings are running, for [`Dings::idle`].
    running: watch::Sender<usize>,
}

impl Dings {
    pub fn new(
        sender: Sender,
        store: RegistrationStore,
        reach: Reach,
        failure_limit: u32,
        dings_per_merchant_per_minute: u32,
    ) -> Result<Self, push::message::TopicError> {
        Ok(Dings {
            sender,
            store,
            reach,
            failure_limit,
            options: Options {
                urgency: Urgency::High,
                ttl: TTL,
                topic: Some(Topic::parse(TOPIC)?),
            },
            per_merchant: RateLimiter::new(dings_per_merchant_per_minute),
            rung: Mutex::new(Rung::default()),
            running: watch::Sender::new(0),
        })
    }

    /// Dings every till registered for `merchant_id`'s `location_id` for
    /// `payment_id`, on a task of its own: the webhook that asked is
    /// answered at once. A payment rung already, lately, isn't rung again.
    pub fn ring(
        self: &Arc<Self>,
        payment_id: PaymentId,
        merchant_id: MerchantId,
        location_id: LocationId,
    ) {
        let now = service_runtime::time::now_unix();
        let first = self
            .rung
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first(payment_id, now);
        if !first {
            tracing::debug!("a payment rung already: no dings");
            return;
        }
        let dings = Arc::clone(self);
        let running = Running::start(&self.running);
        tokio::spawn(async move {
            let _running = running;
            dings.ring_now(&merchant_id, &location_id).await;
        });
    }

    /// Waits until no ring is running: what shutdown waits for, so a ding
    /// started is sent.
    pub async fn idle(&self) {
        let mut running = self.running.subscribe();
        // The sender lives as long as `self`.
        let _ = running.wait_for(|count| *count == 0).await;
    }

    async fn ring_now(self: &Arc<Self>, merchant_id: &MerchantId, location_id: &LocationId) {
        let registrations = match self.store.at_location(merchant_id, location_id).await {
            Ok(registrations) => registrations,
            Err(error) => {
                tracing::error!(error = %error, "a payment's tills couldn't be read: no dings");
                return;
            }
        };
        let mut sending = tokio::task::JoinSet::new();
        for registration in registrations {
            let (dings, merchant_id) = (Arc::clone(self), merchant_id.clone());
            sending.spawn(async move { dings.ding(&merchant_id, registration).await });
        }
        let rung = sending.len();
        while sending.join_next().await.is_some() {}
        tracing::debug!(rung, "dings sent");
    }

    /// One ding to `merchant_id`'s `registration`, and what its answer
    /// means for it.
    async fn ding(&self, merchant_id: &MerchantId, registration: Registration) {
        let now = service_runtime::time::now_unix();
        let endpoint = &registration.subscription.endpoint;
        let outcome = match outbound::check_host(endpoint.as_url(), self.reach) {
            // Counted only when it is to be sent: a ding refused here
            // costs its merchant's share nothing. Over it, the ding is
            // dropped, not the till's failure, as one a push service
            // limits.
            Ok(()) if !self.per_merchant.check(merchant_id.clone(), now) => {
                tracing::warn!(registration = %registration.id, host = endpoint.host(), "the merchant's dings for this minute were spent: a ding was dropped");
                return;
            }
            Ok(()) => match push::encrypt(&registration.subscription.keys, WAKE) {
                Ok(encrypted) => {
                    self.sender
                        .send(endpoint, &Message::Encrypted(encrypted), &self.options, now)
                        .await
                }
                Err(error) => {
                    tracing::error!(registration = %registration.id, error = %error, "a ding couldn't be encrypted");
                    return;
                }
            },
            Err(error) => {
                tracing::warn!(registration = %registration.id, host = endpoint.host(), error = %error, "a ding wasn't sent");
                self.failed(&registration).await;
                return;
            }
        };
        let (id, host) = (&registration.id, endpoint.host());
        match outcome {
            Ok(_) => {
                if let Err(error) = self.store.delivered(id, now).await {
                    tracing::error!(registration = %id, error = %error, "a delivered ding couldn't be recorded");
                }
            }
            Err(SendError::Gone) => match self.store.delete(id).await {
                Ok(_) => {
                    tracing::info!(registration = %id, host, "the push service says the subscription is gone: registration deleted")
                }
                Err(error) => {
                    tracing::error!(registration = %id, error = %error, "a gone registration couldn't be deleted")
                }
            },
            // The push service is limiting the Bell, not refusing the
            // till: not the subscription's failure.
            Err(SendError::RateLimited { retry_after }) => {
                tracing::warn!(registration = %id, host, ?retry_after, "the push service is limiting the Bell: a ding was dropped");
            }
            Err(error) => {
                tracing::warn!(registration = %id, host, error = %error, "a ding wasn't taken");
                self.failed(&registration).await;
            }
        }
    }

    async fn failed(&self, registration: &Registration) {
        let id = &registration.id;
        match self.store.failed(id, self.failure_limit).await {
            Ok(Failed::Counted(_) | Failed::Gone) => {}
            Ok(Failed::Deleted) => tracing::info!(
                registration = %id,
                host = registration.subscription.endpoint.host(),
                limit = self.failure_limit,
                "dings failed too often in a row: registration deleted"
            ),
            Err(error) => {
                tracing::error!(registration = %id, error = %error, "a failed ding couldn't be recorded");
            }
        }
    }
}

/// The payments rung lately, oldest first.
#[derive(Default)]
struct Rung {
    order: VecDeque<(i64, PaymentId)>,
    ids: HashSet<PaymentId>,
}

impl Rung {
    /// Whether `id` is rung for the first time at `now` (and so is
    /// remembered from now): false for one remembered still.
    fn first(&mut self, id: PaymentId, now: i64) -> bool {
        while let Some((at, _)) = self.order.front() {
            if now - at < REMEMBERED_FOR {
                break;
            }
            self.forget_oldest();
        }
        if self.ids.contains(&id) {
            return false;
        }
        if self.order.len() >= REMEMBERED_AT_MOST {
            self.forget_oldest();
        }
        self.ids.insert(id.clone());
        self.order.push_back((now, id));
        true
    }

    fn forget_oldest(&mut self) {
        if let Some((_, id)) = self.order.pop_front() {
            self.ids.remove(&id);
        }
    }
}

/// One running ring, counted in [`Dings::idle`]'s count until dropped,
/// however its task ends.
struct Running(watch::Sender<usize>);

impl Running {
    fn start(running: &watch::Sender<usize>) -> Self {
        running.send_modify(|count| *count += 1);
        Running(running.clone())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count -= 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn payment(n: usize) -> PaymentId {
        PaymentId::try_from(format!("P{n}")).unwrap()
    }

    #[test]
    fn a_payment_rings_once_until_it_is_forgotten() {
        let mut rung = Rung::default();
        assert!(rung.first(payment(1), NOW));
        assert!(!rung.first(payment(1), NOW + 1));
        assert!(rung.first(payment(2), NOW + 1));
        assert!(!rung.first(payment(1), NOW + REMEMBERED_FOR - 1));
        // Remembered from its first ring, not its last.
        assert!(rung.first(payment(1), NOW + REMEMBERED_FOR));
        assert!(!rung.first(payment(2), NOW + REMEMBERED_FOR));
        assert_eq!(rung.order.len(), rung.ids.len());
    }

    #[test]
    fn at_most_so_many_payments_are_remembered_the_oldest_forgotten_first() {
        let mut rung = Rung::default();
        for n in 0..REMEMBERED_AT_MOST {
            assert!(rung.first(payment(n), NOW));
        }
        assert!(rung.first(payment(REMEMBERED_AT_MOST), NOW));
        assert_eq!(rung.ids.len(), REMEMBERED_AT_MOST);
        assert_eq!(rung.order.len(), REMEMBERED_AT_MOST);
        // The oldest went; the next oldest is still there.
        assert!(!rung.first(payment(1), NOW));
        assert!(rung.first(payment(0), NOW));
    }
}
