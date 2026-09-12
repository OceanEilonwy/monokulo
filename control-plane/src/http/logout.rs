//! `POST /logout` (WBS 1.1.3): revokes the session presented in the
//! `Authorization` header.
//!
//! Reuses [`AuthedUser`] rather than re-parsing the `Bearer` header - the
//! extractor already fails the request with `401` for a
//! missing/malformed/unknown session before this handler ever runs, so
//! `/logout` gets that behavior for free, same as any other route behind
//! it. `AuthedUser` also hands back the resolved session's `token_hash`,
//! which is exactly what `Db::delete_session` needs to key on.
//!
//! Always returns `204 No Content` on success, even if the session row
//! happened to already be gone by the time the delete ran (e.g. a
//! concurrent second logout call) - logging out an already-logged-out
//! session isn't an error a client needs to see. `Db::delete_session`'s
//! `bool` return exists for exactly this case, but there's nothing useful
//! to do with `false` here: the end state ("this token no longer has a
//! session") is identical either way, and the caller has no legitimate way
//! to react differently to "it was already gone" versus "you just removed
//! it".

use axum::extract::State;
use axum::http::StatusCode;

use super::{ApiError, AppState, AuthedUser};

pub async fn logout(State(state): State<AppState>, AuthedUser(_user, token_hash): AuthedUser) -> Result<StatusCode, ApiError> {
    state.db.lock().unwrap().delete_session(&token_hash).map_err(|_| ApiError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}
