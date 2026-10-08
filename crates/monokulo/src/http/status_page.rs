//! `GET /status` - the real, styled operator status page: every configured
//! Monero node's live reachability/height and the chain-scanner loop's own
//! recent tick history, across every network the engine is configured for.
//!
//! This used to live directly on the engine (`src/http/status_page.rs` at
//! the repo root) - the wrong layer, since the engine has no product-facing
//! visual identity of its own. The engine's `/status` is now a plain JSON
//! API (`EngineClient::get_status`); this module is the one place that data
//! gets turned into something a person actually reads, styled to match
//! every other monokulo page.
//!
//! Deliberately unauthenticated, same as the engine's own `/status` - it
//! reports on infrastructure health, not any one merchant's private data,
//! and the nav bar's glowing status indicator (`GET /status/summary`, also
//! this module) needs to be checkable from any page, logged in or not.
//!
//! If the engine itself can't be reached at all, that's shown as a plain
//! error banner rather than a 500 or a fabricated "everything's fine" page -
//! the engine being unreachable is itself the most important fact this page
//! can report.
//!
//! **A real incident, and why [`StatusCache`] exists**: the nav bar's status
//! dot (`GET /status/summary`) fires on *every* page load, for every
//! visitor - which turned out to mean one real person just browsing the
//! dashboard could, by itself, exceed the engine's own per-IP rate limit for
//! `/status` (monokulo is the engine's one caller, so every browser's
//! traffic arrives from the same source IP - see `http::rate_limit`'s own
//! module doc comment, which this incident is also what motivated). A short,
//! shared, in-memory TTL cache means every viewer within the TTL window gets
//! one real engine request between them, not one each - the fix that
//! actually addresses the request *volume*, independent of whichever engine-
//! side limit is configured.

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::engine_client::{EngineClientError, EngineStatusResponse, NetworkStatus};
use crate::views;

use super::{AppState, Engine};

/// How long a fetched status is trusted before the next request triggers a
/// fresh one. Short enough that the page (which also has its own 30s meta
/// refresh) never looks stale to a person watching it; long enough that a
/// burst of page loads - many browser tabs, many users, the nav dot firing
/// on every one - costs the engine one real request, not one per view.
const CACHE_TTL: Duration = Duration::from_secs(10);

pub struct CachedStatus {
    fetched_at: Instant,
    // `String`, not `EngineClientError` - the cached value has to be
    // `Clone` to hand out without holding the lock across an `.await`, and
    // `reqwest::Error` (inside `EngineClientError::Request`) isn't.
    result: Result<EngineStatusResponse, String>,
}

#[derive(Default)]
pub struct StatusCacheState {
    cached: Option<CachedStatus>,
    /// A background refresh started by [`known_health`] is in flight, so a
    /// burst of page loads starts one, not one each.
    refreshing: bool,
}

/// The cache, and the one fetch from the engine that may be in flight.
pub struct StatusCacheShared {
    state: Mutex<StatusCacheState>,
    /// Held while fetching: requests that find the cache stale meanwhile
    /// wait for that fetch and take its answer, so a burst of page loads
    /// costs the engine (and every node it asks) one request, not one each.
    fetching: tokio::sync::Mutex<()>,
}

pub type StatusCache = Arc<StatusCacheShared>;

pub(crate) fn new_status_cache() -> StatusCache {
    Arc::new(StatusCacheShared {
        state: Mutex::new(StatusCacheState::default()),
        fetching: tokio::sync::Mutex::new(()),
    })
}

/// The status cache.
pub(crate) fn status_cache(engine: &Engine) -> parking_lot::MutexGuard<'_, StatusCacheState> {
    engine.status_cache.state.lock()
}

/// How old a cached status may be and still be rendered into a page as the
/// status indicator's known state; anything older shows as unknown.
const KNOWN_STATUS_MAX_AGE: Duration = Duration::from_secs(300);

/// The health every page's status indicator is rendered with: whatever the
/// cache last learned, without waiting on the engine. A page load that finds
/// it stale starts a background refresh, so the next page (or the
/// indicator's own poll) sees a fresh answer - which keeps it current for
/// visitors without JavaScript too.
pub fn known_health(engine: &Engine) -> Option<views::Health> {
    let mut cache = status_cache(engine);
    let age = cache
        .cached
        .as_ref()
        .map(|cached| cached.fetched_at.elapsed());
    if age.is_none_or(|age| age >= CACHE_TTL) && !cache.refreshing {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            cache.refreshing = true;
            let engine = engine.clone();
            runtime.spawn(async move {
                // Cleared however the refresh ends: finished, failed,
                // timed out, or the task dropped.
                struct Done(Engine);
                impl Drop for Done {
                    fn drop(&mut self) {
                        status_cache(&self.0).refreshing = false;
                    }
                }
                let _done = Done(engine.clone());
                let _ = tokio::time::timeout(
                    crate::engine_client::ENGINE_CALL_TIMEOUT,
                    get_status_cached(&engine),
                )
                .await;
            });
        }
    }
    let cached = cache
        .cached
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < KNOWN_STATUS_MAX_AGE)?;
    Some(health_of(&cached.result))
}

/// Green, yellow or red: a problem if [`is_healthy`] says so, else slow if
/// any network has a block that has taken over two minutes
/// (docs/engine_scaling.md section 5), else fine.
fn health_of(result: &Result<EngineStatusResponse, String>) -> views::Health {
    if !is_healthy(result) {
        return views::Health::Problem;
    }
    let slow = result.as_ref().is_ok_and(|status| {
        status
            .networks
            .iter()
            .any(|n| n.scaling.as_ref().is_some_and(|s| s.slow.is_some()))
    });
    if slow {
        views::Health::Slow
    } else {
        views::Health::Ok
    }
}

/// The networks with a slow block, as one sentence each
/// (docs/engine_scaling.md section 5). The node and its rate only for an
/// operator: anyone else isn't shown node addresses.
pub fn slow_block_messages(status: &EngineStatusResponse, show_node: bool) -> Vec<String> {
    status
        .networks
        .iter()
        .filter_map(|n| {
            let slow = n.scaling.as_ref()?.slow.as_ref()?;
            Some(views::scaling::slow_block_message(
                &n.network, slow, show_node,
            ))
        })
        .collect()
}

/// The slow-block sentences from the cached status, for an operator's
/// alert bar; nothing while the status isn't known.
pub fn known_slow_blocks(engine: &Engine) -> Vec<String> {
    let cache = status_cache(engine);
    cache
        .cached
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < KNOWN_STATUS_MAX_AGE)
        .and_then(|cached| cached.result.as_ref().ok())
        .map(|status| slow_block_messages(status, true))
        .unwrap_or_default()
}

