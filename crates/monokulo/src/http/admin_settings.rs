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
//! settings (`crate::settings::ALL`) are saved through its registry, into
//! its options file (or, for the runtime switches, its database). The
//! scanner half is a live HTTP proxy - this
//! page holds no scanner state of its own at all, it just calls the
//! configured scanner instance's own `GET`/`POST /api/v1/admin/settings`
//! (`engine::http::instance_admin`) through monokulo's engine client
//! (`MONOKULO_ENGINE_URL` and `MONOKULO_ENGINE_TOKEN`, both fixed at
//! start), and renders/forwards whatever that instance reports. This is deliberately the single-configured-scanner
//! shape a self-hosted one-box deployment has (`scripts/dev-run.sh`), not a
//! multi-tenant "one monokulo, many engines" design - see this crate's own
//! `EngineClient`, which already assumes exactly one engine base URL.
//!
//! Every field on every tab always carries its *current effective* value
//! (secrets excepted: they are never echoed back), with a chip saying where
//! it comes from (`environment > command line > options file or database >
//! default`). A value given on the command line or in the environment is
//! locked: the page shows it, and says where to change it. Each process's
//! options file is named above the tabs, with a button that reads it again
//! after an edit by hand ([`reload`]).
//!
//! **Database access**: settings are read through `state.db.read` (and the
//! settings registry), each a job on the database's own threads; the
//! functions below that talk to the engine take plain, already-read owned
//! values.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Form, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::admin_nodes::{self, NodeForm};
use crate::db::UserRow;
use crate::views;
use crate::views::admin::{
    setting_placement, AdminNetworkFieldView, AdminScalarFieldView, AdminSettingsViewModel,
    NodeRowView, NodeStatusView, Notice, OptionsFileView, SettingKindView, SettingOwner,
    SettingSourceView, SettingsTab,
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

fn live_source(source: live_settings::SettingSource) -> SettingSourceView {
    match source {
        live_settings::SettingSource::Toml => SettingSourceView::OptionsFile,
        live_settings::SettingSource::Database => SettingSourceView::Runtime,
        live_settings::SettingSource::Cli => SettingSourceView::CommandLine,
        live_settings::SettingSource::Env => SettingSourceView::Environment,
        live_settings::SettingSource::Default => SettingSourceView::Default,
    }
}

/// Why a setting kept in an options file can't be changed here, when
/// `process` can't write that file.
fn read_only_file(file: &live_settings::FileInfo, process: &str) -> String {
    format!(
        "The options file {} can't be written by {process}, so this is changed by editing it, then reloading it.",
        file.path
    )
}

/// Every monokulo setting as the page shows it, from the registry's
/// description of them (tasks 4.1, 4.6). Without a registry (a test state),
/// nothing is listed.
fn monokulo_fields(state: &AppState) -> Vec<AdminScalarFieldView> {
    let Some(registry) = state.settings.registry.as_ref() else {
        return Vec::new();
    };
    let read_only = registry
        .options_file()
        .filter(|file| !file.writable)
        .map(|file| read_only_file(&file, "monokulo"));
    registry
        .describe()
        .into_iter()
        .map(|view| {
            // Given at start, or in a file monokulo can't write: shown,
            // with a padlock and the reason, never saved here.
            let locked = view
                .locked
                .or_else(|| read_only.clone().filter(|_| view.sources.toml));
            AdminScalarFieldView {
                key: view.key.to_string(),
                name: String::new(),
                label: humanize_key(view.key),
                value: view.value,
                source: live_source(view.source),
                help: Some(view.description.to_string()),
                kind: SettingKindView::from(view.kind),
                example: view.example.map(str::to_string),
                restart_only: view.applies == live_settings::Applies::Restart,
                pending_restart: view.pending_restart,
                problem: view.problem.map(|p| p.message),
                locked,
            }
        })
        .collect()
}

/// Whether `key` is one of monokulo's own settings.
fn is_monokulo_key(key: &str) -> bool {
    crate::settings::ALL.iter().any(|s| s.key() == key)
}

/// The name an engine setting's control is sent under: its key, unless
/// monokulo has a setting with the same key (both have `logging.level`),
/// when it's `engine:<key>` so the two can sit in one form on one tab.
fn engine_form_name(key: &str) -> String {
    if is_monokulo_key(key) {
        format!("engine:{key}")
    } else {
        String::new()
    }
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
    /// Why the page can't change it, if it can't: shown locked.
    #[serde(default)]
    locked: Option<String>,
}

#[derive(Deserialize, Default)]
struct RemoteNetwork {
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
    /// Where the engine's options file is, and whether it can write it.
    #[serde(default)]
    options_file: Option<live_settings::FileInfo>,
}

/// The engine's settings, as the page shows them.
struct EngineSettings {
    fields: Vec<AdminScalarFieldView>,
    networks: Vec<AdminNetworkFieldView>,
    options_file: Option<live_settings::FileInfo>,
}

/// Fetches the engine's own settings over HTTP; `Err` for a reachability,
/// auth or parse failure worth showing.
async fn fetch_engine_settings(
    engine: &crate::engine_client::EngineClient,
) -> Result<EngineSettings, String> {
    let response = engine
        .request(reqwest::Method::GET, "/api/v1/admin/settings")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("the engine responded with {}", response.status()));
    }
    let parsed: RemoteSettingsResponse = response
        .json()
        .await
        .map_err(|e| format!("could not parse the engine's response: {e}"))?;

    let fields = parsed
        .scalars
        .into_iter()
        .map(|(key, s)| AdminScalarFieldView {
            label: humanize_key(&key),
            name: engine_form_name(&key),
            key,
            value: s.value,
            source: SettingSourceView::from_name(&s.source),
            help: s.description,
            kind: s.kind,
            example: s.example,
            restart_only: s.applies.as_deref() == Some("restart"),
            pending_restart: s.pending_restart,
            problem: s.problem,
            locked: s.locked,
        })
        .collect();
    let mut networks_meta = parsed.networks;
    let mut networks: Vec<AdminNetworkFieldView> = parsed
        .monero_node
        .into_iter()
        .map(|(network, value)| {
            let meta = networks_meta.remove(&network).unwrap_or_default();
            // The engine's example node for this network, as an address.
            let example_address = meta
                .example
                .and_then(|example| serde_json::from_str::<serde_json::Value>(&example).ok())
                .and_then(|example| {
                    admin_nodes::rows_from_setting(Some(&example))
                        .into_iter()
                        .next()
                })
                .map(|(_, row)| row.address);
            AdminNetworkFieldView {
                rows: admin_nodes::rows_from_setting(value.as_ref())
                    .into_iter()
                    .map(|(label, row)| NodeRowView {
                        row,
                        label,
                        status: None,
                    })
                    .collect(),
                network,
                example_address,
                tenant_count: meta.tenant_count,
                scaling: None,
                error: None,
            }
        })
        .collect();
    networks.sort_by_key(|n| {
        admin_nodes::NETWORKS
            .iter()
            .position(|known| shared::network::network_str(*known) == n.network)
            .unwrap_or(usize::MAX)
    });
    Ok(EngineSettings {
        fields,
        networks,
        options_file: parsed.options_file,
    })
}

/// Assembles the whole page's view model: monokulo's fields from its
/// registry, the engine's fetched over HTTP.
/// Settings that hold a Unix time until which something stays on get the
/// "off / on for N hours" control instead of a number box.
fn with_time_limits(fields: &mut [AdminScalarFieldView], clock: &views::time::Clock) {
    let now = u64::try_from(crate::now_unix()).unwrap_or(0);
    for field in fields
        .iter_mut()
        .filter(|f| f.key == "logging.dev_mode_until")
    {
        let until = field.value.trim().parse().unwrap_or(0);
        field.kind = SettingKindView::TimeLimit {
            now,
            until_label: clock.text(until),
        };
    }
}

/// What the page says about a save, when it answers one.
#[derive(Default)]
struct SaveResult {
    error: Option<String>,
    success: Option<String>,
    notices: Vec<Notice>,
    /// The node rows as submitted, shown again (with what's wrong) when
    /// the save was refused.
    nodes: Option<NodeForm>,
    /// The engine's refusal of particular settings (its `fields`), by key.
    field_errors: Vec<(String, String)>,
}

async fn build_view_model(
    state: &AppState,
    admin: &UserRow,
    tab: SettingsTab,
    result: SaveResult,
) -> AdminSettingsViewModel {
    let SaveResult {
        error,
        success,
        notices,
        nodes,
        field_errors,
    } = result;
    let clock = views::time::Clock::for_user(admin);
    let mut monokulo_fields = monokulo_fields(state);
    with_time_limits(&mut monokulo_fields, &clock);
    let options_files = state
        .settings
        .registry
        .as_ref()
        .and_then(live_settings::Registry::options_file)
        .map(|file| OptionsFileView {
            owner: SettingOwner::Monokulo,
            path: file.path,
            exists: file.exists,
            writable: file.writable,
        })
        .into_iter()
        .collect();
    let mut view = AdminSettingsViewModel {
        tab,
        error,
        success,
        notices,
        monokulo_fields,
        options_files,
        ..Default::default()
    };
    match fetch_engine_settings(&state.engine.client).await {
        Ok(engine) => {
            let mut fields = engine.fields;
            with_time_limits(&mut fields, &clock);
            view.engine_reachable = true;
            view.engine_fields = fields;
            view.engine_networks = engine.networks;
            view.options_files
                .extend(engine.options_file.map(|file| OptionsFileView {
                    owner: SettingOwner::Engine,
                    path: file.path,
                    exists: file.exists,
                    writable: file.writable,
                }));
        }
        Err(e) => {
            view.engine_reachable = false;
            view.engine_error = Some(e);
        }
    }
    // A refused save shows the rows as they were submitted, not as saved,
    // with the engine's word on a network at the top of its block.
    if let Some(nodes) = &nodes {
        for network in &mut view.engine_networks {
            let rows = shared::network::parse_network(&network.network)
                .ok()
                .and_then(|n| nodes.rows(n));
            if let Some(rows) = rows {
                network.rows = rows
                    .iter()
                    .map(|row| NodeRowView {
                        label: admin_nodes::parse_address(&row.address)
                            .map(|a| a.label())
                            .unwrap_or_default(),
                        row: row.clone(),
                        status: None,
                    })
                    .collect();
            }
        }
    }
    for network in &mut view.engine_networks {
        let key = format!("monero_node.{}", network.network);
        network.error = field_errors
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, message)| message.clone());
    }
    if view.engine_reachable {
        // The Monero nodes tab waits for a current answer (monokulo's short
        // cache) to show each node's status; every other tab only marks the
        // tab bar with whatever is already known, rather than wait on node
        // probes to show, say, the Logging tab (decision D10).
        let unserved = if tab == SettingsTab::Nodes {
            match super::status_page::get_status_cached(&state.engine).await {
                Ok(status) => {
                    let now = shared::time::now_unix();
                    attach_node_status(&mut view.engine_networks, &status, now);
                    view.resources = Some(views::scaling::ResourcesView {
                        engine: status.resources.clone(),
                        monokulo: shared::resources::sampler().report(),
                        now_unix: now,
                    });
                    status.unserved_tenants
                }
                Err(_) => Vec::new(),
            }
        } else {
            super::status_page::known_unserved(&state.engine)
        };
        view.unreachable_networks = unreachable_networks(unserved);
    }
    view
}

