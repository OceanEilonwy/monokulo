//! `POST /signup` (WBS 1.1.1): creates a monokulo user account with a
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

/// The two ways account creation can fail - kept separate from [`ApiError`]
/// so the browser-facing form handler (`http/dashboard.rs`, WBS 1.3.1) can
/// map a duplicate email to a re-rendered form with a visible error instead
/// of a bare JSON `409`, while the JSON handler below keeps its existing
/// `ApiError::Conflict`/`ApiError::Internal` shape.
pub(super) enum CreateAccountError {
    DuplicateEmail,
    Internal,
}

/// The actual account-creation logic - `Db::create_user` plus
/// `shared::password::hash_password` - shared by `POST /signup` (below) and
/// `POST /dashboard/signup` (`http/dashboard.rs`), so the two surfaces can
/// never drift apart on what "creating an account" means.
pub(super) fn create_account(state: &AppState, email: &str, password: &str) -> Result<String, CreateAccountError> {
    // Argon2id (`shared::password`, WBS 0.4) — deliberately not
    // `shared::auth`'s SHA-256, which is the wrong tool for a low-entropy,
    // human-chosen password (see that module's own doc comment).
    let password_hash = shared::password::hash_password(password).map_err(|_| CreateAccountError::Internal)?;
    let id = Uuid::new_v4().to_string();
    let created_at = now_unix();

    let result = state.db.lock().unwrap().create_user(&id, email, &password_hash, created_at);
    match result {
        Ok(()) => Ok(id),
        Err(e) if e.is_unique_violation() => Err(CreateAccountError::DuplicateEmail),
        Err(_) => Err(CreateAccountError::Internal),
    }
}

pub async fn signup(
    State(state): State<AppState>,
    Json(req): Json<SignupRequest>,
) -> Result<(StatusCode, Json<SignupResponse>), ApiError> {
    match create_account(&state, &req.email, &req.password) {
        Ok(id) => Ok((StatusCode::CREATED, Json(SignupResponse { user_id: id }))),
        Err(CreateAccountError::DuplicateEmail) => Err(ApiError::Conflict),
        Err(CreateAccountError::Internal) => Err(ApiError::Internal),
    }
}
