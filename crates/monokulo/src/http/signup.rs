//! `POST /signup` (WBS 1.1.1): creates a monokulo user account with a
//! hashed password. Unauthenticated by necessity — nobody has an account yet.
//!
//! Gated on this instance's `signup.mode` setting (`crate::settings::SIGNUP_MODE`)
//! — `"public"` (any visitor may sign up, the original behavior) or
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
    /// The password is shorter than [`MIN_PASSWORD_LEN`] (an empty one
    /// included), or the email is not an address.
    WeakPassword,
    InvalidEmail,
    /// First-run setup was already completed by another submission.
    AlreadySetUp,
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
/// `Db::redeem_invite_and_create_user` plus `shared::password::Hasher::hash`
/// — shared by `POST /signup` (below), `POST /dashboard/signup`
/// (`http/dashboard.rs`), and the first-run admin setup wizard
/// (`http/admin_setup.rs`, the one caller that ever passes `is_admin: true`),
/// so all three surfaces can never drift apart on what "creating an account"
/// means. `invite_token` is only ever consulted when `is_admin` is `false`
/// and this instance's `signup.mode` is `"invite_only"` - ignored
/// (regardless of whether it's `Some` or `None`) in every other case.
pub(super) async fn create_account(
    state: &AppState,
    email: &str,
    password: &str,
    is_admin: bool,
    invite_token: Option<&str>,
) -> Result<String, CreateAccountError> {
    let email = normalize_email(email).ok_or(CreateAccountError::InvalidEmail)?;
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(CreateAccountError::WeakPassword);
    }
    let token_hash = invite_token
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|token| shared::auth::RawToken::presented(token).hash());
    // Whether an invite is needed is settled before the password is
    // hashed: a hash costs tens of milliseconds of CPU, which a request
    // with no invite on an invite-only instance never earns.
    let invite_only = !is_admin
        && state
            .db
            .read(|db| Ok::<_, crate::db::DbError>(crate::settings::signup_mode(db)))
            .await
            .map_err(|_| CreateAccountError::Internal)?
            == SignupMode::InviteOnly;
    if invite_only && token_hash.is_none() {
        return Err(CreateAccountError::InviteRequired);
    }

    // Argon2id (`shared::password`, WBS 0.4) — deliberately not
    // `shared::auth`'s SHA-256, which is the wrong tool for a low-entropy,
    // human-chosen password (see that module's own doc comment).
    // Off the async threads (`shared::password::run`).
    let password = password.to_string();
    let password_hash = shared::password::run(move |hasher| hasher.hash(&password))
        .await
        .and_then(Result::ok)
        .ok_or(CreateAccountError::Internal)?;
    let id = crate::db::UserId::new(Uuid::new_v4().to_string());
    let created_at = now_unix();

    // One write job: the signup mode is read again under the same writer
    // as the account is created, so a mode saved in between still holds.
    state
        .db
        .write(move |db| {
            if is_admin {
                return match db.create_admin_and_complete_setup(
                    &id,
                    &email,
                    &password_hash,
                    created_at,
                ) {
                    Ok(true) => Ok(id.into_string()),
                    Ok(false) => Err(CreateAccountError::AlreadySetUp),
                    Err(e) if e.is_unique_violation() => Err(CreateAccountError::DuplicateEmail),
                    Err(_) => Err(CreateAccountError::Internal),
                };
            }
            if crate::settings::signup_mode(db) == SignupMode::InviteOnly {
                let Some(token_hash) = token_hash else {
                    return Err(CreateAccountError::InviteRequired);
                };
                return match db.redeem_invite_and_create_user(
                    &token_hash,
                    &id,
                    &email,
                    &password_hash,
                    created_at,
                ) {
                    Ok(RedeemInviteResult::Created) => Ok(id.into_string()),
                    Ok(RedeemInviteResult::DuplicateEmail) => {
                        Err(CreateAccountError::DuplicateEmail)
                    }
                    Ok(RedeemInviteResult::InvalidOrAlreadyUsed) => {
                        Err(CreateAccountError::InvalidOrUsedInvite)
                    }
                    Err(_) => Err(CreateAccountError::Internal),
                };
            }
            match db.create_user(&id, &email, &password_hash, false, created_at) {
                Ok(()) => Ok(id.into_string()),
                Err(e) if e.is_unique_violation() => Err(CreateAccountError::DuplicateEmail),
                Err(_) => Err(CreateAccountError::Internal),
            }
        })
        .await
}

/// Fewest characters a password may have, for every account. Twelve for
/// the admin account made by first-run setup (`http::admin_setup`).
pub const MIN_PASSWORD_LEN: usize = 8;

/// The form an email address is stored and looked up in: trimmed and
/// lowercased, so `Ann@Example.com ` and `ann@example.com` are one
/// account, not two (or an account that can't be logged into). `None` for
/// something that isn't an address at all.
pub(super) fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || email.len() > 254
        || email.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    Some(email)
}

impl From<shared::sqlite::PoolError> for CreateAccountError {
    fn from(_: shared::sqlite::PoolError) -> Self {
        CreateAccountError::Internal
    }
}

pub async fn signup(
    State(state): State<AppState>,
    Json(req): Json<SignupRequest>,
) -> Result<(StatusCode, Json<SignupResponse>), ApiError> {
    match create_account(
        &state,
        &req.email,
        &req.password,
        false,
        req.invite_token.as_deref(),
    )
    .await
    {
        Ok(id) => Ok((StatusCode::CREATED, Json(SignupResponse { user_id: id }))),
        Err(CreateAccountError::DuplicateEmail) => Err(ApiError::Conflict),
        Err(CreateAccountError::Internal) => Err(ApiError::Internal),
        Err(CreateAccountError::InviteRequired) => Err(ApiError::BadRequest(
            "an invite token is required to sign up".to_string(),
        )),
        Err(CreateAccountError::InvalidOrUsedInvite) => Err(ApiError::BadRequest(
            "that invite link is invalid or has already been used".to_string(),
        )),
        Err(CreateAccountError::WeakPassword) => Err(ApiError::BadRequest(format!(
            "the password must be at least {MIN_PASSWORD_LEN} characters"
        ))),
        Err(CreateAccountError::InvalidEmail) => Err(ApiError::BadRequest(
            "that is not an email address".to_string(),
        )),
        Err(CreateAccountError::AlreadySetUp) => Err(ApiError::Conflict),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_email_is_stored_trimmed_and_lowercased_and_must_be_an_address() {
        assert_eq!(
            normalize_email("  Ann@Example.COM "),
            Some("ann@example.com".to_string())
        );
        for bad in [
            "",
            "ann",
            "@example.com",
            "ann@",
            "ann@localhost",
            "a b@example.com",
        ] {
            assert_eq!(normalize_email(bad), None, "{bad:?}");
        }
    }
}
