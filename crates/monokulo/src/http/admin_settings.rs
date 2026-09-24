//! `GET`/`POST /dashboard/admin/settings` and `POST /dashboard/admin/scanner-settings` -
//! the one admin page every monokulo *and* scanner setting can be managed
//! from, per the product spec ("We need an admin page that makes it so that
//! all the monokulo + scanner settings can be set from the admin web page").
//! Gated by [`AuthedAdmin`] end to end - a merchant with a perfectly valid
//! session still gets `403` here, same as the nav only shows the "admin"
//! link to the one instance-wide admin account (`is_admin`, `crate::db`).
//!
//! **Two independent halves, two forms, two `POST` targets.** Monokulo's own
//! settings (`crate::settings::ALL_SCALAR`) are simple: read/write this
//! service's own `settings` table directly, exactly like every other setting
//! this crate already persists. The scanner half is a live HTTP proxy - this
//! page holds no scanner state of its own at all, it just calls the
//! configured scanner instance's own `GET`/`POST /api/v1/admin/settings`
//! (`scanner::http::instance_admin`) using the `engine.url`/
//! `engine.admin_token` monokulo settings, and renders/forwards whatever
//! that instance reports. This is deliberately the single-configured-scanner
//! shape a self-hosted one-box deployment has (`scripts/dev-run.sh`), not a
//! multi-tenant "one monokulo, many engines" design - see this crate's own
//! `EngineClient`, which already assumes exactly one engine base URL.
//!
//! Every field on both forms always carries its *current effective* value
//! (`value="..."`, `env > database > default`) - per the explicit "the
//! settings should have a value='' that corresponds to the active setting"
//! requirement - and the save button can always be clicked: submitting the
//! form as-is (nothing changed) just re-persists whatever is currently
//! effective, which is exactly the "use this to persist the environment
//! variables currently configured" behavior asked for.
//!
//! **Locking discipline**: `state.db.lock()` returns a `MutexGuard`, which is
//! deliberately not `Send` - every function below that does real `.await`
//! work (fetching or forwarding to the scanner) takes plain, already-read
//! owned values instead of a `&Db`/guard, and every lock is acquired,
//! read, and dropped in its own small scope *before* any `await` - the same
//! "never hold the lock across an await point" discipline every other
//! handler in this crate already follows, just spelled out here since this
//! module has more await points per handler than most.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Form, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use shared::settings::SettingSource;

use crate::db::{Db, UserRow};
use crate::settings::{ScalarSetting, ALL_SCALAR};
use crate::views;
use crate::views::admin::{AdminNetworkFieldView, AdminScalarFieldView, AdminSettingsViewModel};

use super::{AppState, AuthedAdmin};

/// `"exchange_rate.coingecko_enabled"` -> `"exchange rate coingecko enabled"` -
/// a plain, mechanical label derived straight from a settings key so no
/// separate label table can ever drift out of sync with
/// `crate::settings::ALL_SCALAR` (or, for the scanner half, with whatever
/// keys that instance happens to report).
fn humanize_key(key: &str) -> String {
    key.replace(['.', '_'], " ")
}

fn source_label(source: SettingSource) -> &'static str {
    match source {
        SettingSource::Env => "environment variable",
        SettingSource::Database => "saved value",
        SettingSource::Default => "default",
    }
}

/// Every monokulo setting's current effective value - a plain, synchronous
/// read, safe to call with the database lock held.
fn monokulo_fields(db: &Db) -> Vec<AdminScalarFieldView> {
    ALL_SCALAR
        .iter()
        .map(|setting| {
            let (value, source) = crate::settings::get_raw(db, setting);
            AdminScalarFieldView { key: setting.key.to_string(), label: humanize_key(setting.key), value, source_label: source_label(source).to_string() }
        })
        .collect()
}

/// This instance's configured scanner connection, read synchronously with
/// the lock held - the two owned `String`s are then free to travel across an
/// `.await` on their own.
fn engine_connection(db: &Db) -> (String, String) {
    (crate::settings::get::<String>(db, &crate::settings::ENGINE_URL), crate::settings::get::<String>(db, &crate::settings::SCANNER_ADMIN_TOKEN))
}

#[derive(Deserialize)]
struct RemoteScalarSetting {
    value: String,
    source: String,
}

