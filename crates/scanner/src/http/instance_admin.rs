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

use crate::settings::{self, MoneroNodeSetting, NETWORKS};
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
        let store = state.store.lock().unwrap();
        let effective_hash = effective_admin_token_hash(&store).map_err(|_| ApiError::Internal("settings lookup failed".into()))?;
        match effective_hash {
            Some(hash) if hash == presented_hash => Ok(AuthedInstanceAdmin),
            _ => Err(ApiError::Unauthorized),
        }
    }
}

#[derive(Serialize)]
pub struct ScalarSettingView {
    value: String,
    /// `"env"`, `"database"`, or `"default"` - see `shared::settings::SettingSource`.
    /// A plain string over the wire rather than re-deriving `Serialize` for that
    /// enum: this is the only place it's ever exposed externally, so a one-off
    /// `match` here is clearer than a second representation to keep in sync.
    source: &'static str,
}

fn source_str(source: shared::settings::SettingSource) -> &'static str {
    match source {
        shared::settings::SettingSource::Env => "env",
        shared::settings::SettingSource::Database => "database",
        shared::settings::SettingSource::Default => "default",
    }
}

#[derive(Serialize)]
pub struct SettingsView {
    scalars: HashMap<String, ScalarSettingView>,
    monero_node: HashMap<String, Option<MoneroNodeSetting>>,
}

/// `GET /api/v1/admin/settings` - every known setting's current effective
/// value (and where it actually came from), in one response, so an admin
/// page can render the whole form from a single call.
pub async fn get_settings(AuthedInstanceAdmin: AuthedInstanceAdmin, State(state): State<AppState>) -> impl IntoResponse {
    let store = state.store.lock().unwrap();
    let scalars = settings::ALL_SCALAR
        .iter()
        .map(|s| {
            let (value, source) = settings::get_raw(&store, s);
            (s.key.to_string(), ScalarSettingView { value, source: source_str(source) })
        })
        .collect();
    let monero_node =
        NETWORKS.iter().map(|&network| (network.to_string(), settings::monero_node_setting(&store, network))).collect();
    Json(SettingsView { scalars, monero_node })
}

#[derive(Deserialize, Default)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    scalars: HashMap<String, String>,
    /// `null` for a network clears its configuration (removes the row)
    /// rather than being rejected as an invalid `MoneroNodeSetting` - "stop
    /// watching this network" is a legitimate, explicit choice, not a
    /// malformed request.
    #[serde(default)]
    monero_node: HashMap<String, Option<MoneroNodeSetting>>,
}

