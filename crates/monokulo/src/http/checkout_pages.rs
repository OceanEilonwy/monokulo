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
use super::{AppState, AuthedUser};
use crate::db::{ConnectionId, UserRow};
use crate::views::checkout_layouts::Layout;
use crate::views::checkout_pages::{self, WizardStore, PREVIEW_STATES};
use crate::views::checkout_style::stylesheet_hash;

/// How many times a minute one store can save a draft. Each save reads, checks
/// and writes up to two 32 KiB texts.
const SAVES_PER_MINUTE: u32 = 30;
static SAVES: LazyLock<rate_limit::RateLimiter<ConnectionId>> =
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
    let now = crate::now_unix();
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
    let now = crate::now_unix();
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
        (None, None) => (Layout::Compact.source().to_string(), String::new()),
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
    let now = crate::now_unix();
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
    let store_id = row.id.clone();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let saved = state
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
        .await;
    match saved {
        Ok(()) => crate::base::see_other(format!("/stores/{id}/settings/checkout-page/preview")),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
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
    let now = crate::now_unix();
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
    // A link to send someone: whole, where this Monokulo knows its address.
    let path = format!("/checkout-preview/{}", draft.preview_token);
    let link = {
        state.network(&extensions).public_uri().map_or_else(
            || crate::base::url(&path),
            |base| format!("{}{path}", base.trim_end_matches('/')),
        )
    };
    let chrome = super::page_chrome(&state, Some(&user), format!("/stores/{id}/settings")).await;
    checkout_pages::preview_step(
        &chrome,
        &wizard_store(&row),
        &draft.preview_token,
        &link,
        size,
        html! {},
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
    let now = crate::now_unix();
    let lookup = token.clone();
    match state
        .db
        .read(move |db| db.checkout_page_draft_by_token(&lookup, now))
        .await
    {
        Ok(Some(_)) => {
            let chrome = crate::views::PageChrome::from_user(None, "/");
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
    let now = crate::now_unix();
    let found = state
        .db
        .read(move |db| {
            let Some(draft) = db.checkout_page_draft_by_token(&token, now)? else {
                return Ok::<_, crate::db::DbError>(None);
            };
            let store = db.get_store_connection_by_id(&draft.connection_id)?;
            Ok(store.map(|store| (draft, store)))
        })
        .await;
    let (draft, store) = match found {
        Ok(Some(found)) => found,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // Checked when it was saved; a release may have changed what reads.
    let Ok(template) = checkout_template::Template::load(&draft.template) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let style_hash = store.checkout_style.css().map(|css| stylesheet_hash(&css));
    let Ok(order) =
        checkout_pages::sample_order(preview_state(&query), &store.name, style_hash, now)
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let page_css = (!draft.written_css.is_empty()).then(|| stylesheet_hash(&draft.written_css));
    let chrome = crate::views::PageChrome::from_user(None, "/");
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

#[cfg(test)]
mod tests;
