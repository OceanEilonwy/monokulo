//! The control-plane's HTTP API surface.
//!
//! Mirrors the engine's own `moneropay_core::http` module (see its doc
//! comment) in shape, not content: a `Clone`-able `AppState` carrying
//! shared, lock-guarded storage, and a `build_router(state) -> Router`
//! function so any caller — the real binary in `src/main.rs`, or this
//! module's own tests — can construct the exact same router. Tests drive it
//! through `tower::ServiceExt::oneshot` with no bound socket, the same
//! pattern as the engine's `src/http/tests.rs`; that's the right pattern
//! here specifically because these tests only ever need to exercise the
//! control-plane's own router in-process, unlike `engine-test-support`,
//! which exists because *other* crates need a real socket to reach a
//! separately-deployed *engine* instance.
//!
//! No authentication middleware yet: `/signup` is necessarily unauthenticated
//! (WBS 1.1.1). Login/session (1.1.2) and logout (1.1.3) are separate,
//! later tasks.

mod signup;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use serde_json::json;

use crate::db::SharedDb;

#[derive(Clone)]
pub struct AppState {
    pub db: SharedDb,
}

pub fn build_router(state: AppState) -> Router {
    Router::new().route("/signup", post(signup::signup)).with_state(state)
}

/// Deliberately just two cases: a signup either succeeds, hits the one
/// expected, honest conflict (duplicate email), or fails for some other
/// reason. The `Internal` message is fixed and generic on purpose — it must
/// not describe *why* the underlying hash/DB operation failed, so a client
/// can't use error-message differences to fingerprint internals beyond the
/// unavoidable, standard "this email is already taken" signal `Conflict`
/// already gives it.
pub enum ApiError {
    Conflict,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Conflict => (StatusCode::CONFLICT, "email already in use"),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
