//! `POST /signup` (WBS 1.1.1): creates a monokulo user account with a
//! hashed password. Unauthenticated by necessity — nobody has an account yet.
//!
//! Gated on this instance's `signup.mode` setting (`crate::settings::SIGNUP_MODE`)
//! - `"public"` (any visitor may sign up, the original behavior) or
//! `"invite_only"` (the default: a valid, unused invite token is required -
//! see `Db::redeem_invite_and_create_user`'s own doc comment for the
//! single-use guarantee). The first-run admin setup wizard
//! (`http/admin_setup.rs`) is the one caller that bypasses this entirely -
//! `is_admin: true` skips the mode check outright, since a fresh instance
//! has no invite system worth gating its own bootstrap on.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::RedeemInviteResult;
use crate::now_unix;
use crate::settings::SignupMode;

use super::{ApiError, AppState};

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
    /// Only consulted (and only required) when this instance's `signup.mode`
    /// is `"invite_only"` - ignored entirely in `"public"` mode, same as
    /// the form field `http/dashboard.rs`'s `SignupForm::invite` is.
    #[serde(default)]
    pub invite_token: Option<String>,
}

#[derive(Serialize)]
pub struct SignupResponse {
    pub user_id: String,
}

/// The ways account creation can fail - kept separate from [`ApiError`]
/// so the browser-facing form handler (`http/dashboard.rs`, WBS 1.3.1) can
/// map a duplicate email to a re-rendered form with a visible error instead
/// of a bare JSON `409`, while the JSON handler below keeps its existing
/// `ApiError::Conflict`/`ApiError::Internal` shape.
pub(super) enum CreateAccountError {
    DuplicateEmail,
    Internal,
    /// `signup.mode` is `"invite_only"` and no (or an empty) invite token
    /// was presented at all - distinct from [`CreateAccountError::InvalidOrUsedInvite`]
    /// so the two can get different, clearer messages (and so the
    /// browser-facing form knows to show its "you need an invite" state
    /// rather than a re-fillable form with an error banner - see
    /// `dashboard::render_signup`'s own doc comment).
    InviteRequired,
    /// A token was presented, but it doesn't match any invite link with
    /// `used_at_utc IS NULL` - either it's unknown, or (the common real
    /// case) it already redeemed once before.
    InvalidOrUsedInvite,
}

/// The actual account-creation logic - `Db::create_user`/
/// `Db::redeem_invite_and_create_user` plus `shared::password::hash_password`
/// - shared by `POST /signup` (below), `POST /dashboard/signup`
/// (`http/dashboard.rs`), and the first-run admin setup wizard
/// (`http/admin_setup.rs`, the one caller that ever passes `is_admin: true`),
/// so all three surfaces can never drift apart on what "creating an account"
/// means. `invite_token` is only ever consulted when `is_admin` is `false`
/// and this instance's `signup.mode` is `"invite_only"` - ignored
/// (regardless of whether it's `Some` or `None`) in every other case.
pub(super) fn create_account(
    state: &AppState,
    email: &str,
    password: &str,
    is_admin: bool,
    invite_token: Option<&str>,
) -> Result<String, CreateAccountError> {
    // Argon2id (`shared::password`, WBS 0.4) — deliberately not
    // `shared::auth`'s SHA-256, which is the wrong tool for a low-entropy,
    // human-chosen password (see that module's own doc comment).
    let password_hash = shared::password::hash_password(password).map_err(|_| CreateAccountError::Internal)?;
    let id = Uuid::new_v4().to_string();
    let created_at = now_unix();

    if !is_admin && crate::settings::signup_mode(&state.db.lock().unwrap()) == SignupMode::InviteOnly {
        let token = match invite_token.map(str::trim) {
            Some(t) if !t.is_empty() => t,
            _ => return Err(CreateAccountError::InviteRequired),
        };
        let token_hash = shared::auth::hash_secret_token(token);
        return match state.db.lock().unwrap().redeem_invite_and_create_user(&token_hash, &id, email, &password_hash, created_at) {
            Ok(RedeemInviteResult::Created) => Ok(id),
            Ok(RedeemInviteResult::DuplicateEmail) => Err(CreateAccountError::DuplicateEmail),
            Ok(RedeemInviteResult::InvalidOrAlreadyUsed) => Err(CreateAccountError::InvalidOrUsedInvite),
            Err(_) => Err(CreateAccountError::Internal),
        };
    }

    let result = state.db.lock().unwrap().create_user(&id, email, &password_hash, is_admin, created_at);
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
    match create_account(&state, &req.email, &req.password, false, req.invite_token.as_deref()) {
        Ok(id) => Ok((StatusCode::CREATED, Json(SignupResponse { user_id: id }))),
        Err(CreateAccountError::DuplicateEmail) => Err(ApiError::Conflict),
        Err(CreateAccountError::Internal) => Err(ApiError::Internal),
        Err(CreateAccountError::InviteRequired) => Err(ApiError::BadRequest("an invite token is required to sign up".to_string())),
        Err(CreateAccountError::InvalidOrUsedInvite) => {
            Err(ApiError::BadRequest("that invite link is invalid or has already been used".to_string()))
        }
    }
}
