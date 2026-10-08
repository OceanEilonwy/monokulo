//! The instance-wide admin settings API (`GET`/`POST /api/v1/admin/settings`).
//!
//! Server-level configuration (`crate::settings`), not any one tenant's own
//! admin API (`http::admin`, authenticated by that tenant's own `sk_`).
//!
//! This is what replaces the former TOML config file: every setting that used
//! to be read once at boot from a file is now read from the `settings` table
//! (the command line, then the environment, then the table, then the default:
//! `crate::engine_settings`) and editable here at runtime.
//!
//! Like every engine route, reachable only with the engine token
//! (`ENGINE_TOKEN`), which the router checks on every request
//! (`http::engine_token_middleware`); nothing here checks a credential of
//! its own.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::engine_settings::{EngineSettings, NETWORKS};
use crate::store::Database;

use super::{ApiError, Networks};

/// `base`, the scan budget's own help, with the most this machine allows
/// for each of `networks` networks.
fn budget_description(base: &str, networks: usize) -> String {
    let networks = u32::try_from(networks).unwrap_or(u32::MAX).max(1);
    match shared::resources::memory_limit_bytes() {
        Some(limit) => format!(
            "{base} On this machine: at most {} MB each, across {networks} network{}.",
            crate::engine_settings::max_scan_budget_mb(limit, networks),
            if networks == 1 { "" } else { "s" }
        ),
        None => base.to_owned(),
    }
}

/// One setting as the admin page shows it (tasks 4.1, 4.2): its effective
/// value and where that came from, plus what it's for, what it takes and
/// when it applies.
#[derive(Serialize)]
pub struct ScalarSettingView {
    value: String,
    /// `"toml"` (the options file), `"database"`, `"cli"`, `"env"`, or
    /// `"default"`.
    source: &'static str,
    /// How it is given from outside: its environment variable and/or
    /// command-line option, as its sources allow.
    set_with: String,
    /// Why the admin page can't change it now, if it can't: shown locked.
    locked: Option<String>,
    description: String,
    kind: live_settings::SettingKind,
    example: Option<&'static str>,
    /// `"live"` or `"restart"`.
    applies: live_settings::Applies,
    /// Saved, but the engine is still running with the value it started
    /// with (a restart-only setting).
    pending_restart: bool,
    /// Why the value in effect isn't the one set, if it isn't.
    problem: Option<String>,
}

fn source_str(source: live_settings::SettingSource) -> &'static str {
    match source {
        live_settings::SettingSource::Toml => "toml",
        live_settings::SettingSource::Cli => "cli",
        live_settings::SettingSource::Env => "env",
        live_settings::SettingSource::Database => "database",
        live_settings::SettingSource::Default => "default",
    }
}

/// One network's node setting as the admin page shows it.
#[derive(Serialize)]
pub struct NetworkView {
    description: &'static str,
    example: Option<&'static str>,
    source: &'static str,
    /// Enabled stores on this network (task 4.4's confirmation).
    tenant_count: u64,
}

#[derive(Serialize)]
pub struct SettingsView {
    /// Where the engine's options file is, and whether it can be written.
    options_file: Option<live_settings::FileInfo>,
    scalars: HashMap<String, ScalarSettingView>,
    monero_node: BTreeMap<String, Option<serde_json::Value>>,
    networks: HashMap<String, NetworkView>,
}

fn is_node_key(key: &str) -> Option<&'static str> {
    NETWORKS
        .iter()
        .find(|(_, setting)| setting.key == key)
        .map(|(network, _)| *network)
}

