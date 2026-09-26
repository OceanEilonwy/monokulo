//! The dashboard side of verified embed domains (`crate::embed_domains`):
//! adding, checking and removing a store's domains and turning the
//! restriction on or off on its settings page, and the warnings its store
//! page shows - plus [`embed_policy_middleware`], which enforces the
//! restriction on the public `/pay/{pk}/...` routes.

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::db::StoreDomainRow;
use crate::embed_domains::{self, DomainState};
use crate::templates::format_duration_until;
use crate::views::store_detail::{EmbedWarnings, FailingDomainWarning};
use crate::views::store_settings::EmbedDomainView;

use super::dashboard::redirect_302;
use super::orders::{load_owned_connection, render_store_settings_page};
use super::{AppState, AuthedUser};

/// `{pk}` from a public `/pay/{pk}/...` path.
pub(super) fn public_key_of_pay_path(path: &str) -> Option<&str> {
    path.strip_prefix("/pay/")?.split('/').next().filter(|pk| !pk.is_empty())
}

/// Enforces a restricted store's embed policy on its `/pay/{pk}/...` routes
/// (CORS is `super::embed_cors_layer`'s job):
///
/// - every response carries `Content-Security-Policy: frame-ancestors ...`,
///   so browsers only show the checkout inside monokulo itself or a
///   verified domain;
/// - `POST /pay/{pk}/orders` must come either from a browser page on a
///   verified domain (its `Origin`) or from someone holding the store's
///   secret key (`super::store_key`, e.g. the WooCommerce plugin on the
///   shop's server). Anything else gets `403`, including a request with no
///   `Origin` and no key, which could otherwise come from any script
///   anywhere.
///
/// An unrestricted store's requests pass through untouched.
pub async fn embed_policy_middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let policy = public_key_of_pay_path(request.uri().path())
        .and_then(|public_key| embed_domains::policy_for_public_key(&state.db, public_key))
        .filter(|policy| policy.restricted);
    let Some(policy) = policy else { return next.run(request).await };
    let now = crate::now_unix();

    let creates_order = request.method() == Method::POST && request.uri().path().ends_with("/orders") && request.uri().path().matches('/').count() == 3;
    let has_key = request.extensions().get::<super::store_key::StoreKeyAuthenticated>().is_some();
    if creates_order && !has_key {
        let error = match request.headers().get(header::ORIGIN) {
            Some(origin) if origin.to_str().is_ok_and(|origin| policy.allows_origin(origin, now)) => None,
            Some(_) => Some("This store only accepts orders from its verified websites."),
            None => Some(
                "This store only accepts orders from its verified websites, or from its own server using the store's secret key.",
            ),
        };
        if let Some(error) = error {
            return (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({ "error": error }))).into_response();
        }
    }

    let mut response = next.run(request).await;
    if let Some(value) = policy.frame_ancestors(now).and_then(|value| HeaderValue::from_str(&value).ok()) {
        response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

fn settings_url(id: &str) -> String {
    format!("/dashboard/stores/{id}/settings#verified-domains")
}

/// "3h 20m ago", or "just now" under a minute.
fn ago(then: i64, now: i64) -> String {
    if now - then < 60 { "just now".to_string() } else { format!("{} ago", format_duration_until(now, then)) }
}

/// The settings page's rows for a store's domains.
pub(super) fn domain_views(rows: Vec<StoreDomainRow>, now: i64) -> Vec<EmbedDomainView> {
    rows.into_iter()
        .map(|row| {
            let state = DomainState::of(&row, now);
            let (state_tag, state_label, detail) = match state {
                DomainState::Pending => ("unknown", "Waiting for DNS", None),
                DomainState::Verified => ("ok", "Verified", None),
                DomainState::Failing { since } => (
                    "error",
                    "Failing",
                    Some(format!(
                        "Record missing for {}. Still counts as verified for {}.",
                        format_duration_until(now, since),
                        format_duration_until(since + embed_domains::GRACE_SECS, now)
                    )),
                ),
                DomainState::Lapsed { since } => (
                    "error",
                    "No longer verified",
                    Some(format!("Record missing for {}. Publish it again and check.", format_duration_until(now, since))),
                ),
            };
            EmbedDomainView {
                record_name: embed_domains::record_name(&row.domain),
                record_value: embed_domains::record_value(&row.token),
                show_record: state != DomainState::Verified,
                last_checked: row.last_checked_at.map(|at| ago(at, now)).unwrap_or_else(|| "never".to_string()),
                last_error: row.last_error.filter(|_| state != DomainState::Verified),
                id: row.id,
                domain: row.domain,
                state_tag,
                state_label,
                detail,
            }
        })
        .collect()
}

/// The store page's embed warnings.
pub(super) fn store_page_warnings(state: &AppState, connection_id: &str, now: i64) -> EmbedWarnings {
    let (dismissed, restricted, rows) = {
        let db = state.db.lock().unwrap();
        (
            db.embed_warning_dismissed(connection_id).unwrap_or(false),
            db.embed_restricted(connection_id).unwrap_or(false),
            db.list_store_domains(connection_id).unwrap_or_default(),
        )
    };
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
                missing_for: format_duration_until(now, since),
                lapsed,
                counts_for: format_duration_until(since + embed_domains::GRACE_SECS, now),
            })
        })
        .collect();
    EmbedWarnings { restricted, any_site_dismissed: dismissed, shown_nowhere, failing }
}

