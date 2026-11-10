//! A store's custom checkout page: writing it, and previewing it.
//!
//! A draft runs under nobody's approval, so nothing in it reaches a customer. A
//! preview is the one place it's ever shown: with a made-up order, in a sandboxed
//! frame that runs nothing and can't be opened on its own, under a page that says
//! what it is.

use std::sync::LazyLock;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use checkout_template::Problem;
use maud::html;
use serde::Deserialize;

use super::orders::load_owned_connection;
use super::{AppState, AuthedAdmin, AuthedUser};
use crate::db::{ConnectionId, UserRow};
use crate::views::checkout_layouts::Layout;
use crate::views::checkout_pages::{self, WizardStore, PREVIEW_STATES};
use crate::views::checkout_style::stylesheet_hash;

/// How many times a minute one store can save a draft. Each save reads, checks
/// and writes up to two 32 KiB texts.
pub(crate) const SAVES_PER_MINUTE: u32 = 30;
pub(crate) static SAVES: LazyLock<rate_limit::RateLimiter<ConnectionId>> =
    LazyLock::new(|| rate_limit::RateLimiter::new(SAVES_PER_MINUTE));

/// A template and CSS, checked: the CSS as it would be served.
pub struct CheckedDraft {
    pub template: String,
    pub css: String,
    pub written_css: String,
}

/// Everything saving a draft checks, for the site and the API alike, every
/// problem at once.
pub fn check_draft(template: &str, css: &str) -> Result<CheckedDraft, Vec<Problem>> {
    let template = checkout_template::normalize(template);
    let css = checkout_template::normalize(css);
    let mut problems = Vec::new();
    let parsed = match checkout_template::check(&template) {
        Ok(parsed) => Some(parsed),
        Err(found) => {
            problems.extend(found);
            // The CSS is only checked against classes in a template that reads.
            checkout_template::Template::load(&template).ok()
        }
    };
    let classes = parsed
        .as_ref()
        .map(|parsed| parsed.classes().clone())
        .unwrap_or_default();
    let written_css = match checkout_template::css::sanitize(&css, &classes) {
        Ok(written) => written,
        Err(found) => {
            if parsed.is_some() {
                problems.extend(found);
            }
            String::new()
        }
    };
    if problems.is_empty() {
        Ok(CheckedDraft {
            template,
            css,
            written_css,
        })
    } else {
        Err(problems)
    }
}

fn wizard_store(row: &super::OwnedStore) -> WizardStore<'_> {
    WizardStore {
        id: row.id.as_str(),
        name: &row.name,
    }
}