/// `GET /api/v1/admin/settings` - every setting's current effective value,
/// where it came from, and what it is, in one response, so the admin page
/// can render the whole form from a single call.
pub async fn get_settings(
    State(db): State<Database>,
    State(settings): State<Arc<EngineSettings>>,
    State(custody): State<super::Custody>,
) -> Result<Json<SettingsView>, ApiError> {
    let Some(registry) = settings.registry.as_ref() else {
        return Err(ApiError::Unavailable(
            "settings are not available on this engine".into(),
        ));
    };
    let tenant_counts = db
        .read(super::super::store::Store::count_tenants_by_network)
        .await?;
    let options_file = registry.options_file();
    let file_writable = options_file.as_ref().is_none_or(|file| file.writable);
    let read_only = format!(
        "The options file {} can't be written by the engine, so this is changed by editing it.",
        options_file.as_ref().map_or("", |file| file.path.as_str())
    );
    let mut scalars = HashMap::new();
    let mut monero_node = BTreeMap::new();
    let mut networks = HashMap::new();
    for view in registry.describe() {
        if let Some(network) = is_node_key(view.key) {
            let value = if view.value.trim().is_empty() {
                None
            } else {
                serde_json::from_str(&view.value).ok()
            };
            monero_node.insert(network.to_owned(), value);
            networks.insert(
                network.to_owned(),
                NetworkView {
                    description: view.description,
                    example: view.example,
                    source: source_str(view.source),
                    tenant_count: tenant_counts.get(network).copied().unwrap_or(0),
                },
            );
            continue;
        }
        // Inside monokulo, the standalone engine's own server and logging
        // settings do nothing: not shown.
        if settings.embedded && crate::engine_settings::standalone_only(view.key) {
            continue;
        }
        // Inside monokulo, the engine's options are monokulo's
        // `--engine-…` ones.
        let set_with = if settings.embedded {
            live_settings::outside_names(
                &format!("engine.{}", view.key),
                view.env_var,
                view.sources,
            )
        } else {
            live_settings::outside_names(view.key, view.env_var, view.sources)
        };
        scalars.insert(
            view.key.to_owned(),
            ScalarSettingView {
                value: view.value,
                source: source_str(view.source),
                set_with,
                locked: view.locked.or_else(|| {
                    // Kept in the options file, which can't be written.
                    (view.sources.toml && !file_writable).then(|| read_only.clone())
                }),
                description: view.description.to_owned(),
                kind: view.kind,
                example: view.example,
                applies: view.applies,
                pending_restart: view.pending_restart,
                problem: view.problem.map(|p| p.message),
            },
        );
    }
    // A backend that's turned on but can't run (it failed to start) says
    // so on the setting that turned it on.
    let cannot_run = backends_that_cannot_run(&custody).await;
    if let (false, Some(view)) = (
        cannot_run.is_empty(),
        scalars.get_mut(crate::engine_settings::KEY_CUSTODY_ENABLED_BACKENDS.key),
    ) {
        let said: Vec<String> = cannot_run
            .iter()
            .map(UnavailableBackend::sentence)
            .collect();
        view.problem = Some(
            view.problem
                .take()
                .into_iter()
                .chain(said)
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    // The budget's help says what this machine allows, for the networks
    // configured now (docs/engine_scaling.md section 3).
    if let Some(view) =
        scalars.get_mut(crate::engine_settings::PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB.key)
    {
        let configured_networks = monero_node.values().filter(|node| node.is_some()).count();
        view.description = budget_description(&view.description, configured_networks);
    }
    Ok(Json(SettingsView {
        options_file,
        scalars,
        monero_node,
        networks,
    }))
}

#[derive(Deserialize, Default)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    scalars: HashMap<String, String>,
    /// `null` for a network clears its configuration rather than being
    /// rejected: "stop watching this network" is a legitimate choice.
    #[serde(default)]
    monero_node: BTreeMap<String, Option<serde_json::Value>>,
}

/// How long a node being saved has to say which network it's on.
const NODE_INFO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The nodes submitted for each network that can never work there
/// (`nicer_admin_screen.md` T9): one listed twice, or one that answers that
/// it's on another network. Only networks whose nodes changed are checked;
/// their nodes are all asked at once, each for at most
/// [`NODE_INFO_TIMEOUT`], with no lock held. A node that doesn't answer, or
/// doesn't say (an old monerod, a `fakechain` one), is not refused: it may
/// just be down for now (decision D2), and saving it is allowed.
async fn nodes_that_cannot_work(
    submitted: &BTreeMap<String, Option<serde_json::Value>>,
    current: &crate::engine_settings::NodeConfig,
) -> Vec<live_settings::FieldError> {
    use crate::daemon::MoneroDaemonClient as _;
    use crate::settings::MoneroNodeSetting;

    let mut errors = Vec::new();
    let mut probes = Vec::new();
    for (network, value) in submitted {
        let Some((name, setting)) = NETWORKS.iter().find(|(n, _)| n == network) else {
            continue;
        };
        // Not a node the registry would accept: its own check says why.
        let Some(primary) = value
            .as_ref()
            .and_then(|v| serde_json::from_value::<MoneroNodeSetting>(v.clone()).ok())
        else {
            continue;
        };
        if current.nodes.get(name) == Some(&primary) {
            continue;
        }
        let nodes: Vec<MoneroNodeSetting> = std::iter::once(primary.clone())
            .chain(primary.fallbacks.iter().cloned())
            .collect();
        let mut seen = std::collections::HashSet::new();
        if let Some(twice) = nodes
            .iter()
            .find(|n| !seen.insert((n.host.to_ascii_lowercase(), n.port)))
        {
            errors.push(live_settings::FieldError::new(
                setting.key,
                format!("{}:{} is listed twice.", twice.host, twice.port),
            ));
            continue;
        }
        for node in nodes {
            let key = setting.key;
            let network = *name;
            probes.push(async move {
                let client = crate::daemon_rpc::RpcDaemonClient::new(
                    &node.host,
                    node.port,
                    node.ssl,
                    node.accept_self_signed_certs,
                )
                .ok()?;
                let info = tokio::time::timeout(NODE_INFO_TIMEOUT, client.get_info())
                    .await
                    .ok()?
                    .ok()?;
                let on = info.network()?;
                (crate::network::network_str(on) != network).then(|| {
                    live_settings::FieldError::new(
                        key,
                        format!(
                            "{}:{} is on {}, not {network}.",
                            node.host, node.port, info.nettype
                        ),
                    )
                })
            });
        }
    }
    let mut wrong: Vec<live_settings::FieldError> = futures_util::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect();
    // One message per network: the first of its nodes in the order saved.
    let mut reported = std::collections::HashSet::new();
    wrong.retain(|e| reported.insert(e.key.clone()));
    errors.extend(wrong);
    errors.sort_by(|a, b| a.key.cmp(&b.key));
    errors
}

fn refused(errors: &[live_settings::FieldError]) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
            "fields": errors.iter().map(|e| json!({ "key": e.key, "message": e.message })).collect::<Vec<_>>(),
        })),
    )
        .into_response()
}

