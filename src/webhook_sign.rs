//! Webhook delivery signing and SSRF validation.
//!
//! Moved into `shared::webhook_sign` (WBS 0.3) so the control-plane can reuse the
//! same HMAC signing/verification and SSRF-URL-validation logic without duplicating
//! it. This module is a thin re-export so existing call sites (`crate::webhook_sign::...`)
//! keep working unchanged. See `shared::webhook_sign`'s own doc comment for the
//! actual design notes.

pub use shared::webhook_sign::*;