#[derive(Deserialize)]
pub struct EmbedRestrictionForm {
    /// `"on"` or `"off"`.
    pub restricted: String,
}

/// `POST /dashboard/stores/{id}/settings/embed-restriction` - turns "Only my
/// verified domains can show this checkout" on or off. It can only be
/// turned on while at least one domain counts as verified; otherwise the
/// checkout could be shown nowhere.
pub async fn set_embed_restriction(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<EmbedRestrictionForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let restricted = form.restricted == "on";
    let now = crate::now_unix();
    let result = {
        let db = state.db.lock().unwrap();
        match db.list_store_domains(&row.id) {
            Ok(domains) if restricted && !domains.iter().any(|domain| DomainState::of(domain, now).counts()) => {
                Ok(Some("Verify at least one domain before turning this on - otherwise no website could show your checkout."))
            }
            Ok(_) => db.set_embed_restricted(&row.id, restricted).map(|()| None),
            Err(e) => Err(e),
        }
    };
    match result {
        Ok(None) => redirect_302(&settings_url(&id)),
        Ok(Some(error)) => render_store_settings_page(&state, row, &user, Some(error.to_string()), None).await,
        Err(_) => render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None).await,
    }
}

#[derive(Deserialize)]
pub struct AddDomainForm {
    pub domain: String,
}

/// `POST /dashboard/stores/{id}/settings/domains` - adds a domain waiting
/// for its DNS record.
pub async fn add_domain(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<AddDomainForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let domain = match embed_domains::normalize_domain(&form.domain) {
        Ok(domain) => domain,
        Err(message) => return render_store_settings_page(&state, row, &user, Some(message.to_string()), None).await,
    };
    let domain_id = uuid::Uuid::new_v4().to_string();
    let created = state.db.lock().unwrap().create_store_domain(
        &domain_id,
        &row.id,
        &domain,
        &embed_domains::new_token(),
        crate::now_unix(),
        embed_domains::MAX_DOMAINS_PER_STORE,
    );
    let error = match created {
        Ok(true) => return redirect_302(&settings_url(&id)),
        Ok(false) => format!("A store can have at most {} domains. Remove one to add another.", embed_domains::MAX_DOMAINS_PER_STORE),
        Err(e) if e.is_unique_violation() => format!("{domain} is already on this store's list."),
        Err(_) => "Something went wrong. Please try again.".to_string(),
    };
    render_store_settings_page(&state, row, &user, Some(error), None).await
}

/// `POST /dashboard/stores/{id}/settings/domains/{domain_id}/check` - looks
/// the domain's record up now. The result shows on the settings page.
pub async fn check_domain(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, domain_id)): Path<(String, String)>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let domain = match state.db.lock().unwrap().get_store_domain(&row.id, &domain_id) {
        Ok(Some(domain)) => domain,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let now = crate::now_unix();
    if let Some(last) = domain.last_checked_at {
        let wait = embed_domains::MIN_CHECK_GAP_SECS - (now - last);
        if wait > 0 {
            let error = format!("{} was checked moments ago. Try again in {wait} seconds.", domain.domain);
            return render_store_settings_page(&state, row, &user, Some(error), None).await;
        }
    }
    match embed_domains::check_and_record(&state.db, state.dns.as_ref(), &domain, now).await {
        Ok(_) => redirect_302(&settings_url(&id)),
        Err(_) => render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None).await,
    }
}