/// The key custody backends a new store may choose from, with the default
/// first, from the same cache as [`known_health`] (part 5), less any this
/// site doesn't offer now (`key_entry::usable_custody_backends`). Empty
/// unless more than one is left, so forms only show the choice when there
/// is one to make.
fn known_custody_choices(state: &AppState) -> Vec<String> {
    let choices = super::key_entry::usable_custody_backends(state);
    if choices.len() < 2 {
        return Vec::new();
    }
    choices
}

/// Which images the engine's `snp` backend trusts, from the same cache as
/// [`known_health`]: `None` while the status isn't known, `Some(None)`
/// when the engine has no `snp` backend set up.
pub fn known_snp_trust(engine: &Engine) -> Option<Option<crate::engine_client::SnpTrustStatus>> {
    let cache = status_cache(engine);
    cache
        .cached
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < KNOWN_STATUS_MAX_AGE)
        .and_then(|cached| cached.result.as_ref().ok())
        .map(|status| status.key_custody_snp_trust.clone())
}

/// What an operator must fix for SEV-SNP key entry, from `status` and this
/// site's policy, each with the setting of monokulo's it is about:
/// monokulo requires it and the engine has no `snp` backend, or the two
/// trust different images (so `snp` isn't offered). Empty when neither
/// uses it, or they agree.
pub fn snp_policy_problems(
    policy: &crate::settings::SnpEntryPolicy,
    status: &EngineStatusResponse,
) -> Vec<(&'static str, String)> {
    snp_policy_problems_for(policy, status, engine_has_snp(status))
}

/// Whether the engine has its SEV-SNP backend enabled, by its `/status`.
pub fn engine_has_snp(status: &EngineStatusResponse) -> bool {
    status
        .key_custody
        .iter()
        .any(|b| super::key_entry::takes_keys_encrypted(&b.backend))
}

/// [`snp_policy_problems`] as if the engine's SEV-SNP backend were
/// `snp_enabled`: what a save turning it on or off would leave.
pub fn snp_policy_problems_for(
    policy: &crate::settings::SnpEntryPolicy,
    status: &EngineStatusResponse,
    snp_enabled: bool,
) -> Vec<(&'static str, String)> {
    use crate::settings::{KEY_CUSTODY_SNP_ENTRY_ID_KEY, KEY_CUSTODY_SNP_ENTRY_REQUIRED};
    if policy.required && !snp_enabled {
        return vec![(
            KEY_CUSTODY_SNP_ENTRY_REQUIRED.key,
            "key_custody.snp_entry_required is on, but the engine has no SEV-SNP backend enabled: no store can enter its keys".to_owned(),
        )];
    }
    if !snp_enabled {
        return Vec::new();
    }
    match (&policy.trust, &status.key_custody_snp_trust) {
        (None, _) => vec![(
            KEY_CUSTODY_SNP_ENTRY_ID_KEY.key,
            "this site has no engine ID key to trust: set key_custody.snp_entry_id_key".to_owned(),
        )],
        (Some(_), None) => vec![(
            KEY_CUSTODY_SNP_ENTRY_ID_KEY.key,
            "the engine reports no SEV-SNP trust settings".to_owned(),
        )],
        (Some(ours), Some(engine)) => super::key_entry::trust_differences(ours, engine),
    }
}

/// [`snp_policy_problems`] as one sentence for the status page; `None`
/// when there are none.
pub fn snp_policy_alert(
    policy: &crate::settings::SnpEntryPolicy,
    status: &EngineStatusResponse,
) -> Option<String> {
    let problems = snp_policy_problems(policy, status);
    if problems.is_empty() {
        return None;
    }
    let problems: Vec<String> = problems.into_iter().map(|(_, problem)| problem).collect();
    Some(format!(
        "SEV-SNP key storage can't be chosen: this site's key entry settings don't match the engine's. {}.",
        problems.join("; ")
    ))
}

/// [`snp_policy_alert`] from the cached status, for an operator's alert
/// bar; nothing while the status isn't known.
pub fn known_snp_policy_alert(state: &AppState) -> Option<String> {
    let cache = status_cache(&state.engine);
    let status = cache
        .cached
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < KNOWN_STATUS_MAX_AGE)
        .and_then(|cached| cached.result.as_ref().ok())?;
    snp_policy_alert(&state.settings.snp_entry.load(), status)
}

/// Whether a fresh, successful engine status is known.
pub fn known_status_is_fresh(engine: &Engine) -> bool {
    status_cache(engine).cached.as_ref().is_some_and(|cached| {
        cached.fetched_at.elapsed() < KNOWN_STATUS_MAX_AGE && cached.result.is_ok()
    })
}

/// Every key custody backend the engine has enabled, the default first.
/// Empty when the engine offers no choice or its status isn't known.
pub fn known_enabled_custody_backends(engine: &Engine) -> Vec<String> {
    let cache = status_cache(engine);
    let Some(status) = cache
        .cached
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < KNOWN_STATUS_MAX_AGE)
        .and_then(|cached| cached.result.as_ref().ok())
    else {
        return Vec::new();
    };
    let mut choices: Vec<String> = status
        .key_custody
        .iter()
        .map(|b| b.backend.clone())
        .collect();
    if let Some(default) = &status.key_custody_default {
        if let Some(at) = choices.iter().position(|b| b == default) {
            let default = choices.remove(at);
            choices.insert(0, default);
        }
    }
    choices
}

/// [`known_custody_choices`] as form options, `selected` (or the default)
/// selected.
pub fn custody_choice_views(
    state: &AppState,
    selected: Option<&str>,
) -> Vec<crate::views::connect::CustodyChoice> {
    let choices = known_custody_choices(state);
    let selected = selected
        .filter(|s| choices.iter().any(|c| c == s))
        .or(choices.first().map(String::as_str))
        .map(str::to_string);
    choices
        .iter()
        .map(|backend| crate::views::connect::CustodyChoice {
            backend: backend.clone(),
            label: custody_backend_label(backend),
            selected: selected.as_deref() == Some(backend.as_str()),
        })
        .collect()
}

/// What a key custody backend is, for a store owner choosing one.
pub fn custody_backend_label(backend: &str) -> String {
    match backend {
        "plain" => "In the engine (simplest)".to_string(),
        "snp" => "Sealed in the engine's AMD SEV-SNP confidential machine (sent there encrypted; this site never sees them)".to_string(),
        other => other.to_string(),
    }
}

/// Forgets the cached status, so the next page reads the engine's again:
/// after a change that the status reflects (a store moving its keys, the
/// engine address changing).
pub fn invalidate_status_cache(engine: &Engine) {
    status_cache(engine).cached = None;
}

