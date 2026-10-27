//! Web Push, as a sender and a relay need it.
//!
//! A sender has three things to do with a push: encrypt it to the device
//! ([`encrypt()`], RFC 8291's `aes128gcm`), sign who it is from
//! ([`vapid`], RFC 8292), and `POST` it to the subscription's endpoint
//! with the headers RFC 8030 defines ([`Sender`]). A relay (a push service
//! that forwards to APNs or FCM) reads that same request back
//! ([`wire::PushRequest`]) and checks the signature ([`vapid::verify`]).
//!
//! Every value that crosses a boundary is parsed into its own type once:
//! an [`Endpoint`] is https and never an onion address, a [`Topic`] is at
//! most 32 URL-safe characters, a [`P256dh`] is a point on the curve. What
//! a type holds is valid, so nothing downstream checks it again.
//!
//! Times are Unix seconds (`i64`), as the rest of the workspace keeps them,
//! and passed in by the caller, so no function here reads the clock.
//!
//! The crate knows nothing about monokulo, the relay or the Bell, and
//! builds on stable Rust: the Bell lives in another repository and takes
//! it as a git dependency.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod base64url;
pub mod encrypt;
pub mod jwt;
pub mod message;
pub mod send;
pub mod subscription;
pub mod vapid;
pub mod wire;

pub use encrypt::{encrypt, EncryptError, Encrypted};
pub use message::{Message, Options, PlainJson, Topic, Ttl, Urgency, MAX_PLAINTEXT};
pub use send::{SendError, Sender, Sent};
pub use subscription::{AuthSecret, Endpoint, Origin, P256dh, Subscription, SubscriptionKeys};
pub use vapid::{Contact, VapidKey, VapidPublicKey};

/// The largest request body a push service has to accept (RFC 8030
/// §7.2), and so the largest push this crate makes or reads: an
/// [`Encrypted`] body with its header, or a [`PlainJson`] one.
pub const MAX_BODY: usize = 4096;
