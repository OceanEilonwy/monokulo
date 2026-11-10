//! The dashboard side of verified embed domains (`crate::embed_domains`):
//! adding a domain (its own three pages: the domain, the record, the
//! check), checking and removing a store's domains and turning the
//! restriction on or off on its settings page, and the warnings its store
//! page shows - plus [`embed_policy_middleware`], which enforces the
//! restriction on the public routes (`/api/v1`, `/api/v1/checkout/{order_id}`).

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::db::{AddDomainRefused, StoreDomainRow};
use crate::embed_domains;
use crate::templates::{format_duration_since, format_duration_until};
use crate::views::domains::{DomainStatus, DomainView, StoreRef};
use crate::views::store_detail::{EmbedWarnings, FailingDomainWarning, Proof};
use crate::views::time::Clock;
use domain_proof::DomainState;

use super::fx::FxRequest;
use super::orders::{load_owned_connection, render_store_settings_page, saved};
use super::store_key::{ApiCaller, RequestStore};
use super::{AppState, AuthedUser};
use crate::api::runtime::problem::Problem;
use crate::db::Database;
use crate::views::store_settings::StoreSection;

/// Whether a request to `path` makes an order: the one route
/// [`embed_policy_middleware`] may refuse.
fn creates_order(method: &Method, path: &str) -> bool {
    method == Method::POST && path == crate::api::v1::orders::Path::TEMPLATE
}

/// The problems [`embed_policy_middleware`] answers a route of the API with
/// before the route runs, for the OpenAPI document: making an order with
/// the public key, `forbidden` from a site the store hasn't verified and
/// `store-unavailable` while its policy can't be read.
pub(crate) fn api_route_problems(method: &Method, route: &str) -> &'static [Problem] {
    if creates_order(method, route) {
        &[Problem::Forbidden, Problem::StoreUnavailable]
    } else {
        &[]
    }
}

/// Enforces a restricted store's embed policy on its public routes (CORS
/// is `super::embed_cors_layer`'s job). The store is the one
/// `super::abuse::api_and_checkout_middleware` found ([`RequestStore`]):
///
/// - every response carries `Content-Security-Policy: frame-ancestors ...`,
///   so browsers only show the checkout inside monokulo itself or a
///   verified domain;
/// - `POST /api/v1/orders` with the store's public key must come from a
///   browser page on a verified domain (its `Origin`). With the store's
///   secret key or an app's (a shop plugin on the shop's server, a till)
///   it may come from anywhere. Anything else gets `403`, including a
///   request with the public key and no `Origin`, which could otherwise
///   come from any script anywhere.
///
/// An unrestricted store's requests pass through untouched. While the
/// policy can't be read (the database didn't answer), nothing is taken as
/// unrestricted: an order made with the public key is `503` and a page is
/// framed by monokulo alone, since a store that restricted itself may be
/// the one asking.
pub async fn embed_policy_middleware(
    State(state): State<super::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let creates_order = creates_order(request.method(), request.uri().path());
    let has_key = request
        .extensions()
        .get::<ApiCaller>()
        .is_some_and(|caller| caller.credential.is_store_key());
    let policy = match request.extensions().get::<RequestStore>() {
        Some(RequestStore(store)) => {
            embed_domains::policy_for_public_key(&state.db, &store.tenant_public_key).await
        }
        None => Ok(None),
    };
    let policy = match policy {
        Ok(policy) => policy.filter(|policy| policy.restricted),
        Err(_) if creates_order && !has_key => {
            return Problem::StoreUnavailable
                .detail(
                    "This store's settings can't be read right now. Please try again shortly."
                        .to_owned(),
                )
                .into_response();
        }
        Err(_) => {
            let mut response = next.run(request).await;
            add_policy(response.headers_mut(), "frame-ancestors 'self'");
            return response;
        }
    };
    let Some(policy) = policy else {
        return next.run(request).await;
    };
    let now = state.clock.now();

    if creates_order && !has_key {
        let error = match request.headers().get(header::ORIGIN) {
            Some(origin) if origin.to_str().is_ok_and(|origin| policy.allows_origin(origin, now)) => None,
            Some(_) => Some("This store only accepts orders from its verified websites."),
            None => Some(
                "This store only accepts orders from its verified websites, or from its own server using the store's secret key.",
            ),
        };
        if let Some(error) = error {
            return Problem::Forbidden.detail(error.to_owned()).into_response();
        }
    }

    let mut response = next.run(request).await;
    if let Some(value) = policy.frame_ancestors(now) {
        add_policy(response.headers_mut(), &value);
    }
    response
}

/// Adds a Content-Security-Policy beside any the response already has: in the
/// one header, comma-separated, which a browser enforces as separate policies,
/// so neither can loosen the other.
fn add_policy(headers: &mut axum::http::HeaderMap, policy: &str) {
    let value = match headers
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|existing| existing.to_str().ok())
    {
        Some(existing) => format!("{existing}, {policy}"),
        None => policy.to_string(),
    };
    if let Ok(value) = HeaderValue::from_str(&value) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
}

/// One of a store's domains as its card and its pages show it; `wait` is
/// the seconds left before it may be checked again, when a check was asked
/// for too soon.
///
/// `rows` are the store's domains, to say what a change underway replaces and
/// is replaced by.
pub(super) fn domain_view(
    row: StoreDomainRow,
    rows: &[StoreDomainRow],
    now: clock::UnixTime,
    wait: Option<i64>,
) -> DomainView {
    let replaces = row.replaces_id.as_ref().and_then(|id| {
        rows.iter()
            .find(|other| other.id == *id)
            .map(|other| other.domain.clone())
    });
    let being_replaced = rows
        .iter()
        .any(|other| other.replaces_id.as_deref() == Some(row.id.as_str()));
    let clock = Clock::utc(now);
    let checked = row.last_checked_at.map(|at| clock.relative(at));
    // An onion shop is proved by a file it serves over Tor, never by DNS.
    let onion = domain_proof::is_onion(&row.domain);
    let (proof, publish) = if onion {
        ("File", "Serve it again")
    } else {
        ("Record", "Publish it again")
    };
    let (status, meta, reason) = match row.state(now) {
        DomainState::Verified => (
            DomainStatus::Verified,
            // Only a check with no answer leaves an error on a verified
            // domain that isn't failing.
            match &row.last_error {
                Some(error) => format!(
                    "Checked {}, with no answer ({error}) · still verified, trying again soon",
                    checked.as_deref().unwrap_or("never")
                ),
                None => format!(
                    "Checked {} · checked again every day",
                    checked.as_deref().unwrap_or("never")
                ),
            },
            None,
        ),
        DomainState::Pending => (
            DomainStatus::Waiting,
            format!(
                "Added {} · {}",
                clock.relative(row.created_at),
                checked.map_or_else(
                    || "not checked yet".to_string(),
                    |at| format!("checked {at}")
                )
            ),
            Some(row.last_error.clone().unwrap_or_else(|| {
                if onion {
                    "Not checked yet: serve the file, then check again.".to_string()
                } else {
                    "Not checked yet: publish the record, then check again.".to_string()
                }
            })),
        ),
        DomainState::Failing { since } => {
            let (missing, left) = (
                format_duration_since(since, now),
                format_duration_until(since.saturating_add(domain_proof::GRACE), now),
            );
            (
                DomainStatus::Failing,
                format!("{proof} missing for {missing} · still counts as verified for {left}"),
                Some(format!(
                    "The {} was there, and went {missing} ago. {publish} within {left} to keep the domain.",
                    proof.to_lowercase()
                )),
            )
        }
        DomainState::Lapsed { since } => {
            let missing = format_duration_since(since, now);
            (
                DomainStatus::Lapsed,
                format!("{proof} missing for {missing} · it no longer counts"),
                Some(format!(
                    "The {} has been missing for {missing}, so the domain no longer counts. {publish} and check.",
                    proof.to_lowercase()
                )),
            )
        }
    };
    DomainView {
        record_host: domain_proof::VERIFY.label,
        record_value: domain_proof::VERIFY.value(&row.token),
        id: row.id,
        domain: row.domain,
        status,
        meta,
        reason,
        wait,
        replaces,
        being_replaced,
    }
}

/// The settings page's cards for a store's domains.
pub(super) fn domain_views(rows: Vec<StoreDomainRow>, now: clock::UnixTime) -> Vec<DomainView> {
    rows.iter()
        .map(|row| domain_view(row.clone(), &rows, now, None))
        .collect()
}

/// The store page's embed warnings.
pub(super) async fn store_page_warnings(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    now: clock::UnixTime,
) -> EmbedWarnings {
    let id = connection_id.clone();
    let (dismissed, restricted, rows) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.embed_warning_dismissed(&id).unwrap_or(false),
                db.embed_restricted(&id).unwrap_or(false),
                db.list_store_domains(&id).unwrap_or_default(),
            ))
        })
        .await
        .unwrap_or_default();
    let shown_nowhere = restricted && !rows.iter().any(|row| row.state(now).counts());
    let failing = rows
        .into_iter()
        .filter_map(|row| {
            let (since, lapsed) = match row.state(now) {
                DomainState::Failing { since } => (since, false),
                DomainState::Lapsed { since } => (since, true),
                DomainState::Pending | DomainState::Verified => return None,
            };
            Some(FailingDomainWarning {
                proof: Proof::of(&row.domain),
                domain: row.domain,
                missing_for: format_duration_since(since, now),
                lapsed,
                counts_for: format_duration_until(since.saturating_add(domain_proof::GRACE), now),
            })
        })
        .collect();
    EmbedWarnings {
        restricted,
        any_site_dismissed: dismissed,
        shown_nowhere,
        failing,
    }
}

