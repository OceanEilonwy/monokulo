//! A shop's own server authenticating to monokulo's public `/pay/{pk}/...`
//! routes with its store's secret key (`Authorization: Bearer sk_...`), the
//! same `sk_` the store's engine tenant was created with and monokulo keeps
//! encrypted in `store_connections.tenant_secret_token_encrypted`. The
//! store's shop plugin is such a caller: it creates orders with the
//! key, so a store that restricts embedding to its verified domains
//! (`http::embed_domains::embed_policy_middleware`) can still take orders
//! from its own server, and those orders are recorded as created with the
//! key (`order_currency_metadata.created_with_key`).
//!
//! `super::abuse::abuse_middleware` runs [`check`] on every `/pay/...`
//! request carrying an `Authorization` header:
//!
//! - the right key for the store in the path marks the request
//!   [`StoreKeyAuthenticated`] and the client becomes that store, with its
//!   own budget instead of the anonymous per-client one;
//! - any other value (a wrong key, another store's key, not a bearer token
//!   at all) gets `401`. The failed attempt still spends the anonymous
//!   client's budget, so the endpoint can't be used to guess keys quickly.
//!
//! A connected app (`crate::apps`) sends its own key, `ak_...`, instead: it
//! counts as the store's key for its store and adds [`AppKeyAuthenticated`],
//! naming the app. Disconnecting the app makes it `Invalid` at once.
//!
//! A browser can't send this header cross-origin (CORS doesn't allow it),
//! so an embedding page never trips the `401`.

use axum::http::header;
use subtle::ConstantTimeEq;

use super::AppState;

/// Request extension: this request carried the right secret key for the
/// store in its path.
#[derive(Clone, Copy, Debug)]
pub struct StoreKeyAuthenticated;

/// Request extension: this request carried a connected app's key for the
/// store in its path, and this is the app.
#[derive(Clone, Debug)]
pub struct AppKeyAuthenticated(pub crate::db::AppConnection);

/// What a request's `Authorization` header says about the store in its
/// path.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyCheck {
    /// No `Authorization` header at all.
    Absent,
    /// The store's own secret key.
    Valid,
    /// A key of one of the store's connected apps.
    App(crate::db::AppConnection),
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

/// Checks a request's `Authorization` header against the store with public
/// key `pk`. An unknown store is `Invalid` whenever a header is present:
/// there is no key it could match.
pub async fn check(state: &AppState, pk: &str, headers: &axum::http::HeaderMap) -> KeyCheck {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return KeyCheck::Absent;
    };
    // The scheme is case-insensitive (RFC 9110 §11.1).
    let Some(presented) = value
        .to_str()
        .ok()
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token)
    else {
        return KeyCheck::Invalid;
    };
    let presented = presented.trim();
    let key = pk.to_string();
    if presented.starts_with("ak_") {
        let hash = shared::auth::RawToken::presented(presented).hash();
        let found = state
            .db
            .read(move |db| {
                let app = db.app_by_key(&hash)?;
                let store = db.get_store_connection_by_public_key(&key)?;
                Ok::<_, crate::db::DbError>(app.zip(store))
            })
            .await;
        return match found {
            Ok(Some((app, store))) if app.store_id == store.id => KeyCheck::App(app),
            _ => KeyCheck::Invalid,
        };
    }
    let row = match state
        .db
        .read(move |db| db.get_store_connection_by_public_key(&key))
        .await
    {
        Ok(Some(row)) => row,
        _ => return KeyCheck::Invalid,
    };
    if key_matches(
        state,
        row.id.as_str(),
        &row.tenant_secret_token_encrypted,
        presented,
    ) {
        KeyCheck::Valid
    } else {
        KeyCheck::Invalid
    }
}