/// Puts `status` in the cache as if just fetched, for tests of pages that
/// read it without waiting on an engine.
#[cfg(test)]
pub(crate) fn seed_status_for_tests(engine: &Engine, status: EngineStatusResponse) {
    status_cache(engine).cached = Some(CachedStatus {
        fetched_at: Instant::now(),
        result: Ok(status),
    });
}

/// The stores the engine last said it can't scan (task 3.7), from the same
/// cache as [`known_health`], without waiting on the engine. Empty when
/// nothing is known yet.
pub fn known_unserved(engine: &Engine) -> Vec<crate::engine_client::UnservedTenant> {
    let cache = status_cache(engine);
    cache
        .cached
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < KNOWN_STATUS_MAX_AGE)
        .and_then(|cached| cached.result.as_ref().ok())
        .map(|status| status.unserved_tenants.clone())
        .unwrap_or_default()
}

/// `healthy: false` covers both "the engine is unreachable" and "the engine
/// answered but reports a real problem" - the indicator only ever needs to
/// distinguish "everything's fine" from "go look".
fn is_healthy(result: &Result<EngineStatusResponse, String>) -> bool {
    let Ok(status) = result else { return false };
    // `Iterator::all` is vacuously true on an empty list - an engine
    // reporting zero configured networks is not "everything's fine",
    // it's nothing to be fine *about*, so that case is excluded
    // explicitly rather than trusted to fall out of `all` on its own.
    !status.networks.is_empty()
        && status.networks.iter().all(|n| {
            n.nodes.iter().any(|node| node.error.is_none())
                && !n.scanner.is_stale
                && n.scanner.last_tick_ok
        })
}

/// Returns the cached engine status if it's still fresh, otherwise fetches a
/// real one and caches it before returning. One fetch at a time: a request
/// that finds the cache stale while another is fetching waits for that
/// fetch (bounded by the engine call timeout) and returns its answer. Each
/// engine `/status` asks every Monero node, so a burst of visitors to the
/// public status page must not turn into a burst of node requests.
pub(crate) async fn get_status_cached(engine: &Engine) -> Result<EngineStatusResponse, String> {
    if let Some(fresh) = fresh_status(engine) {
        return fresh;
    }
    let _fetching = engine.status_cache.fetching.lock().await;
    // Filled while this one waited.
    if let Some(fresh) = fresh_status(engine) {
        return fresh;
    }
    let result = engine
        .client
        .get_status()
        .await
        .map_err(|e| describe_engine_error(&e));
    status_cache(engine).cached = Some(CachedStatus {
        fetched_at: Instant::now(),
        result: result.clone(),
    });
    result
}

/// The cached status, if it's still fresh.
fn fresh_status(engine: &Engine) -> Option<Result<EngineStatusResponse, String>> {
    status_cache(engine)
        .cached
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < CACHE_TTL)
        .map(|cached| cached.result.clone())
}

/// `GET /status` - the full page. Unauthenticated, so `logged_in` is a real
/// per-request check (see `StatusPageViewModel::logged_in`'s own doc
/// comment), not a fixed literal like most other pages.
pub async fn status_page(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let authed = super::resolve_authed_user(&state, &headers).await;
    let view_model = status_view(
        &state,
        authed.as_ref().is_some_and(|(user, _)| user.is_admin),
    )
    .await;
    let chrome = super::page_chrome(&state, authed.as_ref().map(|(user, _)| user), "/status").await;
    views::status::page(&chrome, &view_model).into_response()
}