/// The store, if it's `user`'s and can have a custom page: only a verified domain
/// can, so a page goes back to Domains otherwise.
async fn store_for_wizard(
    state: &AppState,
    user: &UserRow,
    id: &ConnectionId,
) -> Result<super::OwnedStore, Box<Response>> {
    let row = match load_owned_connection(&state.db, user, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(Box::new(StatusCode::NOT_FOUND.into_response())),
        Err(()) => return Err(Box::new(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
    };
    let now = state.clock.now();
    let store_id = row.id.clone();
    let verified = state
        .db
        .read(move |db| db.list_store_domains(&store_id))
        .await
        .map(|domains| domains.iter().any(|domain| domain.state(now).counts()));
    match verified {
        Ok(true) => Ok(row),
        Ok(false) => Err(Box::new(crate::base::see_other(format!(
            "/stores/{id}/settings?tab=domains"
        )))),
        Err(_) => Err(Box::new(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
    }
}

#[derive(Deserialize)]
pub struct EditQuery {
    /// A layout to start from, replacing the draft on the page.
    start: Option<String>,
}

/// `GET /stores/{id}/settings/checkout-page`: step 1.
pub async fn edit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Query(query): Query<EditQuery>,
) -> Response {
    let row = match store_for_wizard(&state, &user, &id).await {
        Ok(row) => row,
        Err(response) => return *response,
    };
    let now = state.clock.now();
    let store_id = row.id.clone();
    let draft = match state
        .db
        .read(move |db| db.checkout_page_draft(&store_id, now))
        .await
    {
        Ok(draft) => draft,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let start = query.start.as_deref().and_then(Layout::from_name);
    let (template, css) = match (start, draft) {
        // Its classes aren't the draft's, so the draft's CSS wouldn't fit.
        (Some(layout), _) => (layout.source().to_string(), String::new()),
        (None, Some(draft)) => (draft.template, draft.css),
        // Editing what's in use starts a new version of it.
        (None, None) => {
            let store_id = row.id.clone();
            let page = state
                .db
                .read(move |db| db.published_checkout_page(&store_id))
                .await
                .ok()
                .flatten();
            match page {
                Some(page) => (page.template, page.css),
                None => (Layout::Compact.source().to_string(), String::new()),
            }
        }
    };
    let size = checkout_template::Template::load(&template)
        .ok()
        .map(|parsed| parsed.size());
    let chrome = super::page_chrome(&state, Some(&user), format!("/stores/{id}/settings")).await;
    crate::views::checkout_pages::edit_page(
        &chrome,
        &wizard_store(&row),
        &template,
        &css,
        &[],
        size,
    )
    .into_response()
}

#[derive(Deserialize)]
pub struct DraftForm {
    template: String,
    #[serde(default)]
    css: String,
}

/// `POST /stores/{id}/settings/checkout-page`: checks and saves the draft, then
/// shows its preview; or says what's wrong, keeping what was typed.
pub async fn save(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Form(form): Form<DraftForm>,
) -> Response {
    let row = match store_for_wizard(&state, &user, &id).await {
        Ok(row) => row,
        Err(response) => return *response,
    };
    let now = state.clock.now();
    let checked = if SAVES.check(row.id.clone(), now) {
        check_draft(&form.template, &form.css)
    } else {
        Err(Vec::new())
    };
    let too_often = matches!(&checked, Err(problems) if problems.is_empty());
    let checked = match checked {
        Ok(checked) => checked,
        Err(mut problems) => {
            // Said on the page, keeping what was typed.
            if too_often {
                problems.push(Problem {
                    place: checkout_template::Place::EveryState,
                    message: "That's a lot of saves. Wait a minute, then try again.".to_string(),
                    hint: None,
                });
            }
            let size = checkout_template::Template::load(&form.template)
                .ok()
                .map(|parsed| parsed.size());
            let chrome =
                super::page_chrome(&state, Some(&user), format!("/stores/{id}/settings")).await;
            return (
                if too_often {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::UNPROCESSABLE_ENTITY
                },
                checkout_pages::edit_page(
                    &chrome,
                    &wizard_store(&row),
                    &form.template,
                    &form.css,
                    &problems,
                    size,
                ),
            )
                .into_response();
        }
    };
    let saved = save_checked(&state, &row.id, checked, now).await;
    match saved {
        Ok(()) => crate::base::see_other(format!("/stores/{id}/settings/checkout-page/preview")),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A link to send someone: whole, where this Monokulo knows its address.
pub fn preview_link(public_uri: Option<String>, preview_token: &str) -> String {
    let path = format!("/checkout-preview/{preview_token}");
    public_uri.map_or_else(
        || crate::base::url(&path),
        |base| format!("{}{path}", base.trim_end_matches('/')),
    )
}

/// Saves a checked draft as the store's, with a new preview link.
pub async fn save_checked(
    state: &AppState,
    id: &ConnectionId,
    checked: CheckedDraft,
    now: clock::UnixTime,
) -> Result<(), crate::db::DbError> {
    let store_id = id.clone();
    let token = uuid::Uuid::new_v4().simple().to_string();
    state
        .db
        .write(move |db| {
            db.save_checkout_page_draft(
                &store_id,
                &checked.template,
                &checked.css,
                &checked.written_css,
                &token,
                now,
            )
        })
        .await
}

/// `GET /stores/{id}/settings/checkout-page/preview`: step 2.
pub async fn preview_step(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    extensions: axum::http::Extensions,
) -> Response {
    let row = match store_for_wizard(&state, &user, &id).await {
        Ok(row) => row,
        Err(response) => return *response,
    };
    let now = state.clock.now();
    let store_id = row.id.clone();
    let draft = match state
        .db
        .read(move |db| db.checkout_page_draft(&store_id, now))
        .await
    {
        Ok(Some(draft)) => draft,
        Ok(None) => return crate::base::see_other(format!("/stores/{id}/settings/checkout-page")),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let size = checkout_template::Template::load(&draft.template).map_or(0, |parsed| parsed.size());
    let link = preview_link(
        state.network(&extensions).public_uri(),
        &draft.preview_token,
    );
    let chrome = super::page_chrome(&state, Some(&user), format!("/stores/{id}/settings")).await;
    checkout_pages::preview_step(
        &chrome,
        &wizard_store(&row),
        &draft.preview_token,
        &link,
        size,
        html! {
            (ui::LinkButton::new(
                "Approve this version",
                crate::base::url(format!("/stores/{id}/settings/checkout-page/approve")),
            )
            .variant(ui::ButtonVariant::Primary))
        },
    )
    .into_response()
}

/// `POST /stores/{id}/settings/checkout-page/discard`.
pub async fn discard(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let store_id = row.id.clone();
    match state
        .db
        .write(move |db| db.discard_checkout_page_draft(&store_id))
        .await
    {
        Ok(()) => crate::base::see_other(format!("/stores/{id}/settings?tab=checkout")),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// How many times a minute one store can ask for its proof to be looked up. Each
/// can take DNS or Tor up to twenty seconds, so it's kept low.
pub(crate) const CHECKS_PER_MINUTE: u32 = 12;
pub(crate) static CHECKS: LazyLock<rate_limit::RateLimiter<ConnectionId>> =
    LazyLock::new(|| rate_limit::RateLimiter::new(CHECKS_PER_MINUTE));

/// What's needed to approve a store's draft.
pub struct Approval {
    /// Each must approve it.
    pub domains: Vec<String>,
    pub token: String,
    pub draft: crate::db::CheckoutPageDraft,
}

/// A store's draft and where its approval goes, if it has both.
pub async fn approval_for(
    state: &AppState,
    id: &ConnectionId,
) -> Result<Option<Approval>, crate::db::DbError> {
    let now = state.clock.now();
    let store_id = id.clone();
    let (draft, domains) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.checkout_page_draft(&store_id, now)?,
                db.list_store_domains(&store_id)?,
            ))
        })
        .await?;
    let domains = crate::checkout_pages::approver_names(&domains, now);
    let Some(draft) = draft.filter(|_| !domains.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(Approval {
        token: checkout_template::version_token(id.as_str(), &draft.template, &draft.css),
        domains,
        draft,
    }))
}

/// Why a draft wasn't published.
pub enum PublishRefused {
    /// No draft, or no domain that counts.
    NothingToPublish,
    /// A proof wasn't there.
    NotFound(String),
    /// The lookup got no answer.
    CantCheck(String),
    /// This Monokulo's operator has turned the store's page off.
    TurnedOff,
    /// Saved by an earlier release, it no longer passes today's checks.
    NoLongerPasses(Vec<Problem>),
    Failed,
}

/// Looks up a draft's proof on each domain and, if all are there, makes it the page
/// in use. The lookup is here, not just on the page before: publishing is what the approval
/// is for.
pub async fn publish_draft(state: &AppState, id: &ConnectionId) -> Result<(), PublishRefused> {
    let approval = match approval_for(state, id).await {
        Ok(Some(approval)) => approval,
        Ok(None) => return Err(PublishRefused::NothingToPublish),
        Err(_) => return Err(PublishRefused::Failed),
    };
    // What a lookup can't change comes first, so nobody is told to add a proof for
    // nothing.
    let store_id = id.clone();
    let turned_off = state
        .db
        .read(move |db| db.published_checkout_page(&store_id))
        .await
        .map_err(|_| PublishRefused::Failed)?
        .is_some_and(|page| page.turned_off_at.is_some());
    if turned_off {
        return Err(PublishRefused::TurnedOff);
    }
    // Saved by an earlier release, perhaps: what goes live is what today's checks
    // write. The approval still stands, as it's for what the merchant wrote.
    let written_css = check_draft(&approval.draft.template, &approval.draft.css)
        .map_err(PublishRefused::NoLongerPasses)?
        .written_css;
    let checks =
        crate::checkout_pages::check_every(&state.proofs, &approval.domains, &approval.token).await;
    match crate::checkout_pages::approved(&checks) {
        crate::checkout_pages::Approved::Yes => {}
        crate::checkout_pages::Approved::Missing(reason) => {
            return Err(PublishRefused::NotFound(reason))
        }
        crate::checkout_pages::Approved::NoAnswer(reason) => {
            return Err(PublishRefused::CantCheck(reason))
        }
    }
    let now = state.clock.now();
    let Approval {
        mut draft,
        token,
        domains,
    } = approval;
    draft.written_css = written_css;
    let store_id = id.clone();
    let published = state
        .db
        .write(move |db| db.publish_checkout_page(&draft, &token, &domains, now))
        .await;
    match published {
        Ok(true) => {
            tracing::info!(store.id = %store_id, "custom checkout page published");
            Ok(())
        }
        Ok(false) => Err(PublishRefused::TurnedOff),
        Err(_) => Err(PublishRefused::Failed),
    }
}

/// `GET /stores/{id}/settings/checkout-page/approve`: step 3.
pub async fn approve_step(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    show_approval(state, user, id, None).await
}

/// `POST .../approve`: looks for the proof, and says what it found.
pub async fn check_approval(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    show_approval(state, user, id, Some(())).await
}

async fn show_approval(
    state: AppState,
    user: UserRow,
    id: ConnectionId,
    check: Option<()>,
) -> Response {
    let row = match store_for_wizard(&state, &user, &id).await {
        Ok(row) => row,
        Err(response) => return *response,
    };
    let approval = match approval_for(&state, &row.id).await {
        Ok(Some(approval)) => approval,
        Ok(None) => return crate::base::see_other(format!("/stores/{id}/settings/checkout-page")),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let results: Vec<(String, checkout_pages::ApproveResult)> = if check.is_none() {
        approval
            .domains
            .iter()
            .map(|domain| (domain.clone(), checkout_pages::ApproveResult::NotChecked))
            .collect()
    } else if !CHECKS.check(row.id.clone(), state.clock.now()) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    } else {
        crate::checkout_pages::check_every(&state.proofs, &approval.domains, &approval.token)
            .await
            .into_iter()
            .map(|check| {
                let reason = check.reason.unwrap_or_default();
                let result = match check.outcome {
                    domain_proof::CheckOutcome::Found => checkout_pages::ApproveResult::Found,
                    domain_proof::CheckOutcome::LookupFailed(_) => {
                        checkout_pages::ApproveResult::CantCheck(reason)
                    }
                    _ => checkout_pages::ApproveResult::NotFound(reason),
                };
                (check.domain, result)
            })
            .collect()
    };
    let chrome = super::page_chrome(&state, Some(&user), format!("/stores/{id}/settings")).await;
    checkout_pages::approve_step(&chrome, &wizard_store(&row), &approval.token, &results)
        .into_response()
}

/// `POST .../publish`: the "Use this template" button.
pub async fn publish(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    let row = match store_for_wizard(&state, &user, &id).await {
        Ok(row) => row,
        Err(response) => return *response,
    };
    if !CHECKS.check(row.id.clone(), state.clock.now()) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    match publish_draft(&state, &row.id).await {
        Ok(()) => crate::base::see_other(format!(
            "/stores/{id}/settings?tab=checkout#card-custom-page"
        )),
        // Back to where it says what's wrong.
        Err(PublishRefused::NotFound(_) | PublishRefused::CantCheck(_)) => {
            crate::base::see_other(format!("/stores/{id}/settings/checkout-page/approve"))
        }
        Err(PublishRefused::NothingToPublish | PublishRefused::TurnedOff) => {
            crate::base::see_other(format!(
                "/stores/{id}/settings?tab=checkout#card-custom-page"
            ))
        }
        // The form says what's wrong, when it's checked again.
        Err(PublishRefused::NoLongerPasses(_)) => {
            crate::base::see_other(format!("/stores/{id}/settings/checkout-page"))
        }
        Err(PublishRefused::Failed) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `POST .../stop`: back to the store's layout.
pub async fn stop(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let store_id = row.id.clone();
    match state
        .db
        .write(move |db| db.stop_using_checkout_page(&store_id))
        .await
    {
        Ok(_) => crate::base::see_other(format!(
            "/stores/{id}/settings?tab=checkout#card-custom-page"
        )),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `POST .../recheck`: looks for the proof of the page in use, now.
pub async fn recheck(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let now = state.clock.now();
    if !CHECKS.check(row.id.clone(), now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let store_id = row.id.clone();
    let found = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.published_checkout_page(&store_id)?,
                db.list_store_domains(&store_id)?,
            ))
        })
        .await;
    let (page, domains) = match found {
        Ok(found) => found,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let domains = crate::checkout_pages::approver_names(&domains, now);
    if let Some(page) = page.filter(|_| !domains.is_empty()) {
        let checked = crate::checkout_pages::check_and_record(
            &state.db,
            &state.proofs,
            &row.id,
            &domains,
            &page.token,
            now,
        )
        .await;
        if checked.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    crate::base::see_other(format!(
        "/stores/{id}/settings?tab=checkout#card-custom-page"
    ))
}

/// What the Checkout tab's card says about a store's custom page.
pub(super) async fn page_card(
    state: &AppState,
    id: &ConnectionId,
    domains: &[crate::db::StoreDomainRow],
    now: clock::UnixTime,
) -> checkout_pages::PageCard {
    use crate::checkout_pages::{approver_names, showing_on, standing, waiting_on, PageStanding};
    use checkout_pages::PageCard;
    let store_id = id.clone();
    let found = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.published_checkout_page(&store_id)?,
                db.checkout_page_draft(&store_id, now)?,
            ))
        })
        .await;
    let Ok((page, draft)) = found else {
        return PageCard::None;
    };
    let names = approver_names(domains, now);
    let has_draft = draft.is_some();
    let Some(page) = page else {
        return match draft {
            _ if names.is_empty() => PageCard::NeedsDomain,
            Some(draft) => PageCard::Draft {
                domains: names,
                days_left: days_left(
                    clock::UnixTime::from_secs(draft.created_at)
                        .saturating_add(crate::db::DRAFT_LIFETIME),
                    now,
                ),
            },
            None => PageCard::None,
        };
    };
    match standing(&page, domains, now) {
        PageStanding::TurnedOff => PageCard::TurnedOff {
            reason: page.turned_off_reason.filter(|reason| !reason.is_empty()),
        },
        PageStanding::NotShowing => PageCard::NotShowing {
            reason: page.last_error,
            draft: has_draft,
        },
        PageStanding::Failing { since } => PageCard::Failing {
            domains: names,
            reason: page.last_error,
            token: page.token,
            days_left: days_left(since.saturating_add(domain_proof::GRACE), now),
            draft: has_draft,
        },
        PageStanding::Working => PageCard::Working {
            domains: showing_on(&page, domains, now)
                .into_iter()
                .map(|row| row.domain.clone())
                .collect(),
            waiting: waiting_on(&page, domains, now),
            checked: crate::views::time::Clock::utc(now)
                .relative(page.last_checked_at.unwrap_or(page.published_at)),
            trouble: page.last_error,
            draft: has_draft,
        },
    }
}

/// The whole days left until `ends`, counting the one under way: what a
/// card says ("3 days left").
fn days_left(ends: clock::UnixTime, now: clock::UnixTime) -> i64 {
    i64::try_from(now.until(ends).as_secs() / (24 * 60 * 60)).unwrap_or(i64::MAX) + 1
}

#[derive(Deserialize)]
pub struct PreviewQuery {
    state: Option<String>,
}

fn preview_state(query: &PreviewQuery) -> &'static checkout_pages::PreviewState {
    query
        .state
        .as_deref()
        .and_then(|slug| PREVIEW_STATES.iter().find(|state| state.slug == slug))
        .unwrap_or(&PREVIEW_STATES[0])
}

/// A preview is nobody's search result.
const NO_INDEX: (header::HeaderName, &str) = (
    header::HeaderName::from_static("x-robots-tag"),
    "noindex, nofollow",
);

/// `GET /checkout-preview/{token}`: what a preview link opens.
pub async fn shared_preview(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(query): Query<PreviewQuery>,
) -> Response {
    let now = state.clock.now();
    let lookup = token.clone();
    match state
        .db
        .read(move |db| db.checkout_page_draft_by_token(&lookup, now))
        .await
    {
        Ok(Some(_)) => {
            let chrome = crate::views::PageChrome::from_user(None, "/", now);
            (
                [NO_INDEX],
                checkout_pages::shared_preview(&chrome, &token, preview_state(&query)),
            )
                .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Only as a frame of this Monokulo's own pages. Opened on its own, a draft
/// would be a merchant's unapproved words at this Monokulo's address. Browsers don't
/// send fetch metadata over plain http, so there, a `Referer` from one of the pages
/// that frames it stands in: opened directly, it has none.
fn framed_here(headers: &HeaderMap) -> bool {
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let is = |value: Option<&str>, wanted: &[&str]| {
        value.is_some_and(|value| wanted.iter().any(|w| value.eq_ignore_ascii_case(w)))
    };
    let (dest, site) = (text("sec-fetch-dest"), text("sec-fetch-site"));
    if dest.is_some() || site.is_some() {
        return is(dest, &["iframe", "frame"]) && is(site, &["same-origin"]);
    }
    let from_a_preview = text("referer")
        .and_then(|referer| url::Url::parse(referer).ok())
        .zip(text("host"))
        .is_some_and(|(referer, host)| {
            let same_host = referer.host_str().is_some_and(|name| {
                let port = referer
                    .port()
                    .map_or_else(String::new, |port| format!(":{port}"));
                format!("{name}{port}").eq_ignore_ascii_case(host)
            });
            let path = referer.path();
            same_host
                && (path.contains("/checkout-preview/")
                    || path.ends_with("/settings/checkout-page/preview")
                    || path.ends_with("/settings/checkout-layout")
                    || path.contains("/admin/custom-pages/"))
        });
    from_a_preview
}

/// `GET /checkout-preview/{token}/frame`: one state of a draft, which runs nothing
/// and can't go anywhere.
pub async fn preview_frame(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(query): Query<PreviewQuery>,
    headers: HeaderMap,
) -> Response {
    if !framed_here(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let now = state.clock.now();
    let at = now;
    let found = state
        .db
        .read(move |db| {
            let Some(draft) = db.checkout_page_draft_by_token(&token, at)? else {
                return Ok::<_, crate::db::DbError>(None);
            };
            let store = db.get_store_connection_by_id(&draft.connection_id)?;
            Ok(store.map(|store| (draft, store)))
        })
        .await;
    match found {
        Ok(Some((draft, store))) => {
            frame_of(&store, &draft.template, &draft.written_css, &query, now)
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
pub struct LayoutQuery {
    layout: Option<String>,
}

impl LayoutQuery {
    /// The layout asked for, or the store's own.
    fn layout(&self, store: &crate::db::StoreConnectionRow) -> Layout {
        self.layout
            .as_deref()
            .and_then(Layout::from_name)
            .unwrap_or(store.checkout_layout)
    }
}

/// `GET /stores/{id}/settings/checkout-layout`: every state of a stock layout.
pub async fn layout_preview(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Query(query): Query<LayoutQuery>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let layout = query.layout(&row);
    let chrome = super::page_chrome(&state, Some(&user), format!("/stores/{id}/settings")).await;
    checkout_pages::layout_preview(&chrome, &wizard_store(&row), layout).into_response()
}

/// `GET /stores/{id}/settings/checkout-layout/frame`: one state of a stock layout,
/// in the store's colours, as a frame of the page above.
pub async fn layout_preview_frame(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Query(layout): Query<LayoutQuery>,
    Query(query): Query<PreviewQuery>,
    headers: HeaderMap,
) -> Response {
    if !framed_here(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    frame_of(
        &row,
        layout.layout(&row).source(),
        "",
        &query,
        state.clock.now(),
    )
}

/// One state of a page on a made-up order, sandboxed.
fn frame_of(
    store: &crate::db::StoreConnectionRow,
    source: &str,
    written_css: &str,
    query: &PreviewQuery,
    now: clock::UnixTime,
) -> Response {
    // Checked when it was saved; a release may have changed what reads.
    let Ok(template) = checkout_template::Template::load(source) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let style_hash = store.checkout_style.css().map(|css| stylesheet_hash(&css));
    let Ok(order) =
        checkout_pages::sample_order(preview_state(query), &store.name, style_hash, now)
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let page_css = (!written_css.is_empty()).then(|| stylesheet_hash(written_css));
    let chrome = crate::views::PageChrome::from_user(None, "/", now);
    (
        [
            (
                header::CONTENT_SECURITY_POLICY,
                crate::views::checkout::preview_policy(),
            ),
            (NO_INDEX.0, NO_INDEX.1.to_string()),
        ],
        crate::views::checkout::preview_page(&chrome, &order, &template, page_css.as_deref()),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct AdminListQuery {
    q: Option<String>,
}

/// `GET /admin/custom-pages`: every custom page here, for the operator.
pub async fn admin_list(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Query(query): Query<AdminListQuery>,
) -> Response {
    let now = state.clock.now();
    let find = query.q.clone().filter(|q| !q.trim().is_empty());
    let listings = match state
        .db
        .read(move |db| db.list_custom_pages(find.as_deref().map(str::trim)))
        .await
    {
        Ok(listings) => listings,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let clock = crate::views::time::Clock::utc(now);
    let rows: Vec<checkout_pages::AdminPageRow> = listings
        .into_iter()
        .map(|listing| {
            let standing = crate::checkout_pages::standing(&listing.page, &listing.domains, now);
            checkout_pages::AdminPageRow {
                store_id: listing.page.connection_id.to_string(),
                store_name: listing.store_name,
                domain: match crate::checkout_pages::approver_names(&listing.domains, now) {
                    names if names.is_empty() => "None".to_string(),
                    names => names.join(", "),
                },
                status: match standing {
                    crate::checkout_pages::PageStanding::Working => "Working",
                    crate::checkout_pages::PageStanding::Failing { .. } => "Failing",
                    crate::checkout_pages::PageStanding::NotShowing => "Not showing",
                    crate::checkout_pages::PageStanding::TurnedOff => "Turned off",
                },
                published: clock.relative(listing.page.published_at),
                off: standing == crate::checkout_pages::PageStanding::TurnedOff,
            }
        })
        .collect();
    let chrome = super::page_chrome(&state, Some(&admin), "/admin/custom-pages").await;
    checkout_pages::admin_list(&chrome, &rows, query.q.as_deref().unwrap_or("")).into_response()
}

async fn store_with_page(
    state: &AppState,
    id: &ConnectionId,
) -> Result<
    (
        crate::db::StoreConnectionRow,
        crate::db::PublishedCheckoutPage,
    ),
    StatusCode,
> {
    let store_id = id.clone();
    let found = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.get_store_connection_by_id(&store_id)?,
                db.published_checkout_page(&store_id)?,
            ))
        })
        .await;
    match found {
        Ok((Some(store), Some(page))) => Ok((store, page)),
        Ok(_) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// `GET /admin/custom-pages/{id}`: the operator looks at a store's page, on a
/// made-up order so no real one is needed.
pub async fn admin_view(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Path(id): Path<ConnectionId>,
    Query(query): Query<PreviewQuery>,
) -> Response {
    let store = match store_with_page(&state, &id).await {
        Ok((store, _)) => store,
        Err(status) => return status.into_response(),
    };
    let chrome = super::page_chrome(&state, Some(&admin), "/admin/custom-pages").await;
    checkout_pages::admin_view(&chrome, id.as_str(), &store.name, preview_state(&query))
        .into_response()
}

/// `GET /admin/custom-pages/{id}/frame`.
pub async fn admin_frame(
    State(state): State<AppState>,
    AuthedAdmin(_, _): AuthedAdmin,
    Path(id): Path<ConnectionId>,
    Query(query): Query<PreviewQuery>,
    headers: HeaderMap,
) -> Response {
    if !framed_here(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match store_with_page(&state, &id).await {
        Ok((store, page)) => frame_of(
            &store,
            &page.template,
            &page.written_css,
            &query,
            state.clock.now(),
        ),
        Err(status) => status.into_response(),
    }
}

/// `GET /admin/custom-pages/{id}/off`: asks first.
pub async fn admin_ask_off(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Path(id): Path<ConnectionId>,
) -> Response {
    let store = match store_with_page(&state, &id).await {
        Ok((store, _)) => store,
        Err(status) => return status.into_response(),
    };
    let chrome = super::page_chrome(&state, Some(&admin), "/admin/custom-pages").await;
    checkout_pages::admin_turn_off(&chrome, id.as_str(), &store.name, store.checkout_layout)
        .into_response()
}

#[derive(Deserialize)]
pub struct TurnOffForm {
    #[serde(default)]
    reason: String,
}

/// `POST /admin/custom-pages/{id}/off`.
pub async fn admin_turn_off(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Path(id): Path<ConnectionId>,
    Form(form): Form<TurnOffForm>,
) -> Response {
    let reason: String = form.reason.trim().chars().take(280).collect();
    let now = state.clock.now();
    let (store_id, logged) = (id.clone(), reason.clone());
    let changed = state
        .db
        .write(move |db| db.set_checkout_page_turned_off(&store_id, Some((now, &logged))))
        .await;
    match changed {
        Ok(true) => {
            tracing::info!(admin.id = %admin.id, store.id = %id, reason = %reason, "custom checkout page turned off");
            crate::base::see_other("/admin/custom-pages")
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `POST /admin/custom-pages/{id}/on`.
pub async fn admin_turn_on(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Path(id): Path<ConnectionId>,
) -> Response {
    let store_id = id.clone();
    let changed = state
        .db
        .write(move |db| db.set_checkout_page_turned_off(&store_id, None))
        .await;
    match changed {
        Ok(true) => {
            tracing::info!(admin.id = %admin.id, store.id = %id, "custom checkout page turned back on");
            crate::base::see_other("/admin/custom-pages")
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests;
