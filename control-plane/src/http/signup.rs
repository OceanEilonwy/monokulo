//! `POST /signup` (WBS 1.1.1): creates a control-plane user account with a
//! hashed password. Unauthenticated by necessity — nobody has an account yet.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::now_unix;

use super::{ApiError, AppState};

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct SignupResponse {
    pub user_id: String,
}

pub async fn signup(
    State(state): State<AppState>,
    Json(req): Json<SignupRequest>,
) -> Result<(StatusCode, Json<SignupResponse>), ApiError> {
    // Argon2id (`shared::password`, WBS 0.4) — deliberately not
    // `shared::auth`'s SHA-256, which is the wrong tool for a low-entropy,
    // human-chosen password (see that module's own doc comment).
    let password_hash = shared::password::hash_password(&req.password).map_err(|_| ApiError::Internal)?;
    let id = Uuid::new_v4().to_string();
    let created_at = now_unix();

    let result = state.db.lock().unwrap().create_user(&id, &req.email, &password_hash, created_at);
    match result {
        Ok(()) => Ok((StatusCode::CREATED, Json(SignupResponse { user_id: id }))),
        Err(e) if e.is_unique_violation() => Err(ApiError::Conflict),
        Err(_) => Err(ApiError::Internal),
    }
}
