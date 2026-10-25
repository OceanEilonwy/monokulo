//! Who is calling the machine API (`/api/v1`, `http::api`): the
//! `Authorization: Bearer <key>` header names the store, so no URL carries
//! one. Three kinds of key:
//!
//! - `sk_...`, the store's secret key (the same `sk_` the store's engine
//!   tenant was made with, kept encrypted in
//!   `store_connections.tenant_secret_token_encrypted` and found by its hash,
//!   `secret_key_hash`): a shop's own server, such as its plugin;
//! - `ak_...`, a connected app's key (`crate::apps`), found by its hash in
//!   `app_connections`; disconnecting the app makes it unknown at once;
//! - `pk_...`, the store's public key, for a browser page that embeds the
//!   checkout (`monokulo-client.js`). It proves nothing, so it may only
//!   create orders, as anyone could before keys were sent
//!   (`http::api`).
//!
//! `super::abuse::api_and_checkout_middleware` runs [`check`] on every `/api/v1` request
//! with an `Authorization` header. A known key adds [`ApiCaller`] to the
//! request; a secret or app key also makes the client that store, with its
//! own budget instead of the anonymous per-client one. Anything else (an
//! unknown key, not a bearer token at all) gets `401`, after spending the
//! anonymous client's budget, so keys can't be guessed quickly.

use axum::http::header;
use subtle::ConstantTimeEq;

use super::AppState;
use crate::db::{AppConnection, StoreConnectionRow};

/// Which kind of key a caller presented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    /// The store's secret key, `sk_...`.
    Secret,
    /// A connected app's key, `ak_...`: this app.
    App(AppConnection),
    /// The store's public key, `pk_...`: a browser page.
    Public,
}

impl Credential {
    /// A key that proves the caller is the store (its own or an app's).
    pub fn is_store_key(&self) -> bool {
        !matches!(self, Credential::Public)
    }
}

/// Request extension: the store an `/api/v1` request's key names, and the
/// kind of key it was.
#[derive(Clone)]
pub struct ApiCaller {
    pub store: std::sync::Arc<StoreConnectionRow>,
    pub credential: Credential,
}

/// Request extension: the store a public request is about, found by
/// `super::abuse::api_and_checkout_middleware` from the API key or the checkout's order,
/// for the embed policy (`super::embed_domains`) and the checkout pages.
#[derive(Clone)]
pub struct RequestStore(pub std::sync::Arc<StoreConnectionRow>);

/// What a request's `Authorization` header says.
pub enum KeyCheck {
    /// No `Authorization` header at all.
    Absent,
    /// A key this instance knows.
    Known(ApiCaller),
    /// Anything else.
    Invalid,
}

/// Compares `presented` with the store's stored key in constant time (for
/// equal lengths; the length of a key isn't secret). A key that can't be
/// decrypted never matches.
pub fn key_matches(
    state: &AppState,
    connection_id: &str,
    stored_encrypted: &str,
    presented: &str,
) -> bool {
    match crate::crypto::decrypt(
        &state.encryption_key,
        crate::crypto::Binding::StoreSecret(connection_id),
        stored_encrypted,
    ) {
        Ok(stored) => bool::from(stored.as_bytes().ct_eq(presented.as_bytes())),
        Err(e) => {
            tracing::error!(error = %e, "could not decrypt a store's secret key to check a presented one");
            false
        }
    }
}

/// The bearer token in `headers`, `Some(None)` for an `Authorization`
/// header that isn't one, `None` for no header.
fn bearer(headers: &axum::http::HeaderMap) -> Option<Option<String>> {
    let value = headers.get(header::AUTHORIZATION)?;
    // The scheme is case-insensitive (RFC 9110 §11.1).
    Some(
        value
            .to_str()
            .ok()
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim().to_owned())
            .filter(|token| !token.is_empty()),
    )
}

/// Checks a request's `Authorization` header: which store's key it is, and
/// which kind.
pub async fn check(state: &AppState, headers: &axum::http::HeaderMap) -> KeyCheck {
    let Some(presented) = bearer(headers) else {
        return KeyCheck::Absent;
    };
    let Some(presented) = presented else {
        return KeyCheck::Invalid;
    };
    let found = if presented.starts_with("ak_") {
        let hash = credentials::auth::RawToken::presented(&presented).hash();
        state
            .db
            .read(move |db| {
                let Some(app) = db.app_by_key(&hash)? else {
                    return Ok(None);
                };
                let store = db.get_store_connection_by_id(&app.store_id)?;
                Ok::<_, crate::db::DbError>(store.map(|store| ApiCaller {
                    store: std::sync::Arc::new(store),
                    credential: Credential::App(app),
                }))
            })
            .await
    } else if presented.starts_with("pk_") {
        state
            .db
            .read(move |db| db.get_store_connection_by_public_key(&presented))
            .await
            .map(|store| {
                store.map(|store| ApiCaller {
                    store: std::sync::Arc::new(store),
                    credential: Credential::Public,
                })
            })
    } else {
        let hash = credentials::auth::RawToken::presented(&presented).hash();
        let found = state
            .db
            .read(move |db| db.get_store_connection_by_secret_key_hash(&hash))
            .await;
        // The hash found the store; the stored key itself must match too,
        // so a stale hash can never let an old key in.
        found.map(|store| {
            store
                .filter(|store| {
                    key_matches(
                        state,
                        store.id.as_str(),
                        &store.tenant_secret_token_encrypted,
                        &presented,
                    )
                })
                .map(|store| ApiCaller {
                    store: std::sync::Arc::new(store),
                    credential: Credential::Secret,
                })
        })
    };
    match found {
        Ok(Some(caller)) => KeyCheck::Known(caller),
        Ok(None) => KeyCheck::Invalid,
        Err(e) => {
            tracing::error!(error = %e, "a presented key could not be looked up");
            KeyCheck::Invalid
        }
    }
}