/// A key custody backend that's turned on but can't run: it failed to
/// start, or doesn't answer.
#[derive(Serialize)]
pub struct UnavailableBackend {
    backend: String,
    error: String,
}

impl UnavailableBackend {
    fn sentence(&self) -> String {
        format!(
            "The {} backend can't run: {}.",
            self.backend,
            self.error.trim_end_matches('.')
        )
    }
}

/// The enabled key custody backends that can't run, each asked directly.
async fn backends_that_cannot_run(custody: &super::Custody) -> Vec<UnavailableBackend> {
    custody
        .backends
        .backend_health()
        .await
        .into_iter()
        .filter_map(|(backend, error)| {
            Some(UnavailableBackend {
                backend,
                error: error?,
            })
        })
        .collect()
}

/// A network that stores use but that has no node configured after a save
/// (decision D2): the save is accepted and the admin is told.
#[derive(Serialize)]
pub struct UnservedNetwork {
    network: String,
    tenants: u64,
}

/// Why a settings request is refused before the registry sees it.
enum Refusal {
    /// The engine has no settings registry.
    NoRegistry,
    /// Settings it may not take: on an engine inside monokulo, the ones
    /// only a standalone engine uses; nodes that can never work where
    /// they're being saved (T9).
    Fields(Vec<live_settings::FieldError>),
    /// A network the engine doesn't scan.
    UnknownNetwork(String),
}

impl IntoResponse for Refusal {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::NoRegistry => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "settings are not available on this engine" })),
            )
                .into_response(),
            Self::Fields(errors) => refused(&errors),
            Self::UnknownNetwork(network) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("unknown network {network:?}") })),
            )
                .into_response(),
        }
    }
}

/// The registry, when the engine has one.
fn registry_of(settings: &EngineSettings) -> Result<&live_settings::Registry, Refusal> {
    settings.registry.as_ref().ok_or(Refusal::NoRegistry)
}

