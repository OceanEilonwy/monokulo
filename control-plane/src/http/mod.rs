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
//! `/signup` is necessarily unauthenticated (WBS 1.1.1). `/login` (1.1.2)
//! issues a session token; the [`AuthedUser`] extractor below resolves a
//! `Authorization: Bearer <session_token>` header back to a user, the same
//! pattern as the engine's own `AuthedTenant` (see `src/http/mod.rs` at the
//! repo root) resolves `Bearer sk_...`. Logout (1.1.3) is a separate,
//! later task.

mod login;
mod signup;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::{StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use serde_json::json;

use crate::db::{SharedDb, UserRow};

#[derive(Clone)]
pub struct AppState {
    pub db: SharedDb,
}

pub fn build_router(state: AppState) -> Router {
    let router = Router::new().route("/signup", post(signup::signup)).route("/login", post(login::login));

    // Test-only route exercising `AuthedUser` - see its doc comment.
    // Compiled only under `#[cfg(test)]`, so it never exists in the real
    // binary; nothing outside this crate's own tests should ever reach it.
    #[cfg(test)]
    let router = router.route("/_test/whoami", axum::routing::get(test_whoami));

    router.with_state(state)
}

/// Resolves `Authorization: Bearer <session_token>` to the user that
/// session belongs to. `401` for a missing/malformed header or an
/// unknown/invalid token - never distinguishes the two.
pub struct AuthedUser(pub UserRow);

impl FromRequestParts<AppState> for AuthedUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header_value = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = header_value.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
        let token_hash = shared::auth::hash_secret_token(token);

        let db = state.db.lock().unwrap();
        let session = db.find_session(&token_hash).map_err(|_| ApiError::Unauthorized)?.ok_or(ApiError::Unauthorized)?;
        let user = db.get_user_by_id(&session.user_id).map_err(|_| ApiError::Unauthorized)?.ok_or(ApiError::Unauthorized)?;
        Ok(AuthedUser(user))
    }
}

/// Test-only dummy protected route (see WBS 1.1.2): its only purpose is
/// giving this task's own integration tests something protected by
/// [`AuthedUser`] to exercise, since no real protected endpoint exists yet.
/// Not a real API surface - do not build on it.
#[cfg(test)]
async fn test_whoami(AuthedUser(user): AuthedUser) -> Json<serde_json::Value> {
    Json(json!({ "user_id": user.id }))
}

/// `Conflict` (signup, duplicate email), `Unauthorized` (login, or a
/// missing/invalid session), or `Internal` for anything else. Every
/// message is fixed and generic — `Internal` never describes *why* the
/// underlying operation failed, and `Unauthorized` never distinguishes
/// "wrong password" from "unknown email" from "invalid session token" — so
/// a client can't use error-message differences to fingerprint internals
/// or enumerate accounts beyond what each variant already, unavoidably,
/// signals.
pub enum ApiError {
    Conflict,
    Unauthorized,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Conflict => (StatusCode::CONFLICT, "email already in use"),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
