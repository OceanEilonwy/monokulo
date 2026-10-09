//! The Account page (`views::account`): `GET /account?tab=`, and the two
//! forms it posts, `POST /account/profile` and `POST /account/password`.
//!
//! A save redirects back to its tab (303) with what it saved in `saved=`,
//! for the page's cards and toast; one that's refused answers with the tab
//! again (422), showing why and keeping what was sent. That is the same with
//! or without JavaScript: the page reloads, so the nav (the theme, the
//! email in the account menu, the zone) shows the change at once.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::db::{Theme, UserRow};
use crate::views;
use crate::views::account::{AccountTab, AccountViewModel, Card, Outcome, Profile};

use super::dashboard::redirect_303;
use super::signup::{normalize_email, MIN_PASSWORD_LEN};
use super::{AppState, AuthedUser};

#[derive(Deserialize, Default)]
pub struct AccountQuery {
    tab: Option<String>,
    /// What the save that redirected here saved: card names joined with
    /// commas, or `nothing`.
    saved: Option<String>,
}

/// The profile as `user` has it saved.
fn saved_profile(user: &UserRow) -> Profile {
    Profile {
        email: user.email.clone(),
        theme: user.theme,
        timezone: user.timezone.clone(),
    }
}

/// What `saved=` says the last save did.
fn outcome_of(saved: Option<&str>) -> Option<Outcome> {
    match saved? {
        "nothing" => Some(Outcome::Unchanged),
        cards => {
            let cards: Vec<Card> = cards.split(',').filter_map(Card::from_id).collect();
            (!cards.is_empty()).then_some(Outcome::Saved(cards))
        }
    }
}

/// The page, for `tab`, with `shown` in the profile's form.
async fn render(
    state: &AppState,
    user: &UserRow,
    tab: AccountTab,
    shown: Profile,
    outcome: Option<Outcome>,
) -> Response {
    let chrome = super::page_chrome(state, Some(user), tab.href()).await;
    let (wallets, retired) = if tab == AccountTab::Wallets {
        match super::wallets::list(state, user, &chrome.clock).await {
            Some(lists) => lists,
            None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    } else {
        Default::default()
    };
    let refused = matches!(outcome, Some(Outcome::Refused { .. }));
    let data = AccountViewModel {
        tab,
        saved: saved_profile(user),
        shown,
        browser_zone: user.browser_timezone.clone(),
        outcome,
        wallets,
        retired,
    };
    let page = views::account::account_page(&chrome, &data);
    if refused {
        (StatusCode::UNPROCESSABLE_ENTITY, page).into_response()
    } else {
        page.into_response()
    }
}

/// `GET /account?tab=profile|wallets|security`.
pub async fn page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<AccountQuery>,
) -> Response {
    let tab = AccountTab::from_id(query.tab.as_deref());
    let outcome = outcome_of(query.saved.as_deref());
    render(&state, &user, tab, saved_profile(&user), outcome).await
}

#[derive(Deserialize)]
pub struct ProfileForm {
    email: String,
    theme: String,
    /// A zone name, or empty for automatic.
    #[serde(default)]
    timezone: String,
    /// A new email typed again: the dialog's, or the page's that asks for
    /// it without JavaScript. Empty when the email isn't changing.
    #[serde(default)]
    confirm_email: String,
}