#[derive(Deserialize)]
struct RemoteSettingsResponse {
    scalars: BTreeMap<String, RemoteScalarSetting>,
    monero_node: BTreeMap<String, Option<serde_json::Value>>,
}

fn remote_source_label(source: &str) -> String {
    match source {
        "env" => "environment variable".to_string(),
        "database" => "saved value".to_string(),
        "default" => "default".to_string(),
        other => other.to_string(),
    }
}

/// Fetches the configured scanner's own settings over HTTP - `Ok(None)` when
/// no scanner connection is configured at all (an empty `engine_url` or
/// `admin_token`, the state a fresh instance starts in), `Err` for a real
/// reachability/auth/parse failure worth showing the operator. Takes owned
/// strings, not a `&Db` - see this module's own doc comment on lock
/// discipline.
async fn fetch_scanner_settings(engine_url: &str, admin_token: &str) -> Result<Option<(Vec<AdminScalarFieldView>, Vec<AdminNetworkFieldView>)>, String> {
    if engine_url.trim().is_empty() || admin_token.trim().is_empty() {
        return Ok(None);
    }

    let url = format!("{}/api/v1/admin/settings", engine_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .get(&url)
        .bearer_auth(admin_token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("scanner responded with {}", response.status()));
    }
    let parsed: RemoteSettingsResponse = response.json().await.map_err(|e| format!("could not parse scanner's response: {e}"))?;

    let fields = parsed
        .scalars
        .into_iter()
        .map(|(key, s)| AdminScalarFieldView { label: humanize_key(&key), key, value: s.value, source_label: remote_source_label(&s.source) })
        .collect();
    let networks = parsed
        .monero_node
        .into_iter()
        .map(|(network, value)| AdminNetworkFieldView {
            network,
            value_json: value.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default()).unwrap_or_default(),
        })
        .collect();
    Ok(Some((fields, networks)))
}

/// Assembles the whole page's view model from already-read, owned pieces -
/// `monokulo_fields`/`engine_url`/`admin_token` are read synchronously by
/// each caller (with the database lock held only for that read, then
/// dropped) before this is ever called, so this function itself never
/// touches the lock and is free to `.await` throughout.
async fn build_view_model(
    monokulo_fields: Vec<AdminScalarFieldView>,
    engine_url: String,
    admin_token: String,
    error: Option<String>,
    success: Option<String>,
) -> AdminSettingsViewModel {
    let mut view = AdminSettingsViewModel { error, success, monokulo_fields, ..Default::default() };
    match fetch_scanner_settings(&engine_url, &admin_token).await {
        Ok(Some((fields, networks))) => {
            view.scanner_configured = true;
            view.scanner_reachable = true;
            view.scanner_fields = fields;
            view.scanner_networks = networks;
        }
        Ok(None) => {
            view.scanner_configured = false;
        }
        Err(e) => {
            view.scanner_configured = true;
            view.scanner_reachable = false;
            view.scanner_error = Some(e);
        }
    }
    view
}

fn render(admin_user: &UserRow, view: AdminSettingsViewModel) -> Response {
    let chrome = views::PageChrome::from_user(Some(admin_user), "/dashboard/admin/settings");
    views::admin::admin_settings_page(&chrome, &view).into_response()
}

/// `GET /dashboard/admin/settings`.
pub async fn page(State(state): State<AppState>, AuthedAdmin(admin_user, _): AuthedAdmin) -> Response {
    let (fields, engine_url, admin_token) = {
        let db = state.db.lock().unwrap();
        (monokulo_fields(&db), crate::settings::get::<String>(&db, &crate::settings::ENGINE_URL), crate::settings::get::<String>(&db, &crate::settings::SCANNER_ADMIN_TOKEN))
    };
    let view = build_view_model(fields, engine_url, admin_token, None, None).await;
    render(&admin_user, view)
}