/// A settings request as the registry takes it, once what can't be saved
/// at all is refused. A save and a check both start here, so a check
/// refuses exactly what a save would.
async fn changes_from(
    settings: &EngineSettings,
    req: UpdateSettingsRequest,
) -> Result<live_settings::Changes, Refusal> {
    if settings.embedded {
        let standalone: Vec<live_settings::FieldError> = req
            .scalars
            .keys()
            .filter(|key| crate::engine_settings::standalone_only(key))
            .map(|key| {
                live_settings::FieldError::new(
                    key.clone(),
                    "Only the engine running on its own uses this, not the engine inside monokulo.",
                )
            })
            .collect();
        if !standalone.is_empty() {
            return Err(Refusal::Fields(standalone));
        }
    }
    let mut changes: live_settings::Changes =
        req.scalars.into_iter().map(|(k, v)| (k, Some(v))).collect();
    let current_nodes = settings.nodes.load();
    let cannot_work = nodes_that_cannot_work(&req.monero_node, &current_nodes).await;
    if !cannot_work.is_empty() {
        return Err(Refusal::Fields(cannot_work));
    }
    for (network, node) in req.monero_node {
        let Some((_, setting)) = NETWORKS.iter().find(|(n, _)| *n == network) else {
            return Err(Refusal::UnknownNetwork(network));
        };
        let raw = match node {
            Some(value) if !value.is_null() => Some(value.to_string()),
            _ => None,
        };
        changes.push((setting.key.to_owned(), raw));
    }
    Ok(changes)
}

/// What a save, or a check, says changed (or would), and its warnings,
/// in the shape both answer with.
fn report_json(report: &live_settings::SaveReport) -> serde_json::Value {
    json!({
        "ok": true,
        "changed": report.changed,
        "warnings": {
            "restart_required": report.restart_required,
            "env_overridden": report.env_overridden,
            "messages": report.warnings.iter().map(|w| json!({ "key": w.key, "message": w.message })).collect::<Vec<_>>(),
        },
    })
}

/// A save's answer: what changed, its warnings, the networks stores use
/// that it left without a node that answers, and the key custody backends
/// it turned on that can't run.
fn saved(
    report: &live_settings::SaveReport,
    unserved: &[UnservedNetwork],
    unavailable: &[UnavailableBackend],
) -> axum::response::Response {
    let mut body = report_json(report);
    body["warnings"]["unserved_networks"] = json!(unserved);
    body["warnings"]["unavailable_backends"] = json!(unavailable);
    (StatusCode::OK, Json(body)).into_response()
}

/// `POST /api/v1/admin/settings`: saves any subset of settings through the
/// registry (`admin_settings_v2.md` part 1).
///
/// Every value is checked, runtime pieces that depend on changed settings
/// are prepared, everything is stored in one transaction, then applied to
/// the running engine.
///
/// Anything invalid refuses the whole save and changes nothing. The response
/// lists what needs a restart, warnings, settings still overridden by the
/// environment, and networks left without a node that stores use.
pub async fn update_settings(
    State(db): State<Database>,
    State(networks): State<Networks>,
    State(settings): State<Arc<EngineSettings>>,
    State(custody): State<super::Custody>,
    Json(req): Json<UpdateSettingsRequest>,
) -> axum::response::Response {
    let registry = match registry_of(&settings) {
        Ok(registry) => registry,
        Err(refusal) => return refusal.into_response(),
    };
    let changes = match changes_from(&settings, req).await {
        Ok(changes) => changes,
        Err(refusal) => return refusal.into_response(),
    };
    match registry.save(changes).await {
        Ok(report) => {
            // Networks stores use that have no node now, or whose just-saved
            // nodes don't answer (task 2.2, decision D2). Only saved
            // networks are probed, each node briefly, all at once.
            let counts = db
                .read(super::super::store::Store::count_tenants_by_network)
                .await
                .unwrap_or_default();
            let saved_networks: Vec<&str> = NETWORKS
                .iter()
                .filter(|(_, setting)| report.changed.contains(&setting.key))
                .map(|(n, _)| *n)
                .collect();
            let mut unserved = Vec::new();
            for (network, tenants) in counts {
                if tenants == 0 {
                    continue;
                }
                let Ok(parsed) = crate::network::parse_network(&network) else {
                    continue;
                };
                let reachable = match networks.daemons.get(parsed) {
                    None => false,
                    Some(_) if !saved_networks.contains(&network.as_str()) => true,
                    Some(daemon) => {
                        let probes = daemon.nodes().iter().map(|node| {
                            tokio::time::timeout(
                                std::time::Duration::from_secs(3),
                                node.client.get_height(),
                            )
                        });
                        futures_util::future::join_all(probes)
                            .await
                            .into_iter()
                            .any(|r| matches!(r, Ok(Ok(_))))
                    }
                };
                if !reachable {
                    unserved.push(UnservedNetwork { network, tenants });
                }
            }
            unserved.sort_by(|a, b| a.network.cmp(&b.network));
            // A backend this save turned on is started when the save is
            // installed, after it was checked: one that then failed says so.
            let custody_changed = report
                .changed
                .iter()
                .any(|key| key.starts_with("key_custody."));
            let unavailable = if custody_changed {
                backends_that_cannot_run(&custody).await
            } else {
                Vec::new()
            };
            saved(&report, &unserved, &unavailable)
        }
        Err(e) => save_refused(e),
    }
}