/// `GET /status/events`: the status page's content, sent again whenever it
/// changes (checked as often as the cached engine status can change), as
/// ssexi JSON-routed events replacing `#status-live`. One open stream per
/// client, like the checkout's.
pub async fn status_events(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    extensions: axum::http::Extensions,
) -> Response {
    let permit = match extensions.get::<crate::abuse::ClientIdentity>() {
        Some(client) => match state.abuse.streams.try_acquire(client, "status") {
            Some(permit) => Some(permit),
            None => {
                return (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    "too many open update streams",
                )
                    .into_response()
            }
        },
        None => None,
    };
    let admin = super::resolve_authed_user(&state, &headers)
        .await
        .is_some_and(|(user, _)| user.is_admin);
    let stream = futures_util::stream::unfold(
        (state, None::<String>, true),
        move |(state, last, first)| {
            // Held by the stream, so the slot frees when it ends.
            let _permit = &permit;
            async move {
                if !first {
                    tokio::time::sleep(CACHE_TTL).await;
                }
                let html =
                    views::status::live_fragment(&status_view(&state, admin).await).into_string();
                if last.as_deref() == Some(html.as_str()) {
                    // Unchanged: a comment keeps the connection's clock honest.
                    return Some((
                        Ok(axum::response::sse::Event::default().comment("unchanged")),
                        (state, last, false),
                    ));
                }
                let event = axum::response::sse::Event::default()
                    .event(r##"{"target":"#status-live","swap":"outerHTML"}"##)
                    .data(html.clone());
                Some((
                    Ok::<_, std::convert::Infallible>(event),
                    (state, Some(html), false),
                ))
            }
        },
    );
    crate::live::sse(stream)
}

/// The status page's content; abuse figures only for an admin.
async fn status_view(state: &AppState, admin: bool) -> views::status::StatusPageViewModel {
    let mut view_model = match get_status_cached(&state.engine).await {
        Ok(status) => {
            let slow = slow_block_messages(&status, admin);
            // Anyone may know SEV-SNP key storage is off; which settings
            // differ is for operators.
            let snp_alert = snp_policy_alert(&state.settings.snp_entry.load(), &status).map(|detail| {
                if admin {
                    detail
                } else {
                    "SEV-SNP key storage isn't available: this site's settings for it don't match the engine's. Its operator has been alerted.".to_owned()
                }
            });
            views::status::StatusPageViewModel {
                slow_blocks: slow,
                snp_alert,
                ..build_view_model(status)
            }
        }
        Err(message) => views::status::StatusPageViewModel {
            abuse: None,
            engine_error: Some(message),
            networks: Vec::new(),
            poll_interval_secs: 0,
            generated_at_display: String::new(),
            slow_blocks: Vec::new(),
            snp_alert: None,
            engine_page: false,
        },
    };
    // Node addresses and the raw text of node and scanner errors (which
    // can name internal hosts) are for operators only: anyone else sees
    // each node's place in the list and whether it answers.
    if !admin {
        for network in &mut view_model.networks {
            for (i, node) in network.nodes.iter_mut().enumerate() {
                node.label = format!("node {}", i + 1);
                if node.error.is_some() {
                    node.error = Some("not answering".to_string());
                }
            }
            if network.scanner.last_error.is_some() {
                network.scanner.last_error = Some("the last scan failed".to_string());
            }
            // Publisher addresses and their errors, likewise; and what
            // proof-of-work checking found of each node.
            network.announcements = None;
            network.proof = None;
        }
    }
    view_model.engine_page = admin;
    // Challenge activity is for operators only; anonymous visitors and
    // merchants don't see it.
    if admin {
        let counts = state.abuse.stats.last_hour(crate::now_unix());
        view_model.abuse = Some(views::status::AbuseStatusView {
            under_attack: state.abuse.config().under_attack,
            issued: counts.issued,
            solved: counts.solved,
            refused: counts.refused,
        });
    }
    view_model
}

/// `GET /status/summary` - a small, cheap JSON endpoint the status
/// indicator polls to update its color/glow after the page has loaded (the
/// page itself is rendered with [`known_health`]), without pulling in the
/// full status page's own engine round trip. See [`is_healthy`].
pub async fn status_summary(State(engine): State<Engine>) -> Response {
    let health = health_of(&get_status_cached(&engine).await);
    Json(json!({
        "healthy": health != views::Health::Problem,
        "state": health.as_str(),
    }))
    .into_response()
}

fn describe_engine_error(err: &EngineClientError) -> String {
    match err {
        EngineClientError::Request(_)
        | EngineClientError::Middleware(_)
        | EngineClientError::Embedded(_) => "the engine could not be reached".to_string(),
        EngineClientError::EngineError { status, .. } => {
            format!("the engine responded with an error ({status})")
        }
        EngineClientError::Unreadable(_) => "the engine's reply could not be read".to_string(),
        EngineClientError::NotAdminRoute(_) => {
            "monokulo refused to make the request (a bug)".to_string()
        }
    }
}

fn build_view_model(status: EngineStatusResponse) -> views::status::StatusPageViewModel {
    let now = crate::now_unix();
    let networks = status
        .networks
        .into_iter()
        .map(|n| build_network_view(n, now))
        .collect();
    views::status::StatusPageViewModel {
        abuse: None,
        engine_error: None,
        networks,
        poll_interval_secs: status.poll_interval_secs,
        generated_at_display: relative_time(now, status.generated_at),
        slow_blocks: Vec::new(),
        snp_alert: None,
        engine_page: false,
    }
}

fn build_network_view(network: NetworkStatus, now: i64) -> views::status::StatusNetworkView {
    let headers_first = network
        .scaling
        .as_ref()
        .and_then(|scaling| scaling.scan.headers_first.as_ref())
        .map(views::scaling::headers_first_sentence);
    let nodes = network
        .nodes
        .into_iter()
        .map(|node| views::status::StatusNodeView {
            label: node.label,
            is_active: node.is_active,
            is_reachable: node.error.is_none(),
            height_display: node
                .height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "-".to_string()),
            error: node.error,
        })
        .collect();

    let scanner = network.scanner;
    let (status_label, status_tag_class) = if !scanner.ever_ticked {
        ("has not been scanned yet", "tag-unknown")
    } else if scanner.is_stale {
        ("stale", "tag-error")
    } else if !scanner.last_tick_ok {
        ("tick failing", "tag-error")
    } else {
        ("healthy", "tag-ok")
    };

    views::status::StatusNetworkView {
        network: network.network,
        nodes,
        scanner: views::status::StatusScannerView {
            ever_ticked: scanner.ever_ticked,
            status_label: status_label.to_string(),
            status_tag_class: status_tag_class.to_string(),
            last_tick_display: scanner
                .last_tick_finished_at
                .map(|t| relative_time(now, t))
                .unwrap_or_else(|| "never".to_string()),
            tick_count: scanner.tick_count,
            tenants_scanned: scanner.tenants_scanned,
            last_error: scanner.last_error,
        },
        headers_first,
        announcements: network
            .announcements
            .map(|announcements| announcements_view(announcements, now)),
        proof: network.proof.map(|proof| proof_view(proof, now)),
    }
}

/// A network's proof-of-work checking as shown to an admin
/// (docs/proof_of_work.md).
fn proof_view(proof: shared::proof::ProofStatus, now: i64) -> views::status::ProofView {
    use shared::proof::{NodeVerdict, ProofState};
    let (tag_label, tag_class) = match proof.state {
        ProofState::Following => ("checking", "tag-ok"),
        ProofState::Anchoring => ("anchoring", "tag-unknown"),
        ProofState::Held => ("settlement held", "tag-error"),
    };
    let mut facts = Vec::new();
    if let Some(anchor) = &proof.anchor {
        facts.push(format!(
            "Anchored at block {} ({} of {} node{} agreed), {}.",
            anchor.height,
            anchor.agreed,
            anchor.nodes,
            if anchor.nodes == 1 { "" } else { "s" },
            relative_time(now, anchor.anchored_at)
        ));
    }
    if let Some(hashing) = &proof.hashing {
        facts.push(format!(
            "{} blocks checked since the engine started, {:.0} ms each ({}); {} RandomX key{} held, 256 MiB each.",
            proof.blocks_checked,
            hashing.mean_hash_ms,
            if hashing.jit {
                "compiled"
            } else {
                "interpreted: RandomX's compiler can't run here"
            },
            hashing.keys_held,
            if hashing.keys_held == 1 { "" } else { "s" },
        ));
    }
    if let Some(at) = proof.checked_at {
        facts.push(format!("Last checked {}.", relative_time(now, at)));
    }
    let nodes = proof
        .nodes
        .into_iter()
        .map(|node| views::status::ProofNodeView {
            verdict: match node.verdict {
                NodeVerdict::Unknown => "not looked at yet",
                NodeVerdict::OnChain => "on the proven chain",
                NodeVerdict::Ahead => "ahead, being checked",
                NodeVerdict::Lighter => "on a chain with less work",
                NodeVerdict::Diverged => "left the proven chain too far back",
                NodeVerdict::Caught => "served a block that breaks the rules",
                NodeVerdict::Unreachable => "not answering",
            }
            .to_string(),
            height: node
                .height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "-".to_string()),
            node: node.node,
            detail: node.detail,
            excluded: node.excluded,
        })
        .collect();
    views::status::ProofView {
        tag_label: tag_label.to_string(),
        tag_class: tag_class.to_string(),
        summary: proof.summary,
        facts,
        nodes,
        can_take_new_anchor: proof.state == ProofState::Held,
    }
}

