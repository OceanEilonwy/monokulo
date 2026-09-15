//! Thin re-export of `shared::exchange_rate` (`docs/fx_refactor.md` Phase 1.1
//! moved the real logic there) - kept so every existing `crate::exchange_rate::*`
//! reference in this crate keeps compiling unchanged. This whole module, and
//! every one of its callers, is scheduled for removal in that same
//! document's Phase 3/4: the engine is being narrowed to strictly
//! Monero-watching, with no concept of fiat/FX at all.

pub use shared::exchange_rate::*;
