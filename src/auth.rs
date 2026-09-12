//! Tenant credential generation and verification.
//!
//! Moved into `shared::auth` (WBS 0.2) so the control-plane can reuse the same
//! token-generation/hashing logic without duplicating it. This module is a thin
//! re-export so existing call sites (`crate::auth::...`) keep working unchanged.
//! See `shared::auth`'s own doc comment for the actual design notes.

pub use shared::auth::*;
