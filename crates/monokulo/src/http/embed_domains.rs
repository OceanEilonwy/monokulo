//! The dashboard side of verified embed domains (`crate::embed_domains`):
//! adding a domain (its own three pages: the domain, the record, the
//! check), checking and removing a store's domains and turning the
//! restriction on or off on its settings page, and the warnings its store
//! page shows - plus [`embed_policy_middleware`], which enforces the
//! restriction on the public routes (`/api/v1`, `/checkout/{order_id}`).

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::db::StoreDomainRow;
use crate::embed_domains::{self, DomainState};
use crate::templates::{format_duration_since, format_duration_until};
use crate::views::domains::{DomainStatus, DomainView, StoreRef};
use crate::views::store_detail::{EmbedWarnings, FailingDomainWarning};

use super::fx::FxRequest;
use super::orders::{load_owned_connection, render_store_settings_page, saved};
use super::store_key::{ApiCaller, RequestStore};
use super::{AppState, AuthedUser};
use crate::api::runtime::problem::Problem;
use crate::db::Database;
use crate::views::store_settings::StoreSection;

/// Enforces a restricted store's embed policy on its public routes (CORS
/// is `super::embed_cors_layer`'s job). The store is the one
/// `super::abuse::pay_middleware` found ([`RequestStore`]):
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
    State(db): State<Database>,
    request: Request,
    next: Next,
) -> Response {
    let creates_order = request.method() == Method::POST
        && request.uri().path() == crate::api::v1::orders::Path::TEMPLATE;
    let has_key = request
        .extensions()
        .get::<ApiCaller>()
        .is_some_and(|caller| caller.credential.is_store_key());
    let policy = match request.extensions().get::<RequestStore>() {
        Some(RequestStore(store)) => {
            embed_domains::policy_for_public_key(&db, &store.tenant_public_key).await
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
            response.headers_mut().insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("frame-ancestors 'self'"),
            );
            return response;
        }
    };
    let Some(policy) = policy else {
        return next.run(request).await;
    };
    let now = crate::now_unix();

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
    if let Some(value) = policy
        .frame_ancestors(now)
        .and_then(|value| HeaderValue::from_str(&value).ok())
    {
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

/// "3h 20m ago", or "just now" under a minute.
fn ago(then: i64, now: i64) -> String {
    if now - then < 60 {
        "just now".to_string()
    } else {
        format!("{} ago", format_duration_until(now, then))
    }
}

/// One of a store's domains as its card and its pages show it; `wait` is
/// the seconds left before it may be checked again, when a check was asked
/// for too soon.
pub(super) fn domain_view(row: StoreDomainRow, now: i64, wait: Option<i64>) -> DomainView {
    let checked = row.last_checked_at.map(|at| ago(at, now));
    let (status, meta, reason) = match DomainState::of(&row, now) {
        DomainState::Verified => (
            DomainStatus::Verified,
            format!(
                "Checked {} · checked again every day",
                checked.as_deref().unwrap_or("never")
            ),
            None,
        ),
        DomainState::Pending => (
            DomainStatus::Waiting,
            format!(
                "Added {} · {}",
                ago(row.created_at, now),
                checked.map_or_else(
                    || "not checked yet".to_string(),
                    |at| format!("checked {at}")
                )
            ),
            Some(row.last_error.clone().unwrap_or_else(|| {
                "Not checked yet: publish the record, then check again.".to_string()
            })),
        ),
        DomainState::Failing { since } => {
            let (missing, left) = (
                format_duration_since(since, now),
                format_duration_until(since + embed_domains::GRACE_SECS, now),
            );
            (
                DomainStatus::Failing,
                format!("Record missing for {missing} · still counts as verified for {left}"),
                Some(format!(
                    "The record was there, and went {missing} ago. Publish it again within {left} to keep the domain."
                )),
            )
        }
        DomainState::Lapsed { since } => {
            let missing = format_duration_since(since, now);
            (
                DomainStatus::Lapsed,
                format!("Record missing for {missing} · it no longer counts"),
                Some(format!(
                    "The record has been missing for {missing}, so the domain no longer counts. Publish it again and check."
                )),
            )
        }
    };
    DomainView {
        record_host: embed_domains::RECORD_LABEL,
        record_value: embed_domains::record_value(&row.token),
        id: row.id,
        domain: row.domain,
        status,
        meta,
        reason,
        wait,
    }
}

/// The settings page's cards for a store's domains.
pub(super) fn domain_views(rows: Vec<StoreDomainRow>, now: i64) -> Vec<DomainView> {
    rows.into_iter()
        .map(|row| domain_view(row, now, None))
        .collect()
}

/// The store page's embed warnings.
pub(super) async fn store_page_warnings(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    now: i64,
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
    let shown_nowhere = restricted && !rows.iter().any(|row| DomainState::of(row, now).counts());
    let failing = rows
        .into_iter()
        .filter_map(|row| {
            let (since, lapsed) = match DomainState::of(&row, now) {
                DomainState::Failing { since } => (since, false),
                DomainState::Lapsed { since } => (since, true),
                DomainState::Pending | DomainState::Verified => return None,
            };
            Some(FailingDomainWarning {
                record_name: embed_domains::record_name(&row.domain),
                domain: row.domain,
                missing_for: format_duration_since(since, now),
                lapsed,
                counts_for: format_duration_until(since + embed_domains::GRACE_SECS, now),
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
    let now = crate::now_unix();
    let store_id = row.id.clone();
    let result = state
        .db
        .write(move |db| match db.list_store_domains(&store_id) {
            Ok(domains) if restricted && !domains.iter().any(|domain| DomainState::of(domain, now).counts()) => {
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
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    store_page(&state, &user, &row, |chrome, store| {
        crate::views::domains::add_page(chrome, store, "", None)
    })
    .await
    .into_response()
}

#[derive(Deserialize)]
pub struct AddDomainForm {
    pub domain: String,
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
    let refused = |error: String| {
        let (state, user, row, typed) = (&state, &user, &row, form.domain.clone());
        async move {
            let page = store_page(state, user, row, |chrome, store| {
                crate::views::domains::add_page(chrome, store, &typed, Some(&error))
            })
            .await;
            (StatusCode::UNPROCESSABLE_ENTITY, page).into_response()
        }
    };
    let domain = match embed_domains::normalize_domain(&form.domain) {
        Ok(domain) => domain,
        Err(message) => return refused(message.to_string()).await,
    };
    let domain_id = uuid::Uuid::new_v4().to_string();
    let (store_id, new_domain, new_id) = (row.id.clone(), domain.clone(), domain_id.clone());
    let created = state
        .db
        .write(move |db| {
            db.create_store_domain(
                &new_id,
                &store_id,
                &new_domain,
                &embed_domains::new_token(),
                crate::now_unix(),
                embed_domains::MAX_DOMAINS_PER_STORE,
            )
        })
        .await;
    let error = match created {
        Ok(true) => {
            return super::auth::redirect_303(&format!(
                "/stores/{id}/settings/domains/{domain_id}/record"
            ))
        }
        Ok(false) => format!(
            "A store can have at most {} domains. Remove one to add another.",
            embed_domains::MAX_DOMAINS_PER_STORE
        ),
        Err(e) if e.is_unique_violation() => format!("{domain} is already on this store's list."),
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
    let view = domain_view(domain, crate::now_unix(), None);
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
    let view = domain_view(domain, crate::now_unix(), None);
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
/// ([`embed_domains::MIN_CHECK_GAP_SECS`]: then the answer says when it can
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
    let now = crate::now_unix();
    let wait = domain
        .last_checked_at
        .map(|last| embed_domains::MIN_CHECK_GAP_SECS - (now - last))
        .filter(|wait| *wait > 0);
    if wait.is_none()
        && embed_domains::check_and_record(&state.db, &state.proofs, &domain, now)
            .await
            .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let store_id = row.id.clone();
    let (domain, restricted, can_restrict) = match state
        .db
        .read(move |db| {
            let domains = db.list_store_domains(&store_id)?;
            Ok::<_, crate::db::DbError>((
                domains.iter().find(|d| d.id == domain_id).cloned(),
                db.embed_restricted(&store_id).unwrap_or(false),
                domains.iter().any(|d| DomainState::of(d, now).counts()),
            ))
        })
        .await
    {
        Ok((Some(domain), restricted, can_restrict)) => (domain, restricted, can_restrict),
        Ok((None, ..)) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let checked = domain.last_checked_at.is_some();
    let view = domain_view(domain, now, wait);
    let store = store_ref(&row);
    let from_check_page = form.from.as_deref() == Some("check");
    if fx.0 {
        let fragment = if from_check_page {
            crate::views::domains::check_result(&store, &view, checked)
        } else {
            maud::html! {
                (crate::views::domains::card_only(&store, &view))
                (crate::views::domains::restriction_banner_oob(&store, restricted, can_restrict))
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
    super::auth::redirect_303(&format!("/stores/{id}/settings/domains/{}/check", view.id))
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
    let now = crate::now_unix();
    let store_id = row.id.clone();
    let deleted = state
        .db
        .write(move |db| {
            let restricted = db.embed_restricted(&store_id).unwrap_or(false);
            let domains = db.list_store_domains(&store_id).unwrap_or_default();
            let counting: Vec<&StoreDomainRow> = domains
                .iter()
                .filter(|domain| DomainState::of(domain, now).counts())
                .collect();
            if restricted && counting.len() == 1 && counting[0].id == domain_id {
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
            let error = "This is your last verified domain. Turn off \"Only my verified domains can show this checkout\" first - otherwise no website could show your checkout.";
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

    use crate::embed_domains::test_support::FakeDns;
    use crate::embed_domains::{self, GRACE_SECS, RECHECK_EVERY_SECS};
    use crate::engine_client::EngineClient;

    use super::super::{build_router, AppState};

    async fn test_state(dns: Arc<FakeDns>) -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let state = AppState {
            proofs: crate::embed_domains::test_support::proofs(dns, Arc::default()),
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

    /// A new store's site joins its domains straight away; a store from
    /// before domains existed gets its site imported, once.
    #[tokio::test]
    async fn a_new_stores_site_joins_its_domains_and_existing_stores_get_theirs_imported_once() {
        let (state, _engine) = test_state(Arc::new(FakeDns::default())).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "import@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let domains = |state: &AppState, id: &str| -> Vec<String> {
            state
                .db
                .lock()
                .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
                .unwrap()
                .into_iter()
                .map(|d| d.domain)
                .collect()
        };
        assert_eq!(
            domains(&state, &id),
            vec!["store-home.example".to_string()],
            "a new store's site is added straight away"
        );

        // A store from before this existed: only its own site is imported,
        // from monokulo's own records (nothing is read from the engine).
        let site_only = vec!["store-home.example".to_string()];
        let existing = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap();
        for domain in existing {
            state
                .db
                .lock()
                .delete_store_domain(&shared::ids::ConnectionId::new(id.to_string()), &domain.id)
                .unwrap();
        }
        state
            .db
            .lock()
            .reset_store_domains_imported_for_test(&shared::ids::ConnectionId::new(id.to_string()));
        embed_domains::import_existing_domains(&state.db.lock());
        assert_eq!(domains(&state, &id), site_only);

        // Once only: a domain the merchant removes afterwards stays removed.
        let site = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        state
            .db
            .lock()
            .delete_store_domain(&shared::ids::ConnectionId::new(id.to_string()), &site.id)
            .unwrap();
        embed_domains::import_existing_domains(&state.db.lock());
        assert!(domains(&state, &id).is_empty());
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
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(id.to_string()))
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
                &shared::ids::ConnectionId::new(id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
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
        state.abuse = Arc::new(crate::abuse::AbuseProtection::new(
            crate::abuse::AbuseConfig {
                soft_per_min: 4,
                per_store_key_per_min: 5,
                ..Default::default()
            },
        ));
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
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        dns.publish(
            "_monokulo.store-home.example",
            &embed_domains::record_value(&row.token),
        );
        embed_domains::check_and_record(&state.db, &state.proofs, &row, crate::now_unix())
            .await
            .unwrap();
        state
            .db
            .lock()
            .set_embed_restricted(&shared::ids::ConnectionId::new(id.to_string()), true)
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
            format!("/checkout/{}", order["order_id"].as_str().unwrap())
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
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        dns.publish(
            "_monokulo.store-home.example",
            &embed_domains::record_value(&row.token),
        );
        embed_domains::check_and_record(&state.db, &state.proofs, &row, crate::now_unix())
            .await
            .unwrap();
        state
            .db
            .lock()
            .set_embed_restricted(&shared::ids::ConnectionId::new(id.to_string()), true)
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
        let checkout = format!("/checkout/{order_id}");
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
            .client_logging(&shared::ids::ConnectionId::new(id.to_string()))
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
            .client_logging(&shared::ids::ConnectionId::new(id.to_string()))
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
        let checkout = format!("/checkout/{order_id}");
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
        assert!(!page.headers().contains_key("content-security-policy"));

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
            .embed_restricted(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap());

        // Verify the store's own domain, then turn it on.
        let row = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(row.domain, "store-home.example");
        dns.publish(
            "_monokulo.store-home.example",
            &embed_domains::record_value(&row.token),
        );
        embed_domains::check_and_record(&state.db, &state.proofs, &row, crate::now_unix())
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
            page.headers()["content-security-policy"],
            "frame-ancestors 'self' https://store-home.example https://*.store-home.example"
        );

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
            &shared::ids::ConnectionId::new(id.to_string()),
            crate::now_unix(),
        )
        .await;
        assert!(!warnings.shown_nowhere);
        dns.remove("_monokulo.store-home.example");
        let later = crate::now_unix() + embed_domains::RECHECK_EVERY_SECS;
        embed_domains::recheck_due(&state.db, &state.proofs, later).await;
        let failing_since = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap()[0]
            .failing_since
            .unwrap();
        assert!(
            super::store_page_warnings(
                &state,
                &shared::ids::ConnectionId::new(id.to_string()),
                failing_since + GRACE_SECS
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
            Arc::new(crate::embed_domains::test_support::FakeOnion::default()),
        );
        let (state, _engine) = test_state(dns.clone()).await;
        let state = AppState {
            proofs: crate::embed_domains::test_support::proofs(dns, onion.clone()),
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
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
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
                embed_domains::record_value(&row.token)
            ),
        );
        state
            .db
            .lock()
            .record_store_domain_check(&row.id, 0, None)
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
        // Verified domains are on the Connections tab.
        let connections = format!("{settings}?tab=connections");
        let store_page = format!("/stores/{id}");

        // The store's own site is already on its list, waiting for DNS.
        let suggested = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap();
        assert_eq!(
            suggested
                .iter()
                .map(|d| d.domain.as_str())
                .collect::<Vec<_>>(),
            vec!["store-home.example"]
        );

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
                .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
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
            &embed_domains::record_value(&row.token),
        );
        let now = crate::now_unix();
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
        embed_domains::recheck_due(&state.db, &state.proofs, now + RECHECK_EVERY_SECS).await;
        let row = shop(&state);
        assert!(row.failing_since.is_some());
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(
            html.contains("shop.example failed its DNS check"),
            "got: {html}"
        );
        let warnings = super::store_page_warnings(
            &state,
            &shared::ids::ConnectionId::new(id.to_string()),
            row.failing_since.unwrap() + GRACE_SECS,
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
                .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
                .unwrap()
                .len(),
            1
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
        // Verified domains are on the Connections tab.
        let connections = format!("{settings}?tab=connections");
        let row = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
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
            &embed_domains::record_value(&row.token),
        );
        state
            .db
            .lock()
            .record_store_domain_check(
                &row.id,
                crate::now_unix() - embed_domains::MIN_CHECK_GAP_SECS,
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
                r##"href="{settings}?tab=connections#domain-{}""##,
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
    /// verified that hits the outage on its daily re-check is flagged but
    /// keeps working through the grace period, and clears by itself on the
    /// next (hourly) re-check once DNS answers again.
    #[tokio::test]
    async fn a_dns_outage_is_reported_as_such_and_a_verified_domain_recovers_by_itself() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "dns-outage@example.com").await;
        let id = create_store(&router, &state, &session).await;
        let settings = format!("/stores/{id}/settings");
        // Verified domains are on the Connections tab.
        let connections = format!("{settings}?tab=connections");
        let store_page = format!("/stores/{id}");
        let row = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(id.to_string()))
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
        dns.publish(name, &embed_domains::record_value(&row.token));
        let now = crate::now_unix();
        embed_domains::check_and_record(&state.db, &state.proofs, &row, now)
            .await
            .unwrap();
        state
            .db
            .lock()
            .set_embed_restricted(&shared::ids::ConnectionId::new(id.to_string()), true)
            .unwrap();

        // A day later the re-check lands in an outage.
        dns.fail(name, "SERVFAIL");
        embed_domains::recheck_due(&state.db, &state.proofs, now + RECHECK_EVERY_SECS).await;
        let failing_since = state
            .db
            .lock()
            .get_store_domain(&shared::ids::ConnectionId::new(id.to_string()), &row.id)
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
            &shared::ids::ConnectionId::new(id.to_string()),
            failing_since + GRACE_SECS - 60,
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

        // An hour on, DNS answers again: the failing domain's more frequent
        // re-check clears it with no action from the merchant.
        dns.publish(name, &embed_domains::record_value(&row.token));
        // Measured from when the failing check was recorded (the re-check
        // records at the time it was run for, not the wall clock).
        embed_domains::recheck_due(
            &state.db,
            &state.proofs,
            failing_since + embed_domains::FAILING_RECHECK_EVERY_SECS + 1,
        )
        .await;
        let row = state
            .db
            .lock()
            .get_store_domain(&shared::ids::ConnectionId::new(id.to_string()), &row.id)
            .unwrap()
            .unwrap();
        assert!(row.failing_since.is_none());
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(!html.contains("failed its DNS check"), "got: {html}");
    }
}