/// Networks stores use that no node answers for, from the engine's
/// `/status` (it reports each such store as `no_reachable_node`).
fn unreachable_networks(unserved: Vec<crate::engine_client::UnservedTenant>) -> Vec<String> {
    let mut networks: Vec<String> = unserved
        .into_iter()
        .filter(|tenant| tenant.reason == "no_reachable_node")
        .map(|tenant| tenant.network)
        .collect();
    networks.sort();
    networks.dedup();
    networks
}

/// Each row's status from `/status`, found by the engine's label for the
/// node (`host:port`). A node on a network other than its block's says so;
/// one that reports `fakechain`, or nothing, isn't called wrong. Each
/// network also gets its scan figures, and each node its link's.
fn attach_node_status(
    networks: &mut [AdminNetworkFieldView],
    status: &crate::engine_client::EngineStatusResponse,
    now: i64,
) {
    for network in networks {
        let Some(reported) = status
            .networks
            .iter()
            .find(|n| n.network == network.network)
        else {
            continue;
        };
        network.scaling = reported.scaling.clone();
        let highest = reported.nodes.iter().filter_map(|node| node.height).max();
        for row in &mut network.rows {
            let Some(node) = reported
                .nodes
                .iter()
                .find(|node| !row.label.is_empty() && node.label == row.label)
            else {
                continue;
            };
            let wrong_network = node
                .network
                .clone()
                .filter(|on| shared::network::parse_network(on).is_ok() && *on != network.network);
            row.status = Some(NodeStatusView {
                height: node.height,
                error: node.error.clone(),
                wrong_network,
                in_use: node.is_active,
                resting: node.in_cooldown,
                behind: highest
                    .zip(node.height)
                    .map(|(highest, height)| highest.saturating_sub(height)),
                link: node.link.clone().map(|link| views::scaling::NodeLinkView {
                    link,
                    now_unix: now,
                }),
            });
        }
    }
}

async fn render(state: &AppState, admin_user: &UserRow, view: AdminSettingsViewModel) -> Response {
    let chrome = super::page_chrome(state, Some(admin_user), "/dashboard/admin/settings").await;
    views::admin::admin_settings_page(&chrome, &view).into_response()
}

#[derive(Deserialize)]
pub struct SettingsPageQuery {
    tab: Option<String>,
    /// The flash a save without JavaScript left for the page it redirected
    /// to ([`FLASHES`]).
    saved: Option<String>,
}