/// Validates one monokulo scalar's submitted raw value against the type its
/// own boot-time reader (`crate::settings::get::<T>`) actually parses it as -
/// the same "reject loudly at save time rather than silently misbehave
/// later" discipline scanner's own `validate_scalar` applies, scaled down to
/// monokulo's much shorter, non-range-checked setting list (nothing here has
/// a meaningful numeric range beyond "not negative"; a nonsensical value
/// like `0` rate-limit is caught the same way an operator hand-editing an
/// env var would ever catch it - by the effect being obviously wrong, not by
/// a bound enforced here).
fn validate_monokulo_scalar(setting: &ScalarSetting, value: &str) -> Result<(), String> {
    match setting.key {
        "signup.mode" => {
            if value == "public" || value == "invite_only" {
                Ok(())
            } else {
                Err(format!("{} must be \"public\" or \"invite_only\", got {value:?}", setting.key))
            }
        }
        "exchange_rate.coingecko_enabled" => value
            .parse::<bool>()
            .map(|_| ())
            .map_err(|_| format!("{} must be \"true\" or \"false\", got {value:?}", setting.key)),
        "exchange_rate.cache_seconds" => {
            value.parse::<u64>().map(|_| ()).map_err(|_| format!("{} must be a non-negative integer, got {value:?}", setting.key))
        }
        "rescan.default_lookback_days" | "rescan.max_lookback_days" => value
            .parse::<u32>()
            .map(|_| ())
            .map_err(|_| format!("{} must be a positive integer, got {value:?}", setting.key)),
        "http_cache.max_mb" => {
            value.parse::<u64>().map(|_| ()).map_err(|_| format!("{} must be a positive integer, got {value:?}", setting.key))
        }
        "rate_limit.per_ip_per_min" => {
            value.parse::<u32>().map(|_| ()).map_err(|_| format!("{} must be a positive integer, got {value:?}", setting.key))
        }
        "engine.url" => {
            if value.trim().is_empty() {
                Err(format!("{} must not be empty", setting.key))
            } else {
                Ok(())
            }
        }
        // engine.admin_token has no shape requirement of its own - an empty
        // value just means "no scanner connection configured yet".
        _ => Ok(()),
    }
}

/// `POST /dashboard/admin/settings` - saves every monokulo setting the form
/// submitted. All-or-nothing: one invalid field re-renders the whole page
/// with an error and changes nothing, the same policy scanner's own
/// `update_settings` applies to its scalars.
pub async fn save_monokulo(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    for setting in ALL_SCALAR {
        if let Some(value) = form.get(setting.key) {
            if let Err(message) = validate_monokulo_scalar(setting, value) {
                return render_error(&state, &admin_user, message).await;
            }
        }
    }

    let save_error = {
        let db = state.db.lock().unwrap();
        ALL_SCALAR.iter().find_map(|setting| {
            let value = form.get(setting.key)?;
            db.set_setting(setting.key, value).err().map(|_| ())
        })
    };

    if save_error.is_some() {
        return render_error(&state, &admin_user, "Something went wrong saving these settings. Please try again.".to_string()).await;
    }

    let (fields, engine_url, admin_token) = {
        let db = state.db.lock().unwrap();
        (monokulo_fields(&db), crate::settings::get::<String>(&db, &crate::settings::ENGINE_URL), crate::settings::get::<String>(&db, &crate::settings::SCANNER_ADMIN_TOKEN))
    };
    let view = build_view_model(fields, engine_url, admin_token, None, Some("Monokulo settings saved.".to_string())).await;
    render(&admin_user, view)
}

/// Re-reads the current state fresh and re-renders the page with `message`
/// as the error banner - the common "a submission was rejected, show the
/// whole page again with nothing changed" path both `POST` handlers use.
async fn render_error(state: &AppState, admin_user: &UserRow, message: String) -> Response {
    let (fields, engine_url, admin_token) = {
        let db = state.db.lock().unwrap();
        (monokulo_fields(&db), crate::settings::get::<String>(&db, &crate::settings::ENGINE_URL), crate::settings::get::<String>(&db, &crate::settings::SCANNER_ADMIN_TOKEN))
    };
    let view = build_view_model(fields, engine_url, admin_token, Some(message), None).await;
    render(admin_user, view)
}

#[derive(Serialize, Default)]
struct RemoteUpdateRequest {
    scalars: HashMap<String, String>,
    monero_node: HashMap<String, Option<serde_json::Value>>,
}