/// Range/shape checks mirroring the former `config.rs::Config::validate_bounds`
/// - the same "a silent failure mode at the wrong value is worse than a loud
/// rejection" reasoning, applied at *save* time now instead of boot time,
/// since there is no boot moment for a setting changed at runtime to fail
/// loudly at instead. Only checks the scalar being saved in isolation; the one
/// real cross-field rule this instance still has (`key_custody.socket_path`
/// required when `key_custody.backend = "socket"`) is checked separately in
/// `update_settings` against the *merged* post-save state, not here.
fn validate_scalar(key: &str, value: &str) -> Result<(), String> {
    fn require_range<T: std::str::FromStr + PartialOrd + std::fmt::Display>(
        key: &str,
        value: &str,
        min: T,
        max: T,
        expected: &str,
    ) -> Result<(), String> {
        let parsed: T = value.parse().map_err(|_| format!("{key} must be a number, got {value:?}"))?;
        if parsed < min || parsed > max {
            return Err(format!("{key} is {value}, but must be {expected}"));
        }
        Ok(())
    }
    match key {
        "payment.confirmations_required" => {
            require_range::<u64>(key, value, 0, 720, "at least 0 (native 0-conf: paid off a mempool sighting alone) and at most 720 (~24h)")
        }
        "payment.order_expiry_minutes" => {
            require_range::<i64>(key, value, 1, 60 * 24 * 365, "at least 1 minute and at most a year")
        }
        "payment.reorg_check_depth" => require_range::<u64>(key, value, 1, 10_000, "at least 1 block and at most 10000"),
        "payment.mempool_poll_interval_ms" => {
            require_range::<u64>(key, value, 100, 3_600_000, "at least 100ms and at most an hour")
        }
        "payment.default_rescan_lookback_days" => {
            require_range::<u32>(key, value, 1, 3650, "at least 1 day and at most 3650 (10 years)")
        }
        "payment.max_rescan_lookback_days" => {
            require_range::<u32>(key, value, 1, 3650, "at least 1 day and at most 3650 (10 years)")
        }
        "payment.expired_order_grace_period_minutes" => {
            require_range::<i64>(key, value, 0, 60 * 24 * 365, "at least 0 and at most a year")
        }
        "server.bind" => value
            .parse::<std::net::SocketAddr>()
            .map(|_| ())
            .map_err(|_| format!("server.bind {value:?} is not a valid address:port, e.g. \"0.0.0.0:8443\"")),
        "server.worker_threads" => require_range::<usize>(key, value, 1, 1024, "at least 1"),
        "server.rate_limit_per_ip_per_min" => require_range::<u32>(
            key,
            value,
            1,
            1_000_000,
            "at least 1 (0 rejects every request, including the merchant's own)",
        ),
        "server.rate_limit_per_token_per_min" => require_range::<u32>(
            key,
            value,
            1,
            1_000_000,
            "at least 1 (0 rejects every admin API request, including a legitimate tenant's own)",
        ),
        "server.max_body_bytes" => {
            require_range::<usize>(key, value, 256, 16 * 1024 * 1024, "at least 256 bytes and at most 16MiB")
        }
        "webhooks.allow_private_urls" => {
            value.parse::<bool>().map(|_| ()).map_err(|_| format!("{key} must be \"true\" or \"false\", got {value:?}"))
        }
        "webhooks.delivery_timeout_ms" => {
            require_range::<u64>(key, value, 100, 300_000, "at least 100ms and at most 5 minutes")
        }
        "webhooks.max_attempts" => require_range::<u32>(key, value, 1, 64, "at least 1 and at most 64"),
        "key_custody.backend" => {
            if value == "plain" || value == "socket" {
                Ok(())
            } else {
                Err(format!("key_custody.backend {value:?} is not implemented - only \"plain\" and \"socket\" exist"))
            }
        }
        // key_custody.socket_path has no shape of its own to check here - see
        // update_settings's own cross-field check against the merged state.
        _ => Ok(()),
    }
}

/// `POST /api/v1/admin/settings` - persists any subset of scalars and/or
/// `monero_node.<network>` entries present in the request body. Every
/// scalar's own range is checked before anything is written (so a request
/// setting five fields, one of them invalid, changes nothing rather than
/// four fifths of what was asked); the one cross-field rule left
/// (`key_custody.socket_path` required under `backend = "socket"`) is
/// checked against the state this request would actually leave behind -
/// merging what's being saved now with whatever is already stored for the
/// half not being touched in this particular call.
pub async fn update_settings(
    AuthedInstanceAdmin: AuthedInstanceAdmin,
    State(state): State<AppState>,
    Json(req): Json<UpdateSettingsRequest>,
) -> axum::response::Response {
    for (key, value) in &req.scalars {
        if let Err(message) = validate_scalar(key, value) {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response();
        }
    }

    let store = state.store.lock().unwrap();

    let backend = req
        .scalars
        .get("key_custody.backend")
        .cloned()
        .unwrap_or_else(|| settings::get::<String>(&store, &settings::KEY_CUSTODY_BACKEND));
    let socket_path = req
        .scalars
        .get("key_custody.socket_path")
        .cloned()
        .unwrap_or_else(|| settings::get::<String>(&store, &settings::KEY_CUSTODY_SOCKET_PATH));
    if backend == "socket" && socket_path.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "key_custody.backend is \"socket\" but key_custody.socket_path is missing (or empty) - \
                          set it to the Unix socket path a running key-custody-server process is listening on"
            })),
        )
            .into_response();
    }

    for (key, value) in &req.scalars {
        if let Err(e) = store.set_setting(key, value) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
        }
    }
    for (network, node) in &req.monero_node {
        let result = match node {
            Some(node) => settings::set_monero_node_setting(&store, network, node),
            None => store.delete_setting(&format!("monero_node.{network}")),
        };
        if let Err(e) = result {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
        }
    }

    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}
