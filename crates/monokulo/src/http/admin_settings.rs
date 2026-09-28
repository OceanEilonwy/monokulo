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
//! Every field on both forms always carries its *current effective* value (secrets excepted: they are never echoed back, and an empty secret field keeps the current one)
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
use serde::Deserialize;

use crate::db::{Db, UserRow};
use crate::views;
use crate::views::admin::{AdminNetworkFieldView, AdminScalarFieldView, AdminSettingsViewModel, Notice, SettingKindView};

use super::{AppState, AuthedAdmin};

/// `"exchange_rate.coingecko_enabled"` -> `"exchange rate coingecko enabled"` -
/// a plain, mechanical label derived straight from a settings key so no
/// separate label table can ever drift out of sync with the declarations
/// (or, for the engine half, with whatever keys that instance reports).
fn humanize_key(key: &str) -> String {
    key.replace(['.', '_'], " ")
}

fn source_label(source: &str) -> String {
    match source {
        "env" => "environment variable".to_string(),
        "database" => "saved value".to_string(),
        "default" => "default".to_string(),
        other => other.to_string(),
    }
}

fn live_source(source: live_settings::SettingSource) -> &'static str {
    match source {
        live_settings::SettingSource::Env => "env",
        live_settings::SettingSource::Database => "database",
        live_settings::SettingSource::Default => "default",
    }
}

/// Every monokulo setting as the page shows it, from the registry's
/// description of them (tasks 4.1, 4.6). Without a registry (a test state),
/// nothing is listed.
fn monokulo_fields(state: &AppState) -> Vec<AdminScalarFieldView> {
    let Some(registry) = state.settings.registry.as_ref() else { return Vec::new() };
    registry
        .describe()
        .into_iter()
        .map(|view| AdminScalarFieldView {
            key: view.key.to_string(),
            label: humanize_key(view.key),
            value: view.value,
            source_label: source_label(live_source(view.source)),
            help: Some(view.description.to_string()),
            kind: SettingKindView::from(view.kind),
            example: view.example.map(str::to_string),
            restart_only: view.applies == live_settings::Applies::Restart,
            pending_restart: view.pending_restart,
            problem: view.problem.map(|p| p.message),
        })
        .collect()
}

/// A request to the engine carrying this request's trace
/// (structured_logging.md 2.3), like every `EngineClient` call.
fn traced(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match telemetry::trace::current_traceparent() {
        Some(traceparent) => request.header(telemetry::trace::TRACEPARENT, traceparent),
        None => request,
    }
}

/// This instance's engine connection, read synchronously with the lock
/// held - the two owned `String`s are then free to travel across an
/// `.await` on their own.
fn engine_connection(db: &Db) -> (String, String) {
    (
        crate::settings::get(db, &crate::settings::ENGINE_URL).as_str().to_string(),
        crate::settings::get(db, &crate::settings::SCANNER_ADMIN_TOKEN).expose().to_string(),
    )
}

#[derive(Deserialize)]
struct RemoteScalarSetting {
    value: String,
    source: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    kind: SettingKindView,
    #[serde(default)]
    example: Option<String>,
    #[serde(default)]
    applies: Option<String>,
    #[serde(default)]
    pending_restart: bool,
    #[serde(default)]
    problem: Option<String>,
}

#[derive(Deserialize, Default)]
struct RemoteNetwork {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    example: Option<String>,
    #[serde(default)]
    tenant_count: u64,
}

#[derive(Deserialize)]
struct RemoteSettingsResponse {
    scalars: BTreeMap<String, RemoteScalarSetting>,
    monero_node: BTreeMap<String, Option<serde_json::Value>>,
    /// Added with the engine's setting descriptions (task 4.2); an older
    /// engine doesn't send it.
    #[serde(default)]
    networks: BTreeMap<String, RemoteNetwork>,
}

