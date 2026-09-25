//! A cap on how many live-update streams (`/pay/{pk}/orders/{id}/events`)
//! one source IP may hold open at once against one store.
//!
//! [`super::rate_limit`] counts requests as they arrive, which bounded the
//! old status polling (every check was a request) but not a stream: one
//! request stays open until its order finishes, holding a connection and a
//! task and re-reading the order from the engine for as long as it lasts.
//! Counting open streams per `(IP, store pk)` bounds that directly. Keyed by
//! store too, so a busy shared address (a NAT, a proxy) viewing one store
//! doesn't use up its allowance at every other store.
//!
//! The address is the connection's peer, same as the rate limiter: behind a
//! reverse proxy or a Tor onion service every visitor shares one, so the cap
//! then limits how many customers can watch one store's checkouts live at
//! the same time. Over the cap a stream is refused with `429` and the page
//! still works - `checkout.js` backs off and tries again.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

/// Generous for one customer (a few tabs) and for a small shared address;
/// the point is to stop hundreds, not a handful.
pub const MAX_STREAMS_PER_SOURCE: usize = 16;

pub struct StreamLimiter {
    max: usize,
    open: Mutex<HashMap<(IpAddr, String), usize>>,
}

impl Default for StreamLimiter {
    fn default() -> Self { StreamLimiter::new(MAX_STREAMS_PER_SOURCE) }
}

impl StreamLimiter {
    pub fn new(max: usize) -> Self {
        StreamLimiter { max, open: Mutex::new(HashMap::new()) }
    }

    /// A permit to hold for as long as the stream is open, or `None` when
    /// this source already has `max` streams open to this store.
    pub fn try_acquire(self: &Arc<Self>, ip: IpAddr, pk: &str) -> Option<StreamPermit> {
        let key = (ip, pk.to_string());
        let mut open = self.open.lock().unwrap();
        let count = open.entry(key.clone()).or_insert(0);
        if *count >= self.max {
            return None;
        }
        *count += 1;
        Some(StreamPermit { limiter: Arc::clone(self), key })
    }

    #[cfg(test)]
    fn open_count(&self, ip: IpAddr, pk: &str) -> usize {
        self.open.lock().unwrap().get(&(ip, pk.to_string())).copied().unwrap_or(0)
    }
}

/// Releases its slot when dropped - i.e. when the stream it was moved into
/// ends or its client disconnects.
pub struct StreamPermit {
    limiter: Arc<StreamLimiter>,
    key: (IpAddr, String),
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
        let (a, b): (IpAddr, IpAddr) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());

        let first = limiter.try_acquire(a, "pk_one").unwrap();
        let _second = limiter.try_acquire(a, "pk_one").unwrap();
        assert!(limiter.try_acquire(a, "pk_one").is_none(), "a third stream from the same source to the same store is refused");
        assert!(limiter.try_acquire(a, "pk_two").is_some(), "another store has its own allowance");
        assert!(limiter.try_acquire(b, "pk_one").is_some(), "another source has its own allowance");

        drop(first);
        assert_eq!(limiter.open_count(a, "pk_one"), 1);
        assert!(limiter.try_acquire(a, "pk_one").is_some(), "a closed stream frees its slot");
    }

    #[test]
    fn an_empty_entry_is_removed_so_the_map_only_holds_open_streams() {
        let limiter = Arc::new(StreamLimiter::new(1));
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        drop(limiter.try_acquire(ip, "pk_one").unwrap());
        assert!(limiter.open.lock().unwrap().is_empty());
    }
}