/// `POST /dashboard/admin/proof/{network}/reanchor` - the "Take a new
/// anchor" button: the engine forgets the network's anchor and proven
/// chain and takes a new anchor from its nodes next round
/// (docs/proof_of_work.md). For after a reorg deeper than the anchor.
pub async fn take_new_anchor(
    State(state): State<AppState>,
    _admin: super::AuthedAdmin,
    axum::extract::Path(network): axum::extract::Path<String>,
) -> Response {
    let Ok(network) = shared::network::parse_network(&network) else {
        return (axum::http::StatusCode::BAD_REQUEST, "No such network.").into_response();
    };
    match state.engine.client.take_new_anchor(network).await {
        Ok(response) if response.status().is_success() => {
            tracing::warn!(network = ?network, "an operator asked for a new proof-of-work anchor");
            invalidate_status_cache(&state.engine);
            axum::response::Redirect::to("/status").into_response()
        }
        Ok(response) => (
            axum::http::StatusCode::BAD_GATEWAY,
            format!(
                "The engine refused ({}): proof-of-work checking may be off on {}.",
                response.status(),
                shared::network::network_str(network)
            ),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_GATEWAY,
            format!("Could not reach the configured engine: {e}"),
        )
            .into_response(),
    }
}

/// A network's announcements as shown to an admin (docs/monero_zmq.md).
fn announcements_view(
    announcements: shared::announcements::Announcements,
    now: i64,
) -> views::status::AnnouncementsView {
    let publishers = announcements
        .publishers
        .into_iter()
        .map(|p| views::status::PublisherView {
            state_display: match (p.connected, p.connected_since, p.last_error_at) {
                (true, Some(since), _) => format!("since {}", relative_time(now, since)),
                (false, _, Some(lost)) => format!("since {}", relative_time(now, lost)),
                (false, _, None) if p.connections == 0 => "never yet".to_string(),
                _ => String::new(),
            },
            last_announcement_display: p
                .last_announcement_at
                .map(|at| relative_time(now, at))
                .unwrap_or_else(|| "none yet".to_string()),
            last_error: p.last_error.map(|error| match p.last_error_at {
                Some(at) => format!("Last failure {}: {error}", relative_time(now, at)),
                None => format!("Last failure: {error}"),
            }),
            node: p.node,
            endpoint: p.endpoint,
            connected: p.connected,
            connections: p.connections,
            pool_announcements: p.pool_announcements,
            block_announcements: p.block_announcements,
        })
        .collect();
    views::status::AnnouncementsView {
        publishers,
        pool_passes_woken: announcements.pool_passes_woken,
        rounds_woken: announcements.rounds_woken,
    }
}

