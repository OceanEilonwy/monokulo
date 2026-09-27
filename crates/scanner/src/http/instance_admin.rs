//! The instance-wide admin settings API (`GET`/`POST /api/v1/admin/settings`) -
//! server-level configuration (`crate::settings`), not any one tenant's own
//! admin API (`http::admin`, authenticated by that tenant's own `sk_`). This
//! is what replaces the former TOML config file: every setting that used to
//! be read once at boot from a file is now read from the `settings` table
//! (`env > database > default`, `shared::settings`) and editable here at
//! runtime.
//!
//! Authenticated by a single instance-wide admin token
//! ([`shared::auth::generate_admin_token`]), generated once on first boot if
//! neither an existing stored hash nor the `SCANNER_ADMIN_TOKEN` environment
//! variable already provides one (see [`ensure_admin_token_seeded`]) - a
//! distinct credential type from any tenant's own `sk_`, since a tenant
//! having its own admin secret has no business also being able to change
//! this instance's node endpoints, rate limits, or webhook SSRF policy.

use axum::extract::{FromRequestParts, State};
use axum::http::{header, request::Parts, StatusCode};
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

use crate::engine_settings::NETWORKS;
use crate::store::Store;

use super::{ApiError, AppState};

/// The `settings` table key an instance admin token's SHA-256 hash is stored
/// under, once generated - see [`ensure_admin_token_seeded`].
const ADMIN_TOKEN_HASH_KEY: &str = "instance_admin_token_hash";

/// The environment variable that, if set, is this instance's admin token
/// outright - same `env > database` precedence every other setting uses,
/// applied to the credential itself rather than a value it gates.
const ADMIN_TOKEN_ENV_VAR: &str = "SCANNER_ADMIN_TOKEN";

/// The admin token's *currently effective* hash - `SCANNER_ADMIN_TOKEN`
/// (hashed fresh on every check, never persisted just for being present) if
/// set, otherwise whatever is stored. `None` only if neither exists yet,
/// which should never actually happen against a store that has been through
/// [`ensure_admin_token_seeded`] - callers still treat that as "reject every
/// request" (a missing credential can never mean "open access"), not a panic.
fn effective_admin_token_hash(store: &Store) -> Result<Option<String>, StatusCode> {
    if let Ok(raw) = std::env::var(ADMIN_TOKEN_ENV_VAR) {
        if !raw.trim().is_empty() {
            return Ok(Some(shared::auth::hash_secret_token(&raw)));
        }
    }
    store.get_setting(ADMIN_TOKEN_HASH_KEY).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Called once at boot (`main.rs`), before the HTTP server starts accepting
/// requests. If `SCANNER_ADMIN_TOKEN` is set, or a token was already
/// generated on a previous boot, this does nothing (`None`) - a fresh token
/// is only ever minted when *neither* exists yet, the same "first boot"
/// condition `docs/DESIGN.md`'s own tenant-secret story already treats as the
/// one time a credential is shown in the clear at all. Returns the raw token
/// only on that first-boot path, for the caller to print - this function
/// itself never prints anything, so a test can seed a database directly with
/// this instead of scraping stdout for a token it needs to authenticate with.
#[allow(clippy::expect_used, reason = "boot-time only: without a stored token the admin API is unusable, so failing loudly is right")]
pub fn ensure_admin_token_seeded(store: &Store) -> Option<String> {
    if std::env::var(ADMIN_TOKEN_ENV_VAR).is_ok_and(|v| !v.trim().is_empty()) {
        return None;
    }
    if store.get_setting(ADMIN_TOKEN_HASH_KEY).ok().flatten().is_some() {
        return None;
    }
    let token = shared::auth::generate_admin_token();
    let hash = shared::auth::hash_secret_token(&token);
    store.set_setting(ADMIN_TOKEN_HASH_KEY, &hash).expect("failed to persist a freshly generated instance admin token");
    Some(token)
}

/// Seeds a known instance admin token directly - the settings-API analogue of
/// `scanner_test_support`'s own fixed test credentials, for a test that needs
/// to authenticate against this API without depending on (or scraping stdout
/// for) whatever `ensure_admin_token_seeded` would otherwise generate.
#[cfg(test)]
pub fn seed_admin_token_for_tests(store: &Store, raw_token: &str) {
    store.set_setting(ADMIN_TOKEN_HASH_KEY, &shared::auth::hash_secret_token(raw_token)).unwrap();
}

/// Resolves to this only once a presented `Authorization: Bearer <token>`
/// hashes to the instance's own effective admin token - see
/// [`effective_admin_token_hash`]. Structurally separate from `AuthedTenant`
/// (`http::mod`) on purpose: no tenant's own `sk_` can ever satisfy this,
/// and this can never satisfy a route expecting a specific tenant.
pub struct AuthedInstanceAdmin;

impl FromRequestParts<AppState> for AuthedInstanceAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header_value =
            parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).ok_or(ApiError::Unauthorized)?;
        let token = header_value.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
        let presented_hash = shared::auth::hash_secret_token(token);
        let store = state.store.lock();
        let effective_hash = effective_admin_token_hash(&store).map_err(|_| ApiError::Internal("settings lookup failed".into()))?;
        match effective_hash {
            Some(hash) if hash == presented_hash => Ok(AuthedInstanceAdmin),
            _ => Err(ApiError::Unauthorized),
        }
    }
}

/// One setting as the admin page shows it (tasks 4.1, 4.2): its effective
/// value and where that came from, plus what it's for, what it takes and
/// when it applies.
#[derive(Serialize)]
pub struct ScalarSettingView {
    value: String,
    /// `"env"`, `"database"`, or `"default"`.
    source: &'static str,
    description: &'static str,
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
    scalars: HashMap<String, ScalarSettingView>,
    monero_node: HashMap<String, Option<serde_json::Value>>,
    networks: HashMap<String, NetworkView>,
}