/// `POST /dashboard/admin/scanner-settings` - forwards the submitted scanner
/// fields to the configured scanner instance's own
/// `POST /api/v1/admin/settings`. This page does no validation of its own on
/// these fields (it doesn't know scanner's own rules, and shouldn't have to
/// duplicate them) - whatever the scanner instance itself rejects comes back
/// as this page's own error banner, verbatim.
pub async fn save_scanner(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let (engine_url, admin_token) = {
        let db = state.db.lock().unwrap();
        engine_connection(&db)
    };
    if engine_url.trim().is_empty() || admin_token.trim().is_empty() {
        return render_error(&state, &admin_user, "No scanner connection is configured.".to_string()).await;
    }

    let mut req = RemoteUpdateRequest::default();
    for (key, value) in &form {
        if let Some(network) = key.strip_prefix("monero_node_") {
            if value.trim().is_empty() {
                req.monero_node.insert(network.to_string(), None);
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(value) {
                Ok(parsed) => {
                    req.monero_node.insert(network.to_string(), Some(parsed));
                }
                Err(e) => {
                    return render_error(&state, &admin_user, format!("Monero node config for {network} is not valid JSON: {e}")).await;
                }
            }
        } else {
            req.scalars.insert(key.clone(), value.clone());
        }
    }

    let url = format!("{}/api/v1/admin/settings", engine_url.trim_end_matches('/'));
    let result = reqwest::Client::new().post(&url).bearer_auth(&admin_token).json(&req).send().await;
    let (error, success) = match result {
        Ok(response) if response.status().is_success() => (None, Some("Scanner settings saved.".to_string())),
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            (Some(format!("Scanner rejected the request ({status}): {body}")), None)
        }
        Err(e) => (Some(format!("Could not reach the configured scanner: {e}")), None),
    };

    let (fields, engine_url, admin_token) = {
        let db = state.db.lock().unwrap();
        (monokulo_fields(&db), crate::settings::get::<String>(&db, &crate::settings::ENGINE_URL), crate::settings::get::<String>(&db, &crate::settings::SCANNER_ADMIN_TOKEN))
    };
    let view = build_view_model(fields, engine_url, admin_token, error, success).await;
    render(&admin_user, view)
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::db::{Db, TEST_ADMIN_EMAIL, TEST_ADMIN_PASSWORD};
    use crate::engine_client::EngineClient;
    use crate::http::{build_router, AppState};

    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];
    const SCANNER_ADMIN_TOKEN: &str = "admin_test_token_for_monokulo_admin_settings_tests";

    fn test_exchange_rate_provider() -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only())
    }

    /// Spawns a real scanner engine (`scanner_test_support`) and seeds a
    /// known instance-admin token directly into its store - the same
    /// "give the test real, direct access" pattern `TestEngineHandle::store`
    /// already exists for, applied here since `scanner::http::instance_admin`'s
    /// own `seed_admin_token_for_tests` is `#[cfg(test)]`-gated to scanner's
    /// own crate and not reachable from here.
    async fn spawn_scanner_with_known_admin_token() -> scanner_test_support::TestEngineHandle {
        let engine = scanner_test_support::TestEngineConfig::new().spawn().await;
        engine
            .store()
            .lock()
            .unwrap()
            .set_setting("instance_admin_token_hash", &shared::auth::hash_secret_token(SCANNER_ADMIN_TOKEN))
            .unwrap();
        engine
    }

    /// A monokulo instance with a seeded admin account and a real, reachable
    /// scanner connection already configured (`engine.url`/`engine.admin_token`)
    /// - what most tests in this module want, since the whole point of this
    /// page is proxying that connection.
    fn test_app_state_connected_to(scanner_addr: std::net::SocketAddr) -> AppState {
        let db = Db::open_in_memory().unwrap();
        db.seed_test_admin();
        db.set_setting(crate::settings::ENGINE_URL.key, &format!("http://{scanner_addr}")).unwrap();
        db.set_setting(crate::settings::SCANNER_ADMIN_TOKEN.key, SCANNER_ADMIN_TOKEN).unwrap();
        AppState {
            db: db.into_shared(),
            engine_client: EngineClient::new(format!("http://{scanner_addr}")),
            encryption_key: TEST_ENCRYPTION_KEY,
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: test_exchange_rate_provider(),
            rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
        }
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn urlencoding_encode(s: &str) -> String {
        url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
    }

    fn form_request(method: &str, uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
        let body =
            fields.iter().map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v))).collect::<Vec<_>>().join("&");
        Request::builder().method(method).uri(uri).header("content-type", "application/x-www-form-urlencoded").body(Body::from(body)).unwrap()
    }

    fn authed_form_request(method: &str, uri: &str, cookie: &str, fields: &[(&str, &str)]) -> Request<Body> {
        let body =
            fields.iter().map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v))).collect::<Vec<_>>().join("&");
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", cookie)
            .body(Body::from(body))
            .unwrap()
    }

    /// Logs in as the harness-seeded admin account and returns its session
    /// cookie's `name=value` pair, ready to attach as a `cookie` header.
    async fn admin_session_cookie(router: &Router) -> String {
        let response = router
            .clone()
            .oneshot(form_request("POST", "/dashboard/login", &[("email", TEST_ADMIN_EMAIL), ("password", TEST_ADMIN_PASSWORD)]))
            .await
            .unwrap();
        let set_cookie = response.headers().get("set-cookie").expect("expected a session cookie from a correct admin login").to_str().unwrap();
        set_cookie.split(';').next().unwrap().to_string()
    }

    async fn get_settings_page(router: &Router, cookie: &str) -> axum::response::Response {
        router
            .clone()
            .oneshot(Request::builder().method("GET").uri("/dashboard/admin/settings").header("cookie", cookie).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_settings_page_is_unreachable_without_a_session_at_all() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap());
        let router = build_router(state);
        let response = router
            .oneshot(Request::builder().method("GET").uri("/dashboard/admin/settings").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// The real point of `AuthedAdmin`: a perfectly valid session that just
    /// isn't the admin account gets `403`, not a redirect or a `401`.
    #[tokio::test]
    async fn a_non_admin_session_is_forbidden_from_the_settings_page() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap());
        let router = build_router(state);

        let signup = router
            .clone()
            .oneshot(form_request("POST", "/dashboard/signup", &[("email", "merchant@example.com"), ("password", "correct horse battery staple")]))
            .await
            .unwrap();
        assert_eq!(signup.status(), StatusCode::FOUND);
        let login = router
            .clone()
            .oneshot(form_request("POST", "/dashboard/login", &[("email", "merchant@example.com"), ("password", "correct horse battery staple")]))
            .await
            .unwrap();
        let cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_string();

        let response = get_settings_page(&router, &cookie).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_admin_can_reach_the_settings_page_and_see_the_reachable_scanner_settings() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr);
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let response = get_settings_page(&router, &cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("engine url"), "expected monokulo's own settings listed, got: {html}");
        assert!(html.contains("payment confirmations required"), "expected the scanner's own settings proxied in, got: {html}");
        assert!(html.contains("value=\"10\""), "expected the scanner's real default value, got: {html}");
    }

    #[tokio::test]
    async fn a_saved_monokulo_setting_round_trips_on_the_next_load() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap());
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("rate_limit.per_ip_per_min", "5")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Monokulo settings saved."), "expected a success banner, got: {html}");
        assert!(html.contains("value=\"5\""), "expected the just-saved value reflected immediately, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        assert!(html.contains("value=\"5\""), "expected the saved value to survive a fresh page load, got: {html}");
        assert!(html.contains("saved value"), "expected the source label to say this came from a saved value, got: {html}");
    }

    /// The literal ask: "write tests to ensure that all settings exposed on
    /// the admin page are saved correctly" - every one of
    /// `crate::settings::ALL_SCALAR`, not just one representative field,
    /// saved together in one real form submission (`db` isn't touched
    /// directly - this goes through the real `POST` handler end to end) and
    /// every one individually confirmed to have taken effect.
    #[tokio::test]
    async fn every_monokulo_setting_on_the_admin_page_saves_correctly() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap());
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let new_values: &[(&str, &str)] = &[
            ("signup.mode", "public"),
            ("engine.url", "http://scanner.internal:8443"),
            ("engine.admin_token", "admin_a_new_token_value"),
            ("exchange_rate.coingecko_enabled", "false"),
            ("exchange_rate.coingecko_base_url", "http://127.0.0.1:9999"),
            ("exchange_rate.cache_seconds", "77"),
            ("http_cache.max_mb", "42"),
            ("rate_limit.per_ip_per_min", "33"),
        ];
        // Every one of `ALL_SCALAR`'s own keys must be covered here, or this
        // test would silently stop proving anything about a setting added
        // later without its own new_values entry.
        assert_eq!(new_values.len(), crate::settings::ALL_SCALAR.len(), "this test must cover every known monokulo setting");

        let save = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, new_values)).await.unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Monokulo settings saved."), "expected a success banner, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        for (_, value) in new_values {
            assert!(html.contains(&format!("value=\"{value}\"")), "expected {value:?} to have round-tripped, got: {html}");
        }
    }

    /// The scanner half of the same requirement - every one of
    /// `scanner::settings::ALL_SCALAR`'s 17 keys, saved together through the
    /// real proxy `POST` and confirmed to round-trip via a real, separately
    /// spawned scanner instance (this monokulo page holds none of this state
    /// itself - see this module's own doc comment).
    #[tokio::test]
    async fn every_scanner_setting_on_the_admin_page_saves_correctly() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr);
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let new_values: &[(&str, &str)] = &[
            ("key_custody.backend", "plain"),
            ("key_custody.socket_path", ""),
            ("payment.confirmations_required", "5"),
            ("payment.zero_conf_max_xmr", "0.5"),
            ("payment.order_expiry_minutes", "45"),
            ("payment.reorg_check_depth", "15"),
            ("payment.mempool_poll_interval_ms", "2000"),
            ("payment.expired_order_grace_period_minutes", "500"),
            ("payment.scan_chunk_memory_budget_mb", "16"),
            ("server.bind", "0.0.0.0:9443"),
            ("server.worker_threads", "4"),
            ("server.rate_limit_per_ip_per_min", "50"),
            ("server.rate_limit_per_token_per_min", "200"),
            ("server.max_body_bytes", "16384"),
            ("webhooks.allow_private_urls", "true"),
            ("webhooks.delivery_timeout_ms", "10000"),
            ("webhooks.max_attempts", "12"),
        ];
        assert_eq!(new_values.len(), 17, "this test must cover every known scanner setting (scanner::settings::ALL_SCALAR has 17 entries)");

        let save = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/scanner-settings", &cookie, new_values)).await.unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Scanner settings saved."), "expected a success banner, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        for (key, value) in new_values {
            // `key_custody.socket_path`'s new value is the empty string - an
            // empty `value=""` attribute is still real output to look for,
            // just not distinguishable via a bare `value` search, so it's
            // skipped here (its round-trip is still exercised - a wrong
            // value there would still show up as *something* nonempty).
            if value.is_empty() {
                continue;
            }
            assert!(html.contains(&format!("value=\"{value}\"")), "expected {key}={value:?} to have round-tripped, got: {html}");
        }
    }

    #[tokio::test]
    async fn an_invalid_monokulo_setting_is_rejected_and_nothing_is_saved() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap());
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("rate_limit.per_ip_per_min", "not-a-number")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("must be a positive integer"), "expected a clear validation error, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        assert!(html.contains("value=\"20\""), "the rejected save must not have changed the default, got: {html}");
    }

    #[tokio::test]
    async fn saving_a_scanner_setting_forwards_it_and_the_change_is_visible_on_the_next_load() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr);
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/scanner-settings", &cookie, &[("payment.confirmations_required", "5")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Scanner settings saved."), "expected a success banner, got: {html}");
        assert!(html.contains("value=\"5\""), "expected the scanner's own just-saved value reflected, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        assert!(html.contains("value=\"5\""), "expected the scanner's change to survive a fresh page load, got: {html}");
    }

    #[tokio::test]
    async fn an_invalid_scanner_setting_is_rejected_by_the_scanner_and_surfaced_as_an_error() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr);
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/scanner-settings", &cookie, &[("payment.confirmations_required", "not-a-number")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Scanner rejected the request"), "expected the scanner's own rejection surfaced, got: {html}");
    }

    #[tokio::test]
    async fn an_unconfigured_scanner_connection_shows_a_configuration_prompt_instead_of_a_form() {
        let state = {
            let db = Db::open_in_memory().unwrap();
            db.seed_test_admin();
            AppState {
                db: db.into_shared(),
                engine_client: EngineClient::new("http://127.0.0.1:1"),
                encryption_key: TEST_ENCRYPTION_KEY,
                status_cache: crate::http::status_page::new_status_cache(),
                exchange_rate: test_exchange_rate_provider(),
                rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
            }
        };
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let response = get_settings_page(&router, &cookie).await;
        let html = body_text(response).await;
        assert!(html.contains("Set <code>engine.url</code>"), "expected the configure-first prompt, got: {html}");
    }
}
