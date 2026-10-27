//! The relay's three limits, on the workspace's fixed-window limiter
//! (`rate-limit`): pushes per sender (its VAPID key, so one monokulo or
//! Bell can't crowd out the others), pushes per handle (one device), and
//! registrations per address (IPv6 by its /64).
//!
//! A push is counted only once its signature verifies, so nobody can use
//! up a sender's budget with pushes the sender didn't sign.

use std::net::IpAddr;

use abuse::ClientIdentity;
use push::VapidPublicKey;
use rate_limit::RateLimiter;

use crate::handles::HandleId;
use crate::settings::LimitsConfig;

pub struct Limits {
    senders: RateLimiter<VapidPublicKey>,
    handles: RateLimiter<HandleId>,
    registrations: RateLimiter<ClientIdentity>,
}

/// Which limit refused, and how many seconds until it allows again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Sender(u64),
    Handle(u64),
    Registrations(u64),
}

impl Refused {
    pub fn retry_after(self) -> u64 {
        match self {
            Refused::Sender(seconds)
            | Refused::Handle(seconds)
            | Refused::Registrations(seconds) => seconds,
        }
    }
}

impl Limits {
    pub fn new(config: LimitsConfig) -> Self {
        Limits {
            senders: RateLimiter::new(config.per_sender),
            handles: RateLimiter::new(config.per_handle),
            registrations: RateLimiter::new(config.registrations),
        }
    }

    /// One push from `sender` to `handle`, or which of the two is spent.
    /// Counted against the sender only when the handle's limit lets it
    /// through, so pushes a spent handle refuses don't use up the
    /// sender's budget for its other handles (one store's flood to one
    /// device can't stop every store's pushes on a shared monokulo). And
    /// one the sender's limit refuses is given back to the handle, so a
    /// sender over its limit doesn't use up its handles' budgets either:
    /// each limit counts only pushes handed on.
    pub fn push(
        &self,
        sender: &VapidPublicKey,
        handle: &HandleId,
        now: i64,
    ) -> Result<(), Refused> {
        self.handles
            .check_with_retry(handle.clone(), now)
            .map_err(Refused::Handle)?;
        self.senders
            .check_with_retry(*sender, now)
            .map_err(|seconds| {
                self.handles.refund(handle, now);
                Refused::Sender(seconds)
            })
    }

    /// One registration from `client`.
    pub fn register(&self, client: IpAddr, now: i64) -> Result<(), Refused> {
        self.registrations
            .check_with_retry(ClientIdentity::from_address(client), now)
            .map_err(Refused::Registrations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use push::VapidKey;

    const NOW: i64 = 1_800_000_000;

    fn limits(per_sender: u32, per_handle: u32, registrations: u32) -> Limits {
        Limits::new(LimitsConfig {
            per_sender,
            per_handle,
            registrations,
        })
    }

    fn handle(n: u8) -> HandleId {
        format!("ph_{}", hex_digit(n).repeat(32)).parse().unwrap()
    }

    fn hex_digit(n: u8) -> String {
        format!("{:x}", n % 16)
    }

    #[test]
    fn a_sender_is_limited_across_its_handles_and_a_handle_on_its_own() {
        let limits = limits(3, 2, 1);
        let (a, b) = (VapidKey::generate().public(), VapidKey::generate().public());
        assert_eq!(limits.push(&a, &handle(1), NOW), Ok(()));
        assert_eq!(limits.push(&a, &handle(1), NOW), Ok(()));
        assert_eq!(limits.push(&a, &handle(1), NOW), Err(Refused::Handle(60)));
        // Another sender has its own budget, and the handle's is spent.
        assert_eq!(limits.push(&b, &handle(2), NOW), Ok(()));
        assert_eq!(limits.push(&b, &handle(1), NOW), Err(Refused::Handle(60)));
        // A minute on, all is allowed again.
        assert_eq!(limits.push(&a, &handle(1), NOW + 60), Ok(()));
    }

    /// A push the handle's limit refuses isn't counted against its sender:
    /// a store flooding one device leaves the sender's budget to the
    /// others.
    #[test]
    fn a_push_a_spent_handle_refuses_leaves_the_senders_budget() {
        let limits = limits(3, 2, 1);
        let sender = VapidKey::generate().public();
        for _ in 0..2 {
            assert_eq!(limits.push(&sender, &handle(1), NOW), Ok(()));
        }
        for _ in 0..50 {
            assert_eq!(
                limits.push(&sender, &handle(1), NOW + 1),
                Err(Refused::Handle(59))
            );
        }
        // Two pushes handed on, so one more is left for another handle.
        assert_eq!(limits.push(&sender, &handle(2), NOW + 10), Ok(()));
        assert_eq!(
            limits.push(&sender, &handle(3), NOW + 10),
            Err(Refused::Sender(50))
        );
    }

    /// A push the sender's limit refuses is given back to its handle: the
    /// handle's budget is there when the sender's comes back.
    #[test]
    fn a_push_the_senders_limit_refuses_leaves_the_handles_budget() {
        let limits = limits(2, 2, 1);
        let sender = VapidKey::generate().public();
        for _ in 0..2 {
            assert_eq!(limits.push(&sender, &handle(1), NOW), Ok(()));
        }
        // Handle 2's minute starts here, with the sender's spent.
        for _ in 0..5 {
            assert_eq!(
                limits.push(&sender, &handle(2), NOW + 30),
                Err(Refused::Sender(30))
            );
        }
        // The sender's minute is over, handle 2's isn't: it has both its
        // pushes.
        assert_eq!(limits.push(&sender, &handle(2), NOW + 60), Ok(()));
        assert_eq!(limits.push(&sender, &handle(2), NOW + 60), Ok(()));
        assert_eq!(
            limits.push(&sender, &handle(2), NOW + 61),
            Err(Refused::Handle(29))
        );
    }

    #[test]
    fn registrations_are_limited_per_address_and_ipv6_by_its_64() {
        let limits = limits(1, 1, 2);
        let one: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let same_64: IpAddr = "2001:db8:1:2::ffff".parse().unwrap();
        let other: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(limits.register(one, NOW), Ok(()));
        assert_eq!(limits.register(same_64, NOW), Ok(()));
        let refused = limits.register(one, NOW + 15);
        assert_eq!(refused, Err(Refused::Registrations(45)));
        assert_eq!(refused.unwrap_err().retry_after(), 45);
        assert_eq!(limits.register(other, NOW), Ok(()));
    }
}