/// `POST /api/v1/admin/settings/check`: checks a settings request exactly
/// as `POST /api/v1/admin/settings` would save it, and saves nothing.
///
/// It goes through the save's own `changes_from` and `live_settings::Registry::check`.
/// What a save would refuse is refused the same way: `400` with the same
/// `fields`. Otherwise `200` with what a save would report: the keys that
/// would change, `has_changes` (false when none would), what would wait for
/// a restart, what the environment would still override, and the warnings
/// from preparing. Nodes whose settings change are asked which network
/// they're on, as a save asks them.
pub async fn check_settings(
    State(settings): State<Arc<EngineSettings>>,
    Json(req): Json<UpdateSettingsRequest>,
) -> axum::response::Response {
    let registry = match registry_of(&settings) {
        Ok(registry) => registry,
        Err(refusal) => return refusal.into_response(),
    };
    let changes = match changes_from(&settings, req).await {
        Ok(changes) => changes,
        Err(refusal) => return refusal.into_response(),
    };
    match registry.check(changes).await {
        Ok(report) => {
            let mut body = report_json(report.would());
            body["has_changes"] = json!(report.has_changes());
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(e) => save_refused(e),
    }
}

/// Why a save or a reload changed nothing, as the response says it.
fn save_refused(error: live_settings::SaveError) -> axum::response::Response {
    match error {
        live_settings::SaveError::Invalid(errors) => refused(&errors),
        live_settings::SaveError::UnknownKey(key) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("there is no setting called {key:?}") })),
        )
            .into_response(),
        // The options file changed since it was read, can't be written, or
        // has something wrong in it: the admin fixes or reloads it.
        live_settings::SaveError::Store(e) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
        e @ (live_settings::SaveError::NotBooted | live_settings::SaveError::Install(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `DELETE /api/v1/admin/proof/{network}/anchor`: forgets `network`'s
/// proof-of-work anchor and proven chain (`docs/proof_of_work.md`).
///
/// Checking stays on: its next round takes a new anchor from the nodes. For
/// an operator after a reorg deeper than the anchor, once the nodes are
/// trusted again; nothing settles until the new anchor is taken. `404`
/// while checking is off there.
pub async fn forget_anchor(
    State(db): State<Database>,
    axum::extract::Path(network): axum::extract::Path<String>,
) -> Result<StatusCode, ApiError> {
    let network =
        crate::network::parse_network(&network).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let forgotten = db
        .write(move |s| {
            if s.proof_network(network)?.is_none() {
                return Ok(false);
            }
            s.forget_anchor(network)?;
            Ok::<bool, crate::store::StoreError>(true)
        })
        .await?;
    if !forgotten {
        return Err(ApiError::NotFound);
    }
    tracing::warn!(network = ?network, "proof-of-work anchor forgotten by an operator; a new one is taken next round");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/admin/settings/reload`: reads the options file again.
///
/// What changed in it is applied as a save would: all of it, or, when
/// anything in it is wrong, none of it, with every problem named by line.
///
/// The response says what changed and what needs a restart.
pub async fn reload_settings(
    State(settings): State<Arc<EngineSettings>>,
) -> axum::response::Response {
    let Some(registry) = settings.registry.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "settings are not available on this engine" })),
        )
            .into_response();
    };
    match registry.reload().await {
        Ok(report) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "changed": report.changed,
                "warnings": {
                    "restart_required": report.restart_required,
                    "env_overridden": report.env_overridden,
                    "messages": report.warnings.iter().map(|w| json!({ "key": w.key, "message": w.message })).collect::<Vec<_>>(),
                    "unserved_networks": Vec::<UnservedNetwork>::new(),
                },
            })),
        )
            .into_response(),
        Err(e) => save_refused(e),
    }
}
