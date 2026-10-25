//! `monokulo`: accounts, stores, the connect flows, the merchant's pages,
//! the checkout and the machine API (`/api/v1`), in front of the engine.
//!
//! Structured the same way as the engine (`engine`): this crate is a
//! real library — everything is exposed as `pub mod` here — with
//! `src/main.rs` a thin binary wrapper around it, wiring up
//! `http::AppState`/`http::build_router` the same way the engine's own
//! `main.rs` wires up `engine::http::{AppState, build_router}`.

pub mod abuse;
pub mod admin_nodes;
pub mod api;
pub mod apps;
pub mod assets;
pub mod base;
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
pub mod network;
pub mod onion;
pub mod orders;
pub mod plugins;
pub mod qr;
pub mod settings;
pub mod stores;
pub mod templates;
pub mod tor;
pub mod views;
pub mod wallets;
pub mod webhooks;

/// Seconds since the Unix epoch: the one clock both services share.
pub use shared::time::now_unix;
