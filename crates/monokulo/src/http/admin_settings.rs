//! `GET`/`POST /dashboard/admin/settings` -
//! the one admin page every monokulo *and* scanner setting can be managed
//! from, per the product spec ("We need an admin page that makes it so that
//! all the monokulo + scanner settings can be set from the admin web page").
//! Gated by [`AuthedAdmin`] end to end - a merchant with a perfectly valid
//! session still gets `403` here, same as the nav only shows the "admin"
//! link to the one instance-wide admin account (`is_admin`, `crate::db`).
//!
//! **Two owners, all of a tab or none of it.** The page is split into tabs
//! by job (`views::admin::SettingsTab`), and each tab into cards
//! (`views::admin::Group`); a tab can hold both processes' settings. The
//! tab's one Save (the save bar) posts the whole tab here, and [`save`]
//! has both processes check it first (monokulo's registry, the engine's
//! `POST /api/v1/admin/settings/check`), which say what changed: anything
//! refused and nothing is saved; otherwise the engine's part is saved, then
//! monokulo's (`save_tab`). Monokulo's own
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
    AdminNetworkFieldView, AdminScalarFieldView, AdminSettingsViewModel, Failure, Group,
    NodeRowView, NodeStatusView, Notice, OptionsFileView, SaveOutcome, SettingKindView,
    SettingOwner, SettingSourceView, SettingsTab, Toast, ToastKind,
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
                .or_else(|| only_for_a_remote_engine(state, view.key))
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
                saved_value: None,
            }
        })
        .collect()
}

/// Why monokulo's own `key` can't be set here while the engine runs inside
/// monokulo: the engine's URL is only for a remote engine, and saved now it
/// would stop monokulo at its next start.
fn only_for_a_remote_engine(state: &AppState, key: &str) -> Option<String> {
    (state.engine.client.is_embedded() && key == crate::settings::ENGINE_URL.key).then(|| {
        "Only used with a remote engine (engine.mode = remote): the engine runs inside monokulo."
            .to_string()
    })
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

/// Fetches the engine's own settings through its admin API; `Err` for a
/// reachability, auth or parse failure worth showing.
async fn fetch_engine_settings(
    engine: &crate::engine_client::EngineClient,
) -> Result<EngineSettings, String> {
    let response = engine
        .get_settings()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("the engine responded with {}", response.status()));
    }
    let parsed: RemoteSettingsResponse = response
        .json()
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
            saved_value: None,
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
            let rows: Vec<NodeRowView> = admin_nodes::rows_from_setting(value.as_ref())
                .into_iter()
                .enumerate()
                .map(|(at, (label, row))| NodeRowView {
                    row,
                    label,
                    status: None,
                    saved_index: Some(at),
                    saved: None,
                })
                .collect();
            AdminNetworkFieldView {
                saved_count: rows.len(),
                rows,
                network,
                example_address,
                tenant_count: meta.tenant_count,
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
        let label = |value: &str| clock.text(value.trim().parse().unwrap_or(0));
        field.kind = SettingKindView::TimeLimit {
            now,
            until_label: label(&field.value),
            saved_until_label: field.saved_value.as_deref().map(label),
        };
    }
}

/// What the page says about a save (or a reload), when it answers one.
#[derive(Default)]
struct SaveResult {
    /// Banners: what stays true after the save.
    notices: Vec<Notice>,
    toast: Option<Toast>,
    /// What the save did, for the cards and the save bar.
    outcome: Option<SaveOutcome>,
    /// What was sent for a refused save, by form name, shown again to fix.
    posted: HashMap<String, String>,
    /// The node rows as submitted for refused or changed networks of a
    /// refused save, shown again (with what's wrong).
    nodes: Option<NodeForm>,
    /// A refusal's word on particular settings, by form name (an engine
    /// setting monokulo also has is `engine:<key>`), beside each.
    field_errors: Vec<(String, String)>,
}

/// A refused save's node rows, as sent, each matched to the saved node at
/// its address (each saved node once): so the page's script counts only
/// what was changed, added, taken out or moved, and Discard knows what was
/// saved.
fn rows_shown_again(sent: &[admin_nodes::NodeRow], saved: &[NodeRowView]) -> Vec<NodeRowView> {
    let label = |address: &str| admin_nodes::parse_address(address).map(|a| a.label()).ok();
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let mut taken = vec![false; saved.len()];
    sent.iter()
        .map(|row| {
            let at = label(&row.address).and_then(|wanted| {
                saved.iter().enumerate().position(|(i, s)| {
                    !taken[i] && label(&s.row.address).is_some_and(|l| same(&l, &wanted))
                })
            });
            if let Some(at) = at {
                taken[at] = true;
            }
            let matched = at.map(|at| &saved[at]);
            NodeRowView {
                label: label(&row.address).unwrap_or_default(),
                row: row.clone(),
                status: matched.and_then(|m| m.status.clone()),
                saved_index: matched.and(at),
                saved: matched
                    .map(|m| &m.row)
                    .filter(|saved| {
                        (&saved.address, saved.ssl, saved.self_signed, &saved.zmq_pub)
                            != (&row.address, row.ssl, row.self_signed, &row.zmq_pub)
                    })
                    .cloned(),
            }
        })
        .collect()
}

/// A refused save's value for `field`, shown in place of the saved one,
/// which it keeps for the page's script (and Discard).
fn show_posted(field: &mut AdminScalarFieldView, posted: &HashMap<String, String>) {
    if let Some(value) = posted.get(field.form_name()) {
        field.saved_value = Some(std::mem::replace(&mut field.value, value.clone()));
    }
}

