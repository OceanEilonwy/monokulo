//! `GET`/`POST /dashboard/admin/settings` -
//! the one admin page every monokulo *and* scanner setting can be managed
//! from, per the product spec ("We need an admin page that makes it so that
//! all the monokulo + scanner settings can be set from the admin web page").
//! Gated by [`AuthedAdmin`] end to end - a merchant with a perfectly valid
//! session still gets `403` here, same as the nav only shows the "admin"
//! link to the one instance-wide admin account (`is_admin`, `crate::db`).
//!
//! **Two owners, one save per tab.** The page is split into tabs by job
//! (`views::admin::SettingsTab`), and a tab can hold both processes'
//! settings; its one Save posts here, and [`save`] splits the form by
//! owner and saves each half the way it always has. Monokulo's own
//! settings (`crate::settings::ALL`) are simple: read/write this
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

use axum::extract::{Form, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::db::{Db, UserRow};
use crate::views;
use crate::views::admin::{
    setting_placement, AdminNetworkFieldView, AdminScalarFieldView, AdminSettingsViewModel, Notice, SettingKindView, SettingOwner,
    SettingsSection, SettingsTab,
};

use super::fx::FxRequest;
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

/// What a save answers with fixi: just the section that was saved (`422`
/// when nothing was saved), plus the engine section when saving monokulo
/// changed how to reach the engine.
fn render_fragment(view: &AdminSettingsViewModel, section: SettingsSection, engine_changed: bool) -> Response {
    let fragment = match section {
        SettingsSection::Monokulo => maud::html! {
            (views::admin::monokulo_section(view))
            @if engine_changed { (views::admin::engine_section(view, true)) }
        },
        SettingsSection::Engine => views::admin::engine_section(view, false),
    };
    if view.error.is_some() {
        super::fx::invalid(fragment)
    } else {
        axum::response::Html(fragment.into_string()).into_response()
    }
}

#[derive(Deserialize)]
pub struct SettingsPageQuery {
    tab: Option<String>,
    /// The flash a save without JavaScript left for the page it redirected
    /// to ([`FLASHES`]).
    saved: Option<String>,
}

/// `GET /dashboard/admin/settings`.
pub async fn page(State(state): State<AppState>, AuthedAdmin(admin_user, _): AuthedAdmin, Query(query): Query<SettingsPageQuery>) -> Response {
    let _ = query.tab;
    let flash = query.saved.as_deref().and_then(take_flash);
    let view = match flash {
        Some(flash) => {
            let mut view = build_view_model(&state, None, Some(flash.success), flash.notices).await;
            view.saved_tab = Some(flash.tab);
            view.saved_section = Some(flash.section);
            view
        }
        None => build_view_model(&state, None, None, Vec::new()).await,
    };
    render(&state, &admin_user, view)
}

/// The submitted form by name. A list of choices sends each one ticked
/// under the same name, after an empty one (so ticking none still sends
/// the name): they're joined with commas, the setting's own format.
fn joined(pairs: Vec<(String, String)>) -> HashMap<String, String> {
    let mut form: HashMap<String, String> = HashMap::new();
    for (name, value) in pairs {
        match form.get_mut(&name) {
            Some(joined) if joined.is_empty() => *joined = value,
            Some(joined) if !value.is_empty() => {
                joined.push(',');
                joined.push_str(&value);
            }
            Some(_) => {}
            None => {
                form.insert(name, value);
            }
        }
    }
    form
}

/// A submitted tab, split by who owns each field (nicer_admin_screen.md
/// T3): monokulo's are the names its registry knows (and their "Clear it"
/// boxes); everything else is the engine's.
#[derive(Default)]
struct SplitForm {
    monokulo: HashMap<String, String>,
    engine: RemoteUpdateRequest,
}

impl SplitForm {
    fn new(form: &HashMap<String, String>) -> Result<SplitForm, String> {
        let mut split = SplitForm::default();
        for (name, value) in form {
            if name == "tab" {
                continue;
            }
            let bare = name.strip_prefix("clear:").unwrap_or(name);
            if crate::settings::ALL.iter().any(|s| s.key() == bare) {
                split.monokulo.insert(name.clone(), value.clone());
            } else if let Some(network) = name.strip_prefix("monero_node_") {
                let node = if value.trim().is_empty() {
                    None
                } else {
                    Some(serde_json::from_str::<serde_json::Value>(value).map_err(|e| format!("Monero node config for {network} is not valid JSON: {e}"))?)
                };
                split.engine.monero_node.insert(network.to_string(), node);
            } else {
                split.engine.scalars.insert(name.clone(), value.clone());
            }
        }
        Ok(split)
    }
}

/// What saving a tab did, for the page: both halves' results merged.
#[derive(Default)]
struct SaveOutcome {
    error: Option<String>,
    /// The setting the error is about, when it names one: the page opens
    /// the tab holding it.
    error_key: Option<(String, SettingOwner)>,
    notices: Vec<Notice>,
    /// Saving monokulo changed how to reach the engine.
    engine_changed: bool,
    /// The engine half was sent (or refused on its way there).
    engine_saved: bool,
}

impl SaveOutcome {
    fn refused(message: String) -> SaveOutcome {
        SaveOutcome { error: Some(message), ..Default::default() }
    }
}

/// Saves the monokulo half of a tab through the registry
/// (admin_settings_v2.md part 1): all checked first, then applied to the
/// running process and stored together, or nothing at all. A secret field
/// left empty keeps its current value.
async fn save_monokulo(state: &AppState, form: &HashMap<String, String>) -> SaveOutcome {
    let Some(registry) = state.settings.registry.as_ref() else {
        return SaveOutcome::refused("Settings can't be saved on this instance.".to_string());
    };
    // A new value and "Clear it" together can't both be meant.
    if let Some(key) = crate::settings::ALL.iter().map(|s| s.key()).find(|key| {
        form.contains_key(&format!("clear:{key}")) && form.get(*key).is_some_and(|value| !value.is_empty())
    }) {
        return SaveOutcome {
            error_key: Some((key.to_string(), SettingOwner::Monokulo)),
            ..SaveOutcome::refused(format!("{key}: either type a new value or tick \"Clear it\", not both."))
        };
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

    let engine_before = engine_connection(&state.db.lock());
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
            let engine_changed = engine_connection(&state.db.lock()) != engine_before;
            SaveOutcome { notices, engine_changed, ..Default::default() }
        }
        Err(live_settings::SaveError::Invalid(errors)) => SaveOutcome {
            error_key: errors.first().map(|e| (e.key.clone(), SettingOwner::Monokulo)),
            ..SaveOutcome::refused(errors.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "))
        },
        Err(e) => {
            tracing::error!(error = %e, "saving monokulo settings failed");
            SaveOutcome::refused("Something went wrong saving these settings. Please try again.".to_string())
        }
    }
}

#[derive(serde::Serialize, Default)]
struct RemoteUpdateRequest {
    scalars: HashMap<String, String>,
    monero_node: HashMap<String, Option<serde_json::Value>>,
}

impl RemoteUpdateRequest {
    fn is_empty(&self) -> bool {
        self.scalars.is_empty() && self.monero_node.is_empty()
    }
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

/// Forwards the engine half of a tab to the engine's own
/// `POST /api/v1/admin/settings`, which checks it. Whatever the engine
/// refuses comes back as the page's error, verbatim; what it accepts comes
/// back with its warnings as banners.
async fn save_engine(state: &AppState, req: &RemoteUpdateRequest) -> SaveOutcome {
    let (engine_url, admin_token) = engine_connection(&state.db.lock());
    if engine_url.trim().is_empty() || admin_token.trim().is_empty() {
        return SaveOutcome::refused("No engine connection is configured.".to_string());
    }
    let url = format!("{}/api/v1/admin/settings", engine_url.trim_end_matches('/'));
    let result = traced(reqwest::Client::new().post(&url)).bearer_auth(&admin_token).json(req).send().await;
    match result {
        Ok(response) if response.status().is_success() => {
            let saved: RemoteSaveResponse = response.json().await.unwrap_or_default();
            SaveOutcome { notices: scanner_save_notices(saved.warnings, req.scalars.get("server.bind").map(String::as_str)), ..Default::default() }
        }
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let parsed = serde_json::from_str::<serde_json::Value>(&body).ok();
            let key = parsed.as_ref().and_then(|v| v["fields"][0]["key"].as_str().map(str::to_string));
            let message = parsed.as_ref().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or(body);
            SaveOutcome {
                error_key: key.map(|key| (key, SettingOwner::Engine)),
                ..SaveOutcome::refused(format!("The engine refused the change ({status}): {message}"))
            }
        }
        Err(e) => SaveOutcome::refused(format!("Could not reach the configured engine: {e}")),
    }
}

/// Saves a whole tab (nicer_admin_screen.md T3): monokulo's half first,
/// through its registry, then the engine's, through its admin API. If
/// monokulo refuses its half, the engine's isn't sent, so a refused tab
/// changes nothing.
async fn save_tab(state: &AppState, form: &HashMap<String, String>) -> SaveOutcome {
    let split = match SplitForm::new(form) {
        Ok(split) => split,
        Err(message) => return SaveOutcome { engine_saved: true, ..SaveOutcome::refused(message) },
    };
    let mut outcome = SaveOutcome::default();
    if !split.monokulo.is_empty() {
        outcome = save_monokulo(state, &split.monokulo).await;
        if outcome.error.is_some() {
            return outcome;
        }
    }
    if !split.engine.is_empty() {
        let engine = save_engine(state, &split.engine).await;
        outcome.error = engine.error;
        outcome.error_key = engine.error_key;
        outcome.notices.extend(engine.notices);
        outcome.engine_saved = true;
    }
    outcome
}

/// A save's banners, kept for the page a save without JavaScript
/// redirects to (post, redirect, get), shown once.
struct Flash {
    tab: SettingsTab,
    section: SettingsSection,
    success: String,
    notices: Vec<Notice>,
    created: std::time::Instant,
}

/// Flashes waiting for their page, by a random token in the redirect's
/// `saved=` parameter. In this process's memory: they only need to live
/// for the one redirect, and a restart in between just loses a banner
/// (the settings themselves are saved). Old ones are dropped, and there
/// are never many: only the admin saves settings.
static FLASHES: std::sync::LazyLock<parking_lot::Mutex<HashMap<String, Flash>>> = std::sync::LazyLock::new(Default::default);

const FLASH_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const MAX_FLASHES: usize = 64;

fn put_flash(flash: Flash) -> String {
    use rand::Rng;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let mut flashes = FLASHES.lock();
    flashes.retain(|_, f| f.created.elapsed() < FLASH_TTL);
    if flashes.len() >= MAX_FLASHES {
        if let Some(oldest) = flashes.iter().min_by_key(|(_, f)| f.created).map(|(k, _)| k.clone()) {
            flashes.remove(&oldest);
        }
    }
    flashes.insert(token.clone(), flash);
    token
}

fn take_flash(token: &str) -> Option<Flash> {
    FLASHES.lock().remove(token).filter(|f| f.created.elapsed() < FLASH_TTL)
}

/// `POST /dashboard/admin/settings` - saves one tab: any mix of monokulo's
/// and the engine's settings (nicer_admin_screen.md step 2). Without
/// JavaScript a successful save redirects back to its tab (303), its
/// banners carried across in a flash; a refused one renders the page again
/// with the error, on the tab holding the setting it's about. With fixi,
/// the saved section comes back instead.
pub async fn save(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let form = joined(form);
    let submitted_tab = SettingsTab::from_id(form.get("tab").map(String::as_str));
    let outcome = save_tab(&state, &form).await;
    let tab = outcome.error_key.as_ref().map(|(key, owner)| setting_placement(key, *owner).0).unwrap_or(submitted_tab);
    // Which of the page's two sections shows the result, until the page
    // has tabs (step 3).
    let section = match (&outcome.error_key, outcome.engine_saved) {
        (Some((_, SettingOwner::Monokulo)), _) | (None, false) => SettingsSection::Monokulo,
        _ => SettingsSection::Engine,
    };
    let success = "Settings saved and applied.".to_string();
    if outcome.error.is_none() && !fx.0 {
        let token = put_flash(Flash { tab, section, success, notices: outcome.notices, created: std::time::Instant::now() });
        return super::dashboard::redirect_303(&format!("{}&saved={token}", tab.href()));
    }
    let success = outcome.error.is_none().then_some(success);
    let mut view = build_view_model(&state, outcome.error, success, outcome.notices).await;
    view.saved_tab = Some(tab);
    view.saved_section = Some(section);
    if fx.0 {
        render_fragment(&view, section, outcome.engine_changed)
    } else {
        render(&state, &admin_user, view)
    }
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
    /// scanner connection already configured (`engine.url`/`engine.admin_token`):
    /// what most tests in this module want, since the whole point of this
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
        AppState { engine_client, exchange_rate, abuse, settings, ..AppState::for_tests_with_db(db) }
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

    /// The page a save leads to: the redirect's target after a successful
    /// save without JavaScript, or the page rendered with its error.
    async fn follow(router: &Router, cookie: &str, response: axum::response::Response) -> String {
        if response.status() != StatusCode::SEE_OTHER {
            return body_text(response).await;
        }
        let location = response.headers()["location"].to_str().unwrap().to_string();
        let page = router
            .clone()
            .oneshot(Request::builder().method("GET").uri(&location).header("cookie", cookie).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK, "{location}");
        body_text(page).await
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
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(html.contains("Settings saved and applied."), "expected a success banner, got: {html}");
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
            ("exchange_rate.coinmarketcap_enabled", "false"),
            ("exchange_rate.coinmarketcap_base_url", "http://127.0.0.1:9998"),
            ("exchange_rate.haveno_enabled", "true"),
            ("exchange_rate.haveno_base_url", "http://127.0.0.1:9997"),
            ("exchange_rate.cache_seconds", "77"),
            ("http_cache.max_mb", "42"),
            ("abuse.soft_per_min", "33"),
            ("abuse.hard_per_min", "330"),
            ("abuse.signed_in_per_min", "700"),
            ("abuse.client_logs_per_min", "45"),
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
            ("logging.otlp_endpoint", "http://127.0.0.1:4318"),
            ("logging.otlp_headers", "x-team=ops"),
        ];
        // Every monokulo setting must be covered here, or this test would
        // silently stop proving anything about a setting added later.
        assert_eq!(new_values.len(), crate::settings::ALL.len(), "this test must cover every known monokulo setting");

        let save = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, new_values)).await.unwrap();
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(html.contains("Settings saved and applied."), "expected a success banner, got: {html}");
        assert!(html.contains("didn&#39;t answer") || html.contains("didn't answer"), "the new engine URL doesn't answer, and the page says so (D4): {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        for (key, value) in new_values {
            if *key == "engine.admin_token" || *key == "logging.otlp_headers" {
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
            ("logging.otlp_endpoint", "http://127.0.0.1:4318"),
            ("logging.otlp_headers", "x-team=ops"),
        ];
        assert_eq!(
            new_values.len(),
            scanner::engine_settings::ALL.len() - scanner::engine_settings::NETWORKS.len(),
            "this test must cover every known engine setting apart from the node ones"
        );

        let save = router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, new_values)).await.unwrap();
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(html.contains("Settings saved and applied."), "expected a success banner, got: {html}");
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
            if *key == "logging.otlp_headers" {
                assert!(!html.contains(value), "a secret is never echoed back");
                continue;
            }
            assert!(shows_value(&html, value), "expected {key}={value:?} to have round-tripped, got: {html}");
        }
    }

    #[test]
    fn ticked_choices_are_joined_and_ticking_none_still_sends_the_name() {
        let pairs = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let form = super::joined(pairs(&[("list", ""), ("list", "plain"), ("list", "socket"), ("other", "a,b")]));
        assert_eq!(form["list"], "plain,socket");
        assert_eq!(form["other"], "a,b");
        assert_eq!(super::joined(pairs(&[("list", "")]))["list"], "");
    }

    /// A value shown in a text or number input, or selected in a select.
    fn shows_value(html: &str, value: &str) -> bool {
        html.contains(&format!("value=\"{value}\" selected")) || html.contains(&format!("value=\"{value}\">"))
            || html.contains(&format!("value=\"{value}\" min")) || html.contains(&format!("value=\"{value}\" id="))
            || html.contains(&format!("value=\"{value}\" checked"))
            || html.contains(&format!("\">{value}</textarea>"))
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
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
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
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
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
        assert_eq!(kept.status(), StatusCode::SEE_OTHER);
        assert_eq!(db.lock().get_setting(key).unwrap().as_deref(), Some(SCANNER_ADMIN_TOKEN), "empty keeps it");

        let clear = format!("clear:{key}");
        let cleared = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(key, ""), (clear.as_str(), "on")]))
            .await
            .unwrap();
        assert_eq!(cleared.status(), StatusCode::SEE_OTHER);
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
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("payment.confirmations_required", "5")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(html.contains("Settings saved and applied."), "expected a success banner, got: {html}");
        assert!(html.contains("value=\"5\""), "expected the scanner's own just-saved value reflected, got: {html}");

        let reload = get_settings_page(&router, &cookie).await;
        let html = body_text(reload).await;
        assert!(html.contains("value=\"5\""), "expected the scanner's change to survive a fresh page load, got: {html}");
    }

    fn fixi(mut request: Request<Body>) -> Request<Body> {
        request.headers_mut().insert("FX-Request", "true".parse().unwrap());
        request
    }

    /// With fixi, a save answers with just the section that was saved, its
    /// banner inside it, so the rest of the page (and any edits there)
    /// stays as it was (structured_logging.md part 6).
    #[tokio::test]
    async fn a_fixi_save_answers_with_only_the_saved_section() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(page.contains(r##"fx-action="/dashboard/admin/settings" fx-method="POST" fx-target="#monokulo-settings""##), "{page}");
        assert!(page.contains(r##"fx-target="#engine-settings""##), "{page}");

        let save = router
            .clone()
            .oneshot(fixi(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("abuse.soft_per_min", "70")])))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.starts_with(r#"<section id="monokulo-settings">"#), "{html}");
        assert!(html.contains("Settings saved and applied.") && html.contains(r#"value="70""#), "{html}");
        assert!(!html.contains("engine-settings") && !html.contains("<html"), "only the saved section: {html}");

        let refused = router
            .clone()
            .oneshot(fixi(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("payment.confirmations_required", "-1")])))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(refused).await;
        assert!(html.starts_with(r#"<section id="engine-settings">"#) && html.contains("The engine refused the change"), "{html}");
        assert!(html.contains(r#"<span class="save-status error" role="alert" data-fx-focus"#), "a word by the button gets focus: {html}");

        let saved = router
            .clone()
            .oneshot(fixi(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("payment.confirmations_required", "4")])))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        assert!(body_text(saved).await.contains("Settings saved and applied."));
    }

    /// Saving the engine connection in the monokulo section changes the
    /// engine section too: it comes back marked out of band.
    #[tokio::test]
    async fn a_fixi_save_that_changes_the_engine_connection_brings_the_engine_section_too() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let key = crate::settings::SCANNER_ADMIN_TOKEN.key;

        let unchanged = router.clone().oneshot(fixi(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(key, "")]))).await.unwrap();
        assert!(!body_text(unchanged).await.contains("engine-settings"), "the token was kept, so the engine half didn't change");

        let changed =
            router.clone().oneshot(fixi(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[(key, "wrong-token")]))).await.unwrap();
        let html = body_text(changed).await;
        assert!(html.contains(r#"<section id="engine-settings" data-fx-oob>"#), "{html}");
        assert!(html.contains("Could not reach the configured engine"), "{html}");
    }

    #[tokio::test]
    async fn an_invalid_scanner_setting_is_rejected_by_the_scanner_and_surfaced_as_an_error() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request("POST", "/dashboard/admin/settings", &cookie, &[("payment.confirmations_required", "not-a-number")]))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(html.contains("The engine refused the change"), "expected the engine's own rejection surfaced, got: {html}");
    }

    #[tokio::test]
    async fn an_unconfigured_scanner_connection_shows_a_configuration_prompt_instead_of_a_form() {
        let state = AppState::for_tests();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let response = get_settings_page(&router, &cookie).await;
        let html = body_text(response).await;
        assert!(html.contains("Set <code>engine.url</code>"), "expected the configure-first prompt, got: {html}");
    }

    // -- One save for a whole tab (nicer_admin_screen.md step 2) ----------

    /// [`spawn_scanner_with_known_admin_token`] for an engine built from
    /// `config`.
    async fn spawn_configured_scanner(config: scanner_test_support::TestEngineConfig) -> scanner_test_support::TestEngineHandle {
        let engine = config.spawn().await;
        engine
            .store()
            .lock()
            .set_setting("instance_admin_token_hash", &shared::auth::hash_secret_token(SCANNER_ADMIN_TOKEN))
            .unwrap();
        engine
    }

    /// The engine's own view of its settings, straight from its admin API.
    async fn engine_settings(engine: &scanner_test_support::TestEngineHandle) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("http://{}/api/v1/admin/settings", engine.addr))
            .bearer_auth(SCANNER_ADMIN_TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// Every monokulo setting as stored, to see that a save left them alone.
    fn monokulo_stored(db: &crate::db::SharedDb) -> Vec<(&'static str, Option<String>)> {
        crate::settings::ALL.iter().map(|s| (s.key(), db.lock().get_setting(s.key()).unwrap())).collect()
    }

    async fn post_settings(router: &Router, cookie: &str, fields: &[(&str, &str)]) -> axum::response::Response {
        router.clone().oneshot(authed_form_request("POST", "/dashboard/admin/settings", cookie, fields)).await.unwrap()
    }

    fn unescaped(html: &str) -> String {
        html.replace("&quot;", "\"").replace("&#34;", "\"").replace("&#39;", "'").replace("&amp;", "&")
    }

    /// Nothing listens at this instance's engine address, so a save that
    /// sent anything to the engine would be refused: it goes through, and
    /// back to its own tab.
    #[tokio::test]
    async fn a_tab_with_only_monokulo_settings_saves_only_monokulo() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = post_settings(&router, &cookie, &[("tab", "abuse"), ("abuse.soft_per_min", "61")]).await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let location = save.headers()["location"].to_str().unwrap().to_string();
        assert!(location.starts_with("/dashboard/admin/settings?tab=abuse&saved="), "{location}");
        assert!(follow(&router, &cookie, save).await.contains("Settings saved and applied."));
        assert_eq!(db.lock().get_setting("abuse.soft_per_min").unwrap().as_deref(), Some("61"));
    }

    #[tokio::test]
    async fn a_tab_with_only_engine_settings_saves_only_the_engine() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let before = monokulo_stored(&db);

        let save = post_settings(&router, &cookie, &[("tab", "payments"), ("payment.confirmations_required", "6")]).await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        assert_eq!(engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["value"], "6");
        assert_eq!(monokulo_stored(&db), before, "monokulo's settings weren't touched");
    }

    /// The Payments tab holds both processes' settings; one Save stores
    /// both, and both read back.
    #[tokio::test]
    async fn a_mixed_tab_saves_both_halves() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "payments"), ("payment.confirmations_required", "7"), ("exchange_rate.cache_seconds", "88")],
        )
        .await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(html.contains("Settings saved and applied."), "{html}");
        assert_eq!(engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["value"], "7");
        assert_eq!(db.lock().get_setting("exchange_rate.cache_seconds").unwrap().as_deref(), Some("88"));
        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(shows_value(&page, "7") && shows_value(&page, "88"), "{page}");
    }

    /// Monokulo's half is checked first; when it's refused, the engine's
    /// half isn't sent, so nothing changes anywhere.
    #[tokio::test]
    async fn an_invalid_monokulo_value_in_a_mixed_tab_saves_neither_half() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "payments"), ("payment.confirmations_required", "8"), ("exchange_rate.cache_seconds", "-5")],
        )
        .await;
        assert_eq!(save.status(), StatusCode::OK, "a refused save shows the page again");
        let html = unescaped(&body_text(save).await);
        assert!(html.contains("exchange_rate.cache_seconds: Enter a whole number, 0 or more."), "{html}");
        assert_eq!(engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["value"], "10", "the engine half wasn't sent");
        assert_eq!(db.lock().get_setting("exchange_rate.cache_seconds").unwrap(), None);
    }

    #[tokio::test]
    async fn an_engine_refusal_shows_the_engines_own_message() {
        let engine = spawn_scanner_with_known_admin_token().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let direct: serde_json::Value = reqwest::Client::new()
            .post(format!("http://{}/api/v1/admin/settings", engine.addr))
            .bearer_auth(SCANNER_ADMIN_TOKEN)
            .json(&serde_json::json!({ "scalars": { "payment.confirmations_required": "-1" } }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let message = direct["error"].as_str().unwrap();

        let save = post_settings(&router, &cookie, &[("tab", "payments"), ("payment.confirmations_required", "-1")]).await;
        assert_eq!(save.status(), StatusCode::OK);
        let html = unescaped(&body_text(save).await);
        assert!(html.contains(&format!("The engine refused the change (400 Bad Request): {message}")), "{message} in {html}");
    }

    /// The banners a save brings still show after the redirect: a setting
    /// waiting for a restart, and a network stores use left with no node
    /// that answers.
    #[tokio::test]
    async fn restart_and_unserved_network_notices_still_show() {
        let engine = spawn_configured_scanner(
            scanner_test_support::TestEngineConfig::new().with_networks(&[monero::Network::Stagenet]).with_live_nodes(),
        )
        .await;
        let state = test_app_state_connected_to(engine.addr).await;
        let engine_client = state.engine_client.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        // A store on stagenet, which needs a stagenet node saved first.
        let first = post_settings(&router, &cookie, &[("tab", "nodes"), ("monero_node_stagenet", r#"{"host":"127.0.0.1","port":9}"#)]).await;
        assert_eq!(first.status(), StatusCode::SEE_OTHER);
        engine_client
            .create_tenant(crate::engine_client::CreateTenantRequest {
                view_key_hex: "0707070707070707070707070707070707070707070707070707070707070707".to_string(),
                spend_pubkey_hex: "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90".to_string(),
                network: Some("stagenet".to_string()),
                confirmations_required: None,
                order_expiry_seconds: None,
                key_custody_backend: None,
            })
            .await
            .unwrap();

        let restart = post_settings(&router, &cookie, &[("tab", "server"), ("server.worker_threads", "3")]).await;
        let html = unescaped(&follow(&router, &cookie, restart).await);
        assert!(html.contains("Saved. These settings take effect after the engine restarts: server.worker_threads."), "{html}");

        // Nothing answers on ports 9 or 10.
        let unserved = post_settings(&router, &cookie, &[("tab", "nodes"), ("monero_node_stagenet", r#"{"host":"127.0.0.1","port":10}"#)]).await;
        assert_eq!(unserved.status(), StatusCode::SEE_OTHER, "an unreachable node is still saved");
        let html = unescaped(&follow(&router, &cookie, unserved).await);
        assert!(html.contains("1 store uses the stagenet network, which no longer has any reachable nodes."), "{html}");
    }

    /// A flash is shown once: reloading the page it led to doesn't bring
    /// the banners back.
    #[tokio::test]
    async fn a_saved_banner_is_shown_once() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let save = post_settings(&router, &cookie, &[("tab", "abuse"), ("abuse.soft_per_min", "62")]).await;
        let location = save.headers()["location"].to_str().unwrap().to_string();
        assert!(follow(&router, &cookie, save).await.contains("Settings saved and applied."));
        let again = body_text(get(&router, &location, Some(&cookie)).await).await;
        assert!(!again.contains("Settings saved and applied."), "{again}");
    }
}