#[derive(Deserialize)]
pub struct EmbedRestrictionForm {
    /// `"on"` or `"off"`.
    pub restricted: String,
}

/// `POST /stores/{id}/settings/embed-restriction` - turns "Only my
/// verified domains can show this checkout" on or off. It can only be
/// turned on while at least one domain counts as verified; otherwise the
/// checkout could be shown nowhere.
pub async fn set_embed_restriction(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    Form(form): Form<EmbedRestrictionForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Domains;
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let restricted = form.restricted == "on";
    let now = state.clock.now();
    let store_id = row.id.clone();
    let result = state
        .db
        .write(move |db| match db.list_store_domains(&store_id) {
            Ok(domains) if restricted && !domains.iter().any(|domain| domain.state(now).counts()) => {
                Ok(Some("Verify at least one domain before turning this on - otherwise no website could show your checkout."))
            }
            Ok(_) => db.set_embed_restricted(&store_id, restricted).map(|()| None),
            Err(e) => Err(e),
        })
        .await;
    match result {
        Ok(None) => saved(&id, SECTION),
        Ok(Some(error)) => {
            render_store_settings_page(
                &state,
                row,
                &user,
                Some(error.to_string()),
                None,
                Some(SECTION),
            )
            .await
        }
        Err(_) => {
            render_store_settings_page(
                &state,
                row,
                &user,
                Some("Something went wrong. Please try again.".to_string()),
                None,
                Some(SECTION),
            )
            .await
        }
    }
}

fn store_ref(row: &super::OwnedStore) -> StoreRef<'_> {
    StoreRef {
        id: row.id.as_str(),
        name: &row.name,
    }
}

/// A page of a store's settings, as `user` sees it.
async fn store_page(
    state: &AppState,
    user: &crate::db::UserRow,
    row: &super::OwnedStore,
    render: impl FnOnce(&crate::views::PageChrome, &StoreRef) -> maud::Markup,
) -> maud::Markup {
    let chrome =
        super::page_chrome(state, Some(user), format!("/stores/{}/settings", row.id)).await;
    render(&chrome, &store_ref(row))
}

/// `GET /stores/{id}/settings/domains/new` - adding a domain, its
/// first step: the domain.
pub async fn add_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    Query(query): Query<AddDomainQuery>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let replacing = match replacing(&state, &row, query.replaces).await {
        Ok(replacing) => replacing,
        Err(status) => return status.into_response(),
    };
    store_page(&state, &user, &row, |chrome, store| {
        crate::views::domains::add_page(chrome, store, "", None, replacing.as_ref())
    })
    .await
    .into_response()
}