/// Fetches the engine's own settings over HTTP - `Ok(None)` when no engine
/// connection is configured at all, `Err` for a real reachability, auth or
/// parse failure worth showing. Takes owned strings, never a `&Db`, since it
/// awaits.
async fn fetch_scanner_settings(
    engine_url: &str,
    admin_token: &str,
) -> Result<Option<(Vec<AdminScalarFieldView>, Vec<AdminNetworkFieldView>)>, String> {
    if engine_url.trim().is_empty() || admin_token.trim().is_empty() {
        return Ok(None);
    }

    let url = format!("{}/api/v1/admin/settings", engine_url.trim_end_matches('/'));
    let response = traced(reqwest::Client::new().get(&url))
        .bearer_auth(admin_token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("the engine responded with {}", response.status()));
    }
    let parsed: RemoteSettingsResponse = response.json().await.map_err(|e| format!("could not parse the engine's response: {e}"))?;

    let fields = parsed
        .scalars
        .into_iter()
        .map(|(key, s)| AdminScalarFieldView {
            label: humanize_key(&key),
            key,
            value: s.value,
            source_label: source_label(&s.source),
            help: s.description,
            kind: s.kind,
            example: s.example,
            restart_only: s.applies.as_deref() == Some("restart"),
            pending_restart: s.pending_restart,
            problem: s.problem,
        })
        .collect();
    let mut networks_meta = parsed.networks;
    let networks = parsed
        .monero_node
        .into_iter()
        .map(|(network, value)| {
            let meta = networks_meta.remove(&network).unwrap_or_default();
            AdminNetworkFieldView {
                value_json: value.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default()).unwrap_or_default(),
                network,
                description: meta.description,
                example: meta.example,
                tenant_count: meta.tenant_count,
            }
        })
        .collect();
    Ok(Some((fields, networks)))
}

/// Assembles the whole page's view model: monokulo's fields from its
/// registry, the engine's fetched over HTTP.
/// Settings that hold a Unix time until which something stays on get the
/// "off / on for N hours" control instead of a number box.
fn with_time_limits(fields: &mut [AdminScalarFieldView]) {
    let now = u64::try_from(crate::now_unix()).unwrap_or(0);
    for field in fields.iter_mut().filter(|f| f.key == "logging.dev_mode_until") {
        field.kind = SettingKindView::TimeLimit { now };
    }
}

