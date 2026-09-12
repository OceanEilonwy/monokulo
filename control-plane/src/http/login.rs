//! `POST /login` (WBS 1.1.2): verifies email+password and issues a session
//! token.
//!
//! Deliberately returns the exact same `401` for "no such account" as for
//! "wrong password" - a client must not be able to tell the two apart via
//! status code or message (standard account-enumeration defense). The
//! obvious way to get this wrong is an early return for an unknown email
//! that skips `verify_password` entirely, which is also observable via
//! timing; to avoid that specific short-circuit, an unknown email still
//! runs a full `verify_password` call against a fixed dummy hash before
//! failing. This isn't a hard constant-time guarantee (allocation, cache
//! effects, etc. can still differ) but it keeps the two code paths doing
//! the same expensive work rather than one of them being obviously cheaper.

use std::sync::LazyLock;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};

use crate::now_unix;

use super::{ApiError, AppState};

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub session_token: String,
}

/// A real Argon2id hash of a fixed, nobody-has-this-password string,
/// computed once and reused - just something for the "unknown email" path
/// to run `verify_password` against so it does comparable work to the
/// real-user path instead of short-circuiting. See module doc comment.
static DUMMY_PASSWORD_HASH: LazyLock<String> =
    LazyLock::new(|| shared::password::hash_password("not-a-real-account-dummy-password").unwrap());

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<(StatusCode, Json<LoginResponse>), ApiError> {
    let user = state.db.lock().unwrap().get_user_by_email(&req.email).map_err(|_| ApiError::Internal)?;

    let password_hash = user.as_ref().map(|u| u.password_hash.as_str()).unwrap_or(&DUMMY_PASSWORD_HASH);
    let password_ok = shared::password::verify_password(&req.password, password_hash);

    // Require both a real user *and* a correct password - checking
    // `password_ok` alone would (in the astronomically unlikely case
    // someone's actual password collides with the dummy) let an unknown
    // email "succeed" with no user to attach a session to.
    let Some(user) = user.filter(|_| password_ok) else {
        return Err(ApiError::Unauthorized);
    };

    let raw_token = shared::auth::generate_session_token();
    let token_hash = shared::auth::hash_secret_token(&raw_token);
    state
        .db
        .lock()
        .unwrap()
        .create_session(&token_hash, &user.id, now_unix())
        .map_err(|_| ApiError::Internal)?;

    Ok((StatusCode::OK, Json(LoginResponse { session_token: raw_token })))
}