/// `POST /account/profile`: the email, theme and time zone, saved all at
/// once or not at all. A new email is saved only once it was typed again
/// (no confirmation email is sent): without that, the answer is the page
/// asking for it.
pub async fn save_profile(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<ProfileForm>,
) -> Response {
    let saved = saved_profile(&user);
    let theme = match form.theme.as_str() {
        "system" => Some(Theme::System),
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    };
    let zone = form.timezone.trim();
    let shown = Profile {
        email: form.email.trim().to_string(),
        theme: theme.unwrap_or(saved.theme),
        timezone: (!zone.is_empty()).then(|| zone.to_string()),
    };
    let refuse = |card: Card, message: &str| Outcome::Refused {
        card,
        message: message.to_string(),
    };
    let Some(email) = normalize_email(&form.email) else {
        let outcome = refuse(Card::Email, "That is not an email address.");
        return render(&state, &user, AccountTab::Profile, shown, Some(outcome)).await;
    };
    let Some(theme) = theme else {
        let outcome = refuse(Card::Appearance, "Pick System, Light or Dark.");
        return render(&state, &user, AccountTab::Profile, shown, Some(outcome)).await;
    };
    if shown
        .timezone
        .as_deref()
        .is_some_and(|zone| jiff::tz::TimeZone::get(zone).is_err())
    {
        let outcome = refuse(Card::Time, "Pick a time zone from the list.");
        return render(&state, &user, AccountTab::Profile, shown, Some(outcome)).await;
    }
    let profile = Profile {
        email,
        theme,
        timezone: shown.timezone.clone(),
    };

    if profile.email != saved.email {
        let again = form.confirm_email.trim();
        if again.is_empty() || normalize_email(again).as_deref() != Some(profile.email.as_str()) {
            let error =
                (!again.is_empty()).then_some("That isn't the same email. Type the new one again.");
            let chrome = super::page_chrome(&state, Some(&user), "/account").await;
            let page = views::account::email_confirm_page(&chrome, &saved.email, &profile, error);
            let status = if error.is_some() {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::OK
            };
            return (status, page).into_response();
        }
    }

    let changed: Vec<Card> = [
        (Card::Email, profile.email != saved.email),
        (Card::Appearance, profile.theme != saved.theme),
        (Card::Time, profile.timezone != saved.timezone),
    ]
    .into_iter()
    .filter_map(|(card, changed)| changed.then_some(card))
    .collect();
    if changed.is_empty() {
        return redirect_303("/account?saved=nothing");
    }
    let (id, new) = (user.id.clone(), profile.clone());
    let written = state
        .db
        .write(move |db| {
            db.update_user_profile(&id, &new.email, new.theme, new.timezone.as_deref())
        })
        .await;
    match written {
        Ok(()) => {
            let cards: Vec<&str> = changed.iter().map(|card| card.id()).collect();
            redirect_303(&format!("/account?saved={}", cards.join(",")))
        }
        Err(e) if e.is_unique_violation() => {
            let outcome = refuse(
                Card::Email,
                "That email is already used by another account.",
            );
            render(&state, &user, AccountTab::Profile, shown, Some(outcome)).await
        }
        Err(e) => {
            tracing::error!(error = %e, "saving a profile failed");
            let outcome = refuse(Card::Email, "Something went wrong. Please try again.");
            render(&state, &user, AccountTab::Profile, shown, Some(outcome)).await
        }
    }
}

#[derive(Deserialize)]
pub struct PasswordForm {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
}

/// `POST /account/password`: a new password, given the current one. Every
/// other session of the user's ends, so anyone else signed in as them is
/// logged out; the one changing it stays.
pub async fn change_password(
    State(state): State<AppState>,
    AuthedUser(user, session): AuthedUser,
    Form(form): Form<PasswordForm>,
) -> Response {
    let (state, user) = (&state, &user);
    let refused = |message: String| async move {
        let outcome = Outcome::Refused {
            card: Card::Password,
            message,
        };
        render(
            state,
            user,
            AccountTab::Security,
            saved_profile(user),
            Some(outcome),
        )
        .await
    };
    if form.current_password.is_empty() && form.new_password.is_empty() {
        return redirect_303("/account?tab=security&saved=nothing");
    }
    let (current, stored) = (form.current_password.clone(), user.password_hash.clone());
    let current_ok = shared::password::run(move |hasher| hasher.verify(&current, &stored))
        .await
        .unwrap_or(false);
    if !current_ok {
        return refused("That isn't your current password.".to_string()).await;
    }
    if form.new_password.chars().count() < MIN_PASSWORD_LEN {
        return refused(format!(
            "The new password must be at least {MIN_PASSWORD_LEN} characters."
        ))
        .await;
    }
    let new = form.new_password.clone();
    let Some(Ok(hash)) = shared::password::run(move |hasher| hasher.hash(&new)).await else {
        return refused("Something went wrong. Please try again.".to_string()).await;
    };
    let id = user.id.clone();
    let keep = session.clone();
    match state
        .db
        .write(move |db| db.change_password(&id, &hash, &keep))
        .await
    {
        Ok(ended) => {
            tracing::info!(sessions.ended = ended, "a password was changed");
            redirect_303("/account?tab=security&saved=password")
        }
        Err(e) => {
            tracing::error!(error = %e, "changing a password failed");
            refused("Something went wrong. Please try again.".to_string()).await
        }
    }
}

#[cfg(test)]
mod tests;