async fn build_view_model(state: &AppState, error: Option<String>, success: Option<String>, notices: Vec<Notice>) -> AdminSettingsViewModel {
    let mut monokulo_fields = monokulo_fields(state);
    with_time_limits(&mut monokulo_fields);
    let (engine_url, admin_token) = engine_connection(&state.db.lock());
    let mut view = AdminSettingsViewModel { error, success, notices, monokulo_fields, ..Default::default() };
    match fetch_scanner_settings(&engine_url, &admin_token).await {
        Ok(Some((mut fields, networks))) => {
            with_time_limits(&mut fields);
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

fn render(state: &AppState, admin_user: &UserRow, view: AdminSettingsViewModel) -> Response {
    let chrome = super::page_chrome(state, Some(admin_user), "/dashboard/admin/settings");
    views::admin::admin_settings_page(&chrome, &view).into_response()
}

/// `GET /dashboard/admin/settings`.
pub async fn page(State(state): State<AppState>, AuthedAdmin(admin_user, _): AuthedAdmin) -> Response {
    let view = build_view_model(&state, None, None, Vec::new()).await;
    render(&state, &admin_user, view)
}

/// `POST /dashboard/admin/settings` - saves every monokulo setting the form
/// submitted, through the registry (admin_settings_v2.md part 1): all
/// checked first, then applied to the running process and stored together,
/// or nothing at all. A secret field left empty keeps its current value.
pub async fn save_monokulo(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(registry) = state.settings.registry.as_ref() else {
        return render_error(&state, &admin_user, "Settings can't be saved on this instance.".to_string()).await;
    };
    // A new value and "Clear it" together can't both be meant.
    if let Some(key) = crate::settings::ALL.iter().map(|s| s.key()).find(|key| {
        form.contains_key(&format!("clear:{key}")) && form.get(*key).is_some_and(|value| !value.is_empty())
    }) {
        return render_error(&state, &admin_user, format!("{key}: either type a new value or tick \"Clear it\", not both.")).await;
    }
    let secrets: Vec<&str> = crate::settings::ALL
        .iter()
        .filter(|s| matches!(s.kind(), live_settings::SettingKind::Secret))
        .map(|s| s.key())
        .collect();
    let changes: live_settings::Changes = crate::settings::ALL
        .iter()
        .filter_map(|setting| {
            // A secret's field is always empty on the page, so empty means
            // "keep it"; its "Clear it" box removes it.
            if secrets.contains(&setting.key()) && form.contains_key(&format!("clear:{}", setting.key())) {
                return Some((setting.key().to_string(), None));
            }
            let value = form.get(setting.key())?;
            if secrets.contains(&setting.key()) && value.is_empty() {
                return None;
            }
            Some((setting.key().to_string(), Some(value.clone())))
        })
        .collect();

    match registry.save(changes).await {
        Ok(report) => {
            let mut notices = Vec::new();
            for warning in &report.warnings {
                // The only monokulo warning today: the engine didn't answer
                // at the saved URL (decision D4). It's an error-level banner.
                notices.push(Notice::Error(warning.message.clone()));
            }
            if !report.env_overridden.is_empty() {
                notices.push(Notice::Info(format!(
                    "Saved, but these are set by an environment variable, which wins while it is set: {}.",
                    report.env_overridden.join(", ")
                )));
            }
            if !report.restart_required.is_empty() {
                notices.push(Notice::Warning(format!(
                    "Saved. These take effect after monokulo restarts: {}.",
                    report.restart_required.join(", ")
                )));
            }
            let view = build_view_model(&state, None, Some("Monokulo settings saved and applied.".to_string()), notices).await;
            render(&state, &admin_user, view)
        }
        Err(live_settings::SaveError::Invalid(errors)) => {
            let message = errors.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ");
            render_error(&state, &admin_user, message).await
        }
        Err(e) => {
            tracing::error!(error = %e, "saving monokulo settings failed");
            render_error(&state, &admin_user, "Something went wrong saving these settings. Please try again.".to_string()).await
        }
    }
}

/// Re-reads the current state fresh and re-renders the page with `message`
/// as the error banner - the common "a submission was rejected, show the
/// whole page again with nothing changed" path both `POST` handlers use.
async fn render_error(state: &AppState, admin_user: &UserRow, message: String) -> Response {
    let view = build_view_model(state, Some(message), None, Vec::new()).await;
    render(state, admin_user, view)
}

#[derive(serde::Serialize, Default)]
struct RemoteUpdateRequest {
    scalars: HashMap<String, String>,
    monero_node: HashMap<String, Option<serde_json::Value>>,
}

#[derive(Deserialize, Default)]
struct RemoteSaveWarnings {
    #[serde(default)]
    restart_required: Vec<String>,
    #[serde(default)]
    env_overridden: Vec<String>,
    #[serde(default)]
    messages: Vec<RemoteMessage>,
    #[serde(default)]
    unserved_networks: Vec<RemoteUnserved>,
}

#[derive(Deserialize)]
struct RemoteMessage {
    message: String,
}

#[derive(Deserialize)]
struct RemoteUnserved {
    network: String,
    tenants: u64,
}

#[derive(Deserialize, Default)]
struct RemoteSaveResponse {
    #[serde(default)]
    warnings: RemoteSaveWarnings,
}

/// The banners for an accepted engine save (tasks 3.6, 4.5, decisions D1,
/// D2, D8): restart-only settings, networks stores use that no longer have
/// a node, environment overrides and anything else the engine said.
fn scanner_save_notices(warnings: RemoteSaveWarnings, submitted_bind: Option<&str>) -> Vec<Notice> {
    let mut notices = Vec::new();
    for unserved in &warnings.unserved_networks {
        let stores = if unserved.tenants == 1 { "1 store uses".to_string() } else { format!("{} stores use", unserved.tenants) };
        notices.push(Notice::Error(format!(
            "{stores} the {} network, which no longer has any reachable nodes. Their payments won't be detected until a node is set.",
            unserved.network
        )));
    }
    if !warnings.restart_required.is_empty() {
        let mut text = format!(
            "Saved. These settings take effect after the engine restarts: {}.",
            warnings.restart_required.join(", ")
        );
        if warnings.restart_required.iter().any(|k| k == "server.bind") {
            if let Some(bind) = submitted_bind {
                text.push_str(&format!(
                    " After restarting it, set monokulo's engine.url to http://{bind} so monokulo can reach it."
                ));
            }
        }
        notices.push(Notice::Warning(text));
    }
    for message in warnings.messages {
        notices.push(Notice::Warning(message.message));
    }
    if !warnings.env_overridden.is_empty() {
        notices.push(Notice::Info(format!(
            "Saved, but these are set by an environment variable on the engine, which wins while it is set: {}.",
            warnings.env_overridden.join(", ")
        )));
    }
    notices
}

/// `POST /dashboard/admin/scanner-settings` - forwards the submitted engine
/// fields to the engine's own `POST /api/v1/admin/settings`, which checks
/// them. Whatever the engine refuses comes back as this page's error
/// banner, verbatim; what it accepts comes back with its warnings as
/// banners.
pub async fn save_scanner(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let (engine_url, admin_token) = engine_connection(&state.db.lock());
    if engine_url.trim().is_empty() || admin_token.trim().is_empty() {
        return render_error(&state, &admin_user, "No engine connection is configured.".to_string()).await;
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
    let result = traced(reqwest::Client::new().post(&url)).bearer_auth(&admin_token).json(&req).send().await;
    let (error, success, notices) = match result {
        Ok(response) if response.status().is_success() => {
            let saved: RemoteSaveResponse = response.json().await.unwrap_or_default();
            let notices = scanner_save_notices(saved.warnings, form.get("server.bind").map(String::as_str));
            (None, Some("Engine settings saved and applied.".to_string()), notices)
        }
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or(body);
            (Some(format!("The engine refused the change ({status}): {message}")), None, Vec::new())
        }
        Err(e) => (Some(format!("Could not reach the configured engine: {e}")), None, Vec::new()),
    };

    let view = build_view_model(&state, error, success, notices).await;
    render(&state, &admin_user, view)
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
            .set_setting("instance_admin_token_hash", &shared::auth::hash_secret_token(SCANNER_ADMIN_TOKEN))
            .unwrap();
        engine
    }

    /// A monokulo instance with a seeded admin account and a real, reachable
    /// scanner connection already configured (`engine.url`/`engine.admin_token`)
    /// - what most tests in this module want, since the whole point of this
    /// page is proxying that connection.
    async fn test_app_state_connected_to(scanner_addr: std::net::SocketAddr) -> AppState {
        let db = Db::open_in_memory().unwrap();
        db.seed_test_admin();
        db.set_setting(crate::settings::ENGINE_URL.key, &format!("http://{scanner_addr}")).unwrap();
        db.set_setting(crate::settings::SCANNER_ADMIN_TOKEN.key, SCANNER_ADMIN_TOKEN).unwrap();
        let db = db.into_shared();
        let engine_client = EngineClient::new(format!("http://{scanner_addr}"));
        let exchange_rate = test_exchange_rate_provider();
        let abuse: std::sync::Arc<crate::abuse::AbuseProtection> = Default::default();
        let settings = crate::settings::MonokuloSettings::load(
            db.clone(),
            engine_client.clone(),
            exchange_rate.clone(),
            abuse.clone(),
            None,
            live_settings::Env::fixed(Vec::<(String, String)>::new()),
        )
        .await
        .unwrap();
        AppState {
            db,
            engine_client,
            encryption_key: TEST_ENCRYPTION_KEY,
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate,
            abuse,
            dns: std::sync::Arc::new(crate::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
            settings,
            log_store: None,
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
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
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
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
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
        let state = test_app_state_connected_to(engine.addr).await;
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
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("abuse.soft_per_min", "5")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Monokulo settings saved and applied."), "expected a success banner, got: {html}");
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
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
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
            ("abuse.soft_per_min", "33"),
            ("abuse.hard_per_min", "330"),
            ("abuse.signed_in_per_min", "700"),
            ("abuse.challenge_bits", "18"),
            ("abuse.under_attack", "true"),
            ("rate_limit.per_store_key_per_min", "444"),
            ("public_url", "https://pay.example.com"),
            ("abuse.trusted_proxies", "127.0.0.1, 10.0.0.0/8"),
            ("abuse.onion_listener", "127.0.0.1:8082"),
            ("abuse.stream_cap", "9"),
            ("logging.level", "warn,monokulo::http=debug"),
            ("logging.dev_mode_until", "4102444800"),
            ("logging.retention_days", "30"),
            ("logging.max_mb", "250"),
        ];
        // Every monokulo setting must be covered here, or this test would
        // silently stop proving anything about a setting added later.
        assert_eq!(new_values.len(), crate::settings::ALL.len(), "this test must cover every known monokulo setting");

        let save = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, new_values)).await.unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Monokulo settings saved and applied."), "expected a success banner, got: {html}");
        assert!(html.contains("didn&#39;t answer") || html.contains("didn't answer"), "the new engine URL doesn't answer, and the page says so (D4): {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        for (key, value) in new_values {
            if *key == "engine.admin_token" {
                assert!(!html.contains(value), "a secret is never echoed back");
                continue;
            }
            assert!(shows_value(&html, value), "expected {key}={value:?} to have round-tripped, got: {html}");
        }
    }

    /// The scanner half of the same requirement - every one of
    /// `scanner::engine_settings::ALL`'s keys, saved together through the
    /// real proxy `POST` and confirmed to round-trip via a real, separately
    /// spawned scanner instance (this monokulo page holds none of this state
    /// itself - see this module's own doc comment).
    #[tokio::test]
    async fn every_scanner_setting_on_the_admin_page_saves_correctly() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let new_values: &[(&str, &str)] = &[
            ("key_custody.enabled_backends", "plain"),
            ("key_custody.default_backend", "plain"),
            ("key_custody.socket_path", ""),
            ("payment.confirmations_required", "5"),
            ("payment.order_expiry_minutes", "45"),
            ("payment.reorg_check_depth", "15"),
            ("payment.mempool_poll_interval_ms", "2000"),
            ("payment.expired_order_grace_period_minutes", "500"),
            ("payment.scan_chunk_memory_budget_mb", "16"),
            ("server.bind", "127.0.0.1:9443"),
            ("server.worker_threads", "4"),
            ("server.rate_limit_per_token_per_min", "200"),
            ("server.max_body_bytes", "16384"),
            ("webhooks.allow_private_urls", "true"),
            ("webhooks.delivery_timeout_ms", "10000"),
            ("webhooks.max_attempts", "12"),
            ("logging.level", "warn,scanner::loops=debug"),
            ("logging.dev_mode_until", "4102444800"),
            ("logging.retention_days", "30"),
            ("logging.max_mb", "250"),
        ];
        assert_eq!(
            new_values.len(),
            scanner::engine_settings::ALL.len() - scanner::engine_settings::NETWORKS.len(),
            "this test must cover every known engine setting apart from the node ones"
        );

        let save = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/scanner-settings", &cookie, new_values)).await.unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Engine settings saved and applied."), "expected a success banner, got: {html}");
        assert!(html.contains("take effect after the engine restarts"), "worker threads and bind are restart-only: {html}");
        assert!(html.contains("set monokulo&#39;s engine.url to http://127.0.0.1:9443") || html.contains("set monokulo's engine.url to http://127.0.0.1:9443"), "{html}");

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
            assert!(shows_value(&html, value), "expected {key}={value:?} to have round-tripped, got: {html}");
        }
    }

    /// A value shown in a text or number input, or selected in a select.
    fn shows_value(html: &str, value: &str) -> bool {
        html.contains(&format!("value=\"{value}\" selected")) || html.contains(&format!("value=\"{value}\">"))
            || html.contains(&format!("value=\"{value}\" min")) || html.contains(&format!("value=\"{value}\";"))
            || html.contains(&format!("\">{value}</textarea>"))
            || html.contains(&format!("value=\"{value}\"></label>"))
    }

    // -- Task 3.5: settings that were already live stay live --------------

    async fn get(router: &Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().method("GET").uri(uri);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        router.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap()
    }

    #[tokio::test]
    async fn a_saved_signup_mode_applies_to_the_next_signup() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let signup = |email: &'static str| {
            Request::builder()
                .method("POST")
                .uri("/signup")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({ "email": email, "password": "correct horse battery staple" }).to_string()))
                .unwrap()
        };
        // The test instance starts with public signup (`Db::seed_test_admin`).
        assert_eq!(router.clone().oneshot(signup("first@example.com")).await.unwrap().status(), StatusCode::CREATED);

        let saved =
            router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("signup.mode", "invite_only")])).await.unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        assert_ne!(router.clone().oneshot(signup("second@example.com")).await.unwrap().status(), StatusCode::CREATED, "needs an invite now");

        router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("signup.mode", "public")])).await.unwrap();
        assert_eq!(router.clone().oneshot(signup("third@example.com")).await.unwrap().status(), StatusCode::CREATED, "and back");
    }

    #[tokio::test]
    async fn a_saved_public_url_applies_to_the_next_plugin_connection() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let confirm = "/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fdone&nonce=n1";
        let before = body_text(get(&router, confirm, Some(&cookie)).await).await;
        assert!(before.contains("can't connect plugins yet") || before.contains("can&#39;t connect plugins yet"), "{before}");

        let saved = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("public_url", "https://pay.example.com")]))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        let after = body_text(get(&router, confirm, Some(&cookie)).await).await;
        assert!(!after.contains("connect plugins yet"), "the next request sees it: {after}");
        assert!(after.contains(r#"name="view_key_hex""#), "{after}");
    }

    #[tokio::test]
    async fn a_saved_engine_admin_token_is_what_the_next_page_uses() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        assert!(body_text(get_settings_page(&router, &cookie).await).await.contains("Save engine settings"), "the right token");

        router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(crate::settings::SCANNER_ADMIN_TOKEN.key, "wrong-token")]))
            .await
            .unwrap();
        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(page.contains("Could not reach the configured engine"), "the wrong one, used at once: {page}");

        router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(crate::settings::SCANNER_ADMIN_TOKEN.key, SCANNER_ADMIN_TOKEN)]))
            .await
            .unwrap();
        assert!(body_text(get_settings_page(&router, &cookie).await).await.contains("Save engine settings"), "and back");
    }

    #[tokio::test]
    async fn the_engine_admin_token_is_kept_when_left_empty_and_removed_when_cleared() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let key = crate::settings::SCANNER_ADMIN_TOKEN.key;

        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(page.contains(&format!(r#"name="clear:{key}""#)), "a set secret can be cleared: {page}");

        let kept = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(key, "")])).await.unwrap();
        assert_eq!(kept.status(), StatusCode::OK);
        assert_eq!(db.lock().get_setting(key).unwrap().as_deref(), Some(SCANNER_ADMIN_TOKEN), "empty keeps it");

        let clear = format!("clear:{key}");
        let cleared = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(key, ""), (clear.as_str(), "on")]))
            .await
            .unwrap();
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(db.lock().get_setting(key).unwrap(), None);
        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(!page.contains(&format!(r#"name="clear:{key}""#)), "nothing left to clear");

        db.lock().set_setting(key, "some-token").unwrap();
        let both = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(key, "new-token"), (clear.as_str(), "on")]))
            .await
            .unwrap();
        assert!(body_text(both).await.contains("not both"));
        assert_eq!(db.lock().get_setting(key).unwrap().as_deref(), Some("some-token"), "nothing changed");
    }

    #[tokio::test]
    async fn an_invalid_monokulo_setting_is_rejected_and_nothing_is_saved() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("abuse.soft_per_min", "not-a-number")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("abuse.soft_per_min: Enter a whole number"), "expected a clear validation error, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        assert!(html.contains("value=\"60\""), "the rejected save must not have changed the default, got: {html}");
    }

    /// The mistakes an operator makes typing into the settings form: each
    /// is refused with a message naming the setting and what it needs.
    #[tokio::test]
    async fn an_operators_typical_mistakes_are_each_refused_with_what_the_setting_needs() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        for (key, value, expected) in [
            ("signup.mode", "open", "Choose one of: public, invite_only"),
            ("exchange_rate.coingecko_enabled", "yes", "Enter true or false."),
            ("abuse.under_attack", "on", "Enter true or false."),
            ("exchange_rate.cache_seconds", "-5", "Enter a whole number, 0 or more."),
            ("http_cache.max_mb", "1.5", "Enter a whole number"),
            ("abuse.hard_per_min", "0", "Enter a whole number from 1 to 10000000."),
            ("abuse.challenge_bits", "30", "Enter a whole number from 8 to 24."),
            ("rate_limit.per_store_key_per_min", "0", "Enter a whole number from 1 to 10000000."),
            ("abuse.stream_cap", "0", "Enter a whole number from 1 to 100000."),
            ("engine.url", " ", "Enter a full web address"),
            ("public_url", "not a url", "Enter this instance's public address"),
        ] {
            let save = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(key, value)])).await.unwrap();
            assert_eq!(save.status(), StatusCode::OK, "{key}={value:?}");
            let html = body_text(save).await.replace("&quot;", "\"").replace("&#34;", "\"").replace("&#39;", "'");
            assert!(html.contains(key) && html.contains(expected), "{key}={value:?}: expected {expected:?}, got: {}", html.split("role=\"alert\">").nth(1).unwrap_or("").split("<").next().unwrap_or(""));
        }
    }

    #[tokio::test]
    async fn saving_a_scanner_setting_forwards_it_and_the_change_is_visible_on_the_next_load() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/scanner-settings", &cookie, &[("payment.confirmations_required", "5")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("Engine settings saved and applied."), "expected a success banner, got: {html}");
        assert!(html.contains("value=\"5\""), "expected the scanner's own just-saved value reflected, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        assert!(html.contains("value=\"5\""), "expected the scanner's change to survive a fresh page load, got: {html}");
    }

    #[tokio::test]
    async fn an_invalid_scanner_setting_is_rejected_by_the_scanner_and_surfaced_as_an_error() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/scanner-settings", &cookie, &[("payment.confirmations_required", "not-a-number")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("The engine refused the change"), "expected the engine's own rejection surfaced, got: {html}");
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
                abuse: Default::default(),
                dns: std::sync::Arc::new(crate::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
                settings: crate::settings::MonokuloSettings::defaults(),
                log_store: None,
            }
        };
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let response = get_settings_page(&router, &cookie).await;
        let html = body_text(response).await;
        assert!(html.contains("Set <code>engine.url</code>"), "expected the configure-first prompt, got: {html}");
    }
}
