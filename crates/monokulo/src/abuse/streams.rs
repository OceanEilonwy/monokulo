//! A cap on how many live-update streams (`/pay/{pk}/orders/{id}/events`)
//! one client may hold open at once against one store.
//!
//! The rate limiter counts requests as they arrive, which bounded the old
//! status polling (every check was a request) but not a stream: one request
//! stays open until its order finishes, holding a connection and a task and
//! re-reading the order from the engine for as long as it lasts. Counting
//! open streams per `(client, store pk)` bounds that directly. Keyed by store
//! too, so a busy shared address (a NAT) viewing one store doesn't use up its
//! allowance at every other store.
//!
//! The client is the same [`ClientIdentity`] the rate limiter uses: a Tor
//! circuit on the onion listener, or the clearnet address behind any trusted
//! proxies - so one visitor's streams no longer count against everyone who
//! shares its proxy or its onion service. Over the cap a stream is refused
//! with `429` and the page still works - `checkout.js` backs off and tries
//! again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::ClientIdentity;

/// The default cap (`abuse.stream_cap`): generous for one customer (a few
/// tabs) and for a small shared address; the point is to stop hundreds, not
/// a handful.
pub const MAX_STREAMS_PER_SOURCE: usize = 16;

pub struct StreamLimiter {
    max: AtomicUsize,
    open: Mutex<HashMap<(ClientIdentity, String), usize>>,
}

impl Default for StreamLimiter {
    fn default() -> Self { StreamLimiter::new(MAX_STREAMS_PER_SOURCE) }
}

impl StreamLimiter {
    pub fn new(max: usize) -> Self {
        StreamLimiter { max: AtomicUsize::new(max), open: Mutex::new(HashMap::new()) }
    }

    /// Changes the cap for streams opened from now on.
    pub fn set_max(&self, max: usize) {
        self.max.store(max, Ordering::Relaxed);
    }

    /// A permit to hold for as long as the stream is open, or `None` when
    /// this client already has the maximum number of streams open to this
    /// store.
    pub fn try_acquire(self: &Arc<Self>, client: &ClientIdentity, pk: &str) -> Option<StreamPermit> {
        let key = (client.clone(), pk.to_string());
        let mut open = self.open.lock().unwrap();
        let count = open.entry(key.clone()).or_insert(0);
        if *count >= self.max.load(Ordering::Relaxed) {
            return None;
        }
        *count += 1;
        Some(StreamPermit { limiter: Arc::clone(self), key })
    }

    #[cfg(test)]
    fn open_count(&self, client: &ClientIdentity, pk: &str) -> usize {
        self.open.lock().unwrap().get(&(client.clone(), pk.to_string())).copied().unwrap_or(0)
    }
}

/// Releases its slot when dropped - i.e. when the stream it was moved into
/// ends or its client disconnects.
pub struct StreamPermit {
    limiter: Arc<StreamLimiter>,
    key: (ClientIdentity, String),
}

impl Drop for StreamPermit {
    fn drop(&mut self) {
        let mut open = self.limiter.open.lock().unwrap();
        if let Some(count) = open.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_source_and_store_gets_its_own_allowance_and_a_dropped_permit_frees_its_slot() {
        let limiter = Arc::new(StreamLimiter::new(2));
        let a = ClientIdentity::Address("192.0.2.1".parse().unwrap());
        let b = ClientIdentity::Address("192.0.2.2".parse().unwrap());
        let circuit = ClientIdentity::Circuit(7);

        let first = limiter.try_acquire(&a, "pk_one").unwrap();
        let _second = limiter.try_acquire(&a, "pk_one").unwrap();
        assert!(limiter.try_acquire(&a, "pk_one").is_none(), "a third stream from the same client to the same store is refused");
        assert!(limiter.try_acquire(&a, "pk_two").is_some(), "another store has its own allowance");
        assert!(limiter.try_acquire(&b, "pk_one").is_some(), "another client has its own allowance");
        assert!(limiter.try_acquire(&circuit, "pk_one").is_some(), "a Tor circuit is its own client");

        drop(first);
        assert_eq!(limiter.open_count(&a, "pk_one"), 1);
        assert!(limiter.try_acquire(&a, "pk_one").is_some(), "a closed stream frees its slot");

        limiter.set_max(1);
        let _held = limiter.try_acquire(&circuit, "pk_two").unwrap();
        assert!(limiter.try_acquire(&circuit, "pk_two").is_none(), "a lowered cap applies to new streams");
    }

    #[test]
    fn an_empty_entry_is_removed_so_the_map_only_holds_open_streams() {
        let limiter = Arc::new(StreamLimiter::new(1));
        let client = ClientIdentity::Address("192.0.2.1".parse().unwrap());
        drop(limiter.try_acquire(&client, "pk_one").unwrap());
        assert!(limiter.open.lock().unwrap().is_empty());
    }
}
