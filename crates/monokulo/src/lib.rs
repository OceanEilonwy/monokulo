//! `monokulo`: accounts, store connections, connect flow, dashboard
//! backend for Monokulo. See `docs/WOOCOMMERCE_WBS.md` Track A.
//!
//! Structured the same way as the engine (`engine`): this crate is a
//! real library — everything is exposed as `pub mod` here — with
//! `src/main.rs` a thin binary wrapper around it. Later WBS tasks
//! (login/session, tenant provisioning, the dashboard) build on
//! `http::AppState`/`http::build_router` the same way the engine's own
//! `main.rs` wires up `engine::http::{AppState, build_router}`.

pub mod abuse;
pub mod admin_nodes;
pub mod assets;
pub mod cli;
pub mod confirmation_thresholds;
pub mod crypto;
pub mod currencies;
pub mod db;
pub mod embed_domains;
pub mod engine_client;
pub mod engine_view;
pub mod exchange_rate_config;
pub mod fx_provider_settings;
pub mod http;
pub mod live;
pub mod logs;
pub mod qr;
pub mod settings;
pub mod stores;
pub mod templates;
pub mod views;
pub mod wallets;

/// Seconds since the Unix epoch: the one clock both services share.
pub use shared::time::now_unix;
