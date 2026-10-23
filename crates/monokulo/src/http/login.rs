//! Logging in: verifies email+password and starts a session, for the
//! login form (`http/auth.rs`) and the first-run admin setup.
//!
//! Deliberately fails the same way for "no such account" as for "wrong
//! password" - a visitor must not be able to tell the two apart from the
//! answer (standard account-enumeration defense). The
//! obvious way to get this wrong is an early return for an unknown email
//! that skips `Hasher::verify` entirely, which is also observable via
//! timing; to avoid that specific short-circuit, an unknown email still
//! runs a full `Hasher::verify` call against a fixed dummy hash before
//! failing. This isn't a hard constant-time guarantee (allocation, cache
//! effects, etc. can still differ) but it keeps the two code paths doing
//! the same expensive work rather than one of them being obviously cheaper.

use std::sync::OnceLock;

use crate::now_unix;

use super::AppState;

/// A real Argon2id hash of a fixed, nobody-has-this-password string,
/// computed once and reused - just something for the "unknown email" path
/// to run `Hasher::verify` against so it does comparable work to the
/// real-user path instead of short-circuiting. See module doc comment.
/// Made by the first login's own password job (`shared::password::run`).
static DUMMY_PASSWORD_HASH: OnceLock<String> = OnceLock::new();

/// The two ways login can fail, for the login form (`http/auth.rs`) to
/// show as a generic, non-enumerating error. Never
/// distinguished any further than this - see the module doc comment on why
/// "unknown email" and "wrong password" must both land on `Unauthorized`.
pub(super) enum LoginError {
    Unauthorized,
    Internal,
}

/// The actual login logic - verifying email+password (with the
/// account-enumeration-safe dummy-hash comparison described in the module
/// doc comment) and issuing a session - for `POST /login`
/// (`http/auth.rs`) and the first-run admin setup. Returns the
/// resolved user alongside the raw (unhashed) session token, which becomes
/// the session cookie.
pub(super) async fn authenticate(
    state: &AppState,
    email: &str,
    password: &str,
) -> Result<(crate::db::UserRow, shared::auth::RawToken), LoginError> {
    // Looked up as stored (`signup::normalize_email`); an address that
    // can't be normalised matches no account, and still pays for the
    // dummy hash below like any unknown one.
    let email = super::signup::normalize_email(email).unwrap_or_default();
    let user = state
        .db
        .read(move |db| db.get_user_by_email(&email))
        .await
        .map_err(|_| LoginError::Internal)?;

    // Off the async threads (`shared::password::run`), the dummy hash's
    // one-time computation included.
    let (password, stored) = (
        password.to_string(),
        user.as_ref().map(|u| u.password_hash.clone()),
    );
    let password_ok = shared::password::run(move |hasher| {
        // A failed dummy hash is not cached: an empty one would make an
        // unknown email answer faster than a wrong password, which is what
        // the dummy exists to prevent.
        let dummy;
        let hash = match &stored {
            Some(hash) => hash.as_str(),
            None => match DUMMY_PASSWORD_HASH.get() {
                Some(hash) => hash.as_str(),
                None => {
                    dummy = hasher
                        .hash("not-a-real-account-dummy-password")
                        .unwrap_or_default();
                    if !dummy.is_empty() {
                        let _ = DUMMY_PASSWORD_HASH.set(dummy.clone());
                    }
                    dummy.as_str()
                }
            },
        };
        hasher.verify(&password, hash)
    })
    .await
    .ok_or(LoginError::Internal)?;

    // Require both a real user *and* a correct password - checking
    // `password_ok` alone would (in the astronomically unlikely case
    // someone's actual password collides with the dummy) let an unknown
    // email "succeed" with no user to attach a session to.
    let Some(user) = user.filter(|_| password_ok) else {
        return Err(LoginError::Unauthorized);
    };

    let raw_token = start_session(state, &user.id).await?;
    Ok((user, raw_token))
}

/// A new session for `user_id`: the raw token for its cookie. Shared by
/// logging in and signing up (a new account is logged in straight away).
pub(super) async fn start_session(
    state: &AppState,
    user_id: &crate::db::UserId,
) -> Result<shared::auth::RawToken, LoginError> {
    let raw_token = shared::auth::generate_session_token();
    let token_hash = raw_token.hash();
    let (hash, id) = (token_hash.clone(), user_id.clone());
    state
        .db
        .write(move |db| db.create_session(&hash, &id, now_unix()))
        .await
        .map_err(|_| LoginError::Internal)?;
    // The sign-in's own lines start the session's.
    super::record_identity(user_id.as_str(), &token_hash);
    Ok(raw_token)
}