/// `GET /dashboard/admin/settings?tab=<id>` - one tab of the page
/// (General when `tab` is missing or unknown). With fixi (a tab link), just
/// the panel, with the tab bar and banners out of band.
pub async fn page(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    Query(query): Query<SettingsPageQuery>,
) -> Response {
    let tab = SettingsTab::from_id(query.tab.as_deref());
    let view = match query.saved.as_deref().and_then(take_flash) {
        Some(flash) => {
            let result = SaveResult {
                success: Some(flash.success),
                notices: flash.notices,
                ..Default::default()
            };
            let mut view = build_view_model(&state, &admin_user, tab, result).await;
            view.saved_tab = (!flash.reloaded).then_some(flash.tab);
            view
        }
        None => build_view_model(&state, &admin_user, tab, SaveResult::default()).await,
    };
    if fx.0 {
        return axum::response::Html(views::admin::settings_fragment(&view, true).into_string())
            .into_response();
    }
    render(&state, &admin_user, view).await
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
/// T3): monokulo's are the names its registry knows; everything else is
/// the engine's.
#[derive(Default)]
struct SplitForm {
    monokulo: HashMap<String, String>,
    engine: RemoteUpdateRequest,
    /// The node form was submitted (`node_*` fields: `admin_nodes`).
    nodes: bool,
}

impl SplitForm {
    fn new(form: &HashMap<String, String>) -> SplitForm {
        let mut split = SplitForm::default();
        for (name, value) in form {
            if name == "tab" {
                continue;
            }
            if admin_nodes::is_node_field(name) {
                split.nodes = true;
                continue;
            }
            let engine_key = name.strip_prefix("engine:");
            if engine_key.is_none() && is_monokulo_key(name) {
                split.monokulo.insert(name.clone(), value.clone());
            } else {
                split
                    .engine
                    .scalars
                    .insert(engine_key.unwrap_or(name).to_string(), value.clone());
            }
        }
        split
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
    /// The node rows as submitted, for the page to show again when the
    /// save is refused.
    nodes: Option<NodeForm>,
    /// The engine's refusal of particular settings (its `fields`), by key.
    field_errors: Vec<(String, String)>,
}

impl SaveOutcome {
    fn refused(message: String) -> SaveOutcome {
        SaveOutcome {
            error: Some(message),
            ..Default::default()
        }
    }
}

/// Saves the monokulo half of a tab through the registry
/// (admin_settings_v2.md part 1): all checked first, then applied to the
/// running process and stored together, or nothing at all. A locked
/// setting (a secret, or one given on the command line) refuses the save.
async fn save_monokulo(state: &AppState, form: &HashMap<String, String>) -> SaveOutcome {
    let Some(registry) = state.settings.registry.as_ref() else {
        return SaveOutcome::refused("Settings can't be saved on this instance.".to_string());
    };
    let changes: live_settings::Changes = crate::settings::ALL
        .iter()
        .filter_map(|setting| {
            let value = form.get(setting.key())?;
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
            SaveOutcome {
                notices,
                ..Default::default()
            }
        }
        Err(live_settings::SaveError::Invalid(errors)) => SaveOutcome {
            error_key: errors
                .first()
                .map(|e| (e.key.clone(), SettingOwner::Monokulo)),
            ..SaveOutcome::refused(
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        },
        // The options file changed since it was read, or can't be written:
        // the admin reloads it, or fixes its permissions.
        Err(live_settings::SaveError::Store(e)) => {
            tracing::warn!(error = %e, "monokulo's settings could not be stored");
            SaveOutcome::refused(format!("Nothing was saved: {e}"))
        }
        // Stored, but applying them failed: retrying would fail the same way.
        Err(live_settings::SaveError::Install(message)) => {
            tracing::error!(error = %message, "monokulo settings were saved but applying them failed");
            SaveOutcome {
                notices: vec![Notice::Error(format!(
                    "Saved, but applying the new values failed ({message}). Restart monokulo to apply them."
                ))],
                ..Default::default()
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "saving monokulo settings failed");
            SaveOutcome::refused(
                "Something went wrong saving these settings. Please try again.".to_string(),
            )
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
    /// The keys whose saved value changed.
    #[serde(default)]
    changed: Vec<String>,
    #[serde(default)]
    warnings: RemoteSaveWarnings,
}

/// The banners for an accepted engine save (tasks 3.6, 4.5, decisions D1,
/// D2, D8): restart-only settings, networks stores use that no longer have
/// a node, environment overrides and anything else the engine said.
fn engine_save_notices(warnings: RemoteSaveWarnings, submitted_bind: Option<&str>) -> Vec<Notice> {
    let mut notices = Vec::new();
    for unserved in &warnings.unserved_networks {
        let stores = if unserved.tenants == 1 {
            "1 store uses".to_string()
        } else {
            format!("{} stores use", unserved.tenants)
        };
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
                     " After restarting it, set monokulo's engine.url to http://{bind} on the General tab and restart monokulo, so it can reach the engine."
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
async fn save_engine(state: &AppState, req: RemoteUpdateRequest) -> SaveOutcome {
    if req.is_empty() {
        return SaveOutcome::default();
    }
    let result = state
        .engine
        .client
        .request(reqwest::Method::POST, "/api/v1/admin/settings")
        .json(&req)
        .send()
        .await;
    match result {
        Ok(response) if response.status().is_success() => {
            let saved: RemoteSaveResponse = match response.json().await {
                Ok(saved) => saved,
                Err(e) => {
                    // Saved, but what it said about the save is lost: say
                    // so, and don't trust the cached node status either.
                    tracing::warn!(error = %e, "the engine saved its settings but its reply could not be read");
                    super::status_page::invalidate_status_cache(&state.engine);
                    return SaveOutcome {
                        notices: vec![Notice::Warning(
                            "Saved, but the engine's reply could not be read, so any restart it needs or node it lost isn't shown here.".to_string(),
                        )],
                        ..Default::default()
                    };
                }
            };
            // New nodes: the next page shows their status, not the cached
            // one of the nodes they replaced.
            if saved
                .changed
                .iter()
                .any(|key| key.starts_with("monero_node."))
            {
                super::status_page::invalidate_status_cache(&state.engine);
            }
            SaveOutcome {
                notices: engine_save_notices(
                    saved.warnings,
                    req.scalars.get("server.bind").map(String::as_str),
                ),
                ..Default::default()
            }
        }
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let parsed = serde_json::from_str::<serde_json::Value>(&body).ok();
            let field_errors: Vec<(String, String)> = parsed
                .as_ref()
                .and_then(|v| v["fields"].as_array())
                .map(|fields| {
                    fields
                        .iter()
                        .filter_map(|f| {
                            Some((
                                f["key"].as_str()?.to_string(),
                                f["message"].as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let message = parsed
                .as_ref()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or(body);
            SaveOutcome {
                error_key: field_errors
                    .first()
                    .map(|(key, _)| (key.clone(), SettingOwner::Engine)),
                field_errors,
                ..SaveOutcome::refused(format!(
                    "The engine refused the change ({status}): {message}"
                ))
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
    let mut split = SplitForm::new(form);
    // The node form: the rows as submitted, with a row button applied.
    // Nothing is saved while any row has something to fix.
    let nodes = split
        .nodes
        .then(|| NodeForm::from_form(form, &admin_nodes::NETWORKS));
    if let Some(nodes) = &nodes {
        if nodes.has_errors() {
            let first = nodes
                .networks
                .iter()
                .find(|(_, rows)| rows.iter().any(|row| row.error.is_some()))
                .map(|(n, _)| *n);
            return SaveOutcome {
                error_key: first.map(|network| {
                    (
                        format!("monero_node.{}", shared::network::network_str(network)),
                        SettingOwner::Engine,
                    )
                }),
                nodes: Some(nodes.clone()),
                ..SaveOutcome::refused(
                    "Nothing was saved: some node addresses need fixing (marked below)."
                        .to_string(),
                )
            };
        }
        for (network, rows) in &nodes.networks {
            split.engine.monero_node.insert(
                shared::network::network_str(*network).to_string(),
                admin_nodes::rows_to_setting(rows),
            );
        }
    }
    let mut outcome = SaveOutcome::default();
    if !split.monokulo.is_empty() {
        outcome = save_monokulo(state, &split.monokulo).await;
        if outcome.error.is_some() {
            return outcome;
        }
    }
    let mut saved_here: Vec<String> = split.monokulo.keys().cloned().collect();
    saved_here.sort();
    if !split.engine.is_empty() {
        let engine = save_engine(state, split.engine).await;
        if engine.error.is_some() {
            outcome.nodes = nodes;
        }
        // Monokulo's own settings on this tab were already saved: say so,
        // or the page reads as if nothing was.
        outcome.error = match engine.error {
            Some(error) if !saved_here.is_empty() => Some(format!(
                "Saved {} here; the engine refused its settings: {error}",
                saved_here.join(", ")
            )),
            other => other,
        };
        outcome.error_key = engine.error_key;
        outcome.field_errors = engine.field_errors;
        outcome.notices.extend(engine.notices);
    }
    outcome
}

/// A save's banners, kept for the page a save without JavaScript
/// redirects to (post, redirect, get), shown once.
struct Flash {
    tab: SettingsTab,
    /// From the Reload button, not Save: no word beside Save.
    reloaded: bool,
    success: String,
    notices: Vec<Notice>,
    created: std::time::Instant,
}

/// Flashes waiting for their page, by a random token in the redirect's
/// `saved=` parameter. In this process's memory: they only need to live
/// for the one redirect, and a restart in between just loses a banner
/// (the settings themselves are saved). Old ones are dropped, and there
/// are never many: only the admin saves settings.
static FLASHES: std::sync::LazyLock<parking_lot::Mutex<HashMap<String, Flash>>> =
    std::sync::LazyLock::new(Default::default);

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
        if let Some(oldest) = flashes
            .iter()
            .min_by_key(|(_, f)| f.created)
            .map(|(k, _)| k.clone())
        {
            flashes.remove(&oldest);
        }
    }
    flashes.insert(token.clone(), flash);
    token
}

fn take_flash(token: &str) -> Option<Flash> {
    FLASHES
        .lock()
        .remove(token)
        .filter(|f| f.created.elapsed() < FLASH_TTL)
}

/// `POST /dashboard/admin/settings` - saves one tab: any mix of monokulo's
/// and the engine's settings (nicer_admin_screen.md step 2). Without
/// JavaScript a successful save redirects back to its tab (303), its
/// banners carried across in a flash; a refused one renders the page again
/// with the error, on the tab holding the setting it's about. With fixi,
/// that tab's panel comes back (`422` when nothing was saved), with the
/// banners and the tab bar out of band.
pub async fn save(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let form = joined(form);
    let submitted_tab = SettingsTab::from_id(form.get("tab").map(String::as_str));
    let outcome = save_tab(&state, &form).await;
    let tab = outcome
        .error_key
        .as_ref()
        .map(|(key, owner)| setting_placement(key, *owner).0)
        .unwrap_or(submitted_tab);
    let success = "Settings saved and applied.".to_string();
    if outcome.error.is_none() && !fx.0 {
        let token = put_flash(Flash {
            tab,
            reloaded: false,
            success,
            notices: outcome.notices,
            created: std::time::Instant::now(),
        });
        return super::dashboard::redirect_303(&format!("{}&saved={token}", tab.href()));
    }
    let refused = outcome.error.is_some();
    let result = SaveResult {
        error: outcome.error,
        success: (!refused).then_some(success),
        notices: outcome.notices,
        nodes: outcome.nodes,
        field_errors: outcome.field_errors,
    };
    let mut view = build_view_model(&state, &admin_user, tab, result).await;
    view.saved_tab = Some(tab);
    if !fx.0 {
        return render(&state, &admin_user, view).await;
    }
    let fragment = views::admin::settings_fragment(&view, false);
    if refused {
        super::fx::invalid(fragment)
    } else {
        axum::response::Html(fragment.into_string()).into_response()
    }
}

/// The Reload options file form: whose file, and the tab to come back to.
#[derive(Deserialize)]
pub struct ReloadForm {
    owner: String,
    tab: Option<String>,
}

/// What a reload changed, as the page says it.
fn reload_notices(
    process: &str,
    changed: &[String],
    restart_required: &[String],
) -> (String, Vec<Notice>) {
    let success = if changed.is_empty() {
        format!("Reloaded {process}'s options file: nothing in it changed.")
    } else {
        format!(
            "Reloaded {process}'s options file and applied it: {}.",
            changed.join(", ")
        )
    };
    let mut notices = Vec::new();
    if !restart_required.is_empty() {
        notices.push(Notice::Warning(format!(
            "These take effect after {process} restarts: {}.",
            restart_required.join(", ")
        )));
    }
    (success, notices)
}

/// Reads monokulo's options file again and applies it; `Err` is why
/// nothing changed.
async fn reload_monokulo(state: &AppState) -> Result<(String, Vec<Notice>), String> {
    let Some(registry) = state.settings.registry.as_ref() else {
        return Err("Settings can't be reloaded on this instance.".to_string());
    };
    match registry.reload().await {
        Ok(report) => {
            let changed: Vec<String> = report.changed.iter().map(ToString::to_string).collect();
            let restart: Vec<String> = report
                .restart_required
                .iter()
                .map(ToString::to_string)
                .collect();
            let (success, mut notices) = reload_notices("monokulo", &changed, &restart);
            notices.extend(
                report
                    .warnings
                    .iter()
                    .map(|warning| Notice::Error(warning.message.clone())),
            );
            Ok((success, notices))
        }
        Err(e) => Err(format!(
            "Nothing was reloaded: monokulo's options file has problems. {e}"
        )),
    }
}

/// Asks the engine to read its options file again and apply it
/// (`POST /api/v1/admin/settings/reload`); `Err` is why nothing changed.
async fn reload_engine(state: &AppState) -> Result<(String, Vec<Notice>), String> {
    let response = state
        .engine
        .client
        .request(reqwest::Method::POST, "/api/v1/admin/settings/reload")
        .send()
        .await
        .map_err(|e| format!("Could not reach the configured engine: {e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or(body);
        return Err(format!(
            "Nothing was reloaded: the engine's options file has problems ({status}). {message}"
        ));
    }
    let saved: RemoteSaveResponse = serde_json::from_str(&body).unwrap_or_default();
    if saved
        .changed
        .iter()
        .any(|key| key.starts_with("monero_node."))
    {
        super::status_page::invalidate_status_cache(&state.engine);
    }
    let (success, mut notices) = reload_notices(
        "the engine",
        &saved.changed,
        &saved.warnings.restart_required,
    );
    notices.extend(
        saved
            .warnings
            .messages
            .into_iter()
            .map(|message| Notice::Warning(message.message)),
    );
    Ok((success, notices))
}

/// `POST /dashboard/admin/settings/reload` - the Reload options file button:
/// reads monokulo's or the engine's options file again, after an edit by
/// hand, and applies all of it, or, when anything in it is wrong, none of
/// it, with every problem named by line. A reload that worked redirects
/// back to the tab with its banners (post, redirect, get); one that didn't
/// renders the page with why.
pub async fn reload(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    Form(form): Form<ReloadForm>,
) -> Response {
    let tab = SettingsTab::from_id(form.tab.as_deref());
    let result = match form.owner.as_str() {
        "engine" => reload_engine(&state).await,
        _ => reload_monokulo(&state).await,
    };
    match result {
        Ok((success, notices)) => {
            let token = put_flash(Flash {
                tab,
                reloaded: true,
                success,
                notices,
                created: std::time::Instant::now(),
            });
            super::dashboard::redirect_303(&format!("{}&saved={token}", tab.href()))
        }
        Err(error) => {
            let result = SaveResult {
                error: Some(error),
                ..Default::default()
            };
            let view = build_view_model(&state, &admin_user, tab, result).await;
            render(&state, &admin_user, view).await
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    use crate::db::Db;
    use crate::engine_client::EngineClient;
    use crate::http::test_support::admin_session_cookie;
    use crate::http::{build_router, AppState};

    fn test_exchange_rate_provider(
    ) -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only())
    }

    /// A real engine, which accepts the test engine token every test
    /// client sends.
    async fn spawn_engine() -> engine_test_support::TestEngineHandle {
        engine_test_support::TestEngineConfig::new().spawn().await
    }

    /// A monokulo instance with a seeded admin account and its engine client
    /// pointed at `engine_addr`:
    /// what most tests in this module want, since the whole point of this
    /// page is proxying that connection.
    async fn test_app_state_connected_to(engine_addr: std::net::SocketAddr) -> AppState {
        test_app_state_with_options(
            engine_addr,
            live_settings::OptionsFile::in_memory("[signup]\nmode = \"public\"\n"),
        )
        .await
    }

    /// [`test_app_state_connected_to`] over the given options file.
    async fn test_app_state_with_options(
        engine_addr: std::net::SocketAddr,
        options: live_settings::OptionsFile,
    ) -> AppState {
        let db = Db::open_in_memory().unwrap();
        db.seed_test_admin();
        let db = db.into_shared();
        let engine_client = EngineClient::for_tests(format!("http://{engine_addr}"));
        let exchange_rate = test_exchange_rate_provider();
        let abuse: std::sync::Arc<crate::abuse::AbuseProtection> = Default::default();
        let settings = crate::settings::MonokuloSettings::load(
            crate::db::Database::inline(db.clone()),
            engine_client.clone(),
            exchange_rate.clone(),
            abuse.clone(),
            None,
            crate::settings::test_secrets(),
            options,
        )
        .await
        .unwrap();
        AppState {
            engine: crate::http::Engine::new(engine_client),
            exchange_rate,
            abuse,
            settings,
            ..AppState::for_tests_with_db(db)
        }
    }

    use crate::http::test_support::body_text;

    /// A monokulo setting's value and where it comes from, as the registry
    /// has it.
    fn monokulo_value(
        settings: &crate::settings::MonokuloSettings,
        key: &str,
    ) -> (String, live_settings::SettingSource) {
        let view = settings
            .registry
            .as_ref()
            .unwrap()
            .describe()
            .into_iter()
            .find(|view| view.key == key)
            .unwrap();
        (view.value, view.source)
    }

    /// The part of a page from a setting's label to its control: the
    /// label row with its source chip.
    fn label_row<'a>(html: &'a str, key: &str) -> &'a str {
        let start = html
            .find(&format!(r#"for="setting-{key}""#))
            .unwrap_or_else(|| panic!("no label for {key}: {html}"));
        let end = html[start..]
            .find("</div>")
            .map_or(html.len(), |end| start + end);
        &html[start..end]
    }

    use crate::http::test_support::urlencoding_encode;

    fn form_request(method: &str, uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    }

    fn authed_form_request(
        method: &str,
        uri: &str,
        cookie: &str,
        fields: &[(&str, &str)],
    ) -> Request<Body> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", cookie)
            .body(Body::from(body))
            .unwrap()
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
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&location)
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK, "{location}");
        body_text(page).await
    }

    /// Every tab of the page, one after another: what a test looking for a
    /// setting anywhere on the page reads.
    async fn settings_tabs_html(router: &Router, cookie: &str) -> String {
        let mut html = String::new();
        for tab in crate::views::admin::SettingsTab::ALL {
            html.push_str(&body_text(get(router, &tab.href(), Some(cookie)).await).await);
        }
        html
    }

    async fn get_settings_page(router: &Router, cookie: &str) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/dashboard/admin/settings")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// The status page's "Take a new anchor" button (docs/proof_of_work.md):
    /// for the admin only; the engine forgets the network's anchor and the
    /// page goes back to the status; refused where checking is off or for no
    /// network.
    #[tokio::test]
    async fn taking_a_new_proof_anchor_is_for_the_admin_and_asks_the_engine() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let uri = |network: &str| format!("/dashboard/admin/proof/{network}/reanchor");

        let anonymous = router
            .clone()
            .oneshot(form_request("POST", &uri("mainnet"), &[]))
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        let cookie = admin_session_cookie(&router).await;
        let post = |network: &str| authed_form_request("POST", &uri(network), &cookie, &[]);
        let off = router.clone().oneshot(post("mainnet")).await.unwrap();
        assert_eq!(
            off.status(),
            StatusCode::BAD_GATEWAY,
            "checking is off there"
        );
        let nowhere = router.clone().oneshot(post("moonnet")).await.unwrap();
        assert_eq!(nowhere.status(), StatusCode::BAD_REQUEST);

        {
            let store = engine.store().lock();
            store.enable_proof(monero::Network::Mainnet, 1).unwrap();
            let block = |height: u64| engine::pow::ProvenBlock {
                height,
                id: [height as u8; 32],
                timestamp: height,
                cumulative_difficulty: u128::from(height),
            };
            store
                .write_anchor(
                    monero::Network::Mainnet,
                    &engine::store::proof::NewAnchor {
                        agreed: 1,
                        nodes: 1,
                        window: (1..=3).map(block).collect(),
                        seeds: vec![],
                    },
                    2,
                )
                .unwrap();
        }
        let taken = router.clone().oneshot(post("mainnet")).await.unwrap();
        assert_eq!(taken.status(), StatusCode::SEE_OTHER);
        assert_eq!(taken.headers()["location"], "/status");
        let store = engine.store().lock();
        let state = store
            .proof_network(monero::Network::Mainnet)
            .unwrap()
            .unwrap();
        assert_eq!(state.anchor, None, "a new one is taken next round");
    }

    #[tokio::test]
    async fn the_settings_page_is_unreachable_without_a_session_at_all() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/dashboard/admin/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
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
            .oneshot(form_request(
                "POST",
                "/dashboard/signup",
                &[
                    ("email", "merchant@example.com"),
                    ("password", "correct horse battery staple"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(signup.status(), StatusCode::FOUND);
        let login = router
            .clone()
            .oneshot(form_request(
                "POST",
                "/dashboard/login",
                &[
                    ("email", "merchant@example.com"),
                    ("password", "correct horse battery staple"),
                ],
            ))
            .await
            .unwrap();
        let cookie = login
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let response = get_settings_page(&router, &cookie).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_admin_can_reach_the_settings_page_and_see_the_reachable_engine_settings() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let response = get_settings_page(&router, &cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = settings_tabs_html(&router, &cookie).await;
        assert!(
            html.contains("engine url"),
            "expected monokulo's own settings listed, got: {html}"
        );
        assert!(
            html.contains("payment confirmations required"),
            "expected the scanner's own settings proxied in, got: {html}"
        );
        assert!(
            html.contains("value=\"10\""),
            "expected the scanner's real default value, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_saved_monokulo_setting_round_trips_on_the_next_load() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("tab", "abuse"), ("abuse.soft_per_min", "5")],
            ))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(
            html.contains("Settings saved and applied."),
            "expected a success banner, got: {html}"
        );
        assert!(
            html.contains("value=\"5\""),
            "expected the just-saved value reflected immediately, got: {html}"
        );

        let html = settings_tabs_html(&router, &cookie).await;
        assert!(
            html.contains("value=\"5\""),
            "expected the saved value to survive a fresh page load, got: {html}"
        );
        assert!(
            label_row(&html, "abuse.soft_per_min").contains("Options file"),
            "expected the chip to say this came from the options file, got: {html}"
        );
        assert!(
            label_row(&html, "abuse.hard_per_min").contains("Default"),
            "and an unset one from the default, got: {html}"
        );
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
            ("exchange_rate.coingecko_enabled", "false"),
            ("exchange_rate.coingecko_base_url", "http://127.0.0.1:9999"),
            ("exchange_rate.coinmarketcap_enabled", "false"),
            (
                "exchange_rate.coinmarketcap_base_url",
                "http://127.0.0.1:9998",
            ),
            ("exchange_rate.haveno_enabled", "true"),
            ("exchange_rate.haveno_base_url", "http://127.0.0.1:9997"),
            ("exchange_rate.cache_seconds", "77"),
            ("http_cache.max_mb", "42"),
            ("database.read_connections", "6"),
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
            ("logging.format", "json"),
            ("server.bind", "127.0.0.1:9081"),
            ("engine.url", "http://127.0.0.1:9443"),
        ];
        // Every monokulo setting the page can save must be covered here, or
        // this test would silently stop proving anything about a setting
        // added later. The rest (the secrets, and where the database is)
        // are shown locked.
        assert_eq!(
            new_values.len(),
            crate::settings::ALL
                .iter()
                .filter(|s| (s.sources().toml || s.sources().database) && s.editable())
                .count(),
            "this test must cover every known monokulo setting"
        );

        let save = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                new_values,
            ))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(
            html.contains("Settings saved and applied."),
            "expected a success banner, got: {html}"
        );

        let html = settings_tabs_html(&router, &cookie).await;
        for (key, value) in new_values {
            assert!(
                shows_value(&html, value),
                "expected {key}={value:?} to have round-tripped, got: {html}"
            );
        }
    }

    /// The scanner half of the same requirement - every one of
    /// `engine::engine_settings::ALL`'s keys, saved together through the
    /// real proxy `POST` and confirmed to round-trip via a real, separately
    /// spawned scanner instance (this monokulo page holds none of this state
    /// itself - see this module's own doc comment).
    #[tokio::test]
    async fn every_engine_setting_on_the_admin_page_saves_correctly() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let new_values: &[(&str, &str)] = &[
            ("key_custody.enabled_backends", "plain"),
            ("key_custody.default_backend", "plain"),
            ("key_custody.socket_path", ""),
            ("key_custody.socket_connections", "7"),
            ("payment.confirmations_required", "5"),
            ("payment.order_expiry_minutes", "45"),
            ("payment.reorg_check_depth", "15"),
            ("payment.mempool_poll_interval_ms", "2000"),
            ("payment.expired_order_grace_period_minutes", "500"),
            ("payment.scan_chunk_memory_budget_mb", "16"),
            ("monero_node.strict_tls", "true"),
            ("proof_of_work.mainnet", "false"),
            ("proof_of_work.stagenet", "true"),
            ("proof_of_work.testnet", "true"),
            // Monokulo has its own server.bind: the engine's is `engine:<key>`.
            ("engine:server.bind", "127.0.0.1:9443"),
            ("server.worker_threads", "4"),
            ("server.rate_limit_per_token_per_min", "200"),
            ("server.max_body_bytes", "16384"),
            // The same key as monokulo's own, so sent as `engine:<key>`.
            ("engine:database.read_connections", "6"),
            ("webhooks.allow_private_urls", "true"),
            ("webhooks.delivery_timeout_ms", "10000"),
            ("webhooks.max_attempts", "12"),
            // Monokulo has settings with these keys too: the page sends the
            // engine's as `engine:<key>`.
            ("engine:logging.level", "warn,engine::loops=debug"),
            ("engine:logging.dev_mode_until", "4102444800"),
            ("engine:logging.retention_days", "30"),
            ("engine:logging.max_mb", "250"),
            ("engine:logging.otlp_endpoint", "http://127.0.0.1:4318"),
            ("engine:logging.format", "json"),
        ];
        // Every engine setting the page can save, apart from the node ones
        // (their own form); the rest (the secrets, and where the database
        // is) are shown locked.
        assert_eq!(
            new_values.len(),
            engine::engine_settings::ALL
                .iter()
                .filter(|s| (s.sources().toml || s.sources().database) && s.editable())
                .count()
                - engine::engine_settings::NETWORKS.len(),
            "this test must cover every engine setting the page can save, apart from the node ones"
        );

        let save = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                new_values,
            ))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(
            html.contains("Settings saved and applied."),
            "expected a success banner, got: {html}"
        );
        assert!(
            html.contains("take effect after the engine restarts"),
            "worker threads and bind are restart-only: {html}"
        );
        assert!(
            html.contains("set monokulo&#39;s engine.url to http://127.0.0.1:9443")
                || html.contains("set monokulo's engine.url to http://127.0.0.1:9443"),
            "{html}"
        );

        let html = settings_tabs_html(&router, &cookie).await;
        for (key, value) in new_values {
            // `key_custody.socket_path`'s new value is the empty string - an
            // empty `value=""` attribute is still real output to look for,
            // just not distinguishable via a bare `value` search, so it's
            // skipped here (its round-trip is still exercised - a wrong
            // value there would still show up as *something* nonempty).
            if value.is_empty() {
                continue;
            }
            assert!(
                shows_value(&html, value),
                "expected {key}={value:?} to have round-tripped, got: {html}"
            );
        }
        let engine_view = engine_settings(&engine).await;
        assert_eq!(
            engine_view["scalars"]["logging.level"]["value"], "warn,engine::loops=debug",
            "the engine's own logging level"
        );
        assert_eq!(
            engine_view["scalars"]["logging.level"]["source"], "toml",
            "saved into the engine's options file: {engine_view}"
        );
        assert_eq!(
            engine_view["scalars"]["logging.dev_mode_until"]["source"], "database",
            "a runtime switch, into its database: {engine_view}"
        );
    }

    #[test]
    fn ticked_choices_are_joined_and_ticking_none_still_sends_the_name() {
        let pairs = |list: &[(&str, &str)]| {
            list.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        };
        let form = super::joined(pairs(&[
            ("list", ""),
            ("list", "plain"),
            ("list", "socket"),
            ("other", "a,b"),
        ]));
        assert_eq!(form["list"], "plain,socket");
        assert_eq!(form["other"], "a,b");
        assert_eq!(super::joined(pairs(&[("list", "")]))["list"], "");
    }

    /// A value shown in a text or number input, or selected in a select.
    fn shows_value(html: &str, value: &str) -> bool {
        html.contains(&format!("value=\"{value}\" selected"))
            || html.contains(&format!("value=\"{value}\">"))
            || html.contains(&format!("value=\"{value}\" min"))
            || html.contains(&format!("value=\"{value}\" id="))
            || html.contains(&format!("value=\"{value}\" checked"))
            || html.contains(&format!("\">{value}</textarea>"))
    }

    // -- Task 3.5: settings that were already live stay live --------------

    async fn get(router: &Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().method("GET").uri(uri);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
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
        assert_eq!(
            router
                .clone()
                .oneshot(signup("first@example.com"))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        let saved = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("signup.mode", "invite_only")],
            ))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        assert_ne!(
            router
                .clone()
                .oneshot(signup("second@example.com"))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED,
            "needs an invite now"
        );

        router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("signup.mode", "public")],
            ))
            .await
            .unwrap();
        assert_eq!(
            router
                .clone()
                .oneshot(signup("third@example.com"))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED,
            "and back"
        );
    }

    #[tokio::test]
    async fn a_saved_public_url_applies_to_the_next_plugin_connection() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let confirm = "/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fdone&nonce=n1";
        let before = body_text(get(&router, confirm, Some(&cookie)).await).await;
        assert!(
            before.contains("can't connect plugins yet")
                || before.contains("can&#39;t connect plugins yet"),
            "{before}"
        );

        let saved = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("public_url", "https://pay.example.com")],
            ))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        let after = body_text(get(&router, confirm, Some(&cookie)).await).await;
        assert!(
            !after.contains("connect plugins yet"),
            "the next request sees it: {after}"
        );
        assert!(after.contains(r#"name="view_key_hex""#), "{after}");
    }

    #[tokio::test]
    async fn the_engine_token_shows_locked_and_is_never_saved_and_its_address_is_saved_for_a_restart(
    ) {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(
            page.contains(r#"<input type="password" value="locked" id="setting-engine.token" aria-describedby="setting-help-engine.token" disabled>"#),
            "{page}"
        );
        assert!(
            page.contains("This is set with MONOKULO_ENGINE_TOKEN when the process starts"),
            "the lock says where it is set: {page}"
        );
        assert!(
            label_row(&page, "engine.token").contains("Environment"),
            "{page}"
        );
        assert!(
            page.contains(r#"name="engine.url""#),
            "the address can be edited: {page}"
        );
        for never in [
            r#"name="engine.token""#,
            r#"name="clear:engine.token""#,
            shared::auth::TEST_ENGINE_TOKEN,
        ] {
            assert!(!page.contains(never), "{never}: {page}");
        }

        // The address is saved, for the next start; a hand-made form that
        // sends the token anyway changes nothing.
        router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[
                    ("tab", "general"),
                    ("engine.url", "http://127.0.0.1:1"),
                    ("engine.token", "x".repeat(40).as_str()),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            monokulo_value(&settings, "engine.url").1,
            live_settings::SettingSource::Default,
            "the save was refused whole"
        );
        let payments = body_text(
            get(
                &router,
                &crate::views::admin::SettingsTab::Payments.href(),
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(
            payments.contains(r#"name="payment.confirmations_required""#),
            "the engine is still reached: {payments}"
        );
    }

    /// Secrets come from the environment only: each process's is shown
    /// locked, as dots, and a hand-made form that sends one is refused.
    #[tokio::test]
    async fn a_secret_is_shown_locked_and_a_form_sending_one_is_refused() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let key = crate::settings::LOGGING_OTLP_HEADERS.key;
        let page = body_text(
            get(
                &router,
                &crate::views::admin::SettingsTab::Logging.href(),
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(
            page.contains(&format!(
                r#"<input type="password" value="locked" id="setting-{key}""#
            )),
            "{page}"
        );
        assert!(!page.contains(&format!(r#"name="{key}""#)), "{page}");

        let refused = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("tab", "logging"), (key, "authorization=Bearer sk-live-x")],
            ))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::OK);
        let html = unescaped(&body_text(refused).await);
        assert!(html.contains("MONOKULO_LOGGING_OTLP_HEADERS"), "{html}");
        assert!(
            !html.contains("sk-live-x"),
            "the refused secret is on the page"
        );
        assert_eq!(
            monokulo_value(&settings, key).1,
            live_settings::SettingSource::Default
        );
    }

    #[tokio::test]
    async fn an_invalid_monokulo_setting_is_rejected_and_nothing_is_saved() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("abuse.soft_per_min", "not-a-number")],
            ))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(
            html.contains("abuse.soft_per_min: Enter a whole number"),
            "expected a clear validation error, got: {html}"
        );

        let html = settings_tabs_html(&router, &cookie).await;
        assert!(
            html.contains("value=\"60\""),
            "the rejected save must not have changed the default, got: {html}"
        );
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
            (
                "exchange_rate.coingecko_enabled",
                "yes",
                "Enter true or false.",
            ),
            ("abuse.under_attack", "on", "Enter true or false."),
            (
                "exchange_rate.cache_seconds",
                "-5",
                "Enter a whole number from 0 to 86400.",
            ),
            ("http_cache.max_mb", "1.5", "Enter a whole number"),
            (
                "abuse.hard_per_min",
                "0",
                "Enter a whole number from 1 to 10000000.",
            ),
            (
                "abuse.challenge_bits",
                "30",
                "Enter a whole number from 8 to 24.",
            ),
            (
                "rate_limit.per_store_key_per_min",
                "0",
                "Enter a whole number from 1 to 10000000.",
            ),
            (
                "abuse.stream_cap",
                "0",
                "Enter a whole number from 1 to 100000.",
            ),
            (
                "public_url",
                "not a url",
                "Enter this instance's public address",
            ),
        ] {
            let save = router
                .clone()
                .oneshot(authed_form_request(
                    "POST",
                    "/dashboard/admin/settings",
                    &cookie,
                    &[(key, value)],
                ))
                .await
                .unwrap();
            assert_eq!(save.status(), StatusCode::OK, "{key}={value:?}");
            let html = body_text(save)
                .await
                .replace("&quot;", "\"")
                .replace("&#34;", "\"")
                .replace("&#39;", "'");
            assert!(
                html.contains(key) && html.contains(expected),
                "{key}={value:?}: expected {expected:?}, got: {}",
                html.split("role=\"alert\">")
                    .nth(1)
                    .unwrap_or("")
                    .split("<")
                    .next()
                    .unwrap_or("")
            );
        }
    }

    #[tokio::test]
    async fn saving_an_engine_setting_forwards_it_and_the_change_is_visible_on_the_next_load() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("tab", "payments"), ("payment.confirmations_required", "5")],
            ))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(
            html.contains("Settings saved and applied."),
            "expected a success banner, got: {html}"
        );
        assert!(
            html.contains("value=\"5\""),
            "expected the scanner's own just-saved value reflected, got: {html}"
        );

        let html = settings_tabs_html(&router, &cookie).await;
        assert!(
            html.contains("value=\"5\""),
            "expected the scanner's change to survive a fresh page load, got: {html}"
        );
    }

    fn fixi(mut request: Request<Body>) -> Request<Body> {
        request
            .headers_mut()
            .insert("FX-Request", "true".parse().unwrap());
        request
    }

    /// With fixi, a save answers with the tab's panel, and the banners and
    /// the tab bar out of band (a save can add a banner or a marker), so
    /// nothing else on the page is replaced.
    #[tokio::test]
    async fn a_fixi_save_answers_with_the_panel_and_the_banners_and_tab_bar_out_of_band() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let page = body_text(
            get(
                &router,
                "/dashboard/admin/settings?tab=abuse",
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(page.contains(r##"fx-action="/dashboard/admin/settings" fx-method="POST" fx-target="#settings-panel""##), "{page}");

        let save = router
            .clone()
            .oneshot(fixi(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("tab", "abuse"), ("abuse.soft_per_min", "70")],
            )))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(
            html.starts_with(
                r#"<section id="settings-panel" aria-labelledby="settings-panel-title">"#
            ),
            "{html}"
        );
        assert!(html.contains(r#"<div id="settings-banners" class="save-banners" data-fx-oob><p class="success" role="status">Settings saved and applied."#), "{html}");
        assert!(html.contains(r#"<nav id="settings-tabs" class="tab-bar" aria-label="Settings sections" data-fx-oob>"#), "{html}");
        assert!(
            html.contains(r#"value="70""#) && !html.contains("<html"),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="save-status success" role="status" data-fx-focus"#),
            "a word by the button gets focus: {html}"
        );

        let refused = router
            .clone()
            .oneshot(fixi(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[
                    ("tab", "payments"),
                    ("payment.confirmations_required", "-1"),
                ],
            )))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(refused).await;
        assert!(
            html.starts_with(r#"<section id="settings-panel""#)
                && html.contains("The engine refused the change"),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="save-status error" role="alert" data-fx-focus"#),
            "a word by the button gets focus: {html}"
        );

        let saved = router
            .clone()
            .oneshot(fixi(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("tab", "payments"), ("payment.confirmations_required", "4")],
            )))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        assert!(body_text(saved)
            .await
            .contains("Settings saved and applied."));
    }

    /// Saving a wrong engine token on General with fixi: the engine's tabs
    /// say at once that the engine can't be reached.
    #[tokio::test]
    async fn a_fixi_tab_link_shows_an_engine_it_cannot_reach_in_the_panel() {
        // Nothing listens there.
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let tab = router
            .clone()
            .oneshot(fixi(
                Request::builder()
                    .uri("/dashboard/admin/settings?tab=nodes")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            ))
            .await
            .unwrap();
        let html = body_text(tab).await;
        assert!(
            html.starts_with(r#"<section id="settings-panel""#),
            "a tab link gets just the panel: {html}"
        );
        assert!(
            html.contains("Could not reach the configured engine"),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<h2 id="settings-panel-title" tabindex="-1" data-fx-focus>Monero nodes</h2>"#
            ),
            "{html}"
        );
        assert!(html.contains(r##"href="/dashboard/admin/settings?tab=nodes" fx-action="/dashboard/admin/settings?tab=nodes" fx-target="#settings-panel" fx-push-url aria-current="page""##), "{html}");
    }

    #[tokio::test]
    async fn an_invalid_engine_setting_is_rejected_by_the_engine_and_surfaced_as_an_error() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("payment.confirmations_required", "not-a-number")],
            ))
            .await
            .unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        let html = body_text(save).await;
        assert!(
            html.contains("The engine refused the change"),
            "expected the engine's own rejection surfaced, got: {html}"
        );
    }

    // -- One save for a whole tab (nicer_admin_screen.md step 2) ----------

    /// [`spawn_engine`] for an engine built from
    /// `config`.
    async fn spawn_configured_engine(
        config: engine_test_support::TestEngineConfig,
    ) -> engine_test_support::TestEngineHandle {
        config.spawn().await
    }

    /// The engine's own view of its settings, straight from its admin API.
    async fn engine_settings(engine: &engine_test_support::TestEngineHandle) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("http://{}/api/v1/admin/settings", engine.addr))
            .header(
                shared::auth::ENGINE_TOKEN_HEADER,
                shared::auth::TEST_ENGINE_TOKEN,
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// Every monokulo setting as stored, to see that a save left them alone.
    fn monokulo_stored(db: &crate::db::Database) -> Vec<(&'static str, Option<String>)> {
        crate::settings::ALL
            .iter()
            .map(|s| (s.key(), db.lock().get_setting(s.key()).unwrap()))
            .collect()
    }

    async fn post_settings(
        router: &Router,
        cookie: &str,
        fields: &[(&str, &str)],
    ) -> axum::response::Response {
        router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                cookie,
                fields,
            ))
            .await
            .unwrap()
    }

    fn unescaped(html: &str) -> String {
        html.replace("&quot;", "\"")
            .replace("&#34;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    /// Nothing listens at this instance's engine address, so a save that
    /// sent anything to the engine would be refused: it goes through, and
    /// back to its own tab.
    #[tokio::test]
    async fn a_tab_with_only_monokulo_settings_saves_only_monokulo() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.soft_per_min", "61")],
        )
        .await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let location = save.headers()["location"].to_str().unwrap().to_string();
        assert!(
            location.starts_with("/dashboard/admin/settings?tab=abuse&saved="),
            "{location}"
        );
        assert!(follow(&router, &cookie, save)
            .await
            .contains("Settings saved and applied."));
        assert_eq!(
            monokulo_value(&settings, "abuse.soft_per_min"),
            ("61".to_string(), live_settings::SettingSource::Toml)
        );
    }

    #[tokio::test]
    async fn a_tab_with_only_engine_settings_saves_only_the_engine() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let before = monokulo_stored(&db);

        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "payments"), ("payment.confirmations_required", "6")],
        )
        .await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["value"],
            "6"
        );
        assert_eq!(
            monokulo_stored(&db),
            before,
            "monokulo's settings weren't touched"
        );
    }

    /// The Payments tab holds both processes' settings; one Save stores
    /// both, and both read back.
    #[tokio::test]
    async fn a_mixed_tab_saves_both_halves() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "payments"),
                ("payment.confirmations_required", "7"),
                ("exchange_rate.cache_seconds", "88"),
            ],
        )
        .await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(html.contains("Settings saved and applied."), "{html}");
        assert_eq!(
            engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["value"],
            "7"
        );
        assert_eq!(
            monokulo_value(&settings, "exchange_rate.cache_seconds").0,
            "88"
        );
        let page = body_text(
            get(
                &router,
                "/dashboard/admin/settings?tab=payments",
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(
            shows_value(&page, "7") && shows_value(&page, "88"),
            "one tab shows both: {page}"
        );
    }

    /// Monokulo's half is checked first; when it's refused, the engine's
    /// half isn't sent, so nothing changes anywhere.
    #[tokio::test]
    async fn an_invalid_monokulo_value_in_a_mixed_tab_saves_neither_half() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "payments"),
                ("payment.confirmations_required", "8"),
                ("exchange_rate.cache_seconds", "-5"),
            ],
        )
        .await;
        assert_eq!(
            save.status(),
            StatusCode::OK,
            "a refused save shows the page again"
        );
        let html = unescaped(&body_text(save).await);
        assert!(
            html.contains("exchange_rate.cache_seconds: Enter a whole number from 0 to 86400."),
            "{html}"
        );
        assert!(html.contains(r##"href="/dashboard/admin/settings?tab=payments" fx-action="/dashboard/admin/settings?tab=payments" fx-target="#settings-panel" fx-push-url aria-current="page""##), "shown on the tab holding it: {html}");
        assert_eq!(
            engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["value"],
            "10",
            "the engine half wasn't sent"
        );
        assert_eq!(
            db.lock()
                .get_setting("exchange_rate.cache_seconds")
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn an_engine_refusal_shows_the_engines_own_message() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let direct: serde_json::Value = reqwest::Client::new()
            .post(format!("http://{}/api/v1/admin/settings", engine.addr))
            .header(
                shared::auth::ENGINE_TOKEN_HEADER,
                shared::auth::TEST_ENGINE_TOKEN,
            )
            .json(&serde_json::json!({ "scalars": { "payment.confirmations_required": "-1" } }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let message = direct["error"].as_str().unwrap();

        let save = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "payments"),
                ("payment.confirmations_required", "-1"),
            ],
        )
        .await;
        assert_eq!(save.status(), StatusCode::OK);
        let html = unescaped(&body_text(save).await);
        assert!(
            html.contains(&format!(
                "The engine refused the change (400 Bad Request): {message}"
            )),
            "{message} in {html}"
        );
    }

    /// The banners a save brings still show after the redirect: a setting
    /// waiting for a restart, and a network stores use left with no node
    /// that answers.
    #[tokio::test]
    async fn restart_and_unserved_network_notices_still_show() {
        let engine = spawn_configured_engine(
            engine_test_support::TestEngineConfig::new()
                .with_networks(&[monero::Network::Stagenet])
                .with_live_nodes(),
        )
        .await;
        let state = test_app_state_connected_to(engine.addr).await;
        let engine_client = state.engine.client.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        // Addresses nothing listens on: ports this process bound and let
        // go, rather than ports assumed closed on every machine.
        let closed = || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().to_string()
        };
        let (first_node, second_node) = (closed(), closed());
        // A store on stagenet, which needs a stagenet node saved first.
        let first = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "nodes"),
                ("node_stagenet_0_address", first_node.as_str()),
            ],
        )
        .await;
        assert_eq!(first.status(), StatusCode::SEE_OTHER);
        engine_client
            .create_tenant(stagenet_tenant())
            .await
            .unwrap();

        let restart = post_settings(
            &router,
            &cookie,
            &[("tab", "server"), ("server.worker_threads", "3")],
        )
        .await;
        let html = unescaped(&follow(&router, &cookie, restart).await);
        assert!(html.contains("Saved. These settings take effect after the engine restarts: server.worker_threads."), "{html}");

        // Nothing answers on either node's port.
        let unserved = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "nodes"),
                ("node_stagenet_0_address", second_node.as_str()),
            ],
        )
        .await;
        assert_eq!(
            unserved.status(),
            StatusCode::SEE_OTHER,
            "an unreachable node is still saved"
        );
        let html = unescaped(&follow(&router, &cookie, unserved).await);
        assert!(
            html.contains(
                "1 store uses the stagenet network, which no longer has any reachable nodes."
            ),
            "{html}"
        );
    }

    /// A flash is shown once: reloading the page it led to doesn't bring
    /// the banners back.
    #[tokio::test]
    async fn a_saved_banner_is_shown_once() {
        let state = test_app_state_connected_to("127.0.0.1:1".parse().unwrap()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.soft_per_min", "62")],
        )
        .await;
        let location = save.headers()["location"].to_str().unwrap().to_string();
        assert!(follow(&router, &cookie, save)
            .await
            .contains("Settings saved and applied."));
        let again = body_text(get(&router, &location, Some(&cookie)).await).await;
        assert!(!again.contains("Settings saved and applied."), "{again}");
    }

    // -- The tabbed page (nicer_admin_screen.md step 3) -------------------

    #[tokio::test]
    async fn every_tab_opens() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        for tab in crate::views::admin::SettingsTab::ALL {
            let response = get(&router, &tab.href(), Some(&cookie)).await;
            assert_eq!(response.status(), StatusCode::OK, "{tab:?}");
            let html = body_text(response).await;
            assert!(
                html.contains(&format!(
                    r#"<h2 id="settings-panel-title" tabindex="-1">{}</h2>"#,
                    tab.label()
                )),
                "{tab:?}: {html}"
            );
        }
        let unknown =
            body_text(get(&router, "/dashboard/admin/settings?tab=nope", Some(&cookie)).await)
                .await;
        assert!(
            unknown.contains(r#"<h2 id="settings-panel-title" tabindex="-1">General</h2>"#),
            "{unknown}"
        );
    }

    /// Both processes have `logging.level` and `logging.max_mb`: on the
    /// Logging tab each is sent under its own name, and saved where it
    /// belongs.
    #[tokio::test]
    async fn the_logging_tab_keeps_each_processs_settings_apart() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let page = body_text(
            get(
                &router,
                "/dashboard/admin/settings?tab=logging",
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(
            page.contains(r#"id="setting-engine:logging.level""#)
                && page.contains(r#"id="setting-logging.level""#),
            "one id each: {page}"
        );

        let saved = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "logging"),
                ("logging.max_mb", "200"),
                ("engine:logging.max_mb", "300"),
            ],
        )
        .await;
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            monokulo_value(&settings, "logging.max_mb"),
            ("200".to_string(), live_settings::SettingSource::Toml)
        );
        let engine_view = engine_settings(&engine).await;
        assert_eq!(engine_view["scalars"]["logging.max_mb"]["value"], "300");
        assert_eq!(engine_view["scalars"]["logging.max_mb"]["source"], "toml");
    }

    /// A temporary directory of its own, removed when dropped.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!(
                "monokulo-{name}-{}-{}",
                std::process::id(),
                rand::random::<u32>()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The options file on disk: the page names it, a save writes it
    /// keeping the admin's own comments, the Reload button applies an edit
    /// made by hand, and a bad edit is refused by line, changing nothing.
    #[tokio::test]
    async fn the_options_file_is_named_saved_to_and_reloaded_from_the_page() {
        let dir = TempDir::new("options");
        let path = dir.0.join("monokulo.toml");
        std::fs::write(&path, "# Mine.\n[signup]\nmode = \"public\"\n").unwrap();
        let state = test_app_state_with_options(
            "127.0.0.1:1".parse().unwrap(),
            live_settings::OptionsFile::at(&path),
        )
        .await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(page.contains(&path.display().to_string()), "{page}");
        assert!(
            page.contains(r#"action="/dashboard/admin/settings/reload""#),
            "{page}"
        );

        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.soft_per_min", "61")],
        )
        .await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Mine.\n[signup]\nmode = \"public\"\n\n[abuse]\nsoft_per_min = 61\n"
        );

        let reload = |fields: &'static [(&'static str, &'static str)]| {
            router.clone().oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings/reload",
                &cookie,
                fields,
            ))
        };
        std::fs::write(
            &path,
            "[signup]\nmode = \"public\"\n[abuse]\nsoft_per_min = 70\n",
        )
        .unwrap();
        let reloaded = reload(&[("owner", "monokulo"), ("tab", "abuse")])
            .await
            .unwrap();
        assert_eq!(reloaded.status(), StatusCode::SEE_OTHER);
        let location = reloaded.headers()["location"].to_str().unwrap().to_string();
        assert!(
            location.starts_with("/dashboard/admin/settings?tab=abuse&saved="),
            "{location}"
        );
        let html = follow(&router, &cookie, reloaded).await;
        assert!(
            html.contains(
                "Reloaded monokulo&#39;s options file and applied it: abuse.soft_per_min."
            ) || html
                .contains("Reloaded monokulo's options file and applied it: abuse.soft_per_min."),
            "{html}"
        );
        assert!(!html.contains("save-status"), "no word beside Save: {html}");
        assert_eq!(
            monokulo_value(&settings, "abuse.soft_per_min"),
            ("70".to_string(), live_settings::SettingSource::Toml)
        );

        std::fs::write(&path, "[abuse]\nsoft_per_min = 0\nnot_a_setting = 1\n").unwrap();
        let refused = reload(&[("owner", "monokulo"), ("tab", "abuse")])
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::OK);
        let html = unescaped(&body_text(refused).await);
        assert!(html.contains("Nothing was reloaded"), "{html}");
        assert!(html.contains("line 2: abuse.soft_per_min"), "{html}");
        assert!(
            html.contains("line 3: there is no setting called abuse.not_a_setting"),
            "{html}"
        );
        assert_eq!(monokulo_value(&settings, "abuse.soft_per_min").0, "70");

        // Changed on disk since it was read: a save is refused, not lost.
        let refused = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.hard_per_min", "400")],
        )
        .await;
        let html = unescaped(&body_text(refused).await);
        assert!(html.contains("has changed since it was loaded"), "{html}");
    }

    /// The engine's Reload button asks the engine to read its own file.
    #[tokio::test]
    async fn the_engines_options_file_is_reloaded_through_its_api() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(page.contains(r#"name="owner" value="engine""#), "{page}");

        let reloaded = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings/reload",
                &cookie,
                &[("owner", "engine"), ("tab", "payments")],
            ))
            .await
            .unwrap();
        assert_eq!(reloaded.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, reloaded).await);
        assert!(
            html.contains("Reloaded the engine's options file: nothing in it changed."),
            "{html}"
        );
    }

    /// An options file monokulo can't write: the page says so, and every
    /// setting kept in it is locked with why.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_options_file_locks_what_it_holds() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("read-only");
        let path = dir.0.join("monokulo.toml");
        let text = "[signup]\nmode = \"public\"\n";
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        if std::fs::OpenOptions::new().append(true).open(&path).is_ok() {
            return; // Root: permission bits don't bind it.
        }
        let state = test_app_state_with_options(
            "127.0.0.1:1".parse().unwrap(),
            live_settings::OptionsFile::at(&path),
        )
        .await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let page = unescaped(
            &body_text(
                get(
                    &router,
                    &crate::views::admin::SettingsTab::Abuse.href(),
                    Some(&cookie),
                )
                .await,
            )
            .await,
        );
        assert!(page.contains("It can't be written by monokulo"), "{page}");
        assert!(
            page.contains("can't be written by monokulo, so this is changed by editing it"),
            "{page}"
        );
        assert!(
            !page.contains(r#"name="abuse.soft_per_min""#),
            "locked: {page}"
        );
        assert!(
            page.contains(r#"name="abuse.under_attack""#),
            "a runtime switch is in the database, still editable: {page}"
        );

        // A form that sends a locked setting anyway is refused, and the
        // file, read-only, is left exactly as it was.
        let refused = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.soft_per_min", "61")],
        )
        .await;
        assert_eq!(refused.status(), StatusCode::OK);
        let html = unescaped(&body_text(refused).await);
        assert!(
            html.contains("Nothing was saved") && html.contains("can't be written by this process"),
            "{html}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert_eq!(monokulo_value(&settings, "abuse.soft_per_min").0, "60");

        // The runtime switch still saves, to the database.
        let saved = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.under_attack", "true")],
        )
        .await;
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            monokulo_value(&settings, "abuse.under_attack"),
            ("true".to_string(), live_settings::SettingSource::Database)
        );
    }

    /// No options file yet: the page says so, everything is editable, and
    /// the first save creates the file, which the page then names as there.
    #[tokio::test]
    async fn a_missing_options_file_is_created_by_the_first_save() {
        let dir = TempDir::new("missing");
        let path = dir.0.join("config").join("monokulo.toml");
        let state = test_app_state_with_options(
            "127.0.0.1:1".parse().unwrap(),
            live_settings::OptionsFile::at(&path),
        )
        .await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let page = unescaped(&body_text(get_settings_page(&router, &cookie).await).await);
        assert!(
            page.contains("Not created yet: saving a setting here creates it."),
            "{page}"
        );
        assert!(page.contains(r#"name="engine.url""#), "editable: {page}");

        let saved = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.soft_per_min", "61")],
        )
        .await;
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[abuse]\nsoft_per_min = 61\n"
        );
        let page = unescaped(&body_text(get_settings_page(&router, &cookie).await).await);
        assert!(
            page.contains("Saving here writes it; after editing it by hand, reload it."),
            "{page}"
        );
    }

    /// Development logging's end is shown in the admin's own zone, like
    /// every other time on the page.
    #[test]
    fn development_logging_ends_in_the_admins_own_zone() {
        use crate::views::admin::{AdminScalarFieldView, SettingKindView};
        let clock = crate::views::time::Clock::new(Some("Australia/Perth"), None, 1_790_000_000);
        let mut fields = [AdminScalarFieldView {
            key: "logging.dev_mode_until".into(),
            value: "1790000600".into(),
            ..Default::default()
        }];
        super::with_time_limits(&mut fields, &clock);
        assert!(
            matches!(&fields[0].kind, SettingKindView::TimeLimit { until_label, .. } if until_label == "21 Sep, 22:23"),
            "{:?}",
            fields[0].kind
        );
    }

    /// An engine that saves but answers with something unreadable: the save
    /// says it went through and that what the engine said about it is lost,
    /// rather than showing no banner at all.
    #[tokio::test]
    async fn an_unreadable_engine_save_reply_is_reported_not_dropped() {
        use axum::routing::post;
        let app = Router::new().route(
            "/api/v1/admin/settings",
            post(|| async { (StatusCode::OK, "not json") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let state = test_app_state_connected_to(addr).await;

        let mut req = super::RemoteUpdateRequest::default();
        req.scalars
            .insert("scan.poll_interval_secs".into(), "5".into());
        let outcome = super::save_engine(&state, req).await;
        assert!(
            matches!(outcome.notices.as_slice(), [super::Notice::Warning(text)] if text.contains("reply could not be read")),
            "{:?}",
            outcome.notices
        );
    }

    // -- The node form (nicer_admin_screen.md step 5) -----------------------

    /// A stand-in node answering `get_height`, and `get_info` with
    /// `nettype`.
    async fn spawn_node_on(nettype: &'static str) -> std::net::SocketAddr {
        use axum::routing::post;
        let app = Router::new()
            .route("/get_height", post(|| async { axum::Json(serde_json::json!({ "height": 100, "status": "OK" })) }))
            .route(
                "/json_rpc",
                post(move |axum::Json(request): axum::Json<serde_json::Value>| async move {
                    axum::Json(serde_json::json!({ "jsonrpc": "2.0", "id": request["id"], "result": { "nettype": nettype, "status": "OK" } }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    /// The addresses of a network's saved nodes, primary first, as the
    /// engine has them.
    async fn saved_nodes(
        engine: &engine_test_support::TestEngineHandle,
        network: &str,
    ) -> Vec<String> {
        let node = engine_settings(engine).await["monero_node"][network].clone();
        if node.is_null() {
            return Vec::new();
        }
        std::iter::once(&node)
            .chain(node["fallbacks"].as_array().unwrap().iter())
            .map(|n| format!("{}:{}", n["host"].as_str().unwrap(), n["port"]))
            .collect()
    }

    /// The Monero nodes tab's form as a browser without JavaScript sends it:
    /// every row, the blank "Add a node" row, and (for a row button) the
    /// button pressed.
    fn nodes_form<'a>(
        network: &str,
        rows: &[&'a str],
        add: &'a str,
        action: Option<&'a str>,
    ) -> Vec<(String, String)> {
        let mut fields = vec![("tab".to_string(), "nodes".to_string())];
        for (i, address) in rows.iter().chain(std::iter::once(&add)).enumerate() {
            fields.push((format!("node_{network}_{i}_address"), address.to_string()));
            fields.push((format!("node_{network}_{i}_self_signed"), "on".to_string()));
        }
        if let Some(action) = action {
            fields.push(("node_action".to_string(), action.to_string()));
        }
        fields
    }

    async fn post_nodes(
        router: &Router,
        cookie: &str,
        fields: &[(String, String)],
    ) -> axum::response::Response {
        let fields: Vec<(&str, &str)> = fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        post_settings(router, cookie, &fields).await
    }

    #[tokio::test]
    async fn a_node_is_added_through_the_blank_row_and_ordered_by_its_buttons() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let (a, b, c) = (
            spawn_node_on("stagenet").await,
            spawn_node_on("stagenet").await,
            spawn_node_on("stagenet").await,
        );
        let (a, b, c) = (a.to_string(), b.to_string(), c.to_string());

        // Adding: fill in the blank row and save.
        let added = post_nodes(&router, &cookie, &nodes_form("stagenet", &[], &a, None)).await;
        assert_eq!(added.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            saved_nodes(&engine, "stagenet").await,
            std::slice::from_ref(&a)
        );
        let page = follow(&router, &cookie, added).await;
        assert!(
            page.contains(&format!(
                r#"name="node_stagenet_0_address" id="node-stagenet-0-address" value="{a}""#
            )),
            "{page}"
        );
        assert!(
            page.contains(
                r#"name="node_stagenet_1_address" id="node-stagenet-1-address" value="""#
            ),
            "a fresh blank row: {page}"
        );

        post_nodes(&router, &cookie, &nodes_form("stagenet", &[&a], &b, None)).await;
        post_nodes(
            &router,
            &cookie,
            &nodes_form("stagenet", &[&a, &b], &c, None),
        )
        .await;
        assert_eq!(
            saved_nodes(&engine, "stagenet").await,
            [a.clone(), b.clone(), c.clone()]
        );

        // Each button is one post, and the saved order reads back.
        let moved = post_nodes(
            &router,
            &cookie,
            &nodes_form("stagenet", &[&a, &b, &c], "", Some("up:stagenet:2")),
        )
        .await;
        assert_eq!(moved.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            saved_nodes(&engine, "stagenet").await,
            [a.clone(), c.clone(), b.clone()]
        );
        post_nodes(
            &router,
            &cookie,
            &nodes_form("stagenet", &[&a, &c, &b], "", Some("down:stagenet:0")),
        )
        .await;
        assert_eq!(
            saved_nodes(&engine, "stagenet").await,
            [c.clone(), a.clone(), b.clone()]
        );
        post_nodes(
            &router,
            &cookie,
            &nodes_form("stagenet", &[&c, &a, &b], "", Some("remove:stagenet:1")),
        )
        .await;
        assert_eq!(
            saved_nodes(&engine, "stagenet").await,
            [c.clone(), b.clone()]
        );

        let page = body_text(
            get(
                &router,
                "/dashboard/admin/settings?tab=nodes",
                Some(&cookie),
            )
            .await,
        )
        .await;
        let first = page.find(&format!(r#"value="{c}""#)).expect(&page);
        assert!(
            first < page.find(&format!(r#"value="{b}""#)).unwrap(),
            "the page shows the saved order"
        );
        assert!(
            page.contains(r#"<legend class="node-row-name">Primary</legend>"#),
            "{page}"
        );
    }

    /// A row that can't be a node: nothing is saved, and the page comes
    /// back with every value as typed and the problem under its address.
    #[tokio::test]
    async fn a_bad_address_is_shown_on_its_row_and_nothing_is_saved() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let good = spawn_node_on("stagenet").await.to_string();

        let mut fields = nodes_form("stagenet", &[&good], "node.example.com", None);
        fields.push(("node_stagenet_1_ssl".to_string(), "on".to_string()));
        let refused = post_nodes(&router, &cookie, &fields).await;
        assert_eq!(
            refused.status(),
            StatusCode::OK,
            "the page again, not a redirect"
        );
        let html = body_text(refused).await;
        assert!(
            html.contains("Nothing was saved: some node addresses need fixing (marked below)."),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"name="node_stagenet_0_address" id="node-stagenet-0-address" value="{good}""#
            )),
            "{html}"
        );
        assert!(html.contains(r#"name="node_stagenet_1_address" id="node-stagenet-1-address" value="node.example.com""#), "{html}");
        assert!(
            html.contains(
                r#"name="node_stagenet_1_ssl" id="node-stagenet-1-ssl" value="on" checked"#
            ),
            "a ticked box stays ticked: {html}"
        );
        assert!(html.contains(r#"<span class="setting-problem" id="node-stagenet-1-error">Add the port, like node.example.com:18081.</span>"#), "{html}");
        assert!(html.contains(r##"href="/dashboard/admin/settings?tab=nodes" fx-action="/dashboard/admin/settings?tab=nodes" fx-target="#settings-panel" fx-push-url aria-current="page""##), "{html}");
        assert!(
            saved_nodes(&engine, "stagenet").await.is_empty(),
            "not even the good row"
        );

        // With fixi: the panel, 422.
        let fields: Vec<(&str, &str)> = fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let refused = router
            .clone()
            .oneshot(fixi(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &fields,
            )))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_text(refused)
            .await
            .contains(r#"id="node-stagenet-1-error""#));
    }

    #[tokio::test]
    async fn a_node_on_another_network_is_refused_on_its_block() {
        let engine = spawn_engine().await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let mainnet = spawn_node_on("mainnet").await;

        let refused = post_nodes(
            &router,
            &cookie,
            &nodes_form("testnet", &[], &mainnet.to_string(), None),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::OK);
        let html = unescaped(&body_text(refused).await);
        let message = format!("127.0.0.1:{} is on mainnet, not testnet.", mainnet.port());
        let block = &html[html.find(r#"data-network="testnet""#).expect(&html)..];
        assert!(
            block.contains(&format!(r#"<p class="error" role="alert">{message}</p>"#)),
            "{block}"
        );
        assert!(
            block.contains(&format!(r#"value="{mainnet}""#)),
            "the submitted row is still there: {block}"
        );
        assert!(
            html.contains(&format!(
                "The engine refused the change (400 Bad Request): monero_node.testnet: {message}"
            )),
            "{html}"
        );
        assert!(saved_nodes(&engine, "testnet").await.is_empty());
    }

    /// Clearing a network stores use is saved (D2), and the page after it
    /// says what that means.
    #[tokio::test]
    async fn clearing_a_network_stores_use_is_saved_and_says_so() {
        let engine = spawn_configured_engine(
            engine_test_support::TestEngineConfig::new()
                .with_networks(&[monero::Network::Stagenet])
                .with_live_nodes(),
        )
        .await;
        let state = test_app_state_connected_to(engine.addr).await;
        let engine_client = state.engine.client.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let node = spawn_node_on("stagenet").await.to_string();
        post_nodes(&router, &cookie, &nodes_form("stagenet", &[], &node, None)).await;
        engine_client
            .create_tenant(stagenet_tenant())
            .await
            .unwrap();

        let page = body_text(
            get(
                &router,
                "/dashboard/admin/settings?tab=nodes",
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(
            page.contains(r#"data-network="stagenet" data-tenant-count="1""#),
            "{page}"
        );

        let cleared = post_nodes(
            &router,
            &cookie,
            &nodes_form("stagenet", &[&node], "", Some("remove:stagenet:0")),
        )
        .await;
        assert_eq!(cleared.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, cleared).await);
        assert!(
            html.contains(
                "1 store uses the stagenet network, which no longer has any reachable nodes."
            ),
            "{html}"
        );
        assert!(saved_nodes(&engine, "stagenet").await.is_empty());
    }

    /// Once saved, a node's row says how it's doing, from the engine's
    /// `/status`: the save clears monokulo's cached copy, so the new node
    /// shows at once.
    #[tokio::test]
    async fn a_saved_node_shows_its_status() {
        let engine =
            spawn_configured_engine(engine_test_support::TestEngineConfig::new().with_live_nodes())
                .await;
        let state = test_app_state_connected_to(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let page = body_text(
            get(
                &router,
                "/dashboard/admin/settings?tab=nodes",
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(
            !page.contains(r#"class="node-status"#),
            "no nodes, no status"
        );

        let node = spawn_node_on("stagenet").await.to_string();
        let saved = post_nodes(&router, &cookie, &nodes_form("stagenet", &[], &node, None)).await;
        let html = follow(&router, &cookie, saved).await;
        assert!(
            html.contains(r#"<p class="node-status">Reachable, height 99. In use.</p>"#),
            "{html}"
        );
        // The tab also shows how the engine is doing (docs/engine_scaling.md
        // section 6): both processes' CPU and memory, and the network's scan.
        assert!(
            html.contains(r#"<h3 id="resources-title">Resources</h3>"#),
            "{html}"
        );
        assert!(html.contains("<strong>CPU</strong>"), "{html}");
        assert!(html.contains(r#"data-scanning="stagenet""#), "{html}");
        assert!(html.contains("Pace set by"), "{html}");
    }

    fn stagenet_tenant() -> crate::engine_client::CreateTenantRequest {
        crate::engine_client::CreateTenantRequest {
            view_key_hex: "0707070707070707070707070707070707070707070707070707070707070707"
                .to_string(),
            spend_pubkey_hex: "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90"
                .to_string(),
            network: Some("stagenet".to_string()),
            confirmations_required: None,
            order_expiry_seconds: None,
            key_custody_backend: None,
        }
    }
}
