//! The dashboard side of verified embed domains (`crate::embed_domains`):
//! adding, checking and removing a store's domains on its settings page, and
//! the warnings its store page shows.

use axum::extract::{Path, State};
use axum::http::StatusCode;
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

/// The store page's two embed warnings.
pub(super) fn store_page_warnings(state: &AppState, connection_id: &str, now: i64) -> EmbedWarnings {
    let (dismissed, rows) = {
        let db = state.db.lock().unwrap();
        (db.embed_warning_dismissed(connection_id).unwrap_or(false), db.list_store_domains(connection_id).unwrap_or_default())
    };
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
    EmbedWarnings { any_site_dismissed: dismissed, failing }
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
    let deleted = state.db.lock().unwrap().delete_store_domain(&row.id, &domain_id);
    match deleted {
        Ok(true) => redirect_302(&settings_url(&id)),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
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
        let body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://shop.example",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "allowed_origins": [],
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
        json["connection_id"].as_str().unwrap().to_string()
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

        // Before anything: the store page says any website can show the checkout.
        let (_, html) = send(&router, "GET", &store_page, &session, None).await;
        assert!(html.contains("Any website can show this store's checkout</strong>"), "got: {html}");

        // Added from a pasted URL; the record to publish is shown.
        let (status, _) = send(&router, "POST", &format!("{settings}/domains"), &session, Some("domain=https%3A%2F%2FShop.Example%2Fcart")).await;
        assert_eq!(status, StatusCode::FOUND);
        let row = state.db.lock().unwrap().list_store_domains(&id).unwrap().pop().unwrap();
        assert_eq!(row.domain, "shop.example");
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
        let row = state.db.lock().unwrap().list_store_domains(&id).unwrap().pop().unwrap();
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
        assert!(state.db.lock().unwrap().list_store_domains(&id).unwrap().is_empty());
    }
}
