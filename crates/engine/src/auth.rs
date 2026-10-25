//! Tenant credential generation and verification.
//!
//! Moved into `credentials::auth` so monokulo can reuse the same
//! token-generation/hashing logic without duplicating it. This module is a thin
//! re-export so existing call sites (`crate::auth::...`) keep working unchanged.
//! See `credentials::auth`'s own doc comment for the actual design notes.

pub use credentials::auth::*;