/// The store's domain `replaces` names, for the page changing it.
async fn replacing(
    state: &AppState,
    row: &super::OwnedStore,
    replaces: Option<String>,
) -> Result<Option<StoreDomainRow>, StatusCode> {
    let Some(replaces) = replaces else {
        return Ok(None);
    };
    let store_id = row.id.clone();
    match state
        .db
        .read(move |db| db.get_store_domain(&store_id, &replaces))
        .await
    {
        Ok(Some(domain)) => Ok(Some(domain)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
pub struct AddDomainQuery {
    /// Changing this domain, rather than adding one.
    replaces: Option<String>,
}

#[derive(Deserialize)]
pub struct AddDomainForm {
    pub domain: String,
    /// Changing this domain, rather than adding one.
    pub replaces: Option<String>,
}

/// `POST /stores/{id}/settings/domains` - adds a domain waiting
/// for its DNS record, then shows the record to publish. One that can't be
/// added answers with the domain's page again, saying why.
pub async fn add_domain(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    Form(form): Form<AddDomainForm>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let replacing = match replacing(&state, &row, form.replaces.clone()).await {
        Ok(replacing) => replacing,
        Err(status) => return status.into_response(),
    };
    let refused = |error: String| {
        let (state, user, row, typed, replacing) =
            (&state, &user, &row, form.domain.clone(), &replacing);
        async move {
            let page = store_page(state, user, row, |chrome, store| {
                crate::views::domains::add_page(
                    chrome,
                    store,
                    &typed,
                    Some(&error),
                    replacing.as_ref(),
                )
            })
            .await;
            (StatusCode::UNPROCESSABLE_ENTITY, page).into_response()
        }
    };
    let domain = match domain_proof::normalize_domain(&form.domain) {
        Ok(domain) => domain,
        Err(message) => return refused(message.to_string()).await,
    };
    let domain_id = uuid::Uuid::new_v4().to_string();
    let (store_id, new_domain, new_id) = (row.id.clone(), domain.clone(), domain_id.clone());
    let replaces = form.replaces.clone();
    let now = state.clock.now();
    let created = state
        .db
        .write(move |db| {
            db.create_store_domain(
                &new_id,
                &store_id,
                &new_domain,
                &embed_domains::new_token(),
                now,
                replaces.as_deref(),
            )
        })
        .await;
    let error = match created {
        Ok(Ok(())) => {
            return crate::base::see_other(format!(
                "/stores/{id}/settings/domains/{domain_id}/record"
            ))
        }
        Ok(Err(AddDomainRefused::AlreadyOnStore)) => {
            format!("{domain} is already on this store's list.")
        }
        Ok(Err(AddDomainRefused::SlotTaken(existing))) => format!(
            "This store already has {existing}, and a store can have one web domain and one onion address. Change it, or create a store for another website."
        ),
        Ok(Err(AddDomainRefused::ChangeUnderway)) => {
            "A change of domain is already underway. Finish or cancel it first.".to_string()
        }
        Ok(Err(AddDomainRefused::NotSameKind)) => {
            "A web domain can only change to a web domain, and an onion address to an onion address.".to_string()
        }
        Ok(Err(AddDomainRefused::NothingToReplace)) => {
            "The domain you're changing has been removed.".to_string()
        }
        // Another add took the store's one place first.
        Err(e) if e.is_unique_violation() => {
            "This store's domains just changed. Check them, then try again.".to_string()
        }
        Err(_) => "Something went wrong. Please try again.".to_string(),
    };
    refused(error).await
}

/// The store and one of its domains, or the status when either isn't
/// there (or isn't `user`'s).
async fn load_domain(
    state: &AppState,
    user: &crate::db::UserRow,
    id: &crate::db::ConnectionId,
    domain_id: String,
) -> Result<(super::OwnedStore, StoreDomainRow), StatusCode> {
    let row = match load_owned_connection(&state.db, user, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(()) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let store_id = row.id.clone();
    match state
        .db
        .read(move |db| db.get_store_domain(&store_id, &domain_id))
        .await
    {
        Ok(Some(domain)) => Ok((row, domain)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// `GET /stores/{id}/settings/domains/{domain_id}/record` - the
/// TXT record to publish for a domain, and the way to its check.
pub async fn record_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, domain_id)): Path<(crate::db::ConnectionId, String)>,
) -> Response {
    let (row, domain) = match load_domain(&state, &user, &id, domain_id).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    let view = domain_view(domain, &[], state.clock.now(), None);
    store_page(&state, &user, &row, |chrome, store| {
        crate::views::domains::record_page(chrome, store, &view)
    })
    .await
    .into_response()
}

/// `GET /stores/{id}/settings/domains/{domain_id}/check` - what
/// the last check of a domain found.
pub async fn check_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, domain_id)): Path<(crate::db::ConnectionId, String)>,
) -> Response {
    let (row, domain) = match load_domain(&state, &user, &id, domain_id).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    let checked = domain.last_checked_at.is_some();
    let view = domain_view(domain, &[], state.clock.now(), None);
    store_page(&state, &user, &row, |chrome, store| {
        crate::views::domains::check_page(chrome, store, &view, checked)
    })
    .await
    .into_response()
}

#[derive(Deserialize, Default)]
pub struct CheckForm {
    /// `check` from the check page (its result is what's swapped);
    /// absent from a domain's card on the settings page.
    #[serde(default)]
    pub from: Option<String>,
}

/// `POST /stores/{id}/settings/domains/{domain_id}/check` - looks
/// the domain's record up now, unless it was looked up moments ago
/// ([`domain_proof::MIN_CHECK_GAP`]: then the answer says when it can
/// be, in place). The result is never a "saved" toast: with JavaScript it's
/// swapped in where it was asked for (the check page's result, or the
/// domain's card, with the restriction banner, which a verified domain may
/// enable); without, it's the check page.
pub async fn check_domain(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path((id, domain_id)): Path<(crate::db::ConnectionId, String)>,
    Form(form): Form<CheckForm>,
) -> Response {
    let (row, domain) = match load_domain(&state, &user, &id, domain_id.clone()).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    let now = state.clock.now();
    let replaced = domain.replaces_id.clone();
    let wait = domain
        .last_checked_at
        .map(|last| {
            let waited = now.since(clock::UnixTime::from_secs(last));
            i64::try_from(domain_proof::MIN_CHECK_GAP.saturating_sub(waited).as_secs()).unwrap_or(0)
        })
        .filter(|wait| *wait > 0);
    if wait.is_none()
        && embed_domains::check_and_record(&state.db, &state.proofs, &domain, now)
            .await
            .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let store_id = row.id.clone();
    let (domain, domains, restricted, can_restrict) = match state
        .db
        .read(move |db| {
            let domains = db.list_store_domains(&store_id)?;
            Ok::<_, crate::db::DbError>((
                domains.iter().find(|d| d.id == domain_id).cloned(),
                db.embed_restricted(&store_id).unwrap_or(false),
                domains.iter().any(|d| d.state(now).counts()),
                domains,
            ))
        })
        .await
    {
        Ok((Some(domain), restricted, can_restrict, domains)) => {
            (domain, domains, restricted, can_restrict)
        }
        Ok((None, ..)) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let checked = domain.last_checked_at.is_some();
    // A check that found a domain changing to it took the old one's place.
    let gone = replaced.filter(|id| !domains.iter().any(|d| d.id == *id));
    let view = domain_view(domain, &domains, now, wait);
    let store = store_ref(&row);
    let from_check_page = form.from.as_deref() == Some("check");
    if fx.0 {
        let fragment = if from_check_page {
            crate::views::domains::check_result(&store, &view, checked)
        } else {
            maud::html! {
                (crate::views::domains::card_only(&store, &view))
                @if let Some(gone) = &gone {
                    (crate::views::domains::removed_card_oob(gone))
                }
                (crate::views::domains::restriction_banner_oob(&store, restricted, can_restrict))
                (crate::views::domains::guide_now_oob(restricted, can_restrict))
            }
        };
        return Html(fragment.into_string()).into_response();
    }
    if wait.is_some() {
        return store_page(&state, &user, &row, |chrome, store| {
            crate::views::domains::check_page(chrome, store, &view, checked)
        })
        .await
        .into_response();
    }
    crate::base::see_other(format!("/stores/{id}/settings/domains/{}/check", view.id))
}

/// `POST /stores/{id}/settings/domains/{domain_id}/delete`.
pub async fn delete_domain(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, domain_id)): Path<(crate::db::ConnectionId, String)>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Domains;
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let now = state.clock.now();
    let store_id = row.id.clone();
    let deleted = state
        .db
        .write(move |db| {
            if embed_domains::would_leave_restricted_store_without_domain(
                db, &store_id, &domain_id, now,
            )? {
                Ok(None)
            } else {
                db.delete_store_domain(&store_id, &domain_id).map(Some)
            }
        })
        .await;
    match deleted {
        Ok(Some(true)) => saved(&id, SECTION),
        Ok(Some(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(None) => {
            render_store_settings_page(
                &state,
                row,
                &user,
                Some(embed_domains::LAST_DOMAIN_MESSAGE.to_string()),
                None,
                Some(SECTION),
            )
            .await
        }
        Err(_) => {
            render_store_settings_page(
                &state,
                row,
                &user,
                Some("Something went wrong. Please try again.".to_string()),
                None,
                Some(SECTION),
            )
            .await
        }
    }
}

/// `POST /stores/{id}/settings/embed-restriction/dismiss-warning` - shrinks the store
/// page's "any website can show this checkout" warning to one line.
pub async fn dismiss_embed_warning(
    State(db): State<Database>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path(id): Path<crate::db::ConnectionId>,
) -> Response {
    let row = match load_owned_connection(&db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let store_id = row.id.clone();
    let dismissed = db
        .write(move |db| db.dismiss_embed_warning(&store_id))
        .await;
    match dismissed {
        Ok(()) => super::fx::respond(fx, &format!("/stores/{id}"), || {
            crate::views::store_detail::compact_embed_warning(id.as_str())
        }),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::embed_domains;
    use crate::engine_client::EngineClient;
    use domain_proof::test_support::FakeDns;
    use domain_proof::{GRACE, RECHECK_EVERY};

    use super::super::{build_router, AppState};

    /// A span as the whole seconds a stored column holds.
    fn secs(span: std::time::Duration) -> i64 {
        i64::try_from(span.as_secs()).unwrap()
    }

    /// Which sites `response` lets frame it, of its Content-Security-Policy
    /// headers.
    fn frame_policy(response: &axum::response::Response) -> Option<String> {
        response
            .headers()
            .get("content-security-policy")?
            .to_str()
            .unwrap()
            .split(", ")
            .find(|policy| policy.starts_with("frame-ancestors"))
            .map(str::to_string)
    }

    async fn test_state(dns: Arc<FakeDns>) -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let state = AppState {
            proofs: domain_proof::test_support::proofs(dns, Arc::default()),
            engine: crate::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
            ..AppState::for_tests()
        };
        (state, engine)
    }

    async fn send(
        router: &Router,
        method: &str,
        uri: &str,
        session: &str,
        form: Option<&str>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", format!("session={session}"));
        // A browser posts every form urlencoded, even one with no fields.
        if form.is_some() || method == "POST" {
            builder = builder.header("content-type", "application/x-www-form-urlencoded");
        }
        let response = router
            .clone()
            .oneshot(
                builder
                    .body(Body::from(form.unwrap_or("").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// Where a post redirects to.
    async fn post_location(router: &Router, uri: &str, session: &str, form: &str) -> String {
        let response = router
            .clone()
            .oneshot(
                Request::post(uri)
                    .header("cookie", format!("session={session}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(form.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{uri}");
        response.headers()["location"].to_str().unwrap().to_string()
    }

    /// A post as fixi sends it: the fragment that comes back.
    async fn fx_post(
        router: &Router,
        uri: &str,
        session: &str,
        form: &str,
    ) -> (StatusCode, String) {
        let response = router
            .clone()
            .oneshot(
                Request::post(uri)
                    .header("cookie", format!("session={session}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("FX-Request", "true")
                    .body(Body::from(form.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn session_for(router: &Router, email: &str) -> String {
        crate::http::test_support::signed_up_and_logged_in_session_token(
            router,
            email,
            "correct horse battery staple",
        )
        .await
    }

    async fn create_store(router: &Router, state: &AppState, session: &str) -> String {
        create_store_with_key(router, state, session).await.0
    }

    /// `(connection_id, public_key)`.
    async fn create_store_with_key(
        router: &Router,
        state: &AppState,
        session: &str,
    ) -> (String, String) {
        create_store_on(router, state, session, "store-home.example").await
    }

    /// A store on `site` (no two stores share one), made through store
    /// setup; `(connection_id, public_key)`.
    async fn create_store_on(
        router: &Router,
        state: &AppState,
        session: &str,
        site: &str,
    ) -> (String, String) {
        let store =
            crate::http::test_support::store_with_keys(router, state, session, site, "XMR").await;
        (store.id, store.public_key)
    }

    /// A website's `POST /api/v1/orders` (with the store's public key) from
    /// a page on `origin` (or no page at all).
    async fn create_order_from(
        router: &Router,
        pk: &str,
        origin: Option<&str>,
    ) -> axum::response::Response {
        let mut request = crate::http::test_support::api_request(
            "POST",
            "/api/v1/orders",
            Some(pk),
            Some(serde_json::json!({ "amount": "1", "currency": "XMR" })),
        );
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        router.clone().oneshot(request).await.unwrap()
    }

    /// A new store's site joins its domains, and its key's hash is
    /// recorded, straight away; a store from before either existed gets
    /// both at start, by code steps that run once.
    #[tokio::test]
    async fn a_new_stores_site_joins_its_domains_and_existing_stores_get_theirs_imported_once() {
        let (state, _engine) = test_state(Arc::new(FakeDns::default())).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "import@example.com").await;
        let store = crate::http::test_support::store_with_keys(
            &router,
            &state,
            &session,
            "store-home.example",
            "XMR",
        )
        .await;
        let id = engine_protocol::ids::ConnectionId::new(store.id.clone());
        let domains = |state: &AppState| -> Vec<String> {
            state
                .db
                .lock()
                .list_store_domains(&id)
                .unwrap()
                .into_iter()
                .map(|d| d.domain)
                .collect()
        };
        let found_by_key = |state: &AppState| {
            state
                .db
                .lock()
                .get_store_connection_by_secret_key_hash(
                    &credentials::auth::RawToken::presented(&store.secret_key).hash(),
                )
                .unwrap()
                .map(|row| row.id)
        };
        assert_eq!(
            domains(&state),
            vec!["store-home.example".to_string()],
            "a new store's site is added straight away"
        );
        assert_eq!(found_by_key(&state), Some(id.clone()), "and its key's hash");

        // A store from before these existed: only its own site is
        // imported, from monokulo's own records (nothing is read from the
        // engine), and its key's hash recorded from its stored key.
        state.db.lock().reset_store_backfills_for_test(&id);
        assert_eq!(found_by_key(&state), None);
        crate::stores::backfill_at_start(
            &state.db.lock(),
            &state.encryption_key,
            state.clock.now(),
        );
        assert_eq!(domains(&state), vec!["store-home.example".to_string()]);
        assert_eq!(found_by_key(&state), Some(id.clone()));

        // Once only: a domain the merchant removes afterwards stays
        // removed, and neither step runs at the next start, whatever the
        // store's row says.
        state.db.lock().reset_store_backfills_for_test(&id);
        crate::stores::backfill_at_start(
            &state.db.lock(),
            &state.encryption_key,
            state.clock.now(),
        );
        assert!(domains(&state).is_empty());
        assert_eq!(found_by_key(&state), None);
    }

    /// `POST /api/v1/orders` for the store `pk` with an optional page
    /// `Origin`, and an `Authorization` value: the store's public key as
    /// the website sends it unless another is given.
    async fn create_order_as(
        router: &Router,
        pk: &str,
        origin: Option<&str>,
        authorization: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        create_order_from_ip(router, "203.0.113.9", pk, origin, authorization).await
    }

    async fn create_order_from_ip(
        router: &Router,
        ip: &str,
        pk: &str,
        origin: Option<&str>,
        authorization: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/v1/orders")
            .header("content-type", "application/json");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        let website = format!("Bearer {pk}");
        builder = builder.header("authorization", authorization.unwrap_or(&website));
        let body = serde_json::json!({ "amount": "1", "currency": "XMR" }).to_string();
        let mut request = builder.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            format!("{ip}:4000")
                .parse::<std::net::SocketAddr>()
                .unwrap(),
        ));
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    fn secret_key_of(state: &AppState, id: &str) -> String {
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .unwrap();
        crate::crypto::decrypt(
            &state.encryption_key,
            crate::crypto::Binding::StoreSecret(row.id.as_str()),
            &row.tenant_secret_token_encrypted,
        )
        .unwrap()
    }

    fn recorded_with_key(state: &AppState, id: &str, order: &serde_json::Value) -> bool {
        let order_id = order["order_id"].as_str().unwrap();
        state
            .db
            .lock()
            .get_order_currency_metadata(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                &engine_protocol::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap()
            .created_with_key
    }

    #[tokio::test]
    async fn secret_key_orders_are_accepted_and_recorded_and_restricted_stores_need_a_key_or_a_verified_page(
    ) {
        let dns = Arc::new(FakeDns::default());
        let (mut state, _engine) = test_state(dns.clone()).await;
        // A per-client budget of 4 a minute: key requests must not spend it.
        state.abuse = Arc::new(
            abuse::AbuseProtection::new(abuse::AbuseConfig {
                soft_per_min: 4,
                per_store_key_per_min: 5,
                ..Default::default()
            })
            .with_clock(state.clock.clone()),
        );
        let router = build_router(state.clone());
        let session = session_for(&router, "keys@example.com").await;
        let (id, pk) = create_store_with_key(&router, &state, &session).await;
        let key = format!("Bearer {}", secret_key_of(&state, &id));

        // Unrestricted: the public key and no Origin still works, recorded as not keyed.
        let (status, order) = create_order_as(&router, &pk, None, None).await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(!recorded_with_key(&state, &id, &order));

        // The secret key works and is recorded as keyed.
        let (status, order) = create_order_as(&router, &pk, None, Some(&key)).await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
        assert!(recorded_with_key(&state, &id, &order));

        // An unknown secret or public key, or a non-bearer value: 401 with a problem.
        for bad in ["Bearer sk_wrong", "Bearer pk_wrong", "Basic abc"] {
            let (status, body) = create_order_as(&router, &pk, None, Some(bad)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{bad}");
            assert!(
                body["detail"]
                    .as_str()
                    .unwrap()
                    .contains("key was not accepted"),
                "{body}"
            );
        }
        // One public-key order and three failures spent the per-IP budget (4): anonymous
        // requests from this address are now limited, keyed ones are not.
        assert_eq!(
            create_order_as(&router, &pk, None, None).await.0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            create_order_as(&router, &pk, None, Some(&key)).await.0,
            StatusCode::CREATED
        );

        // Restrict the store to its verified domain.
        let row = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        dns.publish(
            "_monokulo.store-home.example",
            &domain_proof::VERIFY.value(&row.token),
        );
        embed_domains::check_and_record(&state.db, &state.proofs, &row, state.clock.now())
            .await
            .unwrap();
        state
            .db
            .lock()
            .set_embed_restricted(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                true,
            )
            .unwrap();

        // From a fresh address (the first one is out of per-IP budget):
        // the public key with no Origin is refused with a clear message.
        let (status, body) = create_order_from_ip(&router, "198.51.100.7", &pk, None, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["detail"].as_str().unwrap().contains("secret key"),
            "{body}"
        );
        // A verified page: fine, and not keyed.
        let (status, order) = create_order_from_ip(
            &router,
            "198.51.100.7",
            &pk,
            Some("https://store-home.example"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(!recorded_with_key(&state, &id, &order));
        // The key, with or without an Origin: fine.
        assert_eq!(
            create_order_as(&router, &pk, None, Some(&key)).await.0,
            StatusCode::CREATED
        );
        assert_eq!(
            create_order_as(&router, &pk, Some("https://elsewhere.example"), Some(&key))
                .await
                .0,
            StatusCode::CREATED
        );

        // The per-store key budget (5) is its own limit: 4 keyed orders so
        // far, 1 more passes, then 429.
        assert_eq!(
            create_order_as(&router, &pk, None, Some(&key)).await.0,
            StatusCode::CREATED
        );
        assert_eq!(
            create_order_as(&router, &pk, None, Some(&key)).await.0,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    /// `GET uri` with an optional `Sec-Fetch-Dest`: `(status, body, Vary)`.
    async fn fetch_as(
        router: &Router,
        uri: &str,
        dest: Option<&str>,
    ) -> (StatusCode, String, String) {
        let mut builder = Request::builder().uri(uri);
        if let Some(dest) = dest {
            builder = builder.header("sec-fetch-dest", dest);
        }
        let response = router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let vary = response
            .headers()
            .get("vary")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap(), vary)
    }

    #[tokio::test]
    async fn a_restricted_stores_browser_created_orders_only_open_inside_a_frame() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "frame-only@example.com").await;
        let (id, pk) = create_store_with_key(&router, &state, &session).await;
        let key = format!("Bearer {}", secret_key_of(&state, &id));

        // Created while the store is still unrestricted, from a page: not keyed.
        let (_, browser_order) =
            create_order_as(&router, &pk, Some("https://store-home.example"), None).await;
        let (_, keyed_order) = create_order_as(&router, &pk, None, Some(&key)).await;
        let checkout = |order: &serde_json::Value| {
            format!("/api/v1/checkout/{}", order["order_id"].as_str().unwrap())
        };

        // Unrestricted: every order opens as a full page.
        assert_eq!(
            fetch_as(&router, &checkout(&browser_order), Some("document"))
                .await
                .0,
            StatusCode::OK
        );

        let row = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        dns.publish(
            "_monokulo.store-home.example",
            &domain_proof::VERIFY.value(&row.token),
        );
        embed_domains::check_and_record(&state.db, &state.proofs, &row, state.clock.now())
            .await
            .unwrap();
        state
            .db
            .lock()
            .set_embed_restricted(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                true,
            )
            .unwrap();

        // Restricted, browser-created: a full page gets the plain "open it from the shop" page.
        let (status, html, vary) =
            fetch_as(&router, &checkout(&browser_order), Some("document")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            html.contains("Open this payment from the shop"),
            "got: {html}"
        );
        assert!(
            !html.contains("<script"),
            "works without JavaScript, got: {html}"
        );
        assert!(
            !html.contains(browser_order["address"].as_str().unwrap()),
            "no payment details leak, got: {html}"
        );
        assert!(
            vary.contains("Sec-Fetch-Dest"),
            "caches must key on the header, got: {vary:?}"
        );
        // ...but inside a frame, or from a browser that sends no Sec-Fetch-Dest, it renders.
        for dest in [Some("iframe"), Some("frame"), None] {
            let (status, html, _) = fetch_as(&router, &checkout(&browser_order), dest).await;
            assert_eq!(status, StatusCode::OK, "{dest:?}");
            assert!(
                html.contains(browser_order["address"].as_str().unwrap()),
                "{dest:?}"
            );
        }
        // Its share page is turned away the same way when opened directly.
        let share = format!("{}/share", checkout(&browser_order));
        assert_eq!(
            fetch_as(&router, &share, Some("document")).await.0,
            StatusCode::FORBIDDEN
        );
        // The order's status keeps working (the embed library follows it).
        let mut status = crate::http::test_support::api_request(
            "GET",
            &format!(
                "/api/v1/orders/{}",
                browser_order["order_id"].as_str().unwrap()
            ),
            Some(&pk),
            None,
        );
        status
            .headers_mut()
            .insert("sec-fetch-dest", "empty".parse().unwrap());
        assert_eq!(
            router.clone().oneshot(status).await.unwrap().status(),
            StatusCode::OK
        );

        // A keyed order (WooCommerce, dashboard, POS, payment links) opens either way, share page included.
        for dest in [Some("document"), Some("iframe"), None] {
            assert_eq!(
                fetch_as(&router, &checkout(&keyed_order), dest).await.0,
                StatusCode::OK,
                "{dest:?}"
            );
        }
        assert_eq!(
            fetch_as(
                &router,
                &format!("{}/share", checkout(&keyed_order)),
                Some("document")
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn diagnostics_turn_on_the_checkouts_problem_reports() {
        let (state, _engine) = test_state(Arc::new(FakeDns::default())).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "diagnostics@example.com").await;
        let (id, pk) = create_store_with_key(&router, &state, &session).await;
        let response = create_order_from(&router, &pk, None).await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let order_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["order_id"]
            .as_str()
            .unwrap()
            .to_string();
        let checkout = format!("/api/v1/checkout/{order_id}");
        let checkout_html = || async {
            let page = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&checkout)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            String::from_utf8(
                page.into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap()
        };
        assert!(
            !checkout_html()
                .await
                .contains(&crate::assets::url("telemetry.js")),
            "off by default (D8)"
        );
        let (_, settings) = send(
            &router,
            "GET",
            &format!("/stores/{id}/settings"),
            &session,
            None,
        )
        .await;
        assert!(
            settings.contains(
                r#"name="client_logging" value="true" id="client-logging" aria-describedby"#
            ),
            "off: {settings}"
        );
        assert!(
            !settings.contains(&crate::assets::url("telemetry.js")),
            "the store's own pages don't report either"
        );

        // The settings form: a redirect back to the page.
        let url = format!("/stores/{id}/settings");
        assert_eq!(
            send(&router, "POST", &url, &session, Some("client_logging=on"))
                .await
                .0,
            StatusCode::SEE_OTHER
        );
        assert!(state
            .db
            .lock()
            .client_logging(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap());
        assert!(checkout_html()
            .await
            .contains(&crate::assets::url("telemetry.js")));
        let (_, settings) = send(
            &router,
            "GET",
            &format!("/stores/{id}/settings"),
            &session,
            None,
        )
        .await;
        assert!(settings.contains(&crate::assets::url("telemetry.js")));

        // Turned off: the form names the switch, and doesn't send it.
        assert_eq!(
            send(
                &router,
                "POST",
                &url,
                &session,
                Some("switches=client_logging")
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );
        assert!(!state
            .db
            .lock()
            .client_logging(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap());

        // Someone else's store: not found.
        let stranger = session_for(&router, "stranger@example.com").await;
        assert_eq!(
            send(&router, "POST", &url, &stranger, Some("client_logging=on"))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_restricted_store_only_works_on_its_verified_domains() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "restricted@example.com").await;
        let (id, pk) = create_store_with_key(&router, &state, &session).await;
        let settings = format!("/stores/{id}/settings");
        let turn = |on: bool| format!("restricted={}", if on { "on" } else { "off" });

        // Unrestricted: any site, no framing header.
        let response = create_order_from(&router, &pk, Some("https://anything.example")).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let order_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["order_id"]
            .as_str()
            .unwrap()
            .to_string();
        let checkout = format!("/api/v1/checkout/{order_id}");
        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&checkout)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(frame_policy(&page), None);

        // Can't turn on before a domain is verified.
        let (_, html) = send(
            &router,
            "POST",
            &format!("{settings}/embed-restriction"),
            &session,
            Some(&turn(true)),
        )
        .await;
        assert!(
            html.contains("Verify at least one domain before turning this on"),
            "got: {html}"
        );
        assert!(!state
            .db
            .lock()
            .embed_restricted(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap());

        // Verify the store's own domain, then turn it on.
        let row = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(row.domain, "store-home.example");
        dns.publish(
            "_monokulo.store-home.example",
            &domain_proof::VERIFY.value(&row.token),
        );
        embed_domains::check_and_record(&state.db, &state.proofs, &row, state.clock.now())
            .await
            .unwrap();
        assert_eq!(
            send(
                &router,
                "POST",
                &format!("{settings}/embed-restriction"),
                &session,
                Some(&turn(true))
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );

        // Framing: only monokulo itself and the verified domain.
        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&checkout)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert_eq!(
            frame_policy(&page).as_deref(),
            Some("frame-ancestors 'self' https://store-home.example https://*.store-home.example")
        );
        // Beside the page's own policy, not in place of it: a browser
        // enforces both.
        assert!(page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .starts_with("default-src 'none'"));

        // Order creation: a subdomain of the verified domain is fine; no
        // page at all with the public key (see
        // `secret_key_orders_are_accepted_and_recorded_and_restricted_stores_need_a_key_or_a_verified_page`),
        // or anywhere else, is refused.
        assert_eq!(
            create_order_from(&router, &pk, Some("https://www.store-home.example"))
                .await
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            create_order_from(&router, &pk, None).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            create_order_from(&router, &pk, Some("https://evilstore-home.example"))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            create_order_from(&router, &pk, Some("http://store-home.example"))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );

        // CORS answers only the verified domain, for the order on the API
        // and its checkout's stream alike.
        for (origin, allowed) in [
            ("https://shop.store-home.example", true),
            ("https://elsewhere.example", false),
        ] {
            let mut request = crate::http::test_support::api_request(
                "GET",
                &format!("/api/v1/orders/{order_id}"),
                Some(&pk),
                None,
            );
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response
                    .headers()
                    .get("access-control-allow-origin")
                    .is_some(),
                allowed,
                "{origin}"
            );
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("{checkout}/events"))
                        .header("origin", origin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response
                    .headers()
                    .get("access-control-allow-origin")
                    .is_some(),
                allowed,
                "{origin}"
            );
        }

        // The last verified domain can't be removed while it's on.
        let (_, html) = send(
            &router,
            "POST",
            &format!("{settings}/domains/{}/delete", row.id),
            &session,
            None,
        )
        .await;
        assert!(
            html.contains("This is your last verified domain."),
            "got: {html}"
        );

        // The store page drops the "any website" warning; if the domain lapses,
        // it says the checkout can be shown nowhere.
        let (_, html) = send(&router, "GET", &format!("/stores/{id}"), &session, None).await;
        assert!(
            !html.contains("Any website can show this store"),
            "got: {html}"
        );
        let warnings = super::store_page_warnings(
            &state,
            &engine_protocol::ids::ConnectionId::new(id.to_string()),
            state.clock.now(),
        )
        .await;
        assert!(!warnings.shown_nowhere);
        dns.remove("_monokulo.store-home.example");
        let later = state
            .clock
            .now()
            .saturating_add(std::time::Duration::from_secs(
                secs(domain_proof::RECHECK_EVERY).unsigned_abs(),
            ))
            .as_secs();
        embed_domains::recheck_due(&state.db, &state.proofs, clock::UnixTime::from_secs(later))
            .await;
        let failing_since = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()[0]
            .failing_since
            .unwrap();
        assert!(
            super::store_page_warnings(
                &state,
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                clock::UnixTime::from_secs(failing_since + secs(GRACE))
            )
            .await
            .shown_nowhere
        );

        // Off again: any site.
        assert_eq!(
            send(
                &router,
                "POST",
                &format!("{settings}/embed-restriction"),
                &session,
                Some(&turn(false))
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            create_order_from(&router, &pk, Some("https://elsewhere.example"))
                .await
                .status(),
            StatusCode::CREATED
        );
    }

    /// An onion shop is added by its address, told to serve a file rather
    /// than publish a record, and verified when Tor fetches the file; until
    /// then the check says what the shop answered.
    #[tokio::test]
    async fn an_onion_shop_is_verified_by_the_file_it_serves_over_tor() {
        const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
        let (dns, onion) = (
            Arc::new(FakeDns::default()),
            Arc::new(domain_proof::test_support::FakeOnion::default()),
        );
        let (state, _engine) = test_state(dns.clone()).await;
        let state = AppState {
            proofs: domain_proof::test_support::proofs(dns, onion.clone()),
            ..state
        };
        let router = build_router(state.clone());
        let session = session_for(&router, "onion-domains@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let settings = format!("/stores/{id}/settings");

        let to = post_location(
            &router,
            &format!("{settings}/domains"),
            &session,
            &format!("domain=http%3A%2F%2F{ONION}%2Fshop"),
        )
        .await;
        let row = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .into_iter()
            .find(|d| d.domain == ONION)
            .expect("the onion shop, as its service address");
        let (status, html) = send(&router, "GET", &to, &session, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("<h1>Serve the file</h1>"), "got: {html}");
        assert!(
            html.contains(&format!("http://{ONION}/.well-known/monokulo-verify")),
            "got: {html}"
        );
        assert!(
            html.contains(&format!("monokulo-verify={}", row.token)),
            "got: {html}"
        );
        assert!(
            !html.contains("_monokulo."),
            "no DNS record for an onion shop: {html}"
        );

        // Not served yet: the check says where the file should be.
        let check = format!("{settings}/domains/{}/check", row.id);
        post_location(&router, &check, &session, "from=check").await;
        let (_, html) = send(&router, "GET", &check, &session, None).await;
        assert!(html.contains("<h1>Check the file</h1>"), "got: {html}");
        assert!(html.contains("The shop answered, but"), "got: {html}");
        assert!(!html.contains("DNS changes"), "got: {html}");

        // Served, among another store's line: verified.
        onion.serve(
            ONION,
            &format!(
                "monokulo-verify=someone-else\n{}\n",
                domain_proof::VERIFY.value(&row.token)
            ),
        );
        state
            .db
            .lock()
            .record_store_domain_check(&row.id, clock::UnixTime::from_secs(0), None)
            .unwrap();
        post_location(&router, &check, &session, "from=check").await;
        let (_, html) = send(&router, "GET", &check, &session, None).await;
        assert!(
            html.contains(&format!("{ONION} is verified.")),
            "got: {html}"
        );
        assert!(html.contains("Keep the file"), "got: {html}");
    }

    #[tokio::test]
    async fn a_domain_is_added_verified_by_its_dns_record_and_warned_about_once_it_goes_missing() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "domains@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let settings = format!("/stores/{id}/settings");
        // Verified domains are on the Domains tab.
        let connections = format!("{settings}?tab=domains");
        let store_page = format!("/stores/{id}");

        // The store's own site is already on its list, waiting for DNS.
        let suggested = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap();
        assert_eq!(
            suggested
                .iter()
                .map(|d| d.domain.as_str())
                .collect::<Vec<_>>(),
            vec!["store-home.example"]
        );
        // This merchant's website is another one, so that makes way for it.
        state
            .db
            .lock()
            .delete_store_domain(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                &suggested[0].id,
            )
            .unwrap();

        // Before anything: the store page says any website can show the checkout.
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(
            html.contains("Any website can show this store's checkout</strong>"),
            "got: {html}"
        );

        // Adding a domain is a page of its own, from the settings card.
        let (status, html) = send(
            &router,
            "GET",
            &format!("{settings}/domains/new"),
            &session,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(r#"<h1>Add a domain</h1>"#), "got: {html}");
        assert!(
            html.contains(r#"<form method="post" action="/stores/"#),
            "got: {html}"
        );

        // Added from a pasted URL: next, the record to publish.
        let to = post_location(
            &router,
            &format!("{settings}/domains"),
            &session,
            "domain=https%3A%2F%2FShop.Example%2Fcart",
        )
        .await;
        let shop = |state: &AppState| {
            state
                .db
                .lock()
                .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
                .unwrap()
                .into_iter()
                .find(|d| d.domain == "shop.example")
                .unwrap()
        };
        let row = shop(&state);
        assert_eq!(to, format!("{settings}/domains/{}/record", row.id));
        let (status, html) = send(&router, "GET", &to, &session, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("<h1>Add the record</h1>"), "got: {html}");
        assert!(html.contains(r#"<td data-label="Host">"#), "got: {html}");
        assert!(
            html.contains("_monokulo.shop.example"),
            "the full name, for providers that want it: {html}"
        );
        assert!(
            html.contains(&format!("monokulo-verify={}", row.token)),
            "got: {html}"
        );
        // It waits in Settings, with its record, until it's verified.
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(
            html.contains(&format!("monokulo-verify={}", row.token)),
            "got: {html}"
        );
        assert!(html.contains(">Not verified</span>"), "got: {html}");

        // The same domain twice, and an old (v2) onion address, are refused
        // on the domain's page, keeping what was typed.
        let (status, html) = send(
            &router,
            "POST",
            &format!("{settings}/domains"),
            &session,
            Some("domain=shop.example"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            html.contains("shop.example is already on this store's list."),
            "got: {html}"
        );
        assert!(html.contains(r#"value="shop.example""#), "got: {html}");
        assert!(html.contains(r#"aria-invalid="true""#), "got: {html}");
        let (_, html) = send(
            &router,
            "POST",
            &format!("{settings}/domains"),
            &session,
            Some("domain=abcdefghijklmnop.onion"),
        )
        .await;
        assert!(html.contains("isn't a valid onion address"), "got: {html}");

        // No record yet: the check page says so, with the reason, and
        // nothing claims it was saved.
        let check = format!("{settings}/domains/{}/check", row.id);
        assert_eq!(
            post_location(&router, &check, &session, "from=check").await,
            check
        );
        let (_, html) = send(&router, "GET", &check, &session, None).await;
        assert!(html.contains("Not found yet."), "got: {html}");
        assert!(
            html.contains("No TXT record at _monokulo.shop.example"),
            "got: {html}"
        );
        assert!(!html.contains("toast-success"), "got: {html}");
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(
            html.contains("No TXT record at _monokulo.shop.example"),
            "got: {html}"
        );
        // A second check straight away isn't looked up: the page says when
        // it can be, in place.
        let (status, html) = send(&router, "POST", &check, &session, Some("from=check")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("Checked moments ago: you can check again in"),
            "got: {html}"
        );

        // Published: verified (checked directly, past the rate limit).
        dns.publish(
            "_monokulo.shop.example",
            &domain_proof::VERIFY.value(&row.token),
        );
        let now = state.clock.now();
        embed_domains::check_and_record(&state.db, &state.proofs, &row, now)
            .await
            .unwrap();
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(html.contains(">Verified</span>"), "got: {html}");
        assert!(
            !html.contains(&format!("monokulo-verify={}", row.token)),
            "a verified domain hides its record"
        );

        // The record disappears: the daily re-check finds it missing and the
        // store page warns, with no way to dismiss that warning.
        dns.remove("_monokulo.shop.example");
        embed_domains::recheck_due(&state.db, &state.proofs, now.saturating_add(RECHECK_EVERY))
            .await;
        let row = shop(&state);
        assert!(row.failing_since.is_some());
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(
            html.contains("shop.example failed its DNS check"),
            "got: {html}"
        );
        let warnings = super::store_page_warnings(
            &state,
            &engine_protocol::ids::ConnectionId::new(id.to_string()),
            clock::UnixTime::from_secs(row.failing_since.unwrap() + secs(GRACE)),
        )
        .await;
        assert!(
            warnings.failing[0].lapsed,
            "past the grace period it no longer counts"
        );

        // Dismissing shrinks only the "any website" warning; fixi gets the
        // shrunk warning to put in its place.
        let dismiss = Request::post(format!(
            "/stores/{id}/settings/embed-restriction/dismiss-warning"
        ))
        .header("cookie", format!("session={session}"))
        .header("FX-Request", "true")
        .body(Body::empty())
        .unwrap();
        let response = router.clone().oneshot(dismiss).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let fragment = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(
            fragment.starts_with(r#"<p class="warning" id="embed-warning">"#),
            "{fragment}"
        );
        let (status, _) = send(
            &router,
            "POST",
            &format!("/stores/{id}/settings/embed-restriction/dismiss-warning"),
            &session,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FOUND);
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(html.contains(r#"<p class="warning" id="embed-warning">"#));
        assert!(html.contains("shop.example failed its DNS check"));

        // Another merchant can't touch this store's domains.
        let other = session_for(&router, "someone-else@example.com").await;
        assert_eq!(
            send(&router, "POST", &check, &other, None).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send(
                &router,
                "POST",
                &format!("{settings}/domains/{}/delete", row.id),
                &other,
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );

        // Removed.
        assert_eq!(
            send(
                &router,
                "POST",
                &format!("{settings}/domains/{}/delete", row.id),
                &session,
                None
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            state
                .db
                .lock()
                .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
                .unwrap()
                .len(),
            0
        );
    }

    /// With JavaScript a check swaps only what asked for it: a card gets
    /// itself back (and the banner, which a verified domain may enable),
    /// the check page its result; never a "saved" toast. Then the
    /// restriction turns on, and Allow any website turns it off again.
    #[tokio::test]
    async fn a_check_answers_in_place_and_a_verified_domain_lets_the_restriction_on() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "in-place@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let settings = format!("/stores/{id}/settings");
        // Verified domains are on the Domains tab.
        let connections = format!("{settings}?tab=domains");
        let row = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        let check = format!("{settings}/domains/{}/check", row.id);

        // From its card, before the record exists: the card, still waiting,
        // with the reason, and the banner, still unable to turn on.
        let (status, fragment) = fx_post(&router, &check, &session, "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            fragment.starts_with(&format!(
                r#"<article class="mk-item-card" id="domain-{}""#,
                row.id
            )),
            "{fragment}"
        );
        assert!(fragment.contains(">Not verified</span>"), "{fragment}");
        assert!(
            fragment.contains("No TXT record at _monokulo.store-home.example"),
            "{fragment}"
        );
        assert!(
            fragment.contains(r#"<form id="embed-restriction""#)
                && fragment.contains("data-fx-oob"),
            "{fragment}"
        );
        // And "How this works" says what that means right now.
        assert!(
            fragment.contains(
                r#"<div id="domains-now" class="guide-now" data-tone="warning" data-fx-oob>"#
            ) && fragment.contains("Verify one of your websites first."),
            "{fragment}"
        );
        assert!(!fragment.contains("toast"), "{fragment}");
        // Asked again at once: the card says when it can be checked.
        let (_, fragment) = fx_post(&router, &check, &session, "").await;
        assert!(
            fragment.contains("Checked moments ago: you can check again in"),
            "{fragment}"
        );

        // Published, and checked from the check page (past the 30 seconds):
        // its result says verified, with Done back to the card.
        dns.publish(
            "_monokulo.store-home.example",
            &domain_proof::VERIFY.value(&row.token),
        );
        state
            .db
            .lock()
            .record_store_domain_check(
                &row.id,
                clock::UnixTime::from_secs(
                    state
                        .clock
                        .now()
                        .saturating_sub(std::time::Duration::from_secs(
                            secs(domain_proof::MIN_CHECK_GAP).unsigned_abs(),
                        ))
                        .as_secs(),
                ),
                Some("missing"),
            )
            .unwrap();
        let (_, fragment) = fx_post(&router, &check, &session, "from=check").await;
        assert!(
            fragment.starts_with(r#"<div id="domain-check" class="domain-check">"#),
            "{fragment}"
        );
        assert!(
            fragment.contains("store-home.example is verified."),
            "{fragment}"
        );
        assert!(
            fragment.contains(&format!(
                r##"href="{settings}?tab=domains#domain-{}""##,
                row.id
            )),
            "{fragment}"
        );
        assert!(
            !fragment.contains("data-recheck"),
            "a verified domain isn't checked again: {fragment}"
        );

        // The banner can turn the restriction on now; Allow any website
        // turns it off.
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(
            html.contains("Any website can show this checkout."),
            "{html}"
        );
        assert_eq!(
            post_location(
                &router,
                &format!("{settings}/embed-restriction"),
                &session,
                "restricted=on"
            )
            .await,
            format!("{settings}?saved=verified-domains#card-verified-domains")
        );
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(
            html.contains("Only your verified domains can show this checkout."),
            "{html}"
        );
        assert!(html.contains(r#"data-opens-dialog="allow-any""#), "{html}");
        post_location(
            &router,
            &format!("{settings}/embed-restriction"),
            &session,
            "restricted=off",
        )
        .await;
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(
            html.contains("Any website can show this checkout."),
            "{html}"
        );
    }

    /// The merchant's DNS provider is having an outage: Verify says the
    /// lookup failed rather than that the record is missing. A domain already
    /// verified stays verified through an outage of any length, retried
    /// hourly. Only an answer without the record starts the grace period, and
    /// the failing domain clears by itself once its record is back.
    #[tokio::test]
    async fn a_dns_outage_is_reported_as_such_and_only_a_missing_record_starts_the_clock() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "dns-outage@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let settings = format!("/stores/{id}/settings");
        // Verified domains are on the Domains tab.
        let connections = format!("{settings}?tab=domains");
        let store_page = format!("/stores/{id}");
        let row = state
            .db
            .lock()
            .list_store_domains(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        let name = "_monokulo.store-home.example";

        dns.fail(name, "timed out");
        assert_eq!(
            send(
                &router,
                "POST",
                &format!("{settings}/domains/{}/check", row.id),
                &session,
                None
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(
            html.contains("The DNS lookup failed: timed out"),
            "got: {html}"
        );
        assert!(!html.contains(">Verified</span>"));

        // DNS is back and the record published: verified; the store is
        // restricted to it.
        dns.publish(name, &domain_proof::VERIFY.value(&row.token));
        let now = state.clock.now();
        embed_domains::check_and_record(&state.db, &state.proofs, &row, now)
            .await
            .unwrap();
        state
            .db
            .lock()
            .set_embed_restricted(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                true,
            )
            .unwrap();

        // A day later the re-check lands in an outage, which lasts longer
        // than the grace period: no answer says nothing about the record,
        // so the domain stays verified throughout. Retried hourly, not daily.
        let row_now = |state: &AppState| {
            state
                .db
                .lock()
                .get_store_domain(
                    &engine_protocol::ids::ConnectionId::new(id.to_string()),
                    &row.id,
                )
                .unwrap()
                .unwrap()
        };
        dns.fail(name, "SERVFAIL");
        let outage = now.saturating_add(RECHECK_EVERY);
        embed_domains::recheck_due(&state.db, &state.proofs, outage).await;
        let checked = row_now(&state);
        assert_eq!(checked.failing_since, None);
        assert_eq!(checked.last_checked_at, Some(outage.as_secs()));
        let retried = outage.saturating_add(domain_proof::FAILING_RECHECK_EVERY);
        embed_domains::recheck_due(&state.db, &state.proofs, retried).await;
        assert_eq!(row_now(&state).last_checked_at, Some(retried.as_secs()));
        let long_after = retried
            .saturating_add(GRACE)
            .saturating_add(domain_proof::FAILING_RECHECK_EVERY);
        embed_domains::recheck_due(&state.db, &state.proofs, long_after).await;
        let checked = row_now(&state);
        assert_eq!(
            checked.state(long_after),
            domain_proof::DomainState::Verified
        );
        let warnings = super::store_page_warnings(
            &state,
            &engine_protocol::ids::ConnectionId::new(id.to_string()),
            long_after,
        )
        .await;
        assert!(warnings.failing.is_empty(), "an outage warns of nothing");
        assert!(!warnings.shown_nowhere);
        let (_, html) = send(&router, "GET", &connections, &session, None).await;
        assert!(
            html.contains("with no answer (The DNS lookup failed: SERVFAIL)"),
            "got: {html}"
        );

        // Then DNS answers, without the record: that is what starts the clock.
        dns.remove(name);
        let answered = long_after.saturating_add(domain_proof::FAILING_RECHECK_EVERY);
        embed_domains::recheck_due(&state.db, &state.proofs, answered).await;
        let failing_since = state
            .db
            .lock()
            .get_store_domain(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                &row.id,
            )
            .unwrap()
            .unwrap()
            .failing_since
            .unwrap();
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(
            html.contains("store-home.example failed its DNS check"),
            "got: {html}"
        );
        let warnings = super::store_page_warnings(
            &state,
            &engine_protocol::ids::ConnectionId::new(id.to_string()),
            clock::UnixTime::from_secs(failing_since + secs(GRACE) - 60),
        )
        .await;
        assert!(
            !warnings.failing[0].lapsed,
            "within the grace period the domain still counts"
        );
        assert!(
            !warnings.shown_nowhere,
            "the restricted checkout still shows on the site"
        );

        // An hour on, the record is back: the failing domain's more frequent
        // re-check clears it with no action from the merchant.
        dns.publish(name, &domain_proof::VERIFY.value(&row.token));
        // Measured from when the failing check was recorded (the re-check
        // records at the time it was run for, not the wall clock).
        embed_domains::recheck_due(
            &state.db,
            &state.proofs,
            clock::UnixTime::from_secs(
                failing_since + secs(domain_proof::FAILING_RECHECK_EVERY) + 1,
            ),
        )
        .await;
        let row = state
            .db
            .lock()
            .get_store_domain(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                &row.id,
            )
            .unwrap()
            .unwrap();
        assert!(row.failing_since.is_none());
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(!html.contains("failed its DNS check"), "got: {html}");
    }

    /// A store has one web domain and one onion address. A second of either
    /// is refused, naming the first; each kind has its own place.
    #[tokio::test]
    async fn a_store_has_one_web_domain_and_one_onion_address() {
        const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "one-of-each@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let store = engine_protocol::ids::ConnectionId::new(id.to_string());
        let domains = |state: &AppState| {
            let mut names: Vec<String> = state
                .db
                .lock()
                .list_store_domains(&store)
                .unwrap()
                .into_iter()
                .map(|d| d.domain)
                .collect();
            names.sort();
            names
        };
        let add = format!("/stores/{id}/settings/domains");
        assert_eq!(domains(&state), ["store-home.example"]);

        let (status, html) = send(
            &router,
            "POST",
            &add,
            &session,
            Some("domain=second.example"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            html.contains("This store already has store-home.example"),
            "{html}"
        );
        let (status, html) = send(
            &router,
            "POST",
            &add,
            &session,
            Some("domain=store-home.example"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(html.contains("is already on this store"), "{html}");
        assert_eq!(domains(&state), ["store-home.example"]);

        post_location(&router, &add, &session, &format!("domain={ONION}")).await;
        let other_onion = ONION.replacen(
            "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid",
            "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd",
            1,
        );
        let (status, _) = send(
            &router,
            "POST",
            &add,
            &session,
            Some(&format!("domain={other_onion}")),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(domains(&state), [ONION, "store-home.example"]);
    }

    /// Changing a domain keeps the old one counting until the new one is
    /// verified, so a restricted store's checkout never stops: only then does
    /// the new one take its place. There's one change at a time, of the same
    /// kind, and removing the new one is how a change is cancelled.
    #[tokio::test]
    async fn changing_a_domain_swaps_it_only_once_the_new_one_is_verified() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "change-domain@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let store = engine_protocol::ids::ConnectionId::new(id.to_string());
        let rows = |state: &AppState| state.db.lock().list_store_domains(&store).unwrap();
        let old = rows(&state).pop().unwrap();
        dns.publish(
            &domain_proof::VERIFY.record_name(&old.domain),
            &domain_proof::VERIFY.value(&old.token),
        );
        let now = state.clock.now();
        embed_domains::check_and_record(&state.db, &state.proofs, &old, now)
            .await
            .unwrap();
        state.db.lock().set_embed_restricted(&store, true).unwrap();
        let policy = |state: &AppState| embed_domains::EmbedPolicy {
            restricted: true,
            domains: rows(state),
        };

        let add = format!("/stores/{id}/settings/domains");
        let form = |domain: &str| format!("domain={domain}&replaces={}", old.id);
        post_location(&router, &add, &session, &form("new-home.example")).await;
        let new = rows(&state).into_iter().find(|d| d.id != old.id).unwrap();
        assert_eq!(new.replaces_id.as_deref(), Some(old.id.as_str()));
        assert!(policy(&state).allows_origin("https://store-home.example", now));
        assert!(!policy(&state).allows_origin("https://new-home.example", now));
        let (_, html) = send(
            &router,
            "GET",
            &format!("/stores/{id}/settings?tab=domains"),
            &session,
            None,
        )
        .await;
        assert!(
            html.contains("Replaces store-home.example once verified"),
            "{html}"
        );

        // One change at a time, and an onion address can't replace a web domain.
        let (status, html) = send(
            &router,
            "POST",
            &add,
            &session,
            Some(&form("third.example")),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(html.contains("already underway"), "{html}");
        let onion = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
        let (status, html) = send(&router, "POST", &add, &session, Some(&form(onion))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(html.contains("can only change"), "{html}");

        // No record yet: nothing moves.
        embed_domains::check_and_record(&state.db, &state.proofs, &new, now)
            .await
            .unwrap();
        assert_eq!(rows(&state).len(), 2);

        // Found: it takes the old one's place.
        dns.publish(
            &domain_proof::VERIFY.record_name(&new.domain),
            &domain_proof::VERIFY.value(&new.token),
        );
        embed_domains::check_and_record(&state.db, &state.proofs, &new, now)
            .await
            .unwrap();
        let after = rows(&state);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].domain, "new-home.example");
        assert_eq!(after[0].replaces_id, None);
        assert!(policy(&state).allows_origin("https://new-home.example", now));
        assert!(!policy(&state).allows_origin("https://store-home.example", now));
    }

    /// What changing a domain must not allow: naming another store's domain,
    /// or removing a restricted store's only verified domain while the new
    /// one is still waiting. With JavaScript, a check that swaps them takes
    /// the old card off the page.
    #[tokio::test]
    async fn a_change_of_domain_stays_within_its_store_and_keeps_one_that_counts() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "change-limits@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let store = engine_protocol::ids::ConnectionId::new(id.to_string());
        let old = state
            .db
            .lock()
            .list_store_domains(&store)
            .unwrap()
            .pop()
            .unwrap();
        let add = format!("/stores/{id}/settings/domains");

        let other_session = session_for(&router, "change-limits-other@example.com").await;
        let other_id = crate::http::test_support::store_with_keys(
            &router,
            &state,
            &other_session,
            "other-store.example",
            "XMR",
        )
        .await
        .id;
        let (status, _) = send(
            &router,
            "POST",
            &format!("/stores/{other_id}/settings/domains"),
            &other_session,
            Some(&format!("domain=stolen.example&replaces={}", old.id)),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        dns.publish(
            &domain_proof::VERIFY.record_name(&old.domain),
            &domain_proof::VERIFY.value(&old.token),
        );
        embed_domains::check_and_record(
            &state.db,
            &state.proofs,
            &old,
            clock::UnixTime::from_secs(
                state
                    .clock
                    .now()
                    .saturating_sub(std::time::Duration::from_secs(60))
                    .as_secs(),
            ),
        )
        .await
        .unwrap();
        state.db.lock().set_embed_restricted(&store, true).unwrap();
        post_location(
            &router,
            &add,
            &session,
            &format!("domain=new-home.example&replaces={}", old.id),
        )
        .await;
        send(
            &router,
            "POST",
            &format!("{add}/{}/delete", old.id),
            &session,
            None,
        )
        .await;
        assert_eq!(
            state.db.lock().list_store_domains(&store).unwrap().len(),
            2,
            "the old domain is the only one counting, so it stays"
        );

        let new = state
            .db
            .lock()
            .list_store_domains(&store)
            .unwrap()
            .into_iter()
            .find(|d| d.id != old.id)
            .unwrap();
        dns.publish(
            &domain_proof::VERIFY.record_name(&new.domain),
            &domain_proof::VERIFY.value(&new.token),
        );
        let (status, html) =
            fx_post(&router, &format!("{add}/{}/check", new.id), &session, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(state.db.lock().list_store_domains(&store).unwrap().len(), 1);
        assert!(
            html.contains(&format!(r#"id="domain-{}" hidden data-fx-oob"#, old.id)),
            "{html}"
        );
    }

    /// Removing a domain while another is on its way to replacing it leaves
    /// that one holding the store's place, so a later add is still refused.
    #[tokio::test]
    async fn removing_the_old_domain_mid_change_leaves_the_new_one_in_its_place() {
        let (state, _engine) = test_state(Arc::new(FakeDns::default())).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "mid-change@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let store = engine_protocol::ids::ConnectionId::new(id.to_string());
        let old = state
            .db
            .lock()
            .list_store_domains(&store)
            .unwrap()
            .pop()
            .unwrap();
        let add = format!("/stores/{id}/settings/domains");
        post_location(
            &router,
            &add,
            &session,
            &format!("domain=new-home.example&replaces={}", old.id),
        )
        .await;

        assert!(state
            .db
            .lock()
            .delete_store_domain(&store, &old.id)
            .unwrap());
        let rows = state.db.lock().list_store_domains(&store).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].replaces_id, None);
        let (status, _) = send(
            &router,
            "POST",
            &add,
            &session,
            Some("domain=third.example"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// No answer leaves what is known about a proof alone, on the web and on
    /// Tor: it doesn't start the clock or clear one running, so an outage
    /// can't hide a real warning, and an answer afterwards puts the domain
    /// back on the daily schedule.
    #[tokio::test]
    async fn no_answer_neither_starts_nor_stops_the_clock_on_the_web_or_on_tor() {
        const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
        let (dns, onion) = (
            Arc::new(FakeDns::default()),
            Arc::new(domain_proof::test_support::FakeOnion::default()),
        );
        let (state, _engine) = test_state(dns.clone()).await;
        let state = AppState {
            proofs: domain_proof::test_support::proofs(dns.clone(), onion.clone()),
            ..state
        };
        let router = build_router(state.clone());
        let session = session_for(&router, "no-answer@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let store = engine_protocol::ids::ConnectionId::new(id.to_string());
        post_location(
            &router,
            &format!("/stores/{id}/settings/domains"),
            &session,
            &format!("domain={ONION}"),
        )
        .await;
        let rows = state.db.lock().list_store_domains(&store).unwrap();
        let onion_row = rows.iter().find(|d| d.domain == ONION).unwrap().clone();
        let web_row = rows.iter().find(|d| d.domain != ONION).unwrap().clone();
        let web_name = domain_proof::VERIFY.record_name(&web_row.domain);
        let current = |row: &crate::db::StoreDomainRow| {
            state
                .db
                .lock()
                .get_store_domain(&store, &row.id)
                .unwrap()
                .unwrap()
        };
        let due = |at| {
            state
                .db
                .lock()
                .list_store_domains_due_for_recheck(
                    at,
                    domain_proof::RECHECK_EVERY,
                    domain_proof::FAILING_RECHECK_EVERY,
                )
                .unwrap()
                .len()
        };

        // Both verified, then neither can be reached.
        let t0 = 1_000_000;
        dns.publish(&web_name, &domain_proof::VERIFY.value(&web_row.token));
        onion.serve(ONION, &domain_proof::VERIFY.value(&onion_row.token));
        for row in [&web_row, &onion_row] {
            embed_domains::check_and_record(
                &state.db,
                &state.proofs,
                row,
                clock::UnixTime::from_secs(t0),
            )
            .await
            .unwrap();
        }
        dns.fail(&web_name, "timed out");
        onion.fail(ONION, "the onion service couldn't be found");
        let t1 = t0 + secs(domain_proof::RECHECK_EVERY);
        for row in [&web_row, &onion_row] {
            embed_domains::check_and_record(
                &state.db,
                &state.proofs,
                row,
                clock::UnixTime::from_secs(t1),
            )
            .await
            .unwrap();
            assert_eq!(current(row).failing_since, None, "{}", row.domain);
            assert!(current(row).last_error.is_some(), "{}", row.domain);
        }
        assert_eq!(
            current(&onion_row).last_error.as_deref(),
            Some("Fetching the file over Tor failed: the onion service couldn't be found")
        );
        // Sooner than a day, since nothing is known.
        assert_eq!(
            due(clock::UnixTime::from_secs(
                t1 + secs(domain_proof::FAILING_RECHECK_EVERY)
            )),
            2
        );

        // An answer with the proof: back to the daily schedule.
        dns.publish(&web_name, &domain_proof::VERIFY.value(&web_row.token));
        let t2 = t1 + secs(domain_proof::FAILING_RECHECK_EVERY);
        embed_domains::check_and_record(
            &state.db,
            &state.proofs,
            &web_row,
            clock::UnixTime::from_secs(t2),
        )
        .await
        .unwrap();
        assert_eq!(current(&web_row).last_error, None);

        // A clock already running keeps running through no answer.
        dns.remove(&web_name);
        let t3 = t2 + secs(domain_proof::RECHECK_EVERY);
        embed_domains::check_and_record(
            &state.db,
            &state.proofs,
            &web_row,
            clock::UnixTime::from_secs(t3),
        )
        .await
        .unwrap();
        assert_eq!(current(&web_row).failing_since, Some(t3));
        dns.fail(&web_name, "timed out");
        let t4 = t3 + secs(domain_proof::FAILING_RECHECK_EVERY);
        embed_domains::check_and_record(
            &state.db,
            &state.proofs,
            &web_row,
            clock::UnixTime::from_secs(t4),
        )
        .await
        .unwrap();
        assert_eq!(current(&web_row).failing_since, Some(t3));
    }

    /// Dismissing shrinks only the "any website" warning: a domain that
    /// failed its check is still warned about, with nothing to dismiss it,
    /// and another merchant can't dismiss this store's warning.
    #[tokio::test]
    async fn dismissing_the_any_site_warning_leaves_a_failing_domain_warned_about() {
        let (state, _engine) = test_state(Arc::new(FakeDns::default())).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "dismiss@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let store = engine_protocol::ids::ConnectionId::new(id.to_string());
        let domain = state.db.lock().list_store_domains(&store).unwrap()[0].clone();
        let now = state.clock.now();
        state
            .db
            .lock()
            .record_store_domain_check(
                &domain.id,
                now.saturating_sub(std::time::Duration::from_secs(60)),
                None,
            )
            .unwrap();
        state
            .db
            .lock()
            .record_store_domain_check(&domain.id, now, Some("no TXT record"))
            .unwrap();
        let dismiss = format!("/stores/{id}/settings/embed-restriction/dismiss-warning");

        let other = session_for(&router, "someone-else@example.com").await;
        assert_eq!(
            send(&router, "POST", &dismiss, &other, None).await.0,
            StatusCode::NOT_FOUND
        );
        assert!(!state.db.lock().embed_warning_dismissed(&store).unwrap());

        assert_eq!(
            send(&router, "POST", &dismiss, &session, None).await.0,
            StatusCode::FOUND
        );
        assert!(state.db.lock().embed_warning_dismissed(&store).unwrap());
        let (_, html) = send(&router, "GET", &format!("/stores/{id}"), &session, None).await;
        assert!(
            html.contains(&format!("{} failed its DNS check", domain.domain)),
            "got: {html}"
        );
        assert!(!html.contains("dismiss-warning"), "got: {html}");
    }
}