/// A plain, four-bucket "Ns/Nm/Nh/Nd ago" formatter - the same
/// presentation-belongs-to-the-renderer split `http/status_page.rs`'s
/// module doc comment describes, now actually living in the renderer.
/// Buckets are half-open on their upper bound (`< 60`, `< 3600`, `< 86400`)
/// so a value exactly on a boundary (e.g. precisely 3600s) falls into the
/// *next* bucket up, never double-counted or skipped.
fn relative_time(now: i64, at: i64) -> String {
    let diff = (now - at).max(0);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One healthy network, its block scan reporting `slow`.
    fn status_with_slow(slow: Option<shared::scaling::SlowBlock>) -> EngineStatusResponse {
        use shared::scaling::{NetworkScaling, Pace, ScanReport, Trend};
        EngineStatusResponse {
            networks: vec![crate::engine_client::NetworkStatus {
                network: "mainnet".into(),
                nodes: vec![crate::engine_client::NodeStatus {
                    label: "node.example:18089".into(),
                    is_active: true,
                    in_cooldown: false,
                    height: Some(3_412_015),
                    error: None,
                    network: None,
                    link: None,
                }],
                scanner: crate::engine_client::ScannerStatusView {
                    ever_ticked: true,
                    last_tick_started_at: Some(0),
                    last_tick_finished_at: Some(0),
                    tick_count: 1,
                    tenants_scanned: 1,
                    last_tick_ok: true,
                    last_error: None,
                    is_stale: false,
                },
                scaling: Some(NetworkScaling {
                    scan: ScanReport {
                        avg_block_bytes: 0,
                        block_size_trend: Trend::Steady,
                        last_chunk: None,
                        blocks_per_minute: 0.0,
                        fetch_secs_recent: 0.0,
                        scan_secs_recent: 0.0,
                        largest_recent: None,
                        in_progress: None,
                        in_progress_secs: None,
                        peak_cache_bytes: None,
                        discarded_cache_bytes_recent: 0,
                        round_budget_secs: None,
                        headers_first: None,
                    },
                    blocks_behind: 14,
                    catch_up_secs: None,
                    pace: Pace::Link,
                    budget_mb: 256,
                    max_budget_mb: Some(1536),
                    round_deadline_secs: 10,
                    round_base_secs: 10,
                    slow,
                }),
                announcements: None,
                proof: None,
            }],
            poll_interval_secs: 2,
            generated_at: 0,
            unserved_tenants: vec![],
            key_custody: vec![],
            key_custody_default: None,
            key_custody_snp_trust: None,
            resources: None,
        }
    }

    fn slow_block() -> shared::scaling::SlowBlock {
        shared::scaling::SlowBlock {
            height: 3_412_001,
            wire_bytes: Some(412_000_000),
            elapsed_secs: 130,
            node: Some("node.example:18089".into()),
            rate_bytes_per_sec: Some(387_500),
            remaining_secs: Some(1_080),
        }
    }

    /// A block over two minutes turns the indicator yellow, not red: the
    /// engine works, it is just slow (docs/engine_scaling.md section 5).
    /// While the engine reads block headers first, the status page says so,
    /// with why and for how long (docs/engine_scaling.md section 4).
    #[test]
    fn the_status_page_says_when_block_headers_are_read_first() {
        let mut status = status_with_slow(None);
        let network = status.networks.remove(0);
        assert_eq!(build_network_view(network.clone(), 0).headers_first, None);
        let mut network = network;
        if let Some(scaling) = network.scaling.as_mut() {
            scaling.scan.headers_first = Some(shared::scaling::HeadersFirst {
                remaining_secs: 3_000,
                reason: shared::scaling::HeadersFirstReason::LargeBlock,
            });
        }
        assert_eq!(
            build_network_view(network, 0).headers_first.as_deref(),
            Some("Reading block headers first for about 50 minutes more: a recent block came near the size that is scanned in pages, so each block's size is checked before it is fetched.")
        );
    }

    #[test]
    fn a_slow_block_is_yellow_and_says_why_and_what_would_help() {
        assert_eq!(health_of(&Ok(status_with_slow(None))), views::Health::Ok);
        let status = status_with_slow(Some(slow_block()));
        assert_eq!(health_of(&Ok(status.clone())), views::Health::Slow);
        assert_eq!(
            slow_block_messages(&status, true),
            vec![
                "Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far, at 3.1 Mbit/s \
                  from node.example:18089. At this rate it needs about 18 minutes more. A \
                  faster node or a larger scan memory budget would help."
                    .to_string()
            ]
        );
        let public = slow_block_messages(&status, false);
        assert!(!public[0].contains("node.example"), "{public:?}");
        assert!(public[0].contains("at 3.1 Mbit/s."), "{public:?}");
        // A failing engine is still red, slow or not.
        assert_eq!(health_of(&Err("down".into())), views::Health::Problem);
    }

    #[test]
    fn relative_time_picks_the_right_bucket() {
        assert_eq!(relative_time(100, 100), "0s ago");
        assert_eq!(relative_time(159, 100), "59s ago");
        assert_eq!(relative_time(160, 100), "1m ago");
        assert_eq!(relative_time(3700, 100), "1h ago");
        assert_eq!(relative_time(3600, 0), "1h ago");
        assert_eq!(relative_time(100_000, 0), "1d ago");
        assert_eq!(relative_time(90_000, 0), "1d ago");
        assert_eq!(relative_time(20_000, 0), "5h ago");
    }

    #[test]
    fn relative_time_never_goes_negative_for_a_clock_that_moved_backward() {
        assert_eq!(relative_time(100, 200), "0s ago");
    }

    #[tokio::test]
    async fn known_health_renders_the_last_known_state_and_refreshes_it_in_the_background() {
        let state = crate::http::AppState::for_tests();
        // Nothing learned yet: unknown, and a background refresh starts.
        assert_eq!(known_health(&state.engine), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while status_cache(&state.engine).refreshing {
            assert!(
                Instant::now() < deadline,
                "the background refresh never finished"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The test engine is unreachable, which is a known problem.
        assert_eq!(known_health(&state.engine), Some(views::Health::Problem));

        // Too old to show as known. (On a host up for less than that, an
        // `Instant` that old can't exist; there is nothing to check.)
        let Some(too_old) = Instant::now().checked_sub(KNOWN_STATUS_MAX_AGE) else {
            return;
        };
        status_cache(&state.engine).cached = Some(CachedStatus {
            fetched_at: too_old,
            result: Err("stale".to_string()),
        });
        status_cache(&state.engine).refreshing = true;
        assert_eq!(known_health(&state.engine), None);
    }

    mod http_tests {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::Router;
        use tower::ServiceExt;

        use crate::engine_client::EngineClient;
        use crate::http::{build_router, AppState};

        use super::super::get_status_cached;

        /// The real fix this cache exists for (see the module's own doc
        /// comment on the incident): a second request arriving within the
        /// TTL must reuse the first's real fetch rather than making its own
        /// - proven by reading the cache's own `fetched_at` back rather than
        /// just checking both calls "look the same" (which a coincidental
        /// same-second real refetch could also produce).
        #[tokio::test]
        async fn get_status_cached_reuses_a_fresh_fetch_instead_of_refetching() {
            let engine =
                engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet])
                    .await;
            let state = state_with_engine(EngineClient::embedded_for_tests(engine.router()));

            let first = get_status_cached(&state.engine)
                .await
                .expect("first fetch should succeed");
            let fetched_at_after_first = super::super::status_cache(&state.engine)
                .cached
                .as_ref()
                .unwrap()
                .fetched_at;

            let second = get_status_cached(&state.engine)
                .await
                .expect("second fetch should succeed");
            let fetched_at_after_second = super::super::status_cache(&state.engine)
                .cached
                .as_ref()
                .unwrap()
                .fetched_at;

            assert_eq!(
                fetched_at_after_first, fetched_at_after_second,
                "a second call within the TTL must reuse the cached fetch, not trigger a new one"
            );
            assert_eq!(
                first.generated_at, second.generated_at,
                "a reused cache entry must hand back the exact same response"
            );
        }

        /// Many requests finding the cache empty at once - a burst of
        /// visitors to the public status page - make one engine request
        /// between them, and all get its answer.
        #[tokio::test]
        async fn requests_that_arrive_during_a_fetch_share_it() {
            use std::sync::atomic::{AtomicUsize, Ordering};

            // A real engine's status body, served by a slow fake that
            // counts how often it's asked.
            let real =
                engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet])
                    .await;
            let body: String = engine_test_support::engine_http_client()
                .get(format!("http://{}/status", real.addr))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let hits = std::sync::Arc::new(AtomicUsize::new(0));
            let (release_tx, release_rx) = tokio::sync::watch::channel(false);
            let app = {
                let hits = hits.clone();
                Router::new().route(
                    "/status",
                    axum::routing::get(move || {
                        let (hits, body, mut release) =
                            (hits.clone(), body.clone(), release_rx.clone());
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            // Answers only once every caller is waiting.
                            let _ = release.wait_for(|released| *released).await;
                            ([("content-type", "application/json")], body)
                        }
                    }),
                )
            };
            let state = state_with_engine(EngineClient::embedded_for_tests(app));
            let callers: Vec<_> = (0..20)
                .map(|_| {
                    let engine = state.engine.clone();
                    tokio::spawn(async move { get_status_cached(&engine).await })
                })
                .collect();
            // The first fetch has reached the engine; the rest are queued
            // behind it (or still on their way). Nothing is timed: the
            // engine doesn't answer until released.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while hits.load(Ordering::SeqCst) == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "no fetch reached the engine"
                );
                tokio::task::yield_now().await;
            }
            release_tx.send(true).unwrap();
            let answers: Vec<_> = futures_util::future::join_all(callers)
                .await
                .into_iter()
                .map(|joined| joined.unwrap().expect("every caller gets the status"))
                .collect();
            assert_eq!(
                hits.load(Ordering::SeqCst),
                1,
                "one engine request for all 20"
            );
            assert!(answers
                .iter()
                .all(|answer| answer.generated_at == answers[0].generated_at));
        }

        fn state_with_engine(engine_client: EngineClient) -> AppState {
            AppState {
                engine: crate::http::Engine::new(engine_client),
                ..AppState::for_tests()
            }
        }

        use crate::http::test_support::body_text;

        use crate::http::test_support::body_json;

        /// Real engine, no daemons configured (`engine_test_support`'s harness
        /// never populates them - see `get_status_round_trips_against_a_real_engine`'s
        /// own doc comment) - proves the page renders the honest "no nodes
        /// configured" state end to end, not a fabricated one.
        #[tokio::test]
        async fn status_page_is_reachable_with_no_authentication_and_shows_no_configured_networks()
        {
            let engine = engine_test_support::spawn_test_engine().await;
            let state = state_with_engine(EngineClient::embedded_for_tests(engine.router()));
            let router: Router = build_router(state);

            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/status")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;
            assert!(
                html.contains("Engine status"),
                "expected the real status page, got: {html}"
            );
            assert!(
                html.contains("No Monero nodes are configured"),
                "expected the honest empty state, got: {html}"
            );
        }

        #[tokio::test]
        async fn status_summary_reports_unhealthy_when_there_are_no_configured_networks() {
            let engine =
                engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet])
                    .await;
            let state = state_with_engine(EngineClient::embedded_for_tests(engine.router()));
            let router: Router = build_router(state);

            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/status/summary")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            // An empty `networks` list vacuously satisfies `Iterator::all`, so
            // this must be pinned down explicitly rather than assumed - an
            // engine reporting no networks at all is not "everything's fine".
            assert_eq!(
                body["healthy"], false,
                "an engine with zero configured networks must not read as healthy, got: {body}"
            );
        }

        /// The real degradation path: the engine is entirely unreachable (no
        /// listener at all at this port) - the page must show a plain error
        /// banner, not a 500 or a fabricated healthy page.
        #[tokio::test]
        async fn status_page_shows_a_plain_error_banner_when_a_remote_engine_is_unreachable_over_http(
        ) {
            let state = state_with_engine(EngineClient::for_tests("http://127.0.0.1:1"));
            let router: Router = build_router(state);

            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/status")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "an unreachable engine is not this page's own server error"
            );
            let html = body_text(response).await;
            assert!(
                html.contains("could not be reached"),
                "expected a plain error banner, got: {html}"
            );
        }

        /// The page as it normally looks: a configured network whose node
        /// answers. Before the first scan tick it says so rather than
        /// claiming health.
        #[tokio::test]
        async fn status_page_lists_a_configured_networks_node_and_a_scanner_not_yet_run() {
            let engine = engine_test_support::TestEngineConfig::new()
                .with_networks(&[monero::Network::Mainnet])
                .with_admin_lookup_daemon()
                .spawn()
                .await;
            let router: Router = build_router(state_with_engine(EngineClient::embedded_for_tests(
                engine.router(),
            )));
            let html = body_text(
                router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/status")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap(),
            )
            .await;
            assert!(html.contains("mainnet"), "got: {html}");
            // An anonymous visitor sees the node's place in the list, not
            // its address (`status_view`).
            assert!(html.contains("node 1"), "the node is listed: {html}");
            assert!(
                !html.contains("lookup-test-daemon"),
                "its label is for operators only: {html}"
            );
            assert!(html.contains("has not been scanned yet"), "got: {html}");
            let summary = body_json(
                router
                    .oneshot(
                        Request::builder()
                            .uri("/status/summary")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(
                summary["healthy"], false,
                "not healthy before the first scan: {summary}"
            );
        }

        /// What an operator sees for each scanner state the engine reports:
        /// healthy, stale (ticks stopped), failing (with the reason), or never
        /// run; and an unreachable node beside the active one.
        #[test]
        fn status_page_describes_each_scanner_state_and_an_unreachable_node() {
            use crate::engine_client::{
                EngineStatusResponse, NetworkStatus, NodeStatus, ScannerStatusView,
            };
            let now = crate::now_unix();
            let scanner =
                |ever_ticked, last_tick_ok, is_stale, last_error: Option<&str>| ScannerStatusView {
                    ever_ticked,
                    last_tick_started_at: Some(now - 90),
                    last_tick_finished_at: ever_ticked.then_some(now - 90),
                    tick_count: 7,
                    tenants_scanned: 3,
                    last_tick_ok,
                    last_error: last_error.map(str::to_string),
                    is_stale,
                };
            let network = |name: &str, scanner| NetworkStatus {
                network: name.to_string(),
                nodes: vec![
                    NodeStatus {
                        label: "node-a:18081".into(),
                        is_active: true,
                        in_cooldown: false,
                        height: Some(3_700_000),
                        error: None,
                        network: None,
                        link: None,
                    },
                    NodeStatus {
                        label: "node-b:18081".into(),
                        is_active: false,
                        in_cooldown: false,
                        height: None,
                        error: Some("connection refused".into()),
                        network: None,
                        link: None,
                    },
                ],
                scanner,
                scaling: None,
                announcements: None,
                proof: None,
            };
            let view = super::super::build_view_model(EngineStatusResponse {
                networks: vec![
                    network("mainnet", scanner(true, true, false, None)),
                    network("stagenet", scanner(true, true, true, None)),
                    network(
                        "testnet",
                        scanner(true, false, false, Some("daemon request failed: timed out")),
                    ),
                ],
                poll_interval_secs: 2,
                generated_at: now - 5,
                unserved_tenants: vec![],
                key_custody: vec![],
                key_custody_default: None,
                key_custody_snp_trust: None,
                resources: None,
            });
            let labels: Vec<(&str, &str)> = view
                .networks
                .iter()
                .map(|n| {
                    (
                        n.scanner.status_label.as_str(),
                        n.scanner.status_tag_class.as_str(),
                    )
                })
                .collect();
            assert_eq!(
                labels,
                vec![
                    ("healthy", "tag-ok"),
                    ("stale", "tag-error"),
                    ("tick failing", "tag-error")
                ]
            );
            assert_eq!(
                view.networks[2].scanner.last_error.as_deref(),
                Some("daemon request failed: timed out")
            );
            assert_eq!(view.networks[0].scanner.last_tick_display, "1m ago");
            let node_b = &view.networks[0].nodes[1];
            assert!(!node_b.is_reachable);
            assert_eq!(node_b.height_display, "-");
            let html = crate::views::status::page(
                &crate::views::PageChrome::from_user(None, "/status"),
                &view,
            )
            .into_string();
            assert!(
                html.contains("connection refused")
                    && html.contains("3700000")
                    && html.contains("daemon request failed: timed out"),
                "got: {html}"
            );
        }

        /// A node's ZMQ announcements (docs/monero_zmq.md) are shown to an
        /// admin, with each publisher's state, and to nobody else: the
        /// publisher addresses and errors can name internal hosts.
        #[tokio::test]
        async fn announcements_are_shown_to_admins_only() {
            let now = crate::now_unix();
            let mut status = super::status_with_slow(None);
            status.networks[0].announcements = Some(shared::announcements::Announcements {
                publishers: vec![
                    shared::announcements::Publisher {
                        node: "node.example:18089".into(),
                        endpoint: "tcp://10.0.0.5:18083".into(),
                        connected: true,
                        connected_since: Some(now - 600),
                        connections: 2,
                        pool_announcements: 41,
                        block_announcements: 3,
                        last_announcement_at: Some(now - 150),
                        last_error: Some("the node closed the connection".into()),
                        last_error_at: Some(now - 700),
                    },
                    shared::announcements::Publisher {
                        node: "backup.example:18089".into(),
                        endpoint: "tcp://10.0.0.6:18083".into(),
                        ..Default::default()
                    },
                ],
                pool_passes_woken: 40,
                rounds_woken: 3,
            });
            let state = state_with_engine(EngineClient::for_tests("http://127.0.0.1:1"));
            super::super::seed_status_for_tests(&state.engine, status);

            let anonymous = super::super::status_view(&state, false).await;
            assert!(anonymous.networks[0].announcements.is_none());

            let admin = super::super::status_view(&state, true).await;
            let shown = admin.networks[0].announcements.as_ref().unwrap();
            assert_eq!(shown.publishers[0].state_display, "since 10m ago");
            assert_eq!(shown.publishers[0].last_announcement_display, "2m ago");
            assert_eq!(
                shown.publishers[0].last_error.as_deref(),
                Some("Last failure 11m ago: the node closed the connection")
            );
            assert_eq!(shown.publishers[1].state_display, "never yet");
            assert_eq!(shown.publishers[1].last_announcement_display, "none yet");

            let html = crate::views::status::live_fragment(&admin).into_string();
            for expected in [
                "Announcements (ZMQ)",
                "<code>tcp://10.0.0.5:18083</code>",
                r#"<span class="tag tag-ok">connected</span>"#,
                r#"<span class="tag tag-error">not connected</span>"#,
                "<td>41</td>",
                "Started early: 40 mempool checks, 3 scan rounds.",
                "Dandelion++",
            ] {
                assert!(html.contains(expected), "{expected} in {html}");
            }
            let html = crate::views::status::live_fragment(&anonymous).into_string();
            assert!(
                !html.contains("10.0.0.5") && !html.contains("Announcements"),
                "{html}"
            );
        }

        /// Proof-of-work checking (docs/proof_of_work.md) is shown to an
        /// admin, each node with what it was found to serve, and to nobody
        /// else; a held network offers a new anchor, a following one
        /// doesn't.
        #[tokio::test]
        async fn proof_of_work_checking_is_shown_to_admins_only() {
            use shared::proof::{
                AnchorStatus, Hashing, NodeProof, NodeVerdict, ProofState, ProofStatus,
            };
            let now = crate::now_unix();
            let mut status = super::status_with_slow(None);
            status.networks[0].proof = Some(ProofStatus {
                state: ProofState::Following,
                summary: "Proven up to block 120; orders settle on blocks up to 118.".into(),
                anchor: Some(AnchorStatus {
                    height: 100,
                    hash: "ab".repeat(32),
                    agreed: 2,
                    nodes: 3,
                    anchored_at: now - 7200,
                }),
                proven_height: Some(120),
                proven_hash: Some("cd".repeat(32)),
                ceiling: Some(118),
                nodes: vec![
                    NodeProof {
                        node: "10.0.0.5:18081".into(),
                        height: Some(125),
                        verdict: NodeVerdict::Caught,
                        detail: Some(
                            "block 121's proof of work doesn't meet its difficulty 7".into(),
                        ),
                        excluded: true,
                    },
                    NodeProof {
                        node: "node.example:18089".into(),
                        height: Some(120),
                        verdict: NodeVerdict::OnChain,
                        detail: None,
                        excluded: false,
                    },
                ],
                blocks_checked: 20,
                hashing: Some(Hashing {
                    jit: true,
                    mean_hash_ms: 16.2,
                    mean_key_build_ms: 250.0,
                    keys_held: 1,
                }),
                checked_at: Some(now - 5),
            });
            let state = state_with_engine(EngineClient::for_tests("http://127.0.0.1:1"));
            super::super::seed_status_for_tests(&state.engine, status.clone());

            let anonymous = super::super::status_view(&state, false).await;
            assert!(anonymous.networks[0].proof.is_none());
            let html = crate::views::status::live_fragment(&anonymous).into_string();
            assert!(
                !html.contains("10.0.0.5") && !html.contains("Proof of work"),
                "{html}"
            );

            let admin = super::super::status_view(&state, true).await;
            let html = crate::views::status::live_fragment(&admin).into_string();
            for expected in [
                "Proof of work",
                r#"<span class="tag tag-ok">checking</span>"#,
                "Proven up to block 120; orders settle on blocks up to 118.",
                "Anchored at block 100 (2 of 3 nodes agreed), 2h ago.",
                "20 blocks checked since the engine started, 16 ms each (compiled); 1 RandomX key held, 256 MiB each.",
                "<code>10.0.0.5:18081</code>",
                "served a block that breaks the rules",
                r#"<span class="tag tag-error">left out</span>"#,
                "on the proven chain",
            ] {
                assert!(html.contains(expected), "{expected} in {html}");
            }
            assert!(!html.contains("Take a new anchor"), "only while held");

            let mut held = status;
            held.networks[0].proof.as_mut().unwrap().state = ProofState::Held;
            let state = state_with_engine(EngineClient::for_tests("http://127.0.0.1:1"));
            super::super::seed_status_for_tests(&state.engine, held);
            let admin = super::super::status_view(&state, true).await;
            let html = crate::views::status::live_fragment(&admin).into_string();
            assert!(html.contains(r#"<span class="tag tag-error">settlement held</span>"#));
            assert!(
                html.contains(
                    r#"<a class="hint" href="/status/engine?network=mainnet">Watch it live</a>"#
                ),
                "an admin is linked to the engine page: {html}"
            );
            let anonymous = super::super::status_view(&state, false).await;
            assert!(!crate::views::status::live_fragment(&anonymous)
                .into_string()
                .contains("/status/engine"));
            assert!(
                html.contains(
                    r#"<form method="post" action="/dashboard/admin/proof/mainnet/reanchor">"#
                ),
                "{html}"
            );
        }

        #[tokio::test]
        async fn status_summary_reports_unhealthy_when_a_remote_engine_is_unreachable_over_http() {
            let state = state_with_engine(EngineClient::for_tests("http://127.0.0.1:1"));
            let router: Router = build_router(state);

            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/status/summary")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            assert_eq!(body["healthy"], false);
        }
    }

    /// This site's policy is checked against the engine as the same save
    /// would leave it: turning the engine's snp backend on in that save
    /// counts.
    #[test]
    fn the_snp_policy_is_checked_against_the_backends_the_save_leaves() {
        let status = EngineStatusResponse {
            key_custody: Vec::new(),
            key_custody_snp_trust: None,
            ..status_with_slow(None)
        };
        let id_key = "ab".repeat(48);
        let required =
            crate::settings::SnpEntryPolicy::from_values(Some(&id_key), 0, None, true).unwrap();
        // Required, and the engine has no snp backend (now, or after).
        assert_eq!(
            snp_policy_problems_for(&required, &status, false)[0].0,
            crate::settings::KEY_CUSTODY_SNP_ENTRY_REQUIRED.key
        );
        assert!(!engine_has_snp(&status));
        // With the save turning it on, it's about the trust settings instead.
        let problems = snp_policy_problems_for(&required, &status, true);
        assert!(
            problems
                .iter()
                .all(|(key, _)| *key != crate::settings::KEY_CUSTODY_SNP_ENTRY_REQUIRED.key),
            "{problems:?}"
        );
    }
}