async fn build_view_model(
    state: &AppState,
    admin: &UserRow,
    tab: SettingsTab,
    result: SaveResult,
) -> AdminSettingsViewModel {
    let SaveResult {
        notices,
        toast,
        outcome,
        posted,
        nodes,
        field_errors,
    } = result;
    let clock = views::time::Clock::for_user(admin);
    // By form name: monokulo and the engine both have `logging.level`.
    let problem = |field: &mut AdminScalarFieldView| {
        if let Some((_, message)) = field_errors
            .iter()
            .find(|(name, _)| name == field.form_name())
        {
            field.problem = Some(message.clone());
        }
    };
    let mut monokulo_fields = monokulo_fields(state);
    // A refused save's word on a setting, beside it, and what was typed.
    for field in &mut monokulo_fields {
        problem(field);
        show_posted(field, &posted);
    }
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
        notices,
        toast,
        outcome,
        monokulo_fields,
        options_files,
        ..Default::default()
    };
    match fetch_engine_settings(&state.engine.client).await {
        Ok(engine) => {
            let mut fields = engine.fields;
            for field in &mut fields {
                problem(field);
                show_posted(field, &posted);
            }
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
                network.rows = rows_shown_again(rows, &network.rows);
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
                    attach_node_status(&mut view.engine_networks, &status);
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
/// one that reports `fakechain`, or nothing, isn't called wrong. How the
/// scan and the link to the node in use are doing is on the engine page.
fn attach_node_status(
    networks: &mut [AdminNetworkFieldView],
    status: &crate::engine_client::EngineStatusResponse,
) {
    for network in networks {
        let Some(reported) = status
            .networks
            .iter()
            .find(|n| n.network == network.network)
        else {
            continue;
        };
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
    welcome: Option<bool>,
    /// The flash a save without JavaScript left for the page it redirected
    /// to ([`FLASHES`]).
    saved: Option<String>,
}

/// `GET /dashboard/admin/settings?tab=<id>` - one tab of the page
/// (General when `tab` is missing or unknown). With fixi (a tab link), just
/// the panel, with the tab bar, banners and toast out of band.
pub async fn page(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    Query(query): Query<SettingsPageQuery>,
) -> Response {
    let tab = SettingsTab::from_id(query.tab.as_deref());
    let result = match query.saved.as_deref().and_then(take_flash) {
        Some(flash) => flash.result,
        None => SaveResult {
            notices: query
                .welcome
                .filter(|v| *v)
                .map(|_| Notice::Success("Your admin account is ready. Connect a Monero node below to start scanning payments. Save its address and check the connection status.".to_string()))
                .into_iter()
                .collect(),
            ..Default::default()
        },
    };
    let view = build_view_model(&state, &admin_user, tab, result).await;
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
    // An on/off switch (`views::controls::switch`) sends `true` only when
    // on; its `switches` field says it was on the form, so off is `false`.
    let mut switches = Vec::new();
    for (name, value) in pairs {
        if name == "switches" {
            switches.push(value);
            continue;
        }
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
    for name in switches {
        form.entry(name).or_insert_with(|| "false".to_string());
    }
    form
}

/// A submitted tab, split by owner (nicer_admin_screen.md T3): monokulo's
/// settings are the names its registry knows; everything else is the
/// engine's, the node form included (`monero_node.<network>`). The whole
/// tab is sent to both checks, which say what changed.
#[derive(Default)]
struct SubmittedTab {
    monokulo: HashMap<String, String>,
    engine: RemoteUpdateRequest,
    /// The values as sent, by form name, to show again if the save is
    /// refused; never a secret, or what the page shows locked.
    posted: HashMap<String, String>,
    /// The node rows as sent, with a row button applied.
    nodes: Option<NodeForm>,
    /// The networks whose rows have something to fix: refused before
    /// anything is asked.
    bad_networks: Vec<monero::Network>,
}

fn split_tab(
    form: &HashMap<String, String>,
    monokulo_fields: &[AdminScalarFieldView],
    engine_fields: &[AdminScalarFieldView],
) -> SubmittedTab {
    let mut tab = SubmittedTab::default();
    let editable =
        |f: &&AdminScalarFieldView| f.locked.is_none() && f.kind != SettingKindView::Secret;
    for (name, value) in form {
        if name == "tab" || admin_nodes::is_node_field(name) {
            continue;
        }
        let engine_key = name.strip_prefix("engine:");
        let shown = match engine_key {
            None if is_monokulo_key(name) => {
                tab.monokulo.insert(name.clone(), value.clone());
                monokulo_fields.iter().find(|f| f.key == *name)
            }
            other => {
                let key = other.unwrap_or(name);
                tab.engine.scalars.insert(key.to_string(), value.clone());
                engine_fields.iter().find(|f| f.key == key)
            }
        };
        if shown.filter(editable).is_some() {
            tab.posted.insert(name.clone(), value.clone());
        }
    }
    if form.keys().any(|name| admin_nodes::is_node_field(name)) {
        let nodes = NodeForm::from_form(form, &admin_nodes::NETWORKS);
        for (network, rows) in &nodes.networks {
            if rows.iter().any(|row| row.error.is_some()) {
                tab.bad_networks.push(*network);
            } else {
                tab.engine.monero_node.insert(
                    shared::network::network_str(*network).to_string(),
                    admin_nodes::rows_to_setting(rows),
                );
            }
        }
        tab.nodes = Some(nodes);
    }
    tab
}

/// Why one process refused its part of a save (or of its check): the
/// settings it names, each with what's wrong in its own words, and what to
/// say when it names none.
#[derive(Debug)]
struct Refusal {
    fields: Vec<live_settings::FieldError>,
    message: String,
}

impl Refusal {
    fn new(message: String) -> Refusal {
        Refusal {
            fields: Vec::new(),
            message,
        }
    }
}

/// A registry's refusal as the page says it.
fn registry_refusal(error: live_settings::SaveError) -> Refusal {
    match error {
        live_settings::SaveError::Invalid(errors) => Refusal {
            message: errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            fields: errors,
        },
        // The options file changed since it was read, or can't be written:
        // the admin reloads it, or fixes its permissions.
        live_settings::SaveError::Store(e) => {
            tracing::warn!(error = %e, "monokulo's settings could not be stored");
            Refusal::new(format!("Nothing was saved: {e}"))
        }
        e => {
            tracing::error!(error = %e, "saving monokulo settings failed");
            Refusal::new(
                "Something went wrong saving these settings. Please try again.".to_string(),
            )
        }
    }
}

/// What a save leaves to say besides whether it saved, by where it's said.
#[derive(Debug, Clone, PartialEq)]
enum SaveNote {
    /// Saved, and waits for a restart: the toast says so, in amber.
    Restart(String),
    /// Worth knowing (an environment variable still wins): in the toast.
    Remark(String),
    /// Still true after the save (a network stores use without a node): a
    /// banner above the tabs.
    Banner(String),
}

/// Checks monokulo's part of a tab as its save would save it
/// (`live_settings::Registry::check`), saving nothing. A setting the page
/// shows locked is refused, as the save would refuse it; SEV-SNP key entry
/// settings that would change are checked against the engine's too.
async fn check_monokulo(
    state: &AppState,
    form: &HashMap<String, String>,
    engine_backends: Option<&str>,
) -> Result<live_settings::CheckReport, Refusal> {
    if form.is_empty() {
        return Ok(live_settings::CheckReport::default());
    }
    let Some(registry) = state.settings.registry.as_ref() else {
        return Err(Refusal::new(
            "Settings can't be saved on this instance.".to_string(),
        ));
    };
    // Shown locked; a form that sends it anyway is refused.
    if let Some(why) = form
        .keys()
        .find_map(|key| only_for_a_remote_engine(state, key))
    {
        return Err(Refusal {
            fields: vec![live_settings::FieldError::new(
                crate::settings::ENGINE_URL.key,
                why.clone(),
            )],
            message: why,
        });
    }
    let report = registry
        .check(monokulo_changes(form))
        .await
        .map_err(registry_refusal)?;
    use live_settings::Section;
    let site_changed = crate::settings::SnpEntryPolicy::keys()
        .iter()
        .any(|setting| report.would().changed.contains(&setting.key()));
    if let Some(refused) =
        check_snp_entry_against_engine(state, registry, form, engine_backends, site_changed).await
    {
        return Err(refused);
    }
    Ok(report)
}

/// The engine's setting that turns its key custody backends on: the same
/// save's value is what this site's SEV-SNP key entry is checked against.
const ENGINE_ENABLED_BACKENDS: &str = "key_custody.enabled_backends";

/// Monokulo's part of a form as its registry takes it.
fn monokulo_changes(form: &HashMap<String, String>) -> live_settings::Changes {
    crate::settings::ALL
        .iter()
        .filter_map(|setting| {
            let value = form.get(setting.key())?;
            Some((setting.key().to_string(), Some(value.clone())))
        })
        .collect()
}

/// Saves monokulo's part of a tab through its registry, after its check:
/// what it leaves to say, or why it was refused (the options file changed
/// on disk since the check, say).
async fn commit_monokulo(
    state: &AppState,
    form: &HashMap<String, String>,
) -> Result<Vec<SaveNote>, Refusal> {
    let Some(registry) = state.settings.registry.as_ref() else {
        return Err(Refusal::new(
            "Settings can't be saved on this instance.".to_string(),
        ));
    };
    match registry.save(monokulo_changes(form)).await {
        Ok(report) => {
            let mut notes = Vec::new();
            for warning in &report.warnings {
                // The only monokulo warning today: the engine didn't answer
                // at the saved URL (decision D4).
                notes.push(SaveNote::Banner(warning.message.clone()));
            }
            if !report.env_overridden.is_empty() {
                notes.push(SaveNote::Remark(format!(
                    "Saved, but these are set by an environment variable, which wins while it is set: {}.",
                    report.env_overridden.join(", ")
                )));
            }
            if !report.restart_required.is_empty() {
                notes.push(SaveNote::Restart(format!(
                    "Saved. These take effect after monokulo restarts: {}.",
                    report.restart_required.join(", ")
                )));
            }
            Ok(notes)
        }
        // Stored, but applying them failed: retrying would fail the same way.
        Err(live_settings::SaveError::Install(message)) => {
            tracing::error!(error = %message, "monokulo settings were saved but applying them failed");
            Ok(vec![SaveNote::Banner(format!(
                "Saved, but applying the new values failed ({message}). Restart monokulo to apply them."
            ))])
        }
        Err(e) => Err(registry_refusal(e)),
    }
}

/// The SEV-SNP key entry settings as `form` would leave them, checked
/// against the engine's before anything is saved: a policy that disagrees
/// with the engine's (or requires a backend it doesn't have) is refused,
/// each setting with what differs. A change isn't saved while the engine
/// can't be asked. `None` lets the save go on (values that don't parse are
/// the registry's to refuse).
async fn check_snp_entry_against_engine(
    state: &AppState,
    registry: &live_settings::Registry,
    form: &HashMap<String, String>,
    engine_backends: Option<&str>,
    site_changed: bool,
) -> Option<Refusal> {
    use crate::settings::{
        SnpEntryPolicy, KEY_CUSTODY_SNP_ENTRY_ID_KEY as ID_KEY,
        KEY_CUSTODY_SNP_ENTRY_MIN_GUEST_SVN as MIN_SVN, KEY_CUSTODY_SNP_ENTRY_MIN_TCB as MIN_TCB,
        KEY_CUSTODY_SNP_ENTRY_REQUIRED as REQUIRED,
    };
    // Checked when this site's own policy changes, as before; the engine's
    // backends a save changes on their own are the status page's to alert
    // about, not a reason to refuse it.
    if !site_changed {
        return None;
    }
    let saved: HashMap<&str, String> = registry
        .describe()
        .into_iter()
        .map(|view| (view.key, view.value))
        .collect();
    let value = |key: &str| {
        form.get(key)
            .or_else(|| saved.get(key))
            .cloned()
            .unwrap_or_default()
    };
    let proposed = SnpEntryPolicy::from_values(
        ID_KEY.parse(&value(ID_KEY.key)).ok()?.as_deref(),
        MIN_SVN.parse(&value(MIN_SVN.key)).ok()?,
        MIN_TCB.parse(&value(MIN_TCB.key)).ok()?.as_deref(),
        REQUIRED.parse(&value(REQUIRED.key)).ok()?,
    )
    .ok()?;
    let changed = proposed != *state.settings.snp_entry.load();
    if changed {
        // Asked now, not taken from a status cached before the change.
        super::status_page::invalidate_status_cache(&state.engine);
    }
    let status = match super::status_page::get_status_cached(&state.engine).await {
        Ok(status) => status,
        Err(_) if !changed => return None,
        Err(e) => {
            return Some(Refusal {
                message: format!(
                    "Nothing was saved: the SEV-SNP key entry settings are checked against the engine's before they're saved, and the engine isn't answering ({e})."
                ),
                fields: vec![live_settings::FieldError::new(
                    ID_KEY.key,
                    format!("The engine isn't answering, so this can't be checked against its settings ({e})."),
                )],
            })
        }
    };
    // The engine's backends as the same save would leave them: turning snp
    // on (or off) and setting this site's policy to match go in one save.
    let has_snp = super::status_page::engine_has_snp(&status);
    let will_have_snp = engine_backends.map_or(has_snp, |backends| {
        backends
            .split(',')
            .any(|b| super::key_entry::takes_keys_encrypted(b.trim()))
    });
    let problems = super::status_page::snp_policy_problems_for(&proposed, &status, will_have_snp);
    if problems.is_empty() {
        return None;
    }
    let message: Vec<&str> = problems
        .iter()
        .map(|(_, problem)| problem.as_str())
        .collect();
    Some(Refusal {
        fields: problems
            .iter()
            .map(|(key, problem)| live_settings::FieldError::new(*key, problem.clone()))
            .collect(),
        message: format!(
            "Nothing was saved: the SEV-SNP key entry settings must match the engine's. {}.",
            message.join("; ")
        ),
    })
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
    /// Key custody backends the save turned on that can't run (one that
    /// failed to start when the save was installed).
    #[serde(default)]
    unavailable_backends: Vec<RemoteUnavailable>,
}

#[derive(Deserialize)]
struct RemoteUnavailable {
    backend: String,
    error: String,
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

/// What an accepted engine save leaves to say (tasks 3.6, 4.5, decisions
/// D1, D2, D8): networks stores use that no longer have a node (a banner),
/// restart-only settings, environment overrides and anything else the
/// engine said.
fn engine_save_notes(warnings: RemoteSaveWarnings, submitted_bind: Option<&str>) -> Vec<SaveNote> {
    let mut notes = Vec::new();
    for unserved in &warnings.unserved_networks {
        let stores = if unserved.tenants == 1 {
            "1 store uses".to_string()
        } else {
            format!("{} stores use", unserved.tenants)
        };
        notes.push(SaveNote::Banner(format!(
            "{stores} the {} network, which no longer has any reachable nodes. Their payments won't be detected until a node is set.",
            unserved.network
        )));
    }
    for unavailable in &warnings.unavailable_backends {
        notes.push(SaveNote::Banner(format!(
            "The {} key custody backend can't run: {}. Stores on it aren't scanned until it can.",
            unavailable.backend,
            unavailable.error.trim_end_matches('.')
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
        notes.push(SaveNote::Restart(text));
    }
    for message in warnings.messages {
        notes.push(SaveNote::Remark(message.message));
    }
    if !warnings.env_overridden.is_empty() {
        notes.push(SaveNote::Remark(format!(
            "Saved, but these are set by an environment variable on the engine, which wins while it is set: {}.",
            warnings.env_overridden.join(", ")
        )));
    }
    notes
}

/// The engine's refusal of a save or a check: its own message, and the
/// settings it names (its `fields`).
fn engine_refusal(status: axum::http::StatusCode, body: String) -> Refusal {
    let parsed = serde_json::from_str::<serde_json::Value>(&body).ok();
    let fields: Vec<live_settings::FieldError> = parsed
        .as_ref()
        .and_then(|v| v["fields"].as_array())
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| {
                    Some(live_settings::FieldError::new(
                        f["key"].as_str()?,
                        f["message"].as_str()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let message = parsed
        .as_ref()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or(body);
    Refusal {
        fields,
        message: format!("The engine refused the change ({status}): {message}"),
    }
}

/// What the engine's check said a save would change: its keys, none when
/// nothing would.
#[derive(Deserialize, Default)]
struct RemoteCheckResponse {
    #[serde(default)]
    changed: Vec<String>,
}

/// Checks the engine's part of a tab through its
/// `POST /api/v1/admin/settings/check`, saving nothing: what a save would
/// change, or the engine's refusal, verbatim.
async fn check_engine(
    state: &AppState,
    req: &RemoteUpdateRequest,
) -> Result<RemoteCheckResponse, Refusal> {
    if req.is_empty() {
        return Ok(RemoteCheckResponse::default());
    }
    match state.engine.client.check_settings(req).await {
        Ok(response) if response.status().is_success() => {
            response.json::<RemoteCheckResponse>().map_err(|e| {
                Refusal::new(format!(
                    "Nothing was saved: the engine's check could not be read ({e})."
                ))
            })
        }
        Ok(response) => {
            let status = response.status();
            Err(engine_refusal(status, response.text()))
        }
        Err(e) => Err(Refusal::new(format!(
            "Could not reach the configured engine: {e}"
        ))),
    }
}

/// Saves the engine's part of a tab through its own
/// `POST /api/v1/admin/settings`, after its check: what it leaves to say
/// (restarts, networks left without a node), or its refusal, verbatim.
async fn commit_engine(
    state: &AppState,
    req: RemoteUpdateRequest,
) -> Result<Vec<SaveNote>, Refusal> {
    match state.engine.client.save_settings(&req).await {
        Ok(response) if response.status().is_success() => {
            let saved: RemoteSaveResponse = match response.json() {
                Ok(saved) => saved,
                Err(e) => {
                    // Saved, but what it said about the save is lost: say
                    // so, and don't trust the cached node status either.
                    tracing::warn!(error = %e, "the engine saved its settings but its reply could not be read");
                    super::status_page::invalidate_status_cache(&state.engine);
                    return Ok(vec![SaveNote::Remark(
                        "Saved, but the engine's reply could not be read, so any restart it needs or node it lost isn't shown here.".to_string(),
                    )]);
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
            Ok(engine_save_notes(
                saved.warnings,
                req.scalars.get("server.bind").map(String::as_str),
            ))
        }
        Ok(response) => {
            let status = response.status();
            Err(engine_refusal(status, response.text()))
        }
        Err(e) => Err(Refusal::new(format!(
            "Could not reach the configured engine: {e}"
        ))),
    }
}

/// What saving a tab did: its outcome, what it leaves to say, and what to
/// show again when it was refused.
struct TabSave {
    outcome: SaveOutcome,
    notes: Vec<SaveNote>,
    posted: HashMap<String, String>,
    nodes: Option<NodeForm>,
    field_errors: Vec<(String, String)>,
}

/// One process's refusal, as failures on the cards holding the settings it
/// names (one per card), or the save's own when it names none; and each
/// named setting's own word on itself, to show beside it.
fn failures_of(
    owner: SettingOwner,
    refusal: Refusal,
    failures: &mut Vec<Failure>,
    field_errors: &mut Vec<(String, String)>,
) {
    if refusal.fields.is_empty() {
        failures.push(Failure {
            group: None,
            message: refusal.message,
        });
        return;
    }
    for field in refusal.fields {
        let group = Group::of(&field.key, owner);
        // Named, so the card and the toast say which setting; a network's
        // card is the network.
        let said = match group {
            Group::Network(_) => field.message.clone(),
            _ => format!("{}: {}", field.key, field.message),
        };
        match failures.iter_mut().find(|f| f.group == Some(group)) {
            Some(failure) => {
                failure.message.push(' ');
                failure.message.push_str(&said);
            }
            None => failures.push(Failure {
                group: Some(group),
                message: said,
            }),
        }
        let name = match owner {
            SettingOwner::Engine if is_monokulo_key(&field.key) => {
                format!("engine:{}", field.key)
            }
            _ => field.key,
        };
        field_errors.push((name, field.message));
    }
}

/// The cards holding `keys`, in order, each once.
fn groups_of<'a>(keys: impl IntoIterator<Item = &'a str>, owner: SettingOwner) -> Vec<Group> {
    let mut groups = Vec::new();
    for key in keys {
        let group = Group::of(key, owner);
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups
}

/// Saves a tab, all of it or nothing (the save bar). The whole tab goes to
/// both checks at once, monokulo's registry's and the engine's, which say
/// what changed or why it would be refused; anything refused, by either,
/// and nothing is saved, and every card a refusal names shows it. With
/// every check passed, the engine's part is saved first, then monokulo's:
/// the engine's is the likelier to fail now, and when it does nothing has
/// been saved yet. Monokulo's can still be refused after its check passed
/// (its options file changed on disk), which leaves the engine's saved and
/// says so. Nothing changed, and nothing is sent to be saved. `at` is when,
/// in the admin's clock, for the cards saved.
async fn save_tab(state: &AppState, form: &HashMap<String, String>, at: String) -> TabSave {
    let monokulo_fields = monokulo_fields(state);
    let engine = fetch_engine_settings(&state.engine.client).await.ok();
    let engine_fields = engine.as_ref().map(|e| e.fields.as_slice()).unwrap_or(&[]);
    let tab = split_tab(form, &monokulo_fields, engine_fields);
    let mut failures: Vec<Failure> = tab
        .bad_networks
        .iter()
        .map(|network| Failure {
            group: Some(Group::Network(*network)),
            message: "Some node addresses need fixing (marked below).".to_string(),
        })
        .collect();
    let mut field_errors = Vec::new();
    let (monokulo_check, engine_check) = futures_util::future::join(
        check_monokulo(
            state,
            &tab.monokulo,
            tab.engine
                .scalars
                .get(ENGINE_ENABLED_BACKENDS)
                .map(String::as_str),
        ),
        check_engine(state, &tab.engine),
    )
    .await;
    let monokulo_changed = match monokulo_check {
        Ok(report) => groups_of(
            report.would().changed.iter().copied(),
            SettingOwner::Monokulo,
        ),
        Err(refusal) => {
            failures_of(
                SettingOwner::Monokulo,
                refusal,
                &mut failures,
                &mut field_errors,
            );
            Vec::new()
        }
    };
    let engine_changed = match engine_check {
        Ok(checked) => groups_of(
            checked.changed.iter().map(String::as_str),
            SettingOwner::Engine,
        ),
        Err(refusal) => {
            failures_of(
                SettingOwner::Engine,
                refusal,
                &mut failures,
                &mut field_errors,
            );
            Vec::new()
        }
    };
    let changed: Vec<Group> = engine_changed
        .iter()
        .chain(&monokulo_changed)
        .copied()
        .fold(Vec::new(), |mut all, group| {
            if !all.contains(&group) {
                all.push(group);
            }
            all
        });
    let refused = |failures: Vec<Failure>, posted, nodes, field_errors, notes| TabSave {
        outcome: SaveOutcome::Refused { failures },
        notes,
        posted,
        nodes,
        field_errors,
    };
    if !failures.is_empty() {
        let nodes = tab.nodes;
        return refused(failures, tab.posted, nodes, field_errors, Vec::new());
    }
    if changed.is_empty() {
        return TabSave {
            outcome: SaveOutcome::Unchanged,
            notes: Vec::new(),
            posted: HashMap::new(),
            nodes: None,
            field_errors,
        };
    }
    let mut notes = Vec::new();
    if !engine_changed.is_empty() {
        match commit_engine(state, tab.engine).await {
            Ok(said) => notes.extend(said),
            Err(refusal) => {
                failures_of(
                    SettingOwner::Engine,
                    refusal,
                    &mut failures,
                    &mut field_errors,
                );
                let nodes = tab.nodes;
                return refused(failures, tab.posted, nodes, field_errors, notes);
            }
        }
    }
    if !monokulo_changed.is_empty() {
        match commit_monokulo(state, &tab.monokulo).await {
            Ok(said) => notes.extend(said),
            Err(refusal) => {
                failures_of(
                    SettingOwner::Monokulo,
                    refusal,
                    &mut failures,
                    &mut field_errors,
                );
                // Only monokulo's typed values are still unsaved.
                let posted = tab
                    .posted
                    .into_iter()
                    .filter(|(name, _)| tab.monokulo.contains_key(name))
                    .collect();
                if engine_changed.is_empty() {
                    return refused(failures, posted, None, field_errors, notes);
                }
                return TabSave {
                    outcome: SaveOutcome::PartlySaved {
                        saved: engine_changed,
                        at,
                        failures,
                    },
                    notes,
                    posted,
                    nodes: None,
                    field_errors,
                };
            }
        }
    }
    TabSave {
        outcome: SaveOutcome::Saved {
            groups: changed,
            at,
        },
        notes,
        posted: HashMap::new(),
        nodes: None,
        field_errors,
    }
}

/// `A`, `A and B`, `A, B and C`.
fn joined_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The toast after a save: saved, nothing saved and why, partly saved, or
/// nothing to save.
fn save_toast(outcome: &SaveOutcome, notes: &[SaveNote]) -> Toast {
    let titles =
        |groups: &[Group]| joined_names(&groups.iter().map(|g| g.title()).collect::<Vec<_>>());
    let remarks: Vec<String> = notes
        .iter()
        .filter_map(|note| match note {
            SaveNote::Restart(text) | SaveNote::Remark(text) => Some(text.clone()),
            SaveNote::Banner(_) => None,
        })
        .collect();
    let restart = notes
        .iter()
        .any(|note| matches!(note, SaveNote::Restart(_)));
    let refusal = |title: &str, failures: &[Failure], more: Option<String>| {
        let first = failures.first();
        let mut lines: Vec<String> = first.map(|f| f.message.clone()).into_iter().collect();
        lines.extend(more);
        Toast {
            kind: ToastKind::Error,
            title: title.to_string(),
            lines,
            show: first.and_then(|f| f.group).map(|group| group.to_string()),
        }
    };
    match outcome {
        SaveOutcome::Unchanged => Toast {
            kind: ToastKind::Neutral,
            title: "Nothing to save".to_string(),
            lines: vec!["Nothing on this tab had changed.".to_string()],
            show: None,
        },
        SaveOutcome::Saved { .. } => Toast {
            kind: if restart {
                ToastKind::Warning
            } else {
                ToastKind::Success
            },
            title: if restart {
                "Changes saved".to_string()
            } else {
                "Changes saved and applied".to_string()
            },
            lines: remarks,
            show: None,
        },
        SaveOutcome::Refused { failures } => {
            let others: Vec<Group> = failures.iter().skip(1).filter_map(|f| f.group).collect();
            let more = (!others.is_empty()).then(|| format!("Also to fix: {}.", titles(&others)));
            refusal("Nothing saved", failures, more)
        }
        SaveOutcome::PartlySaved {
            saved, failures, ..
        } => refusal(
            "Changes partly saved",
            failures,
            Some(format!("{} saved; the rest wasn't.", titles(saved))),
        ),
    }
}

impl From<TabSave> for SaveResult {
    fn from(save: TabSave) -> SaveResult {
        SaveResult {
            toast: Some(save_toast(&save.outcome, &save.notes)),
            notices: save
                .notes
                .into_iter()
                .filter_map(|note| match note {
                    SaveNote::Banner(text) => Some(Notice::Error(text)),
                    SaveNote::Restart(_) | SaveNote::Remark(_) => None,
                })
                .collect(),
            outcome: Some(save.outcome),
            posted: save.posted,
            nodes: save.nodes,
            field_errors: save.field_errors,
        }
    }
}

/// A save's result, kept for the page a save without JavaScript redirects
/// to (post, redirect, get), shown once.
struct Flash {
    result: SaveResult,
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

/// `POST /dashboard/admin/settings` - saves one tab, all of it or none of
/// it: any mix of monokulo's and the engine's settings (`save_tab`).
/// Without JavaScript it redirects back to the tab (303), at the card a
/// refusal is about or else the first one saved, with the result carried
/// across in a flash. With fixi, the tab's panel comes back (`422` when the
/// save was refused), with the banners, toast and tab bar out of band.
pub async fn save(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let form = joined(pairs);
    let tab = SettingsTab::from_id(form.get("tab").map(String::as_str));
    let at = views::time::Clock::for_user(&admin_user).text(crate::now_unix());
    let result = SaveResult::from(save_tab(&state, &form, at).await);
    let (refused, card) = match &result.outcome {
        Some(SaveOutcome::Refused { failures } | SaveOutcome::PartlySaved { failures, .. }) => {
            (true, failures.iter().find_map(|f| f.group))
        }
        Some(SaveOutcome::Saved { groups, .. }) => (false, groups.first().copied()),
        _ => (false, None),
    };
    if !fx.0 {
        let token = put_flash(Flash {
            result,
            created: std::time::Instant::now(),
        });
        let anchor = card
            .map(|card| format!("#{}", card.card_id()))
            .unwrap_or_default();
        return super::dashboard::redirect_303(&format!("{}&saved={token}{anchor}", tab.href()));
    }
    let mut view = build_view_model(&state, &admin_user, tab, result).await;
    view.answers_save = true;
    // Focus lands where the save says what it did: the first card saved,
    // the save bar's refusal, or, when nothing had changed, the heading.
    let unchanged = matches!(view.outcome, Some(SaveOutcome::Unchanged));
    let fragment = views::admin::settings_fragment(&view, unchanged);
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
fn reload_notes(
    process: &str,
    changed: &[String],
    restart_required: &[String],
) -> (String, Vec<SaveNote>) {
    let success = if changed.is_empty() {
        format!("Reloaded {process}'s options file: nothing in it changed.")
    } else {
        format!(
            "Reloaded {process}'s options file and applied it: {}.",
            changed.join(", ")
        )
    };
    let mut notes = Vec::new();
    if !restart_required.is_empty() {
        notes.push(SaveNote::Restart(format!(
            "These take effect after {process} restarts: {}.",
            restart_required.join(", ")
        )));
    }
    (success, notes)
}

/// Reads monokulo's options file again and applies it; `Err` is why
/// nothing changed.
async fn reload_monokulo(state: &AppState) -> Result<(String, Vec<SaveNote>), String> {
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
            let (success, mut notes) = reload_notes("monokulo", &changed, &restart);
            notes.extend(
                report
                    .warnings
                    .iter()
                    .map(|warning| SaveNote::Banner(warning.message.clone())),
            );
            Ok((success, notes))
        }
        Err(e) => Err(format!(
            "Nothing was reloaded: monokulo's options file has problems. {e}"
        )),
    }
}

/// Asks the engine to read its options file again and apply it
/// (`POST /api/v1/admin/settings/reload`); `Err` is why nothing changed.
async fn reload_engine(state: &AppState) -> Result<(String, Vec<SaveNote>), String> {
    let response = state
        .engine
        .client
        .reload_options()
        .await
        .map_err(|e| format!("Could not reach the configured engine: {e}"))?;
    let status = response.status();
    let body = response.text();
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
    let (success, mut notes) = reload_notes(
        "the engine",
        &saved.changed,
        &saved.warnings.restart_required,
    );
    notes.extend(
        saved
            .warnings
            .messages
            .into_iter()
            .map(|message| SaveNote::Remark(message.message)),
    );
    Ok((success, notes))
}

/// `POST /dashboard/admin/settings/reload` - the Reload options file button:
/// reads monokulo's or the engine's options file again, after an edit by
/// hand, and applies all of it, or, when anything in it is wrong, none of
/// it, with every problem named by line. A reload that worked redirects
/// back to the tab with a toast saying what changed (post, redirect, get);
/// one that didn't renders the page with a toast saying why.
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
        Ok((success, notes)) => {
            let restart = notes
                .iter()
                .any(|note| matches!(note, SaveNote::Restart(_)));
            let mut result = SaveResult::default();
            let mut lines = vec![success];
            for note in notes {
                match note {
                    SaveNote::Restart(text) | SaveNote::Remark(text) => lines.push(text),
                    SaveNote::Banner(text) => result.notices.push(Notice::Error(text)),
                }
            }
            result.toast = Some(Toast {
                kind: if restart {
                    ToastKind::Warning
                } else {
                    ToastKind::Success
                },
                title: "Options file reloaded".to_string(),
                lines,
                show: None,
            });
            let token = put_flash(Flash {
                result,
                created: std::time::Instant::now(),
            });
            super::dashboard::redirect_303(&format!("{}&saved={token}", tab.href()))
        }
        Err(error) => {
            let result = SaveResult {
                toast: Some(Toast {
                    kind: ToastKind::Error,
                    title: "Options file not reloaded".to_string(),
                    lines: vec![error],
                    show: None,
                }),
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
    /// client sends, with the settings an engine inside monokulo has: for
    /// [`test_app_state_in_process`].
    async fn spawn_engine() -> engine_test_support::TestEngineHandle {
        engine_test_support::TestEngineConfig::new()
            .embedded()
            .spawn()
            .await
    }

    /// The same as a standalone engine, with its own `server.bind` and
    /// `logging.*`: for [`test_app_state_over_http`].
    async fn spawn_remote_engine() -> engine_test_support::TestEngineHandle {
        engine_test_support::TestEngineConfig::new().spawn().await
    }

    /// A monokulo instance with a seeded admin account and `engine` inside
    /// it, as monokulo runs by default: what most tests in this module want,
    /// since the whole point of this page is proxying that connection. The
    /// HTTP transport is covered once, for every call, by
    /// `engine_client::contract_tests`.
    async fn test_app_state_in_process(engine: &engine_test_support::TestEngineHandle) -> AppState {
        test_app_state_with_client(
            EngineClient::embedded_for_tests(engine.router()),
            live_settings::OptionsFile::in_memory("[signup]\nmode = \"public\"\n"),
        )
        .await
    }

    /// The same with a remote engine at `engine_addr`, over HTTP: for what
    /// only a remote engine has (its standalone-only settings, `engine.url`),
    /// and for an engine that can't be reached at all.
    async fn test_app_state_over_http(engine_addr: std::net::SocketAddr) -> AppState {
        test_app_state_with_options(
            engine_addr,
            live_settings::OptionsFile::in_memory("[signup]\nmode = \"public\"\n"),
        )
        .await
    }

    /// [`test_app_state_over_http`] over the given options file.
    async fn test_app_state_with_options(
        engine_addr: std::net::SocketAddr,
        options: live_settings::OptionsFile,
    ) -> AppState {
        test_app_state_with_client(
            EngineClient::for_tests(format!("http://{engine_addr}")),
            options,
        )
        .await
    }

    /// The same, reaching the engine through `engine_client`.
    async fn test_app_state_with_client(
        engine_client: EngineClient,
        options: live_settings::OptionsFile,
    ) -> AppState {
        let db = Db::open_in_memory().unwrap();
        db.seed_test_admin();
        let db = db.into_shared();
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
    /// Whether a page says a save was saved: its toast, green, or amber for
    /// a restart it waits for, and nothing refused.
    fn is_saved(html: &str) -> bool {
        (html.contains(r#"class="toast toast-success""#)
            || html.contains(r#"class="toast toast-warning""#))
            && !html.contains(r#"class="toast toast-error""#)
    }

    /// The toast's text, when a page has one.
    fn toast_text(html: &str) -> String {
        let Some(at) = html.find(r#"<div id="settings-toasts""#) else {
            return String::new();
        };
        let rest = &html[at..];
        unescaped(&rest[..rest.find("</button>").unwrap_or(rest.len())])
    }

    async fn follow(router: &Router, cookie: &str, response: axum::response::Response) -> String {
        if response.status() != StatusCode::SEE_OTHER {
            return body_text(response).await;
        }
        // The card to land on (`#card-...`) is the browser's, not the server's.
        let location = response.headers()["location"].to_str().unwrap();
        let location = location.split('#').next().unwrap().to_string();
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
        let state = test_app_state_in_process(&engine).await;
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        let state = test_app_state_in_process(&engine).await;
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        assert!(is_saved(&html), "expected a success banner, got: {html}");
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
            ("engine.mode", "remote"),
            ("engine.url", "http://127.0.0.1:9443"),
            ("key_custody.snp_bundles_per_user", "17"),
            ("key_custody.snp_bundles_per_user_per_min", "29"),
            ("webhooks.allow_private_urls", "true"),
            ("webhooks.delivery_timeout_ms", "10000"),
            ("webhooks.max_attempts", "12"),
            ("webhooks.keep_delivered_days", "14"),
            ("webhooks.keep_given_up_days", "60"),
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
                // Checked against the engine's before saving: their own
                // test below.
                .filter(|s| !s.key().starts_with("key_custody.snp_entry_"))
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
        assert!(is_saved(&html), "expected a success banner, got: {html}");

        let html = settings_tabs_html(&router, &cookie).await;
        for (key, value) in new_values {
            assert!(
                shows_setting(&html, key, value),
                "expected {key}={value:?} to have round-tripped, got: {html}"
            );
        }
    }

    /// The scanner half of the same requirement - every one of
    /// `engine::engine_settings::ALL`'s keys, saved together through the
    /// real proxy `POST` and confirmed to round-trip via a real, separately
    /// spawned scanner instance (this monokulo page holds none of this state
    /// itself - see this module's own doc comment). Over HTTP: the list
    /// includes a remote engine's own `server.bind` and `logging.*`.
    #[tokio::test]
    async fn every_remote_engine_setting_on_the_admin_page_saves_correctly_over_http() {
        let engine = spawn_remote_engine().await;
        let state = test_app_state_over_http(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let new_values: &[(&str, &str)] = &[
            ("key_custody.enabled_backends", "plain"),
            ("key_custody.default_backend", "plain"),
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
            ("server.cpus", "0"),
            ("server.nice", "5"),
            ("server.rate_limit_per_token_per_min", "200"),
            ("server.max_body_bytes", "16384"),
            // The same key as monokulo's own, so sent as `engine:<key>`.
            ("engine:database.read_connections", "6"),
            ("order_events.retention_days", "14"),
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
        assert!(is_saved(&html), "expected a success banner, got: {html}");
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
            assert!(
                shows_setting(&html, key, value),
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
            ("list", "snp"),
            ("other", "a,b"),
        ]));
        assert_eq!(form["list"], "plain,snp");
        assert_eq!(form["other"], "a,b");
        assert_eq!(super::joined(pairs(&[("list", "")]))["list"], "");
    }

    #[test]
    fn a_switch_left_off_is_sent_as_false() {
        let pairs = |list: &[(&str, &str)]| {
            list.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        };
        let form = super::joined(pairs(&[
            ("switches", "on"),
            ("on", "true"),
            ("switches", "off"),
        ]));
        assert_eq!(form["on"], "true");
        assert_eq!(form["off"], "false");
        assert!(!form.contains_key("switches"));
    }

    /// A setting shown with this value: an on/off switch on or off, else
    /// as [`shows_value`].
    fn shows_setting(html: &str, key: &str, value: &str) -> bool {
        for name in [key.to_string(), format!("engine:{key}")] {
            let switch = format!(r#"role="switch" name="{name}" value="true""#);
            if let Some(at) = html.find(&switch) {
                let tag = &html[at..at + html[at..].find('>').unwrap()];
                return tag.contains("checked") == (value == "true");
            }
        }
        shows_value(html, value)
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        // The next request sees it: the link works now, so an account
        // with no store on the shop's site is sent to set one up.
        let after = get(&router, confirm, Some(&cookie)).await;
        assert_eq!(after.status(), StatusCode::FOUND);
        assert!(
            after.headers()["location"]
                .to_str()
                .unwrap()
                .starts_with("/setup?plugin=woocommerce&site_url=https%3A%2F%2Fshop.example.com"),
            "{:?}",
            after.headers()["location"]
        );
    }

    #[tokio::test]
    async fn with_the_engine_inside_monokulo_its_address_is_locked_and_never_saved() {
        let engine = spawn_engine().await;
        let state = test_app_state_with_client(
            EngineClient::embedded_for_tests(engine.router()),
            live_settings::OptionsFile::in_memory("[signup]\nmode = \"public\"\n"),
        )
        .await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let page = body_text(get_settings_page(&router, &cookie).await).await;
        assert!(
            page.contains("Only used with a remote engine (engine.mode = remote): the engine runs inside monokulo."),
            "the lock says why: {page}"
        );
        assert!(
            !page.contains(r#"name="engine.url""#),
            "not editable: {page}"
        );

        // A hand-made form that sends it anyway is refused, so the next
        // start isn't stopped by a URL it would refuse.
        let response = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/settings",
                &cookie,
                &[("tab", "general"), ("engine.url", "http://127.0.0.1:1")],
            ))
            .await
            .unwrap();
        let said = follow(&router, &cookie, response).await;
        assert!(said.contains("Only used with a remote engine"), "{said}");
        assert_eq!(
            monokulo_value(&settings, "engine.url").1,
            live_settings::SettingSource::Default,
            "nothing was saved"
        );
    }

    /// Over HTTP: the engine token and `engine.url` are a remote engine's.
    #[tokio::test]
    async fn a_remote_engines_token_shows_locked_and_is_never_saved_and_its_url_is_saved_for_a_restart_over_http(
    ) {
        let engine = spawn_remote_engine().await;
        let state = test_app_state_over_http(engine.addr).await;
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
    /// This site's SEV-SNP key entry settings sit in the snp backend's
    /// section, and are checked against the engine's before they're saved:
    /// a value the engine doesn't share is refused, named beside its field,
    /// and nothing changes; matching values save and apply at once.
    #[cfg(feature = "snp")]
    #[tokio::test]
    async fn sev_snp_key_entry_settings_are_saved_only_when_they_match_the_engines() {
        let engine = engine_test_support::TestEngineConfig::new()
            .embedded()
            .with_snp_backend()
            .spawn()
            .await;
        let state = test_app_state_in_process(&engine).await;
        let settings = state.settings.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let engines = hex::encode(engine_test_support::snp_test_trust().id_key_digest);
        let other = "cd".repeat(48);
        let custody = crate::views::admin::SettingsTab::Custody;

        let html = body_text(get(&router, &custody.href(), Some(&cookie)).await).await;
        let section = html
            .find(r#"data-shown-by="key_custody.enabled_backends=snp""#)
            .expect(&html);
        assert!(
            html.find(r#"name="key_custody.snp_entry_id_key""#)
                .expect(&html)
                > section,
            "in the snp backend's section: {html}"
        );

        let before = settings.snp_entry.load();
        let save = |fields: Vec<(&'static str, String)>| {
            let router = router.clone();
            let cookie = cookie.clone();
            async move {
                let mut form: Vec<(&str, &str)> = vec![("tab", "custody")];
                form.extend(fields.iter().map(|(k, v)| (*k, v.as_str())));
                router
                    .oneshot(authed_form_request(
                        "POST",
                        "/dashboard/admin/settings",
                        &cookie,
                        &form,
                    ))
                    .await
                    .unwrap()
            }
        };
        let refused = save(vec![("key_custody.snp_entry_id_key", other.clone())]).await;
        assert_eq!(refused.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, refused).await);
        assert!(html.contains("must match the engine's"), "{html}");
        assert!(
            html.contains(&format!("key_custody.snp_entry_id_key here is {other}")),
            "{html}"
        );
        assert_eq!(settings.snp_entry.load(), before, "nothing changed");

        let saved = save(vec![
            ("key_custody.snp_entry_id_key", engines.clone()),
            ("key_custody.snp_entry_min_guest_svn", "0".into()),
            ("key_custody.snp_entry_required", "true".into()),
        ])
        .await;
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        let policy = settings.snp_entry.load();
        assert_eq!(
            policy.trust.map(|t| t.id_key_digest),
            Some(engine_test_support::snp_test_trust().id_key_digest)
        );
        assert!(policy.required);

        let refused = save(vec![("key_custody.snp_entry_min_guest_svn", "3".into())]).await;
        assert_eq!(refused.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, refused).await);
        assert!(
            html.contains("key_custody.snp_entry_min_guest_svn here is 3, the engine's key_custody.snp_min_guest_svn is 0"),
            "{html}"
        );
        assert_eq!(settings.snp_entry.load().trust.unwrap().min_guest_svn, 0);
    }

    /// Settings that disagree with the engine's from the start (the options
    /// file): a red alert on the status page naming what differs, for the
    /// operator, and SEV-SNP key storage isn't a choice on the forms.
    #[cfg(feature = "snp")]
    #[tokio::test]
    async fn a_mismatch_from_the_options_file_is_a_red_alert_and_snp_is_not_offered() {
        let engine = engine_test_support::TestEngineConfig::new()
            .embedded()
            .with_snp_backend()
            .spawn()
            .await;
        let other = "cd".repeat(48);
        let state = test_app_state_with_client(
            EngineClient::embedded_for_tests(engine.router()),
            live_settings::OptionsFile::in_memory(format!(
                "[signup]\nmode = \"public\"\n[key_custody]\nsnp_entry_id_key = \"{other}\"\n"
            )),
        )
        .await;
        crate::http::status_page::get_status_cached(&state.engine)
            .await
            .unwrap();
        assert!(crate::http::key_entry::snp_unusable(&state).is_some());
        assert_eq!(
            crate::http::key_entry::usable_custody_backends(&state),
            vec!["plain".to_owned()]
        );
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let html = unescaped(&body_text(get(&router, "/status", Some(&cookie)).await).await);
        assert!(
            html.contains(r#"<div class="error" role="alert" id="snp-policy-alert">"#),
            "{html}"
        );
        assert!(
            html.contains(&format!("key_custody.snp_entry_id_key here is {other}")),
            "{html}"
        );
        let anonymous = unescaped(&body_text(get(&router, "/status", None).await).await);
        assert!(anonymous.contains("snp-policy-alert"), "{anonymous}");
        assert!(
            !anonymous.contains(&other),
            "which settings differ is for operators"
        );
    }

    #[tokio::test]
    async fn a_secret_is_shown_locked_and_a_form_sending_one_is_refused() {
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        assert_eq!(refused.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, refused).await);
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
            assert_eq!(save.status(), StatusCode::SEE_OTHER, "{key}={value:?}");
            let html = follow(&router, &cookie, save)
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
        let state = test_app_state_in_process(&engine).await;
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
        assert!(is_saved(&html), "expected a success banner, got: {html}");
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
        let state = test_app_state_in_process(&engine).await;
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
                r#"<section id="settings-panel" aria-labelledby="settings-panel-title" data-tab="abuse""#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<div id="settings-banners" class="save-banners" data-fx-oob>"#),
            "{html}"
        );
        assert!(html.contains(r#"<div id="settings-toasts" class="toasts" data-fx-oob><div class="toast toast-success" role="status" data-toast><span class="toast-icon" aria-hidden="true">✓</span><div class="toast-text"><strong>Changes saved and applied</strong>"#), "{html}");
        assert!(html.contains(r#"<nav id="settings-tabs" class="tab-bar" aria-label="Settings sections" data-leave-asks data-fx-oob>"#), "{html}");
        assert!(
            html.contains(r#"value="70""#) && !html.contains("<html"),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="card-meta card-saved" data-card-saved tabindex="-1" data-fx-focus>Saved "#),
            "the card says it was saved, and gets focus: {html}"
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
                && html.contains("payment.confirmations_required: Enter a whole number"),
            "{html}"
        );
        assert!(
            html.contains(r#"<mk-save-bar id="save-bar" class="save-bar is-failed""#)
                && html.contains(r#"<strong>Nothing saved.</strong>"#)
                && html.contains(r#"data-save-bar-message tabindex="-1" data-fx-focus>"#),
            "the save bar says which card, and gets focus: {html}"
        );
        assert!(
            html.contains(r#"<mk-settings-card id="card-orders" class="settings-card is-failed""#),
            "{html}"
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
        assert!(is_saved(&body_text(saved).await));
    }

    /// Saving a wrong engine token on General with fixi: the engine's tabs
    /// say at once that the engine can't be reached.
    #[tokio::test]
    async fn a_fixi_tab_link_shows_a_remote_engine_it_cannot_reach_over_http_in_the_panel() {
        // Nothing listens there.
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        let state = test_app_state_in_process(&engine).await;
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
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = follow(&router, &cookie, save).await;
        assert!(
            html.contains("payment.confirmations_required: Enter a whole number"),
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
        EngineClient::embedded_for_tests(engine.router())
            .get_settings()
            .await
            .unwrap()
            .json()
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
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
        assert!(is_saved(&follow(&router, &cookie, save).await));
        assert_eq!(
            monokulo_value(&settings, "abuse.soft_per_min"),
            ("61".to_string(), live_settings::SettingSource::Toml)
        );
    }

    #[tokio::test]
    async fn a_tab_with_only_engine_settings_saves_only_the_engine() {
        let engine = spawn_engine().await;
        let state = test_app_state_in_process(&engine).await;
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
        let state = test_app_state_in_process(&engine).await;
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
        assert!(is_saved(&html), "{html}");
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

    /// A tab is sent whole, but only what the checks say differs from the
    /// settings in effect is saved; sent again unchanged, nothing is.
    #[tokio::test]
    async fn a_save_saves_only_the_cards_whose_settings_changed() {
        let state = test_app_state_over_http(shared::unreachable::address()).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        // The tab as shown, with one setting changed: the engine isn't
        // reachable, but nothing of its is sent, so nothing is refused.
        let tab = [
            ("tab", "abuse"),
            ("abuse.soft_per_min", "61"),
            ("abuse.hard_per_min", "300"),
            ("abuse.challenge_bits", "16"),
            ("abuse.under_attack", "false"),
            ("abuse.trusted_proxies", ""),
        ];
        let save = post_settings(&router, &cookie, &tab).await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let location = save.headers()["location"].to_str().unwrap().to_string();
        assert!(
            location.ends_with("#card-abuse-limits"),
            "back at the saved card: {location}"
        );
        let html = follow(&router, &cookie, save).await;
        assert!(
            toast_text(&html).contains("Changes saved and applied"),
            "{html}"
        );
        assert!(
            html.contains(r#"<mk-settings-card id="card-abuse-limits" class="settings-card""#)
                && html[html.find(r#"id="card-abuse-limits""#).unwrap()..]
                    .contains(r#"<span class="card-meta card-saved" data-card-saved>Saved "#),
            "{html}"
        );
        assert_eq!(
            db.lock().get_setting("abuse.soft_per_min").unwrap(),
            None,
            "kept in the options file, not the database"
        );
        assert_eq!(
            db.lock().get_setting("abuse.under_attack").unwrap(),
            None,
            "unchanged, not saved"
        );

        // The same again: nothing to save, and it says so.
        let again = post_settings(&router, &cookie, &tab).await;
        let html = follow(&router, &cookie, again).await;
        let toast = toast_text(&html);
        assert!(
            toast.contains("Nothing to save") && toast.contains("Nothing on this tab had changed."),
            "{toast}"
        );
        assert!(!is_saved(&html), "{html}");
    }

    /// All of a save or none of it: a value either check refuses, and
    /// nothing on the tab is saved; every changed card keeps what was typed,
    /// and the refused one says why.
    #[tokio::test]
    async fn a_value_a_check_refuses_saves_nothing_on_the_tab() {
        let engine = spawn_engine().await;
        let state = test_app_state_in_process(&engine).await;
        let db = state.db.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        for (bad, card) in [
            // Monokulo's check refuses it, the engine's passes.
            (("exchange_rate.cache_seconds", "-5"), "exchange-rates"),
            // The engine's check refuses it, monokulo's passes.
            (("payment.order_expiry_minutes", "0"), "orders"),
        ] {
            let mut form = vec![
                ("tab", "payments"),
                ("payment.confirmations_required", "8"),
                ("exchange_rate.coingecko_enabled", "false"),
            ];
            form.push(bad);
            let save = post_settings(&router, &cookie, &form).await;
            assert_eq!(save.status(), StatusCode::SEE_OTHER);
            let location = save.headers()["location"].to_str().unwrap().to_string();
            assert!(location.ends_with(&format!("#card-{card}")), "{location}");
            let html = unescaped(&follow(&router, &cookie, save).await);
            let toast = toast_text(&html);
            assert!(
                toast.contains("Nothing saved") && toast.contains(bad.0),
                "{toast}"
            );
            assert!(
                html.contains(&format!(
                    r#"<mk-settings-card id="card-{card}" class="settings-card is-failed""#
                )),
                "{html}"
            );
            // Nothing saved anywhere.
            assert_eq!(
                engine_settings(&engine).await["scalars"]["payment.confirmations_required"]
                    ["value"],
                "10"
            );
            assert_eq!(
                db.lock()
                    .get_setting("exchange_rate.coingecko_enabled")
                    .unwrap(),
                None
            );
            // What was typed is still there, with what's saved for Discard.
            assert!(
                html.contains(r#"name="payment.confirmations_required" value="8" min="0" max="720" step="1" id="setting-payment.confirmations_required" aria-describedby="setting-help-payment.confirmations_required" data-saved="10">"#),
                "{html}"
            );
        }
    }

    /// A saved value is shown as it's stored, not as it was typed: after a
    /// save, nothing on the page reads as unsaved.
    #[tokio::test]
    async fn after_a_save_the_page_shows_what_was_stored() {
        let state = test_app_state_over_http(shared::unreachable::address()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.soft_per_min", " 61 ")],
        )
        .await;
        let html = follow(&router, &cookie, save).await;
        assert!(is_saved(&html), "{html}");
        assert!(
            html.contains(r#"name="abuse.soft_per_min" value="61""#),
            "{html}"
        );
        assert!(
            !html.contains(r#"data-saved=""#),
            "no value left unsaved: {html}"
        );
    }

    /// The toast says a save waits for a restart only when one does; a
    /// warning from preparing is said, in green.
    #[test]
    fn only_a_restart_makes_the_toast_amber() {
        use crate::views::admin::{Group, SaveOutcome, ToastKind};
        let saved = SaveOutcome::Saved {
            groups: vec![Group::Orders],
            at: "now".into(),
        };
        let toast = super::save_toast(
            &saved,
            &[super::SaveNote::Remark(
                "The node is slow to answer.".into(),
            )],
        );
        assert_eq!(toast.kind, ToastKind::Success);
        assert_eq!(toast.title, "Changes saved and applied");
        assert_eq!(toast.lines, ["The node is slow to answer."]);
        let toast = super::save_toast(
            &saved,
            &[super::SaveNote::Restart("Restart the engine.".into())],
        );
        assert_eq!(
            (toast.kind, toast.title.as_str()),
            (ToastKind::Warning, "Changes saved")
        );
        let toast = super::save_toast(
            &saved,
            &[super::SaveNote::Banner("A network has no node.".into())],
        );
        assert!(
            toast.lines.is_empty(),
            "a banner is a banner, not a line: {toast:?}"
        );
    }

    /// The engine's part is saved first; when monokulo's is then refused
    /// (its options file changed on disk after its check passed), the
    /// engine's stays saved and the save says it was partly saved. The
    /// stand-in engine here is the change on disk: its save edits the file.
    #[tokio::test]
    async fn a_monokulo_refusal_after_the_engine_saved_says_it_was_partly_saved() {
        use axum::routing::post;
        let dir = TempDir::new("partly");
        let path = dir.0.join("monokulo.toml");
        std::fs::write(&path, "[signup]\nmode = \"public\"\n").unwrap();
        let edited = path.clone();
        let engine_saves = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let saves = engine_saves.clone();
        let app = Router::new()
            .route(
                "/api/v1/admin/settings/check",
                post(|| async {
                    axum::Json(serde_json::json!({ "ok": true, "checked": true, "has_changes": true, "changed": ["payment.confirmations_required"], "warnings": {} }))
                }),
            )
            .route(
                "/api/v1/admin/settings",
                post(move || {
                    let (edited, saves) = (edited.clone(), saves.clone());
                    async move {
                        saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::fs::write(&edited, "[signup]\nmode = \"invite_only\"\n").unwrap();
                        axum::Json(serde_json::json!({ "ok": true, "changed": ["payment.confirmations_required"], "warnings": {} }))
                    }
                }),
            );
        let state = test_app_state_with_client(
            EngineClient::embedded_for_tests(app),
            live_settings::OptionsFile::at(&path),
        )
        .await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let save = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "payments"),
                ("payment.confirmations_required", "5"),
                // Sent, but the engine's check doesn't name it as changed:
                // its card wasn't saved.
                ("order_events.retention_days", "7"),
                ("exchange_rate.cache_seconds", "45"),
            ],
        )
        .await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, save).await);
        assert!(!html.contains("Orders and Webhooks"), "{html}");
        let toast = toast_text(&html);
        assert!(
            toast.contains("Changes partly saved")
                && toast.contains("has changed since it was loaded")
                && toast.contains("Orders saved; the rest wasn't."),
            "{toast}"
        );
        assert!(
            html.contains("<strong>Changes partly saved.</strong>"),
            "{html}"
        );
        assert_eq!(engine_saves.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("cache_seconds"),
            "monokulo's part wasn't written"
        );
    }

    #[tokio::test]
    async fn an_engine_refusal_shows_the_engines_own_message() {
        let engine = spawn_engine().await;
        let state = test_app_state_in_process(&engine).await;
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
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, save).await);
        // On its card, and in the toast, which says nothing was saved.
        assert!(html.contains(message), "{message} in {html}");
        assert!(toast_text(&html).contains("Nothing saved"), "{html}");
        assert!(
            html.contains(r#"<mk-settings-card id="card-orders" class="settings-card is-failed""#),
            "{html}"
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
        let state = test_app_state_in_process(&engine).await;
        let engine_client = state.engine.client.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        // Addresses that hang up on every request, rather than closed ports,
        // which Windows takes two seconds to refuse each time.
        let (first_node, second_node) = (
            shared::unreachable::address().to_string(),
            shared::unreachable::address().to_string(),
        );
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
        let state = test_app_state_over_http(shared::unreachable::address()).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let save = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("abuse.soft_per_min", "62")],
        )
        .await;
        let location = save.headers()["location"].to_str().unwrap().to_string();
        assert!(is_saved(&follow(&router, &cookie, save).await));
        let again = body_text(get(&router, &location, Some(&cookie)).await).await;
        assert!(!is_saved(&again), "{again}");
    }

    // -- The tabbed page (nicer_admin_screen.md step 3) -------------------

    #[tokio::test]
    async fn every_tab_opens() {
        let engine = spawn_engine().await;
        let state = test_app_state_in_process(&engine).await;
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
    /// belongs. Over HTTP: only a remote engine has `logging.*` of its own.
    #[tokio::test]
    async fn the_logging_tab_keeps_monokulos_and_a_remote_engines_settings_apart_over_http() {
        let engine = spawn_remote_engine().await;
        let state = test_app_state_over_http(engine.addr).await;
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
    /// keeping the admin's own comments and the engine's table, the Reload
    /// button applies an edit made by hand, and a bad edit is refused by
    /// line, changing nothing. On the file as monokulo runs it, the engine
    /// inside it keeping its settings in the same file.
    #[tokio::test]
    async fn the_options_file_is_named_saved_to_and_reloaded_from_the_page() {
        let dir = TempDir::new("options");
        let path = dir.0.join("monokulo.toml");
        std::fs::write(
            &path,
            "# Mine.\n[signup]\nmode = \"public\"\n\n[engine.payment]\nconfirmations_required = 3\n",
        )
        .unwrap();
        let (engine, state) = embedded_on_file(&path).await;
        assert_eq!(
            engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["value"],
            "3",
            "the engine reads its table of the file"
        );
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
            "# Mine.\n[signup]\nmode = \"public\"\n\n[engine.payment]\nconfirmations_required = 3\n\n[abuse]\nsoft_per_min = 61\n"
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
        assert!(
            toast_text(&html).contains("Options file reloaded"),
            "{html}"
        );
        assert!(
            !html.contains(r#"class="card-meta card-saved""#),
            "no card was saved: {html}"
        );
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
        let html = unescaped(&follow(&router, &cookie, refused).await);
        assert!(html.contains("has changed since it was loaded"), "{html}");
    }

    /// The engine's Reload button asks the engine to read its own file.
    #[tokio::test]
    async fn the_engines_options_file_is_reloaded_through_its_api() {
        let engine = spawn_engine().await;
        let state = test_app_state_in_process(&engine).await;
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
        let (engine, state) = embedded_on_file(&path).await;
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
        assert_eq!(refused.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, refused).await);
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

        // The engine keeps its settings in the same file: locked too, and
        // a form that sends one anyway saves nothing.
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
            !payments.contains(r#"name="payment.confirmations_required""#),
            "the engine's settings in the file are locked: {payments}"
        );
        let refused = post_settings(
            &router,
            &cookie,
            &[("tab", "payments"), ("payment.confirmations_required", "4")],
        )
        .await;
        if refused.status() == StatusCode::SEE_OTHER {
            let html = unescaped(&follow(&router, &cookie, refused).await);
            assert!(!is_saved(&html), "{html}");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(
            engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["source"],
            "default"
        );

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

        // Switched off: an unticked switch sends only its `switches` name.
        let saved = post_settings(
            &router,
            &cookie,
            &[("tab", "abuse"), ("switches", "abuse.under_attack")],
        )
        .await;
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            monokulo_value(&settings, "abuse.under_attack"),
            ("false".to_string(), live_settings::SettingSource::Database)
        );
    }

    /// No options file yet: the page says so, everything is editable, and
    /// the first save creates the file, which the page then names as there.
    #[tokio::test]
    async fn a_missing_options_file_is_created_by_the_first_save() {
        let dir = TempDir::new("missing");
        let path = dir.0.join("config").join("monokulo.toml");
        let (engine, state) = embedded_on_file(&path).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let page = unescaped(&body_text(get_settings_page(&router, &cookie).await).await);
        assert!(
            page.contains("Not created yet: saving a setting here creates it."),
            "{page}"
        );
        assert!(page.contains(r#"name="public_url""#), "editable: {page}");

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

        // The engine's first save adds its table to the file monokulo made.
        let saved = post_settings(
            &router,
            &cookie,
            &[("tab", "payments"), ("payment.confirmations_required", "4")],
        )
        .await;
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[abuse]\nsoft_per_min = 61\n\n[engine.payment]\nconfirmations_required = 4\n"
        );
        assert_eq!(
            engine_settings(&engine).await["scalars"]["payment.confirmations_required"]["source"],
            "toml"
        );
    }

    /// What a browser sends for `tab`: every setting the page shows there
    /// that it can change, at the value shown, monokulo's and the engine's.
    async fn tab_as_shown(
        state: &AppState,
        engine: &engine_test_support::TestEngineHandle,
        tab: crate::views::admin::SettingsTab,
    ) -> Vec<(String, String)> {
        use crate::views::admin::{Group, SettingOwner};
        let mut form = vec![("tab".to_string(), tab.id().to_string())];
        for view in state.settings.registry.as_ref().unwrap().describe() {
            if Group::of(view.key, SettingOwner::Monokulo).tab() == tab
                && view.locked.is_none()
                && super::only_for_a_remote_engine(state, view.key).is_none()
            {
                form.push((view.key.to_string(), view.value));
            }
        }
        let engine_view = engine_settings(engine).await;
        for (key, view) in engine_view["scalars"].as_object().unwrap() {
            if Group::of(key, SettingOwner::Engine).tab() == tab && view["locked"].is_null() {
                let name = if super::is_monokulo_key(key) {
                    format!("engine:{key}")
                } else {
                    key.clone()
                };
                form.push((name, view["value"].as_str().unwrap().to_string()));
            }
        }
        form
    }

    /// Saving a tab as it is shown, nothing changed, leaves the options
    /// file exactly as it was, for monokulo's settings and the engine's:
    /// a field left at its default isn't a change. (Every default on the
    /// tab used to be written into the file, pinned there for good.)
    /// Changing one field then writes that one alone.
    #[tokio::test]
    async fn saving_a_tab_as_shown_leaves_the_options_file_as_it_was() {
        use crate::views::admin::SettingsTab;
        let dir = TempDir::new("as-shown");
        let path = dir.0.join("monokulo.toml");
        let text = "# Mine.\n[signup]\nmode = \"public\"\n";
        std::fs::write(&path, text).unwrap();
        let (engine, state) = embedded_on_file(&path).await;
        let mut forms = Vec::new();
        for tab in [
            SettingsTab::Abuse,
            SettingsTab::Payments,
            SettingsTab::Server,
        ] {
            forms.push(tab_as_shown(&state, &engine, tab).await);
        }
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        for form in &forms {
            assert!(form.len() > 2, "a tab with settings on it: {form:?}");
            let fields: Vec<(&str, &str)> =
                form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let save = post_settings(&router, &cookie, &fields).await;
            assert_eq!(save.status(), StatusCode::SEE_OTHER, "{form:?}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                text,
                "nothing changed, nothing written: {form:?}"
            );
        }

        // The Payments tab again, one engine field and one of monokulo's
        // changed: those two are written, nothing else.
        let mut payments = forms[1].clone();
        for (key, value) in &mut payments {
            match key.as_str() {
                "payment.confirmations_required" => *value = "4".to_string(),
                "exchange_rate.cache_seconds" => *value = "77".to_string(),
                _ => {}
            }
        }
        let fields: Vec<(&str, &str)> = payments
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let save = post_settings(&router, &cookie, &fields).await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER);
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with(text), "{saved}");
        let added: Vec<&str> = saved[text.len()..]
            .lines()
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(
            added,
            // The engine's part is saved first.
            [
                "[engine.payment]",
                "confirmations_required = 4",
                "[exchange_rate]",
                "cache_seconds = 77"
            ],
            "{saved}"
        );
    }

    /// monokulo with the engine inside it, on one options file at `path`,
    /// wired as monokulo's `main` wires them: monokulo's own settings through
    /// `file.leaving("engine")`, the engine's through `file.scoped("engine")`,
    /// so its settings are the `[engine.*]` tables of the same file.
    async fn embedded_on_file(
        path: &std::path::Path,
    ) -> (engine_test_support::TestEngineHandle, AppState) {
        let file = live_settings::OptionsFile::at(path);
        let engine = engine_test_support::TestEngineConfig::new()
            .embedded()
            .with_options(file.clone().scoped(crate::settings::ENGINE_TABLE))
            .spawn()
            .await;
        let state = test_app_state_with_client(
            EngineClient::embedded_for_tests(engine.router()),
            file.leaving(crate::settings::ENGINE_TABLE),
        )
        .await;
        (engine, state)
    }

    /// Every setting an engine inside monokulo has saves from the admin
    /// page into the `[engine.*]` tables of monokulo's own options file,
    /// leaving monokulo's settings and the admin's comments as they were,
    /// and reads back from there. The embedded counterpart of
    /// `every_remote_engine_setting_on_the_admin_page_saves_correctly_over_http`,
    /// less the remote engine's own `server.bind` and `logging.*`.
    #[tokio::test]
    async fn every_embedded_engine_setting_saves_from_the_admin_page_into_monokulos_options_file() {
        let dir = TempDir::new("embedded-every");
        let path = dir.0.join("monokulo.toml");
        let mine = "# Mine.\n[signup]\nmode = \"public\"\n";
        std::fs::write(&path, mine).unwrap();
        let (engine, state) = embedded_on_file(&path).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let new_values: &[(&str, &str)] = &[
            ("key_custody.enabled_backends", "plain"),
            ("key_custody.default_backend", "plain"),
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
            ("server.worker_threads", "4"),
            ("server.cpus", "0"),
            ("server.nice", "5"),
            ("server.rate_limit_per_token_per_min", "200"),
            ("server.max_body_bytes", "16384"),
            // The same key as monokulo's own, so sent as `engine:<key>`.
            ("engine:database.read_connections", "6"),
            ("order_events.retention_days", "14"),
        ];
        assert_eq!(
            new_values.len(),
            engine::engine_settings::embedded_settings()
                .iter()
                .filter(|s| (s.sources().toml || s.sources().database) && s.editable())
                .count()
                - engine::engine_settings::NETWORKS.len(),
            "this test must cover every setting an embedded engine has that the page can save, \
             apart from the node ones"
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
        let html = unescaped(&follow(&router, &cookie, save).await);
        assert!(is_saved(&html), "{html}");
        assert!(
            html.contains("take effect after the engine restarts"),
            "worker threads are restart-only: {html}"
        );
        assert!(
            !html.contains("set monokulo's engine.url to"),
            "an engine inside monokulo has no address to point monokulo at: {html}"
        );

        let html = settings_tabs_html(&router, &cookie).await;
        for (key, value) in new_values {
            if value.is_empty() {
                continue; // see the remote test: an empty value can't be told apart
            }
            assert!(
                shows_setting(&html, key, value),
                "expected {key}={value:?} to have round-tripped, got: {html}"
            );
        }

        // Where each one went: the file's `[engine.*]` tables, or the
        // engine's database for a runtime switch. Nothing else in the file
        // moved.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with(mine),
            "monokulo's own part is kept: {text}"
        );
        let view = engine_settings(&engine).await;
        let mut in_file = 0;
        for (key, _) in new_values {
            let key = key.trim_start_matches("engine:");
            let source = view["scalars"][key]["source"].as_str().unwrap_or_default();
            let (table, name) = key.rsplit_once('.').unwrap();
            match source {
                "toml" => {
                    in_file += 1;
                    let table_text = text
                        .split(&format!("[engine.{table}]\n"))
                        .nth(1)
                        .unwrap_or_else(|| panic!("no [engine.{table}] for {key}: {text}"));
                    let table_text = table_text.split("\n[").next().unwrap();
                    assert!(
                        table_text
                            .lines()
                            .any(|line| line.starts_with(&format!("{name} = "))),
                        "{key} under [engine.{table}]: {text}"
                    );
                }
                "database" => assert!(
                    !text.contains(&format!("{name} = ")),
                    "{key} is a runtime switch, not in the file: {text}"
                ),
                // Sent at its default (`plain` is the only backend here):
                // not a change, so not written.
                "default" => assert!(
                    !text.contains(&format!("{name} = ")),
                    "{key} is at its default, not pinned into the file: {text}"
                ),
                other => panic!("{key} came back from {other:?}: {view}"),
            }
        }
        assert!(in_file > 0, "{text}");
        assert!(
            !text.contains("[engine.logging]") && !text.contains("bind"),
            "no standalone-only setting reaches the file: {text}"
        );
    }

    /// A standalone engine's own settings (`server.bind`, `server.token`,
    /// `logging.*`) mean nothing to an engine inside monokulo: the page
    /// doesn't offer them, and a form that sends them anyway saves nothing,
    /// to the engine or to the file.
    #[tokio::test]
    async fn an_embedded_engines_standalone_only_settings_are_not_offered_and_a_form_sending_them_saves_nothing(
    ) {
        let dir = TempDir::new("embedded-standalone-only");
        let path = dir.0.join("monokulo.toml");
        let text = "[signup]\nmode = \"public\"\n";
        std::fs::write(&path, text).unwrap();
        let (engine, state) = embedded_on_file(&path).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let html = settings_tabs_html(&router, &cookie).await;
        for setting in engine::engine_settings::STANDALONE_ONLY {
            assert!(
                !html.contains(&format!("engine:{}", setting.key())),
                "{} isn't offered for an engine inside monokulo: {html}",
                setting.key()
            );
        }
        let before = engine_settings(&engine).await;
        for setting in engine::engine_settings::STANDALONE_ONLY {
            assert!(
                before["scalars"].get(setting.key()).is_none(),
                "{} isn't among an embedded engine's settings: {before}",
                setting.key()
            );
        }

        for (tab, key, value) in [
            ("server", "engine:server.bind", "127.0.0.1:9443"),
            ("logging", "engine:logging.level", "debug"),
            ("logging", "engine:logging.max_mb", "300"),
        ] {
            let response = post_settings(&router, &cookie, &[("tab", tab), (key, value)]).await;
            let saved = response.status() == StatusCode::SEE_OTHER;
            let html = if saved {
                unescaped(&follow(&router, &cookie, response).await)
            } else {
                unescaped(&body_text(response).await)
            };
            assert!(!is_saved(&html), "{key} is not saved: {html}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                text,
                "{key}: the file is untouched"
            );
        }
        let after = engine_settings(&engine).await;
        assert_eq!(
            after["scalars"], before["scalars"],
            "the engine is untouched"
        );
    }

    /// One tab with both monokulo's and the engine's settings: both halves
    /// are written into the one file, each in its own place, the admin's
    /// comments and the tables already there kept.
    #[tokio::test]
    async fn a_mixed_tab_writes_both_halves_into_the_one_options_file() {
        let dir = TempDir::new("embedded-mixed");
        let path = dir.0.join("monokulo.toml");
        std::fs::write(
            &path,
            "# Mine.\n[signup]\nmode = \"public\"\n\n# The engine's.\n[engine.payment]\nreorg_check_depth = 12\n",
        )
        .unwrap();
        let (engine, state) = embedded_on_file(&path).await;
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
        assert!(is_saved(&html), "{html}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# Mine.\n[signup]\nmode = \"public\"\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "# The engine's.\n[engine.payment]\nreorg_check_depth = 12\nconfirmations_required = 7\n"
            ),
            "the engine's half joins its table: {text}"
        );
        assert!(
            text.contains("[exchange_rate]\ncache_seconds = 88\n"),
            "monokulo's half, its own table: {text}"
        );
        let view = engine_settings(&engine).await;
        assert_eq!(
            view["scalars"]["payment.reorg_check_depth"]["value"], "12",
            "the engine read its table: {view}"
        );
        assert_eq!(
            view["scalars"]["payment.confirmations_required"]["source"],
            "toml"
        );

        // And again, now that each handle has written the file: neither
        // refuses the next save as "changed since it was loaded".
        let again = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "payments"),
                ("payment.confirmations_required", "8"),
                ("exchange_rate.cache_seconds", "89"),
            ],
        )
        .await;
        assert_eq!(again.status(), StatusCode::SEE_OTHER);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("confirmations_required = 8\n") && text.contains("cache_seconds = 89\n"),
            "{text}"
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
        let state = test_app_state_with_client(
            EngineClient::embedded_for_tests(app),
            live_settings::OptionsFile::in_memory("[signup]\nmode = \"public\"\n"),
        )
        .await;

        let mut req = super::RemoteUpdateRequest::default();
        req.scalars
            .insert("scan.poll_interval_secs".into(), "5".into());
        let notices = super::commit_engine(&state, req).await.unwrap();
        assert!(
            matches!(notices.as_slice(), [super::SaveNote::Remark(text)] if text.contains("reply could not be read")),
            "{notices:?}"
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
        let state = test_app_state_in_process(&engine).await;
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
            page.contains(r#"<span class="node-row-name" data-node-place>Primary</span>"#),
            "{page}"
        );

        // Rows the page's script reordered come numbered in their new order,
        // with no button pressed: saved in that order.
        let reordered = post_nodes(
            &router,
            &cookie,
            &nodes_form("stagenet", &[&b, &c], "", None),
        )
        .await;
        let page = follow(&router, &cookie, reordered).await;
        assert!(is_saved(&page), "{page}");
        assert!(
            toast_text(&page).contains("Changes saved and applied"),
            "{page}"
        );
        assert_eq!(
            saved_nodes(&engine, "stagenet").await,
            [b.clone(), c.clone()]
        );
    }

    /// A row that can't be a node: nothing is saved, and the page comes
    /// back with every value as typed and the problem under its address.
    #[tokio::test]
    async fn a_bad_address_is_shown_on_its_row_and_nothing_is_saved() {
        let engine = spawn_engine().await;
        let state = test_app_state_in_process(&engine).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let good = spawn_node_on("stagenet").await.to_string();

        let mut fields = nodes_form("stagenet", &[&good], "node.example.com", None);
        fields.push(("node_stagenet_1_ssl".to_string(), "on".to_string()));
        let refused = post_nodes(&router, &cookie, &fields).await;
        assert_eq!(
            refused.status(),
            StatusCode::SEE_OTHER,
            "the page again, not a redirect"
        );
        let html = follow(&router, &cookie, refused).await;
        assert!(
            html.contains("Some node addresses need fixing (marked below)."),
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
        let state = test_app_state_in_process(&engine).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let mainnet = spawn_node_on("mainnet").await;

        let refused = post_nodes(
            &router,
            &cookie,
            &nodes_form("testnet", &[], &mainnet.to_string(), None),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::SEE_OTHER);
        let html = unescaped(&follow(&router, &cookie, refused).await);
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
        assert!(toast_text(&html).contains(&message), "{html}");
        assert!(saved_nodes(&engine, "testnet").await.is_empty());
    }

    /// A refused save keeps every network's rows as sent, not only the
    /// refused one's: another network's valid change is still there to save
    /// or discard, and its unchanged rows read as saved.
    #[tokio::test]
    async fn a_refused_save_keeps_every_networks_rows_as_sent() {
        let engine = spawn_engine().await;
        let state = test_app_state_in_process(&engine).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let (a, b) = (
            spawn_node_on("stagenet").await,
            spawn_node_on("stagenet").await,
        );
        let mainnet = spawn_node_on("mainnet").await;
        let saved = post_nodes(
            &router,
            &cookie,
            &nodes_form("stagenet", &[], &a.to_string(), None),
        )
        .await;
        assert!(is_saved(&follow(&router, &cookie, saved).await));

        // Stagenet gains a node (fine); testnet gets one on mainnet (refused).
        let mut form = nodes_form("stagenet", &[&a.to_string()], &b.to_string(), None);
        form.extend(
            nodes_form("testnet", &[], &mainnet.to_string(), None)
                .into_iter()
                .filter(|(name, _)| name != "tab"),
        );
        let refused = post_nodes(&router, &cookie, &form).await;
        let html = unescaped(&follow(&router, &cookie, refused).await);
        assert!(toast_text(&html).contains("Nothing saved"), "{html}");
        let stagenet = &html[html.find(r#"id="card-network-stagenet""#).expect(&html)..];
        let stagenet = &stagenet[..stagenet.find("</section>").unwrap()];
        assert!(
            stagenet.contains(&format!(r#"value="{b}""#)),
            "the new node is still there: {stagenet}"
        );
        // The saved node matched as saved: first, unchanged.
        assert!(
            stagenet.contains(r#"data-node-row="0" data-node-saved="0">"#),
            "{stagenet}"
        );
        assert_eq!(saved_nodes(&engine, "stagenet").await, [a.to_string()]);
    }

    /// The engine's refusal of a setting monokulo has too (`logging.level`)
    /// shows beside the engine's, not monokulo's.
    #[tokio::test]
    async fn an_engine_refusal_shows_beside_the_engines_setting_not_monokulos_namesake_over_http() {
        // A remote engine: inside monokulo the engine's logging settings
        // are monokulo's.
        let engine = spawn_remote_engine().await;
        let state = test_app_state_over_http(engine.addr).await;
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let refused = post_settings(
            &router,
            &cookie,
            &[
                ("tab", "logging"),
                ("engine:logging.level", "info,engine=loud"),
            ],
        )
        .await;
        let html = unescaped(&follow(&router, &cookie, refused).await);
        let card = |id: &str| {
            let at = html.find(&format!(r#"id="card-{id}""#)).expect(&html);
            let rest = &html[at..];
            rest[..rest.find("</mk-settings-card>").unwrap()].to_string()
        };
        assert!(
            card("logging-engine").contains(r#"class="setting-problem""#),
            "{html}"
        );
        assert!(
            !card("logging-monokulo").contains(r#"class="setting-problem""#),
            "{html}"
        );
    }

    /// A key custody backend a save turned on that can't run is a banner.
    #[test]
    fn a_backend_that_cannot_run_after_a_save_is_a_banner() {
        let warnings: super::RemoteSaveWarnings = serde_json::from_value(serde_json::json!({
            "unavailable_backends": [{ "backend": "snp", "error": "this engine is not a trusted SEV-SNP image." }],
        }))
        .unwrap();
        assert_eq!(
            super::engine_save_notes(warnings, None),
            [super::SaveNote::Banner(
                "The snp key custody backend can't run: this engine is not a trusted SEV-SNP image. Stores on it aren't scanned until it can.".into()
            )]
        );
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
        let state = test_app_state_in_process(&engine).await;
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
        let state = test_app_state_in_process(&engine).await;
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
        // How the engine is doing (docs/engine_scaling.md section 6) is on
        // the engine page, which the network's card links to.
        assert!(
            html.contains(r#"<a class="engine-link" href="/status/engine?network=stagenet">See it on the engine page</a>"#),
            "{html}"
        );
        assert!(!html.contains("resources-title"), "{html}");
        assert!(!html.contains("data-scanning"), "{html}");
    }

    fn stagenet_tenant() -> crate::engine_client::CreateTenantRequest {
        crate::engine_client::CreateTenantRequest {
            keys: crate::engine_client::StoreKeys {
                view_key_hex: "0707070707070707070707070707070707070707070707070707070707070707"
                    .to_string(),
                spend_pubkey_hex:
                    "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90".to_string(),
                encrypted_keys: None,
            },
            network: Some("stagenet".to_string()),
            confirmations_required: None,
            order_expiry_seconds: None,
            key_custody_backend: None,
        }
    }
}