fn is_node_key(key: &str) -> Option<&'static str> {
    NETWORKS.iter().find(|(_, setting)| setting.key == key).map(|(network, _)| *network)
}

/// `GET /api/v1/admin/settings` - every setting's current effective value,
/// where it came from, and what it is, in one response, so the admin page
/// can render the whole form from a single call.
pub async fn get_settings(
    AuthedInstanceAdmin: AuthedInstanceAdmin,
    State(state): State<AppState>,
) -> Result<Json<SettingsView>, ApiError> {
    let Some(registry) = state.settings.registry.as_ref() else {
        return Err(ApiError::Unavailable("settings are not available on this engine".into()));
    };
    let tenant_counts = state.store.lock().count_tenants_by_network()?;
    let mut scalars = HashMap::new();
    let mut monero_node = HashMap::new();
    let mut networks = HashMap::new();
    for view in registry.describe() {
        if let Some(network) = is_node_key(view.key) {
            let value = if view.value.trim().is_empty() { None } else { serde_json::from_str(&view.value).ok() };
            monero_node.insert(network.to_string(), value);
            networks.insert(
                network.to_string(),
                NetworkView {
                    description: view.description,
                    example: view.example,
                    source: source_str(view.source),
                    tenant_count: tenant_counts.get(network).copied().unwrap_or(0),
                },
            );
            continue;
        }
        scalars.insert(
            view.key.to_string(),
            ScalarSettingView {
                value: view.value,
                source: source_str(view.source),
                description: view.description,
                kind: view.kind,
                example: view.example,
                applies: view.applies,
                pending_restart: view.pending_restart,
                problem: view.problem.map(|p| p.message),
            },
        );
    }
    Ok(Json(SettingsView { scalars, monero_node, networks }))
}

#[derive(Deserialize, Default)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    scalars: HashMap<String, String>,
    /// `null` for a network clears its configuration rather than being
    /// rejected: "stop watching this network" is a legitimate choice.
    #[serde(default)]
    monero_node: HashMap<String, Option<serde_json::Value>>,
}

/// A network that stores use but that has no node configured after a save
/// (decision D2): the save is accepted and the admin is told.
#[derive(Serialize)]
pub struct UnservedNetwork {
    network: String,
    tenants: u64,
}

/// `POST /api/v1/admin/settings` - saves any subset of settings through the
/// registry (admin_settings_v2.md part 1): every value is checked, runtime
/// pieces that depend on changed settings are prepared, everything is
/// stored in one transaction, then applied to the running engine. Anything
/// invalid refuses the whole save and changes nothing. The response lists
/// what needs a restart, warnings, settings still overridden by the
/// environment, and networks left without a node that stores use.
pub async fn update_settings(
    AuthedInstanceAdmin: AuthedInstanceAdmin,
    State(state): State<AppState>,
    Json(req): Json<UpdateSettingsRequest>,
) -> axum::response::Response {
    let Some(registry) = state.settings.registry.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "settings are not available on this engine" })))
            .into_response();
    };
    let mut changes: live_settings::Changes = req.scalars.into_iter().map(|(k, v)| (k, Some(v))).collect();
    for (network, node) in req.monero_node {
        let Some((_, setting)) = NETWORKS.iter().find(|(n, _)| *n == network) else {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("unknown network {network:?}") }))).into_response();
        };
        let raw = match node {
            Some(value) if !value.is_null() => Some(value.to_string()),
            _ => None,
        };
        changes.push((setting.key.to_string(), raw));
    }

    match registry.save(changes).await {
        Ok(report) => {
            // Networks stores use that have no node now, or whose just-saved
            // nodes don't answer (task 2.2, decision D2). Only saved
            // networks are probed, each node briefly, all at once.
            let counts = state.store.lock().count_tenants_by_network().unwrap_or_default();
            let saved_networks: Vec<&str> =
                NETWORKS.iter().filter(|(_, setting)| report.changed.contains(&setting.key)).map(|(n, _)| *n).collect();
            let mut unserved = Vec::new();
            for (network, tenants) in counts {
                if tenants == 0 {
                    continue;
                }
                let Ok(parsed) = crate::network::parse_network(&network) else { continue };
                let reachable = match state.daemons.get(parsed) {
                    None => false,
                    Some(_) if !saved_networks.contains(&network.as_str()) => true,
                    Some(daemon) => {
                        let probes = daemon.nodes().iter().map(|node| {
                            tokio::time::timeout(std::time::Duration::from_secs(3), node.client.get_height())
                        });
                        futures_util::future::join_all(probes).await.into_iter().any(|r| matches!(r, Ok(Ok(_))))
                    }
                };
                if !reachable {
                    unserved.push(UnservedNetwork { network, tenants });
                }
            }
            unserved.sort_by(|a, b| a.network.cmp(&b.network));
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "changed": report.changed,
                    "warnings": {
                        "restart_required": report.restart_required,
                        "env_overridden": report.env_overridden,
                        "messages": report.warnings.iter().map(|w| json!({ "key": w.key, "message": w.message })).collect::<Vec<_>>(),
                        "unserved_networks": unserved,
                    },
                })),
            )
                .into_response()
        }
        Err(live_settings::SaveError::Invalid(errors)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
                "fields": errors.iter().map(|e| json!({ "key": e.key, "message": e.message })).collect::<Vec<_>>(),
            })),
        )
            .into_response(),
        Err(live_settings::SaveError::UnknownKey(key)) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("there is no setting called {key:?}") }))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