/// `POST /dashboard/stores/{id}/settings/domains/{domain_id}/delete`.
pub async fn delete_domain(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, domain_id)): Path<(String, String)>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let now = crate::now_unix();
    let deleted = {
        let db = state.db.lock().unwrap();
        let restricted = db.embed_restricted(&row.id).unwrap_or(false);
        let domains = db.list_store_domains(&row.id).unwrap_or_default();
        let counting: Vec<&StoreDomainRow> = domains.iter().filter(|domain| DomainState::of(domain, now).counts()).collect();
        if restricted && counting.len() == 1 && counting[0].id == domain_id {
            Ok(None)
        } else {
            db.delete_store_domain(&row.id, &domain_id).map(Some)
        }
    };
    match deleted {
        Ok(Some(true)) => redirect_302(&settings_url(&id)),
        Ok(Some(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(None) => {
            let error = "This is your last verified domain. Turn off \"Only my verified domains can show this checkout\" first - otherwise no website could show your checkout.";
            render_store_settings_page(&state, row, &user, Some(error.to_string()), None).await
        }
        Err(_) => render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None).await,
    }
}

/// `POST /dashboard/stores/{id}/embed-warning/dismiss` - shrinks the store
/// page's "any website can show this checkout" warning to one line.
pub async fn dismiss_embed_warning(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let dismissed = state.db.lock().unwrap().dismiss_embed_warning(&row.id);
    match dismissed {
        Ok(()) => redirect_302(&format!("/dashboard/stores/{id}")),
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

    use crate::db::Db;
    use crate::embed_domains::test_support::FakeDns;
    use crate::embed_domains::{self, GRACE_SECS, RECHECK_EVERY_SECS};
    use crate::engine_client::EngineClient;

    use super::super::{build_router, AppState};

    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    async fn test_state(dns: Arc<FakeDns>) -> (AppState, scanner_test_support::TestEngineHandle) {
        let engine = scanner_test_support::TestEngineConfig::new().with_networks(&[monero::Network::Mainnet]).spawn().await;
        let state = AppState {
            db: {
                let db = Db::open_in_memory().unwrap();
                db.seed_test_admin();
                db.into_shared()
            },
            engine_client: EngineClient::new(format!("http://{}", engine.addr)),
            encryption_key: [7u8; 32],
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only()),
            rate_limiter: Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
            event_streams: Default::default(),
            store_key_rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
            dns,
        };
        (state, engine)
    }

    async fn send(router: &Router, method: &str, uri: &str, session: &str, form: Option<&str>) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(uri).header("authorization", format!("Bearer {session}"));
        if form.is_some() {
            builder = builder.header("content-type", "application/x-www-form-urlencoded");
        }
        let response = router.clone().oneshot(builder.body(Body::from(form.unwrap_or("").to_string())).unwrap()).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn session_for(router: &Router, email: &str) -> String {
        let credentials = serde_json::json!({ "email": email, "password": "correct horse battery staple" }).to_string();
        for uri in ["/signup", "/login"] {
            let response = router
                .clone()
                .oneshot(Request::builder().method("POST").uri(uri).header("content-type", "application/json").body(Body::from(credentials.clone())).unwrap())
                .await
                .unwrap();
            assert!(response.status().is_success(), "{uri}: {}", response.status());
            if uri == "/login" {
                let body = response.into_body().collect().await.unwrap().to_bytes();
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                return json["session_token"].as_str().unwrap().to_string();
            }
        }
        unreachable!()
    }

    async fn create_store(router: &Router, session: &str) -> String {
        create_store_with_key(router, session).await.0
    }

    /// `(connection_id, public_key)`.
    async fn create_store_with_key(router: &Router, session: &str) -> (String, String) {
        let body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://store-home.example/shop",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "domains": [],
            "base_currency": "XMR",
        });
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (json["connection_id"].as_str().unwrap().to_string(), json["public_key"].as_str().unwrap().to_string())
    }

    /// `POST /pay/{pk}/orders` from a page on `origin` (or no page at all).
    async fn create_order_from(router: &Router, pk: &str, origin: Option<&str>) -> axum::response::Response {
        let mut builder = Request::builder().method("POST").uri(format!("/pay/{pk}/orders")).header("content-type", "application/json");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        let body = serde_json::json!({ "amount": "1", "currency": "XMR" }).to_string();
        router.clone().oneshot(builder.body(Body::from(body)).unwrap()).await.unwrap()
    }

    async fn create_store_via_api(router: &Router, session: &str, extra: serde_json::Value) -> (StatusCode, String) {
        let mut body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://store-home.example",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "base_currency": "XMR",
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default();
        (status, json["connection_id"].as_str().unwrap_or_default().to_string())
    }

    #[tokio::test]
    async fn api_domains_join_the_store_and_existing_stores_get_their_site_imported_once() {
        let (state, _engine) = test_state(Arc::new(FakeDns::default())).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "import@example.com").await;
        let (status, id) = create_store_via_api(
            &router,
            &session,
            serde_json::json!({ "domains": ["headless.example", "https://second.example", "http://abcdefghijklmnop.onion"] }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let domains = |state: &AppState, id: &str| -> Vec<String> {
            state.db.lock().unwrap().list_store_domains(id).unwrap().into_iter().map(|d| d.domain).collect()
        };
        assert_eq!(
            domains(&state, &id),
            vec!["headless.example".to_string(), "second.example".to_string(), "store-home.example".to_string()],
            "a new store's site and domains (bare or as origins) are added straight away; onion skipped"
        );

        // A store from before this existed: only its own site is imported,
        // from monokulo's own records (nothing is read from the engine).
        let site_only = vec!["store-home.example".to_string()];
        let existing = state.db.lock().unwrap().list_store_domains(&id).unwrap();
        for domain in existing {
            state.db.lock().unwrap().delete_store_domain(&id, &domain.id).unwrap();
        }
        state.db.lock().unwrap().reset_store_domains_imported_for_test(&id);
        embed_domains::import_existing_domains(&state.db);
        assert_eq!(domains(&state, &id), site_only);

        // Once only: a domain the merchant removes afterwards stays removed.
        let site = state.db.lock().unwrap().list_store_domains(&id).unwrap().pop().unwrap();
        state.db.lock().unwrap().delete_store_domain(&id, &site.id).unwrap();
        embed_domains::import_existing_domains(&state.db);
        assert!(domains(&state, &id).is_empty());
    }

    /// Old API callers still sending `allowed_origins` (the field's name when
    /// it was forwarded to the engine) get the same treatment as `domains`,
    /// and the engine tenant is created with no origins either way.
    #[tokio::test]
    async fn the_old_allowed_origins_field_is_accepted_as_an_alias_for_domains() {
        let (state, _engine) = test_state(Arc::new(FakeDns::default())).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "alias@example.com").await;
        let (status, id) =
            create_store_via_api(&router, &session, serde_json::json!({ "allowed_origins": ["https://legacy.example"] })).await;
        assert_eq!(status, StatusCode::CREATED);
        let domains: Vec<String> =
            state.db.lock().unwrap().list_store_domains(&id).unwrap().into_iter().map(|d| d.domain).collect();
        assert_eq!(domains, vec!["legacy.example".to_string(), "store-home.example".to_string()]);

        let (status, _) = create_store_via_api(&router, &session, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::CREATED, "domains is optional");

        let (status, _) = create_store_via_api(
            &router,
            &session,
            serde_json::json!({ "allowed_origins": ["https://a.example"], "domains": ["b.example"] }),
        )
        .await;
        assert!(status.is_client_error(), "sending both names is refused rather than silently picking one");
    }

    /// `POST /pay/{pk}/orders` with an optional page `Origin` and optional
    /// `Authorization` value.
    async fn create_order_as(router: &Router, pk: &str, origin: Option<&str>, authorization: Option<&str>) -> (StatusCode, serde_json::Value) {
        create_order_from_ip(router, "203.0.113.9", pk, origin, authorization).await
    }

    async fn create_order_from_ip(
        router: &Router,
        ip: &str,
        pk: &str,
        origin: Option<&str>,
        authorization: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method("POST").uri(format!("/pay/{pk}/orders")).header("content-type", "application/json");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        if let Some(authorization) = authorization {
            builder = builder.header("authorization", authorization);
        }
        let body = serde_json::json!({ "amount": "1", "currency": "XMR" }).to_string();
        let mut request = builder.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(format!("{ip}:4000").parse::<std::net::SocketAddr>().unwrap()));
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    fn secret_key_of(state: &AppState, id: &str) -> String {
        let row = state.db.lock().unwrap().get_store_connection_by_id(id).unwrap().unwrap();
        crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap()
    }

    fn recorded_with_key(state: &AppState, id: &str, order: &serde_json::Value) -> bool {
        let order_id = order["order_id"].as_str().unwrap();
        state.db.lock().unwrap().get_order_currency_metadata(id, order_id).unwrap().unwrap().created_with_key
    }

    #[tokio::test]
    async fn secret_key_orders_are_accepted_and_recorded_and_restricted_stores_need_a_key_or_a_verified_page() {
        let dns = Arc::new(FakeDns::default());
        let (mut state, _engine) = test_state(dns.clone()).await;
        // A per-IP budget of 4 a minute: key requests must not spend it.
        state.rate_limiter = Arc::new(shared::rate_limit::RateLimiter::new(4));
        state.store_key_rate_limiter = Arc::new(shared::rate_limit::RateLimiter::new(5));
        let router = build_router(state.clone());
        let session = session_for(&router, "keys@example.com").await;
        let (id, pk) = create_store_with_key(&router, &session).await;
        let (other_id, _) = create_store_with_key(&router, &session).await;
        let key = format!("Bearer {}", secret_key_of(&state, &id));
        let other_key = format!("Bearer {}", secret_key_of(&state, &other_id));

        // Unrestricted: no key and no Origin still works, recorded as not keyed.
        let (status, order) = create_order_as(&router, &pk, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!recorded_with_key(&state, &id, &order));

        // The right key works and is recorded as keyed.
        let (status, order) = create_order_as(&router, &pk, None, Some(&key)).await;
        assert_eq!(status, StatusCode::OK, "{order}");
        assert!(recorded_with_key(&state, &id, &order));

        // A wrong key, another store's key or a non-bearer value: 401 with a JSON error.
        for bad in ["Bearer sk_wrong", other_key.as_str(), "Basic abc"] {
            let (status, body) = create_order_as(&router, &pk, None, Some(bad)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{bad}");
            assert!(body["error"].as_str().unwrap().contains("secret key"), "{body}");
        }
        // One unkeyed order and three failures spent the per-IP budget (4): unauthenticated
        // requests from this address are now limited, keyed ones are not.
        assert_eq!(create_order_as(&router, &pk, None, None).await.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(create_order_as(&router, &pk, None, Some(&key)).await.0, StatusCode::OK);

        // Restrict the store to its verified domain.
        let row = state.db.lock().unwrap().list_store_domains(&id).unwrap().pop().unwrap();
        dns.publish("_monokulo.store-home.example", &embed_domains::record_value(&row.token));
        embed_domains::check_and_record(&state.db, dns.as_ref(), &row, crate::now_unix()).await.unwrap();
        state.db.lock().unwrap().set_embed_restricted(&id, true).unwrap();

        // From a fresh address (the first one is out of per-IP budget):
        // neither a key nor an Origin is refused with a clear message.
        let (status, body) = create_order_from_ip(&router, "198.51.100.7", &pk, None, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("secret key"), "{body}");
        // A verified page: fine, and not keyed.
        let (status, order) = create_order_from_ip(&router, "198.51.100.7", &pk, Some("https://store-home.example"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!recorded_with_key(&state, &id, &order));
        // The key, with or without an Origin: fine.
        assert_eq!(create_order_as(&router, &pk, None, Some(&key)).await.0, StatusCode::OK);
        assert_eq!(create_order_as(&router, &pk, Some("https://elsewhere.example"), Some(&key)).await.0, StatusCode::OK);
        // Another store's key is still a 401, not a pass.
        assert_eq!(create_order_from_ip(&router, "198.51.100.7", &pk, None, Some(&other_key)).await.0, StatusCode::UNAUTHORIZED);

        // The per-store key budget (5) is its own limit: 4 keyed orders so
        // far, 1 more passes, then 429.
        assert_eq!(create_order_as(&router, &pk, None, Some(&key)).await.0, StatusCode::OK);
        assert_eq!(create_order_as(&router, &pk, None, Some(&key)).await.0, StatusCode::TOO_MANY_REQUESTS);
    }

    /// `GET uri` with an optional `Sec-Fetch-Dest`: `(status, body, Vary)`.
    async fn fetch_as(router: &Router, uri: &str, dest: Option<&str>) -> (StatusCode, String, String) {
        let mut builder = Request::builder().uri(uri);
        if let Some(dest) = dest {
            builder = builder.header("sec-fetch-dest", dest);
        }
        let response = router.clone().oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
        let status = response.status();
        let vary = response.headers().get("vary").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap(), vary)
    }

    #[tokio::test]
    async fn a_restricted_stores_browser_created_orders_only_open_inside_a_frame() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "frame-only@example.com").await;
        let (id, pk) = create_store_with_key(&router, &session).await;
        let key = format!("Bearer {}", secret_key_of(&state, &id));

        // Created while the store is still unrestricted, from a page: not keyed.
        let (_, browser_order) = create_order_as(&router, &pk, Some("https://store-home.example"), None).await;
        let (_, keyed_order) = create_order_as(&router, &pk, None, Some(&key)).await;
        let checkout = |order: &serde_json::Value| format!("/pay/{pk}/orders/{}", order["order_id"].as_str().unwrap());

        // Unrestricted: every order opens as a full page.
        assert_eq!(fetch_as(&router, &checkout(&browser_order), Some("document")).await.0, StatusCode::OK);

        let row = state.db.lock().unwrap().list_store_domains(&id).unwrap().pop().unwrap();
        dns.publish("_monokulo.store-home.example", &embed_domains::record_value(&row.token));
        embed_domains::check_and_record(&state.db, dns.as_ref(), &row, crate::now_unix()).await.unwrap();
        state.db.lock().unwrap().set_embed_restricted(&id, true).unwrap();

        // Restricted, browser-created: a full page gets the plain "open it from the shop" page.
        let (status, html, vary) = fetch_as(&router, &checkout(&browser_order), Some("document")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(html.contains("Open this payment from the shop"), "got: {html}");
        assert!(!html.contains("<script"), "works without JavaScript, got: {html}");
        assert!(!html.contains(browser_order["address"].as_str().unwrap()), "no payment details leak, got: {html}");
        assert!(vary.contains("Sec-Fetch-Dest"), "caches must key on the header, got: {vary:?}");
        // ...but inside a frame, or from a browser that sends no Sec-Fetch-Dest, it renders.
        for dest in [Some("iframe"), Some("frame"), None] {
            let (status, html, _) = fetch_as(&router, &checkout(&browser_order), dest).await;
            assert_eq!(status, StatusCode::OK, "{dest:?}");
            assert!(html.contains(browser_order["address"].as_str().unwrap()), "{dest:?}");
        }
        // Its share page is turned away the same way when opened directly.
        let share = format!("{}/share", checkout(&browser_order));
        assert_eq!(fetch_as(&router, &share, Some("document")).await.0, StatusCode::FORBIDDEN);
        // Status and live updates keep working (the embed library and the framed page use them).
        assert_eq!(fetch_as(&router, &format!("{}/status", checkout(&browser_order)), Some("empty")).await.0, StatusCode::OK);

        // A keyed order (WooCommerce, dashboard, POS, payment links) opens either way, share page included.
        for dest in [Some("document"), Some("iframe"), None] {
            assert_eq!(fetch_as(&router, &checkout(&keyed_order), dest).await.0, StatusCode::OK, "{dest:?}");
        }
        assert_eq!(fetch_as(&router, &format!("{}/share", checkout(&keyed_order)), Some("document")).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_restricted_store_only_works_on_its_verified_domains() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "restricted@example.com").await;
        let (id, pk) = create_store_with_key(&router, &session).await;
        let settings = format!("/dashboard/stores/{id}/settings");
        let turn = |on: bool| format!("restricted={}", if on { "on" } else { "off" });

        // Unrestricted: any site, no framing header.
        let response = create_order_from(&router, &pk, Some("https://anything.example")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let order_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["order_id"].as_str().unwrap().to_string();
        let checkout = format!("/pay/{pk}/orders/{order_id}");
        let page = router.clone().oneshot(Request::builder().uri(&checkout).body(Body::empty()).unwrap()).await.unwrap();
        assert!(!page.headers().contains_key("content-security-policy"));

        // Can't turn on before a domain is verified.
        let (_, html) = send(&router, "POST", &format!("{settings}/embed-restriction"), &session, Some(&turn(true))).await;
        assert!(html.contains("Verify at least one domain before turning this on"), "got: {html}");
        assert!(!state.db.lock().unwrap().embed_restricted(&id).unwrap());

        // Verify the store's own domain, then turn it on.
        let row = state.db.lock().unwrap().list_store_domains(&id).unwrap().pop().unwrap();
        assert_eq!(row.domain, "store-home.example");
        dns.publish("_monokulo.store-home.example", &embed_domains::record_value(&row.token));
        embed_domains::check_and_record(&state.db, dns.as_ref(), &row, crate::now_unix()).await.unwrap();
        assert_eq!(send(&router, "POST", &format!("{settings}/embed-restriction"), &session, Some(&turn(true))).await.0, StatusCode::FOUND);

        // Framing: only monokulo itself and the verified domain.
        let page = router.clone().oneshot(Request::builder().uri(&checkout).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert_eq!(
            page.headers()["content-security-policy"],
            "frame-ancestors 'self' https://store-home.example https://*.store-home.example"
        );

        // Order creation: a subdomain of the verified domain is fine; no
        // page at all and no secret key (see
        // `secret_key_orders_are_accepted_and_recorded_and_restricted_stores_need_a_key_or_a_verified_page`),
        // or anywhere else, is refused.
        assert_eq!(create_order_from(&router, &pk, Some("https://www.store-home.example")).await.status(), StatusCode::OK);
        assert_eq!(create_order_from(&router, &pk, None).await.status(), StatusCode::FORBIDDEN);
        assert_eq!(create_order_from(&router, &pk, Some("https://evilstore-home.example")).await.status(), StatusCode::FORBIDDEN);
        assert_eq!(create_order_from(&router, &pk, Some("http://store-home.example")).await.status(), StatusCode::FORBIDDEN);

        // CORS answers only the verified domain.
        let status_uri = format!("{checkout}/status");
        for (origin, allowed) in [("https://shop.store-home.example", true), ("https://elsewhere.example", false)] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(&status_uri).header("origin", origin).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.headers().get("access-control-allow-origin").is_some(), allowed, "{origin}");
        }

        // The last verified domain can't be removed while it's on.
        let (_, html) = send(&router, "POST", &format!("{settings}/domains/{}/delete", row.id), &session, None).await;
        assert!(html.contains("This is your last verified domain."), "got: {html}");

        // The store page drops the "any website" warning; if the domain lapses,
        // it says the checkout can be shown nowhere.
        let (_, html) = send(&router, "GET", &format!("/dashboard/stores/{id}"), &session, None).await;
        assert!(!html.contains("Any website can show this store"), "got: {html}");
        let warnings = super::store_page_warnings(&state, &id, crate::now_unix());
        assert!(!warnings.shown_nowhere);
        dns.remove("_monokulo.store-home.example");
        let later = crate::now_unix() + embed_domains::RECHECK_EVERY_SECS;
        embed_domains::recheck_due(&state.db, dns.as_ref(), later).await;
        let failing_since = state.db.lock().unwrap().list_store_domains(&id).unwrap()[0].failing_since.unwrap();
        assert!(super::store_page_warnings(&state, &id, failing_since + GRACE_SECS).shown_nowhere);

        // Off again: any site.
        assert_eq!(send(&router, "POST", &format!("{settings}/embed-restriction"), &session, Some(&turn(false))).await.0, StatusCode::FOUND);
        assert_eq!(create_order_from(&router, &pk, Some("https://elsewhere.example")).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_domain_is_added_verified_by_its_dns_record_and_warned_about_once_it_goes_missing() {
        let dns = Arc::new(FakeDns::default());
        let (state, _engine) = test_state(dns.clone()).await;
        let router = build_router(state.clone());
        let session = session_for(&router, "domains@example.com").await;
        let id = create_store(&router, &session).await;
        let settings = format!("/dashboard/stores/{id}/settings");
        let store_page = format!("/dashboard/stores/{id}");

        // The store's own site is already on its list, waiting for DNS.
        let suggested = state.db.lock().unwrap().list_store_domains(&id).unwrap();
        assert_eq!(suggested.iter().map(|d| d.domain.as_str()).collect::<Vec<_>>(), vec!["store-home.example"]);

        // Before anything: the store page says any website can show the checkout.
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(html.contains("Any website can show this store's checkout</strong>"), "got: {html}");

        // Added from a pasted URL; the record to publish is shown.
        let (status, _) = send(&router, "POST", &format!("{settings}/domains"), &session, Some("domain=https%3A%2F%2FShop.Example%2Fcart")).await;
        assert_eq!(status, StatusCode::FOUND);
        let shop = |state: &AppState| state.db.lock().unwrap().list_store_domains(&id).unwrap().into_iter().find(|d| d.domain == "shop.example").unwrap();
        let row = shop(&state);
        let (_, html) = send(&router, "GET", &settings, &session, None).await;
        assert!(html.contains("<code>_monokulo.shop.example</code>"), "got: {html}");
        assert!(html.contains(&format!("<code>monokulo-verify={}</code>", row.token)));
        assert!(html.contains("Waiting for DNS"));

        // The same domain twice, and an onion address, are refused.
        let (_, html) = send(&router, "POST", &format!("{settings}/domains"), &session, Some("domain=shop.example")).await;
        assert!(html.contains("shop.example is already on this store's list."), "got: {html}");
        let (_, html) = send(&router, "POST", &format!("{settings}/domains"), &session, Some("domain=abcdefghijklmnop.onion")).await;
        assert!(html.contains("Onion addresses can"), "got: {html}");

        // No record yet: still waiting, with the reason.
        let check = format!("{settings}/domains/{}/check", row.id);
        assert_eq!(send(&router, "POST", &check, &session, None).await.0, StatusCode::FOUND);
        let (_, html) = send(&router, "GET", &settings, &session, None).await;
        assert!(html.contains("No TXT record at _monokulo.shop.example"), "got: {html}");
        // A second check straight away is refused.
        let (_, html) = send(&router, "POST", &check, &session, None).await;
        assert!(html.contains("was checked moments ago"), "got: {html}");

        // Published: verified (checked directly, past the rate limit).
        dns.publish("_monokulo.shop.example", &embed_domains::record_value(&row.token));
        let now = crate::now_unix();
        embed_domains::check_and_record(&state.db, dns.as_ref(), &row, now).await.unwrap();
        let (_, html) = send(&router, "GET", &settings, &session, None).await;
        assert!(html.contains(">Verified</span>"), "got: {html}");
        assert!(!html.contains(&format!("monokulo-verify={}", row.token)), "a verified domain hides its record");

        // The record disappears: the daily re-check finds it missing and the
        // store page warns, with no way to dismiss that warning.
        dns.remove("_monokulo.shop.example");
        embed_domains::recheck_due(&state.db, dns.as_ref(), now + RECHECK_EVERY_SECS).await;
        let row = shop(&state);
        assert!(row.failing_since.is_some());
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(html.contains("shop.example failed its DNS check"), "got: {html}");
        let warnings = super::store_page_warnings(&state, &id, row.failing_since.unwrap() + GRACE_SECS);
        assert!(warnings.failing[0].lapsed, "past the grace period it no longer counts");

        // Dismissing shrinks only the "any website" warning.
        let (status, _) = send(&router, "POST", &format!("/dashboard/stores/{id}/embed-warning/dismiss"), &session, None).await;
        assert_eq!(status, StatusCode::FOUND);
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(html.contains(r#"class="embed-warning is-compact""#));
        assert!(html.contains("shop.example failed its DNS check"));

        // Another merchant can't touch this store's domains.
        let other = session_for(&router, "someone-else@example.com").await;
        assert_eq!(send(&router, "POST", &check, &other, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(send(&router, "POST", &format!("{settings}/domains/{}/delete", row.id), &other, None).await.0, StatusCode::NOT_FOUND);

        // Removed.
        assert_eq!(send(&router, "POST", &format!("{settings}/domains/{}/delete", row.id), &session, None).await.0, StatusCode::FOUND);
        assert_eq!(state.db.lock().unwrap().list_store_domains(&id).unwrap().len(), 1);
    }
}
