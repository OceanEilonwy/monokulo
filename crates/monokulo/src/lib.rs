//! `monokulo`: accounts, store connections, connect flow, dashboard
//! backend for Monokulo. See `docs/WOOCOMMERCE_WBS.md` Track A.
//!
//! Structured the same way as the engine (`scanner`): this crate is a
//! real library — everything is exposed as `pub mod` here — with
//! `src/main.rs` a thin binary wrapper around it. Later WBS tasks
//! (login/session, tenant provisioning, the dashboard) build on
//! `http::AppState`/`http::build_router` the same way the engine's own
//! `main.rs` wires up `scanner::http::{AppState, build_router}`.

pub mod confirmation_thresholds;
pub mod crypto;
pub mod currencies;
pub mod db;
pub mod engine_client;
pub mod exchange_rate_config;
pub mod http;
pub mod settings;
pub mod templates;

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}
