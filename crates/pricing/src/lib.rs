//! What an order costs in XMR. Monokulo prices orders; the engine only
//! watches Monero and has no concept of fiat or exchange rates.
//!
//! - [`xmr_amount`]: XMR decimal strings and piconero, as exact integers.
//! - [`exchange_rate`]: fiat to XMR, the Coingecko provider and the
//!   arithmetic every provider's rate goes through.
//! - [`coinmarketcap`] and [`haveno`]: the other two live rate providers.
//! - [`http_cache`]: the cache-aware HTTP client the providers fetch with.

pub mod coinmarketcap;
pub mod exchange_rate;
pub mod haveno;
pub mod http_cache;
pub mod xmr_amount;
