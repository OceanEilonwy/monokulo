//! The generic, platform-agnostic connect flow (WBS 1.4.1,
//! `docs/WOOCOMMERCE_ROADMAP.md` Stage 6) - "OAuth-style one-click install"
//! for any platform's plugin, written once and shared by every platform
//! (WooCommerce first, per the WBS; nothing here is WooCommerce-specific -
//! `platform` is just a path parameter threaded straight through into
//! `store_connections.platform`, exactly like `connections::create_connection_for_user`
//! already treats it for the JSON/dashboard-form surfaces).
//!
//! The five steps (see the roadmap doc for the full narrative):
//! 1. `GET /connect/{platform}?site_url=...&return_url=...&nonce=...` - the
//!    plugin sends the merchant's browser here.
//! 2. No session yet: redirect to `/dashboard/login?next=<this same URL>` -
//!    `next` is validated as a safe, same-origin relative path before ever
//!    being used as a redirect target (`dashboard::is_safe_redirect_path`);
//!    a real session logging in there redirects back here automatically
//!    (`dashboard::login_submit`).
//! 3. A valid session: render a confirm screen with the same
//!    wallet-connection fields `/dashboard/connect` has, as a
//!    `POST /connect/{platform}` form (same path, not a `/confirm`
//!    sub-path) - `return_url`/`nonce` ride along as hidden fields.
//! 4. `POST /connect/{platform}` (behind [`AuthedUser`]): calls the exact
//!    same [`connections::create_connection_for_user`] every other surface
//!    uses, mints a single-use connect token, and redirects to `return_url`
//!    with `token`/`nonce` appended as query parameters (via the `url`
//!    crate, so an existing query string on `return_url` is preserved
//!    correctly rather than string-concatenated).
//! 5. `POST /connect/{platform}/finish` - deliberately *not* behind
//!    [`AuthedUser`]: the plugin calls this server-to-server, with no
//!    monokulo session at all. Redeems the token exactly once (see
//!    [`crate::db::Db::consume_connect_token`]'s atomicity doc comment) and
//!    returns `{public_key, secret_token, endpoint}` (`endpoint` is this
//!    instance's public address, never the engine's) plus, as of WBS 1.4.4,
//!    a `webhook_signing_secret` when the request carried a `webhook_url` -
//!    see [`finish`]'s own doc comment for the registration/failure policy.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::db::UserRow;
use crate::now_unix;
use crate::templates::network_selected_flags;
use crate::views::connect::{ExistingStoreOption, PlatformConnectViewModel};

use super::connections::{self, CreateConnectionError, CreateConnectionFields};
use super::dashboard::redirect_302;
use super::orders::display_name_for;
use super::{AppState, AuthedUser};

/// How long a connect token remains redeemable after issuance. It only
/// needs to survive the redirect round trip out to the plugin's
/// `return_url` and the plugin's own immediate server-to-server `/finish`
/// call right after - a few minutes comfortably covers real network
/// latency/retries without leaving a redeemable (even if single-use) token
/// valid for long if it somehow ends up somewhere it shouldn't (a proxy
/// log, browser history) before being redeemed.
const CONNECT_TOKEN_TTL_SECONDS: i64 = 10 * 60;

#[derive(Deserialize)]
pub struct ConnectQuery {
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
}

/// The plugin's connect request, carried through every render of the confirm
/// form, whether it came from the first `GET` or a resubmitted `POST`.
#[derive(Clone, Copy)]
struct ConnectRequest<'a> {
    site_url: &'a str,
    return_url: &'a str,
    nonce: &'a str,
}

/// Where a connecting plugin's store credentials may be sent: its
/// `return_url`, checked against the `site_url` the merchant is shown. Made
/// only by [`ConnectTarget::parse`], so the redirect carrying the connect
/// token can only go back to the site being connected - never to a
/// `return_url` a crafted link pointed somewhere else.
struct ConnectTarget {
    return_to: Url,
}

impl ConnectTarget {
    /// The return address, where the credentials travel, must be https
    /// (plain http only for a loopback or `.onion` host) and carry no
    /// credentials of its own; it must name the same host as the shop's
    /// address, which may itself be plain http (WordPress can serve its admin
    /// pages over https while the shop is on http).
    fn parse(site_url: &str, return_url: &str) -> Result<Self, &'static str> {
        let site = Url::parse(site_url.trim())
            .ok()
            .filter(|site| matches!(site.scheme(), "http" | "https"))
            .ok_or("The shop's address isn't a valid web address.")?;
        let return_to = web_url(return_url)
            .ok_or("The plugin's return address isn't a valid https address.")?;
        // Same host, and the same port when both use one scheme (the admin
        // may be on https while the shop is on plain http, where the
        // default ports differ for that reason alone).
        let same_host = match (site.host_str(), return_to.host_str()) {
            (Some(site_host), Some(back)) => {
                site_host.eq_ignore_ascii_case(back)
                    && (site.scheme() != return_to.scheme()
                        || site.port_or_known_default() == return_to.port_or_known_default())
            }
            _ => false,
        };
        if !same_host {
            return Err("The plugin asked for this store's credentials to be sent to a different website than the shop's. Start connecting again from the shop's own settings page.");
        }
        Ok(ConnectTarget { return_to })
    }
}

/// `raw` as a web address a plugin may use, or `None`.
fn web_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    if url.host().is_none() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    match url.scheme() {
        "https" => Some(url),
        "http" if plain_http_allowed(&url) => Some(url),
        _ => None,
    }
}

/// Plain http is only for a shop on this machine (development) or a Tor
/// onion service, whose address already authenticates it.
fn plain_http_allowed(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".onion")
        }
        None => false,
    }
}

impl ConnectQuery {
    fn request(&self) -> ConnectRequest<'_> {
        ConnectRequest {
            site_url: &self.site_url,
            return_url: &self.return_url,
            nonce: &self.nonce,
        }
    }
}

/// `resubmit` is `None` on a plain `GET` (empty key/origin fields, mainnet
/// selected) or `Some(&form)` re-rendering after a rejected `POST` - see
/// `ConnectViewModel`'s doc comment (`templates.rs`) for why every submitted
/// field, including the two key hex fields, gets echoed back rather than
/// lost.
///
/// Looks up `user_id`'s existing `store_connections` on every render so the
/// "use an existing store" picker (`PlatformConnectViewModel::existing_stores`)
/// is never stale - cheap, and consistent with `home::dashboard_home`
/// already doing one query per connected store on every dashboard load.
async fn render_confirm_form(
    state: &AppState,
    platform: &str,
    request: ConnectRequest<'_>,
    error: Option<&str>,
    resubmit: Option<&ConfirmForm>,
    user: &UserRow,
) -> Response {
    let ConnectRequest {
        site_url,
        return_url,
        nonce,
    } = request;
    let (network_mainnet_selected, network_stagenet_selected, network_testnet_selected) =
        network_selected_flags(
            resubmit
                .and_then(|f| f.network.as_deref())
                .unwrap_or("mainnet"),
        );
    let selected_currency = resubmit
        .and_then(|f| f.base_currency.as_deref())
        .unwrap_or("XMR");
    let (selected, user_id) = (selected_currency.to_string(), user.id.clone());
    let (currency_options, stores) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                crate::currencies::currency_options(db, &selected).unwrap_or_default(),
                db.list_store_connections_for_user(&user_id)
                    .unwrap_or_default(),
            ))
        })
        .await
        .unwrap_or_default();
    let existing_stores = stores
        .into_iter()
        .map(|row| ExistingStoreOption {
            connection_id: row.id,
            display_name: display_name_for(&row.site_url),
            platform: row.platform,
        })
        .collect();
    // A link whose return address isn't the shop's shows why, and no form.
    let unavailable = match ConnectTarget::parse(site_url, return_url) {
        Err(reason) => Some(reason.to_string()),
        Ok(_) => public_url_for_plugins(state).await.err(),
    };
    let chrome = super::page_chrome(state, Some(user), format!("/connect/{platform}")).await;
    let data = PlatformConnectViewModel {
        platform: platform.to_string(),
        site_url: site_url.to_string(),
        return_url: return_url.to_string(),
        nonce: nonce.to_string(),
        error: error.map(str::to_string),
        view_key_hex: resubmit
            .and_then(|f| f.view_key_hex.clone())
            .unwrap_or_default(),
        spend_pubkey_hex: resubmit
            .and_then(|f| f.spend_pubkey_hex.clone())
            .unwrap_or_default(),
        network_mainnet_selected,
        network_stagenet_selected,
        network_testnet_selected,
        currency_options,
        custody_choices: super::status_page::custody_choice_views(
            &state.engine,
            resubmit.and_then(|f| f.key_custody_backend.as_deref()),
        ),
        existing_stores,
        unavailable,
    };
    crate::views::connect::platform_page(&chrome, &data).into_response()
}

/// Why plugins can't connect while this instance has no public address.
const NO_PUBLIC_URL: &str = "This Monokulo instance can't connect plugins yet: its operator hasn't set its public \
     address (the public URL setting on the admin settings page). Plugins need it to create orders and send \
     customers to the checkout. Ask the operator to set it, then try connecting again.";

/// This instance's public address, which is what a plugin is given as its
/// `endpoint` - or, while it isn't set, the message explaining that
/// connecting can't work yet. Checked on the confirm screen (so the merchant
/// sees it before typing anything), again when it is submitted, and in
/// `/finish`, so a plugin is never handed a wrong address.
async fn public_url_for_plugins(state: &AppState) -> Result<String, String> {
    state
        .settings
        .public_url()
        .ok_or_else(|| NO_PUBLIC_URL.to_string())
}

/// Percent-encodes `s` for safe embedding as one query-string value - the
/// same encoding a browser's own `application/x-www-form-urlencoded`
/// submission uses. Used here to build the `next` URL handed to
/// `/dashboard/login`, not to build `return_url`'s own query string (that
/// one goes through the `url` crate's `query_pairs_mut`, which encodes
/// correctly on its own).
fn encode_query_value(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// `GET /connect/{platform}` (WBS 1.4.1, step 1-3). No valid session:
/// redirect to `/dashboard/login` carrying a `next` that reconstructs this
/// exact URL (see `dashboard::login_submit`/`is_safe_redirect_path` - this
/// value is always a same-origin relative path built entirely from this
/// route's own known shape, so it always passes that validation on the way
/// back). A valid session: render the confirm form.
/// The platforms the connect flow takes: anything else in the path is not
/// stored as a store's platform.
const PLATFORMS: [&str; 2] = ["woocommerce", "custom"];

pub async fn start(
    State(state): State<AppState>,
    Path(platform): Path<String>,
    Query(query): Query<ConnectQuery>,
    headers: HeaderMap,
) -> Response {
    if !PLATFORMS.contains(&platform.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some((user, _)) = super::resolve_authed_user(&state, &headers).await else {
        let this_url = format!(
            "/connect/{}?site_url={}&return_url={}&nonce={}",
            platform,
            encode_query_value(&query.site_url),
            encode_query_value(&query.return_url),
            encode_query_value(&query.nonce),
        );
        return redirect_302(&format!(
            "/dashboard/login?next={}",
            encode_query_value(&this_url)
        ));
    };

    render_confirm_form(&state, &platform, query.request(), None, None, &user).await
}

/// `POST /connect/{platform}`'s form fields (WBS 1.4.1, step 4) - the same
/// wallet-connection fields `dashboard::ConnectForm` has, plus `site_url`
/// (shown, not editable, on the confirm screen) and the `return_url`/`nonce`
/// hidden fields carried through from `GET /connect/{platform}`'s query
/// string so they survive the round trip.
fn default_confirm_mode() -> String {
    "new".to_string()
}

#[derive(Deserialize)]
pub struct ConfirmForm {
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    /// `"new"` (provision a fresh tenant from the key fields below - the
    /// only mode that ever existed before the "use an existing store"
    /// picker) or `"existing"` (skip provisioning entirely and reuse
    /// `connection_id`, an already-owned `store_connections` row).
    /// `#[serde(default)]` to `"new"` keeps every pre-existing caller/test
    /// that never sent this field parsing exactly as it always has.
    #[serde(default = "default_confirm_mode")]
    pub mode: String,
    /// Required when `mode == "existing"`, ignored otherwise. Ownership is
    /// verified against the authenticated user in `confirm_existing_store`
    /// before it's ever trusted for anything - this is untrusted input, not
    /// a capability.
    #[serde(default)]
    pub connection_id: Option<String>,
    /// `Option`, not `String`: absent entirely on an "existing"-mode
    /// submission, since the confirm screen's "use an existing store" form
    /// never renders these fields at all - there is nothing to parse there.
    #[serde(default)]
    pub view_key_hex: Option<String>,
    #[serde(default)]
    pub spend_pubkey_hex: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
    /// Not shown on the confirm screen (no UI field for it yet) — carried purely so
    /// a caller who needs a non-default tenant `order_expiry_seconds` (e.g. WBS
    /// 1.4.4's forced-expiry test) has a real way to set it through this flow rather
    /// than only via the JSON `POST /connections` surface. `#[serde(default)]` keeps
    /// every existing form submission (none of which send this field) parsing
    /// exactly as before, defaulting to the engine's own default.
    #[serde(default)]
    pub order_expiry_seconds: Option<i64>,
    /// Same reasoning as `order_expiry_seconds` immediately above: not shown on the
    /// confirm screen (no UI field for it yet), carried purely so a caller who needs a
    /// non-default `confirmations_required` (WBS 1.4.5's real stagenet connect-flow
    /// test wants `0` - native 0-conf, since real stagenet blocks land roughly every
    /// ~2 minutes and the engine's own default of 10 would make a test wait ~20
    /// minutes) has a real way to set it through this flow. `#[serde(default)]` keeps
    /// every existing form submission parsing exactly as before, defaulting to the
    /// engine's own default (10).
    #[serde(default)]
    pub confirmations_required: Option<u64>,
    /// Required when `mode == "new"`, ignored for `"existing"` (an existing
    /// store already has its own base currency). Validated against
    /// `crate::currencies` in `connections::create_connection_for_user`.
    #[serde(default)]
    pub base_currency: Option<String>,
    /// Only sent when the form offered a choice (part 5).
    #[serde(default)]
    pub key_custody_backend: Option<String>,
}

impl ConfirmForm {
    fn request(&self) -> ConnectRequest<'_> {
        ConnectRequest {
            site_url: &self.site_url,
            return_url: &self.return_url,
            nonce: &self.nonce,
        }
    }
}

/// `POST /connect/{platform}` (behind [`AuthedUser`], WBS 1.4.1 step 4): the
/// confirm-form submission - either mode (see [`ConfirmForm::mode`]) ends the
/// same way, minting a single-use connect token and redirecting to
/// `return_url` (`mint_token_and_redirect`).
pub async fn confirm_submit(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
    Path(platform): Path<String>,
    Form(form): Form<ConfirmForm>,
) -> Response {
    if !PLATFORMS.contains(&platform.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let target = ConnectTarget::parse(&form.site_url, &form.return_url);
    let target = match target {
        Ok(target) if public_url_for_plugins(&state).await.is_ok() => target,
        // `render_confirm_form` shows the reason instead of the form.
        _ => {
            return render_confirm_form(&state, &platform, form.request(), None, Some(&form), &user)
                .await
        }
    };
    if form.mode == "existing" {
        confirm_existing_store(&state, &user, &platform, &form, &target).await
    } else {
        confirm_new_store(&state, &user, &platform, &form, &target).await
    }
}

/// `mode == "new"` (the only mode that existed before the "use an existing
/// store" picker): provisions a brand-new tenant via the exact same
/// [`connections::create_connection_for_user`] every other surface uses. On
/// an engine rejection or internal error, re-renders the confirm form with a
/// visible error - same pattern as `dashboard::connect_submit`.
async fn confirm_new_store(
    state: &AppState,
    user: &UserRow,
    platform: &str,
    form: &ConfirmForm,
    target: &ConnectTarget,
) -> Response {
    let fields = CreateConnectionFields {
        platform: platform.to_string(),
        site_url: form.site_url.clone(),
        view_key_hex: form.view_key_hex.clone().unwrap_or_default(),
        spend_pubkey_hex: form.spend_pubkey_hex.clone().unwrap_or_default(),
        network: form.network.clone(),
        domains: Vec::new(),
        confirmations_required: form.confirmations_required,
        order_expiry_seconds: form.order_expiry_seconds,
        base_currency: form.base_currency.clone().unwrap_or_default(),
        key_custody_backend: form.key_custody_backend.clone().filter(|b| !b.is_empty()),
    };

    let outcome = match connections::create_connection_for_user(state, user, fields).await {
        Ok(outcome) => outcome,
        Err(CreateConnectionError::BadRequest(message)) => {
            return render_confirm_form(
                state,
                platform,
                form.request(),
                Some(&message),
                Some(form),
                user,
            )
            .await;
        }
        Err(CreateConnectionError::Internal) => {
            return render_confirm_form(
                state,
                platform,
                form.request(),
                Some("Something went wrong. Please try again."),
                Some(form),
                user,
            )
            .await;
        }
    };

    mint_token_and_redirect(state, &outcome.connection_id, platform, form, user, target).await
}

/// `mode == "existing"`: no new tenant is provisioned at all - the plugin is
/// handed credentials for a `store_connections` row the user already has,
/// selected via `form.connection_id`. The ownership check below is a real
/// security boundary, not a courtesy: without it, a signed-in attacker could
/// submit *any* connection id (not just their own) and have that store's
/// genuine `sk_...` secret token delivered to their own attacker-controlled
/// `return_url` via `/connect/{platform}/finish` - exactly the kind of IDOR
/// `orders.rs`'s own `load_owned_connection` already guards against
/// elsewhere in this crate, applied here to the one place that grants a
/// *credential*, not just a read.
async fn confirm_existing_store(
    state: &AppState,
    user: &UserRow,
    platform: &str,
    form: &ConfirmForm,
    target: &ConnectTarget,
) -> Response {
    let connection_id = match form.connection_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => &crate::db::ConnectionId::new(id),
        None => {
            return render_confirm_form(
                state,
                platform,
                form.request(),
                Some("Choose a store to connect."),
                Some(form),
                user,
            )
            .await;
        }
    };

    /// What attaching the site to an existing store found.
    enum Attach {
        Attached,
        NotFound,
    }
    // Ownership check, domain suggestion and site URL update in one write
    // job, so the store can't change hands in between.
    let (id, user_id, site_url) = (
        connection_id.clone(),
        user.id.clone(),
        form.site_url.clone(),
    );
    let attached = state
        .db
        .write(move |db| {
            // Same enumeration-defense convention `orders.rs` documents for
            // its own ownership check: a nonexistent id and someone else's id
            // must be indistinguishable to the caller.
            let owned = db
                .get_store_connection_by_id(&id)?
                .and_then(|row| super::OwnedStore::check(row, &user_id));
            if owned.is_none() {
                return Ok(Attach::NotFound);
            }
            // Attaching this WordPress site to an already-existing store: its
            // domain joins the store's domains, waiting for the merchant to
            // verify it (`crate::embed_domains`), and the row's `site_url` is
            // updated so the dashboard reflects the most recent site this
            // store is actually serving.
            crate::embed_domains::suggest_site_domain(db, &id, &site_url, now_unix());
            db.update_store_connection_site_url(&id, &site_url)?;
            Ok::<_, crate::db::DbError>(Attach::Attached)
        })
        .await;
    match attached {
        Ok(Attach::Attached) => {}
        Ok(Attach::NotFound) => {
            return render_confirm_form(
                state,
                platform,
                form.request(),
                Some("That store could not be found."),
                Some(form),
                user,
            )
            .await;
        }
        Err(_) => {
            return render_confirm_form(
                state,
                platform,
                form.request(),
                Some("Something went wrong. Please try again."),
                Some(form),
                user,
            )
            .await;
        }
    }

    mint_token_and_redirect(state, connection_id, platform, form, user, target).await
}

/// The step common to both modes once a connection id is settled on
/// (freshly created, or an existing one the ownership check above already
/// approved): mint a single-use connect token and redirect to `return_url`
/// with `token`/`nonce` appended (parsed and re-serialized via the `url`
/// crate, so a `return_url` that already carries its own query string is
/// handled correctly - never a naive string-concatenated `?`).
async fn mint_token_and_redirect(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    platform: &str,
    form: &ConfirmForm,
    user: &UserRow,
    target: &ConnectTarget,
) -> Response {
    let raw_token = shared::auth::generate_connect_token();
    let token_hash = raw_token.hash();
    let (id, nonce) = (connection_id.clone(), form.nonce.clone());
    let stored = state
        .db
        .write(move |db| db.create_connect_token(&token_hash, &id, &nonce, now_unix()))
        .await;
    if stored.is_err() {
        return render_confirm_form(
            state,
            platform,
            form.request(),
            Some("Something went wrong. Please try again."),
            Some(form),
            user,
        )
        .await;
    }

    let mut redirect_url = target.return_to.clone();
    // `query_pairs_mut` appends to whatever query string `return_url`
    // already has (parsing it properly first, per the `url` crate's own
    // model) rather than string-concatenating a `?`/`&`, which would
    // produce a broken URL for a `return_url` that already has its own
    // query parameters.
    redirect_url
        .query_pairs_mut()
        .append_pair("token", raw_token.expose())
        .append_pair("nonce", &form.nonce);

    redirect_302(redirect_url.as_str())
}

#[derive(Deserialize)]
pub struct FinishRequest {
    pub token: String,
    /// The plugin's own webhook receiver URL (WBS 1.4.4) — it can only be known once
    /// the plugin holds real credentials, hence this arriving here rather than at
    /// the earlier confirm step. `Option` for backward compatibility: a caller that
    /// omits it (or an older plugin build) simply gets no webhook registered, and
    /// `FinishResponse::webhook_signing_secret` is absent from the response, exactly
    /// as it always has been for every caller before this field existed.
    #[serde(default)]
    pub webhook_url: Option<String>,
}

#[derive(Serialize)]
pub struct FinishResponse {
    pub public_key: String,
    pub secret_token: String,
    /// This instance's public address (`crate::settings::public_url`): where
    /// the plugin creates orders (`POST {endpoint}/pay/{pk}/orders` with the
    /// secret key) and sends customers (`{endpoint}/pay/{pk}/orders/{id}`).
    /// Never the engine's address - the engine is private.
    pub endpoint: String,
    /// Present only when `webhook_url` was supplied and registration succeeded.
    /// `skip_serializing_if` keeps the wire shape for a caller with no webhook
    /// exactly what it always was — a bare `{public_key, secret_token, endpoint}` —
    /// rather than growing a permanent `null` field for every existing caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_signing_secret: Option<String>,
}

/// `POST /connect/{platform}/finish` (WBS 1.4.1 step 5, extended by 1.4.4) -
/// deliberately *not* behind [`AuthedUser`]: this is a server-to-server call
/// from the plugin, which has no monokulo session at all (it's the
/// browser, not the plugin's backend, that ever holds one). Redeems the
/// connect token exactly once (see [`crate::db::Db::consume_connect_token`])
/// and returns the real credentials - plus, when `webhook_url` was supplied,
/// registers a real webhook against the engine (via
/// [`crate::engine_client::EngineClient::create_webhook`]) and returns its
/// `signing_secret`.
///
/// Every failure mode - unknown token, already-consumed token, expired
/// token, a connection/decrypt failure that should never actually happen for
/// a row this service itself wrote, or (new in 1.4.4) a failed webhook
/// registration - collapses to a bare `401`. That's deliberate for the first
/// three, same enumeration-defense principle used everywhere else in this
/// crate; for webhook registration specifically it is a considered policy
/// choice, not just "reuse the existing pattern" - see the doc comment
/// immediately above the webhook-registration branch below for the tradeoff
/// this accepts and why.
///
/// While this instance has no public address, `/finish` answers `503` with
/// a JSON `{"error": ...}` the plugin can show, *before* redeeming the
/// token, so the same token still works once the operator sets it.
pub async fn finish(State(state): State<AppState>, Json(req): Json<FinishRequest>) -> Response {
    let endpoint = match public_url_for_plugins(&state).await {
        Ok(url) => url,
        Err(message) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": message })),
            )
                .into_response()
        }
    };
    let token_hash = shared::auth::RawToken::presented(&req.token).hash();

    // The token is spent and its connection read in one write job.
    let row = state
        .db
        .write(move |db| {
            match db.consume_connect_token(&token_hash, now_unix(), CONNECT_TOKEN_TTL_SECONDS)? {
                Some(id) => db.get_store_connection_by_id(&id),
                None => Ok(None),
            }
        })
        .await;
    let row = match row {
        Ok(Some(row)) => row,
        // An unknown or expired token, or one pointing at a connection that
        // no longer exists - shouldn't happen (nothing deletes
        // `store_connections` rows), but this is this service's own
        // problem, not a credential the caller could have gotten right some
        // other way.
        Ok(None) | Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let secret_token = match super::orders::decrypt_sk(&state.encryption_key, &row) {
        Ok(v) => v,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    // Webhook registration (WBS 1.4.4): only attempted when the caller supplied a
    // `webhook_url`. On failure this collapses the *entire* `/finish` call to `401`,
    // exactly like every other failure mode above - deliberately, per this task's own
    // spec, even though the token has by this point already been irreversibly
    // consumed (see `consume_connect_token`'s atomicity doc comment) and a
    // registration failure here is `EngineClientError`-shaped, not enumeration-shaped
    // (unlike the branches above, this one has nothing to hide from a legitimate
    // caller - a network blip or a rejected URL isn't a secret). The accepted
    // tradeoff: a plugin whose *webhook* registration fails (a transient network
    // issue between the control plane and the engine, say, with credential retrieval
    // itself having fully succeeded) gets no credentials at all and cannot retry with
    // the same token - it must restart the whole connect flow from `GET
    // /connect/{platform}` to mint a fresh one. This was judged the safer default
    // over the alternative (return credentials anyway, with no webhook and no way to
    // signal that clearly in a shape existing callers already parse) rather than
    // because the collapse-to-401 pattern was merely convenient to reuse - flagged
    // here explicitly in case a real deployment prefers "credentials now, webhook
    // registration retried separately" instead.
    let webhook_signing_secret = match &req.webhook_url {
        Some(url) => match state
            .engine
            .client
            .create_webhook(&secret_token, url, &Default::default())
            .await
        {
            Ok((_webhook_id, signing_secret)) => Some(signing_secret),
            Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
        },
        None => None,
    };

    Json(FinishResponse {
        public_key: row.tenant_public_key,
        secret_token: secret_token.expose().to_string(),
        endpoint,
        webhook_signing_secret,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    use crate::engine_client::EngineClient;

    use super::super::{build_router, AppState};

    /// Same fixed-scalar construction `connections.rs`'s and
    /// `engine_client.rs`'s own tests use.
    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    /// This test instance's configured public address - what `/finish`
    /// must hand plugins as `endpoint`.
    const TEST_PUBLIC_URL: &str = "https://pay.example.test";

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let engine_client = EngineClient::for_tests(format!("http://{}", engine.addr));
        let state = AppState {
            engine: crate::http::Engine::new(engine_client),
            ..AppState::for_tests()
        }
        .with_options(&format!(
            "public_url = \"{TEST_PUBLIC_URL}\"\n[signup]\nmode = \"public\"\n"
        ))
        .await;
        (state, engine)
    }

    fn form_body(fields: &[(&str, &str)]) -> String {
        fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&")
    }

    use crate::http::test_support::urlencoding_encode;

    fn form_request(uri: &str, cookie: Option<&str>, fields: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::from(form_body(fields))).unwrap()
    }

    use crate::http::test_support::body_json;

    use crate::http::test_support::body_text;

    /// Signs up and logs in a fresh user through the browser form flow,
    /// returning the `session=<value>` pair a browser would send back as a
    /// `Cookie` header - same helper `http/tests.rs` uses for its own
    /// WBS 1.3.2 tests.
    async fn signed_up_and_logged_in_session_cookie(
        router: &Router,
        email: &str,
        password: &str,
    ) -> String {
        let signup = router
            .clone()
            .oneshot(form_request(
                "/dashboard/signup",
                None,
                &[("email", email), ("password", password)],
            ))
            .await
            .unwrap();
        assert_eq!(signup.status(), StatusCode::FOUND);

        let login = router
            .clone()
            .oneshot(form_request(
                "/dashboard/login",
                None,
                &[("email", email), ("password", password)],
            ))
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::FOUND);
        let set_cookie = login
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        set_cookie.split(';').next().unwrap().to_string()
    }

    fn parse_query_params(url: &str) -> std::collections::HashMap<String, String> {
        let parsed = url::Url::parse(url).unwrap();
        parsed
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn full_round_trip_confirm_then_redirect_then_finish_yields_real_working_credentials() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "connect-flow@example.com",
            "correct horse battery staple",
        )
        .await;

        // Step 1-3: GET the connect start URL with a valid session - expect
        // the confirm form, not a login redirect.
        let get_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fsettings%3Fpage%3Dmonero&nonce=nonce-xyz")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            get_response.status(),
            StatusCode::OK,
            "expected the confirm form, not a redirect"
        );
        let html = body_text(get_response).await;
        assert!(
            html.contains("shop.example.com"),
            "expected the site_url shown on the confirm page, got: {html}"
        );
        assert!(
            html.contains(r#"action="/connect/woocommerce""#),
            "expected the confirm form to post back to /connect/woocommerce, got: {html}"
        );

        // Step 4: POST the confirm form with valid wallet fields.
        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    (
                        "return_url",
                        "https://shop.example.com/settings?page=monero",
                    ),
                    ("nonce", "nonce-xyz"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                    ("base_currency", "XMR"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            post_response.status(),
            StatusCode::FOUND,
            "expected a 302 redirect to return_url"
        );
        let location = post_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // The existing `page=monero` query param must survive alongside the
        // newly appended ones - proof the redirect is built by parsing and
        // re-serializing `return_url`, not by naively concatenating a `?`.
        let params = parse_query_params(&location);
        assert_eq!(params.get("page").map(String::as_str), Some("monero"));
        let token = params
            .get("token")
            .expect("expected a token query param")
            .clone();
        assert!(!token.is_empty());
        assert_eq!(
            params.get("nonce").map(String::as_str),
            Some("nonce-xyz"),
            "the nonce must round-trip unchanged"
        );

        // Step 5: POST the token to /finish - server-to-server, no session
        // at all.
        let finish_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": token }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            finish_response.status(),
            StatusCode::OK,
            "expected the first finish call to succeed"
        );
        let finish_body = body_json(finish_response).await;
        let obj = finish_body.as_object().unwrap();
        let public_key = obj.get("public_key").unwrap().as_str().unwrap();
        assert!(public_key.starts_with("pk_"));
        let secret_token = obj.get("secret_token").unwrap().as_str().unwrap();
        assert!(secret_token.starts_with("sk_"));
        let endpoint = obj.get("endpoint").unwrap().as_str().unwrap();
        assert_eq!(
            endpoint, TEST_PUBLIC_URL,
            "plugins get monokulo's public address"
        );
        assert_ne!(
            endpoint,
            format!("http://{}", engine.addr),
            "never the engine's"
        );
        // No webhook was asked for, so no signing secret comes back.
        assert!(!obj.contains_key("webhook_signing_secret"));

        // Strong proof: `secret_token` is the tenant's real, working `sk_`
        // credential, not just a string that happens to start with `sk_` -
        // same pattern 1.2.3/1.3.2 already established.
        let engine_client = EngineClient::for_tests(format!("http://{}", engine.addr));
        let tenant_view = engine_client.get_tenant(&shared::auth::RawToken::presented(secret_token)).await.expect(
            "the returned secret_token should be the tenant's genuine, functioning sk_ credential",
        );
        assert_eq!(tenant_view.public_key, public_key);
    }

    #[tokio::test]
    async fn finishing_the_same_token_twice_only_succeeds_once() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "connect-single-use@example.com",
            "correct horse battery staple",
        )
        .await;

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-single-use"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                    ("base_currency", "XMR"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let location = post_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let token = parse_query_params(&location).get("token").unwrap().clone();

        let finish_request = || {
            Request::builder()
                .method("POST")
                .uri("/connect/woocommerce/finish")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "token": token }).to_string(),
                ))
                .unwrap()
        };

        // Prove the first call actually succeeds before asserting the
        // second fails - otherwise a second-call 401 could just mean the
        // token never worked at all.
        let first = router.clone().oneshot(finish_request()).await.unwrap();
        assert_eq!(
            first.status(),
            StatusCode::OK,
            "the first finish call must actually succeed"
        );

        let second = router.oneshot(finish_request()).await.unwrap();
        assert_eq!(
            second.status(),
            StatusCode::UNAUTHORIZED,
            "reusing an already-consumed token must fail"
        );
    }

    #[tokio::test]
    async fn finish_with_a_garbage_token_returns_unauthorized() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": "conn_nobody_ever_issued_this" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// WBS 1.4.4: a `webhook_url` supplied to `/finish` genuinely registers a
    /// webhook against the real engine - not just a plausibly-shaped response.
    /// Also exercises this task's other addition, `ConfirmForm::order_expiry_seconds`
    /// (threaded all the way to `CreateTenantRequest`), in the same round trip: the
    /// created tenant's `order_expiry_seconds` is fetched back via `get_tenant` and
    /// must match exactly what the confirm form sent, proving the field genuinely
    /// reaches the engine rather than being silently dropped.
    #[tokio::test]
    async fn finish_with_a_webhook_url_registers_a_real_webhook_and_carries_the_signing_secret() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "webhook-register@example.com",
            "correct horse battery staple",
        )
        .await;

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-webhook"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                    ("order_expiry_seconds", "1"),
                    ("base_currency", "XMR"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let location = post_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let token = parse_query_params(&location).get("token").unwrap().clone();

        let finish_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": token, "webhook_url": "https://merchant.example/hook" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_response.status(), StatusCode::OK);
        let body = body_json(finish_response).await;
        let obj = body.as_object().unwrap();
        let secret_token = obj
            .get("secret_token")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let signing_secret = obj
            .get("webhook_signing_secret")
            .expect("expected webhook_signing_secret in the response")
            .as_str()
            .unwrap();
        assert!(!signing_secret.is_empty());

        // Strong proof, not just a well-shaped response: the webhook genuinely
        // exists on the real engine, under this tenant, with the exact URL
        // submitted - and the tenant's order_expiry_seconds genuinely reached the
        // engine too.
        let engine_client = EngineClient::for_tests(format!("http://{}", engine.addr));
        let webhooks = engine_client
            .list_webhooks(&shared::auth::RawToken::presented(&secret_token))
            .await
            .expect("list_webhooks against the real engine should succeed");
        assert_eq!(webhooks.len(), 1);
        assert_eq!(webhooks[0].url, "https://merchant.example/hook");

        let tenant_view = engine_client
            .get_tenant(&shared::auth::RawToken::presented(&secret_token))
            .await
            .expect("get_tenant against the real engine should succeed");
        assert_eq!(
            tenant_view.order_expiry_seconds, 1,
            "order_expiry_seconds must have reached the engine's real tenant record"
        );
    }

    /// A `webhook_url` the engine rejects (WBS 1.4.4's collapse-to-401 policy, see
    /// `finish`'s own doc comment) fails the *entire* `/finish` call, not just the
    /// webhook part - the caller never sees `public_key`/`secret_token` at all, and
    /// (since the token was already consumed by this point) can't simply retry the
    /// same token once it supplies a valid URL.
    #[tokio::test]
    async fn finish_with_a_rejected_webhook_url_fails_the_whole_call() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "webhook-reject@example.com",
            "correct horse battery staple",
        )
        .await;

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-webhook-reject"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                    ("base_currency", "XMR"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let location = post_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let token = parse_query_params(&location).get("token").unwrap().clone();

        // `ftp://` is neither `http` nor `https` - the engine's own
        // `create_webhook` rejects it with a real `400`, which `EngineClient`
        // surfaces as an `Err`, which this handler collapses to `401`.
        let finish_response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "token": token, "webhook_url": "ftp://not-http.example/hook" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_unauthenticated_connect_start_redirects_through_login_and_back_to_the_original_url()
    {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let original_uri = "/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fsettings&nonce=detour-nonce";

        let get_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(original_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            get_response.status(),
            StatusCode::FOUND,
            "expected a redirect to the login page"
        );
        let login_location = get_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            login_location.starts_with("/dashboard/login?next="),
            "expected a next-carrying login redirect, got: {login_location}"
        );

        // Extract the (still percent-encoded) `next` value exactly as a
        // browser would receive it in the `Location` header, then sign up
        // and log in, submitting that same value back as the login form's
        // hidden `next` field - the real end-to-end detour, not just a call
        // to the validator function in isolation.
        let next_value = login_location
            .strip_prefix("/dashboard/login?next=")
            .unwrap();
        let decoded_next = url::form_urlencoded::parse(format!("x={next_value}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(
            decoded_next, original_uri,
            "the next value must reconstruct the exact original connect URL"
        );

        let email = "connect-detour@example.com";
        let password = "correct horse battery staple";
        let signup = router
            .clone()
            .oneshot(form_request(
                "/dashboard/signup",
                None,
                &[("email", email), ("password", password)],
            ))
            .await
            .unwrap();
        assert_eq!(signup.status(), StatusCode::FOUND);

        let login_response = router
            .clone()
            .oneshot(form_request(
                "/dashboard/login",
                None,
                &[
                    ("email", email),
                    ("password", password),
                    ("next", &decoded_next),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            login_response.status(),
            StatusCode::FOUND,
            "a successful login with a valid next must redirect"
        );
        let final_location = login_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(
            final_location, original_uri,
            "must land back on the exact original connect URL, query params intact"
        );
    }

    /// Same real UX bug `http/tests.rs`'s
    /// `a_rejected_connect_submission_re_fills_every_field_the_merchant_typed`
    /// covers for `/dashboard/connect`, here for the plugin-driven
    /// `/connect/{platform}` confirm form: a rejected submission must not
    /// throw away the site_url/view key/spend key/network/allowed_origins
    /// the merchant already typed in.
    #[tokio::test]
    async fn a_rejected_confirm_submission_re_fills_every_field_the_merchant_typed() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "keep-my-confirm-inputs@example.com",
            "correct horse battery staple",
        )
        .await;

        let response = router
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-abc"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    // Well-formed hex, real, verified invalid curve point -
                    // same value `http/tests.rs`'s sibling test uses.
                    ("spend_pubkey_hex", &"ff".repeat(32)),
                    ("network", "stagenet"),
                    ("allowed_origins", "https://shop.example.com"),
                    ("base_currency", "XMR"),
                ],
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("class=\"error\""),
            "expected a visible error, got: {html}"
        );
        assert!(
            html.contains(&format!(r#"value="{TEST_VIEW_KEY_HEX}""#)),
            "expected the valid view key kept, got: {html}"
        );
        assert!(
            html.contains(&format!(r#"value="{}""#, "ff".repeat(32))),
            "expected the rejected spend key re-filled, got: {html}"
        );
        assert!(
            !html.contains("allowed_origins"),
            "allowed origins are no longer asked for, got: {html}"
        );
        assert!(
            html.contains(r#"value="stagenet" selected"#),
            "expected stagenet to stay selected, got: {html}"
        );
        // The hidden site_url/return_url/nonce fields were already always
        // preserved (they're passed straight through, not part of this
        // bug) - confirmed here too so a future refactor can't silently
        // break that while fixing something else nearby.
        assert!(html.contains(r#"name="nonce" value="nonce-abc""#));
    }

    #[tokio::test]
    async fn an_unknown_base_currency_on_the_generic_connect_flow_is_rejected_before_provisioning_a_tenant(
    ) {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "bad-currency-generic-flow@example.com",
            "correct horse battery staple",
        )
        .await;

        let response = router
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-bad-currency"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("allowed_origins", ""),
                    ("base_currency", "NOTREAL"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a rejected submission re-renders the confirm form, not a redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("class=\"error\""),
            "expected a visible error, got: {html}"
        );
    }

    /// Creates a real `store_connections` row for the given session cookie
    /// via the JSON `/connections` API (which accepts a session cookie the
    /// same way it accepts a bearer token - `AuthedUser` takes either),
    /// returning `(connection_id, public_key)`. Test-only setup for the
    /// "use an existing store" tests below - a store to actually pick.
    async fn create_a_store(router: &Router, cookie: &str, site_url: &str) -> (String, String) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("cookie", cookie)
                    .body(Body::from(
                        serde_json::json!({
                            "platform": "custom",
                            "site_url": site_url,
                            "view_key_hex": TEST_VIEW_KEY_HEX,
                            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
                            "network": "mainnet",
                            "domains": [],
                            "base_currency": "XMR",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        let obj = body.as_object().unwrap();
        (
            obj.get("connection_id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            obj.get("public_key").unwrap().as_str().unwrap().to_string(),
        )
    }

    #[tokio::test]
    async fn the_confirm_screen_offers_no_existing_store_picker_for_a_user_with_no_stores_yet() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "no-stores-yet@example.com",
            "correct horse battery staple",
        )
        .await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fsettings&nonce=n")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(!html.contains("Use an existing store"), "a user with nothing connected yet shouldn't be offered a picker with nothing in it, got: {html}");
    }

    #[tokio::test]
    async fn the_confirm_screen_offers_an_existing_store_picker_once_the_user_has_one() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "has-a-store-already@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) =
            create_a_store(&router, &cookie, "https://my-existing-shop.example.com").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/connect/woocommerce?site_url=https%3A%2F%2Fnew-wp-site.example.com&return_url=https%3A%2F%2Fnew-wp-site.example.com%2Fsettings&nonce=n")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Use an existing store"),
            "expected the picker, got: {html}"
        );
        assert!(
            html.contains(&format!(r#"value="{connection_id}""#)),
            "expected the store's connection id as an option, got: {html}"
        );
        assert!(
            html.contains("my-existing-shop.example.com"),
            "expected the store's derived display name, got: {html}"
        );
        assert!(!html.contains(&public_key), "the picker only needs to identify the store by name, not expose its public key on this page");
    }

    /// The actual feature: choosing "use an existing store" must not
    /// provision a second tenant - it hands the plugin credentials for the
    /// *same* store, and the store's own order history (a real signal that
    /// no fresh tenant was minted) must be unaffected.
    #[tokio::test]
    async fn connecting_with_an_existing_store_reuses_it_instead_of_creating_a_new_one() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "reuse-existing@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) =
            create_a_store(&router, &cookie, "https://my-existing-shop.example.com").await;

        let user_id = state
            .db
            .lock()
            .get_user_by_email("reuse-existing@example.com")
            .unwrap()
            .unwrap()
            .id;
        let connections_before = state
            .db
            .lock()
            .list_store_connections_for_user(&user_id)
            .unwrap()
            .len();
        assert_eq!(connections_before, 1);

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://new-wp-site.example.com"),
                    ("return_url", "https://new-wp-site.example.com/settings"),
                    ("nonce", "nonce-existing"),
                    ("mode", "existing"),
                    ("connection_id", &connection_id),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            post_response.status(),
            StatusCode::FOUND,
            "expected a redirect to return_url"
        );
        let location = post_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let params = parse_query_params(&location);
        let token = params
            .get("token")
            .expect("expected a real connect token")
            .clone();
        assert_eq!(
            params.get("nonce").map(String::as_str),
            Some("nonce-existing")
        );

        // No second store_connections row was created for this "existing"-mode
        // submission.
        let connections_after = state
            .db
            .lock()
            .list_store_connections_for_user(&user_id)
            .unwrap()
            .len();
        assert_eq!(
            connections_after, 1,
            "using an existing store must not provision a second one"
        );

        // /finish hands back credentials for the *same* store - same
        // public_key as the one already created, not a fresh one.
        let finish_response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": token }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_response.status(), StatusCode::OK);
        let finish_body = body_json(finish_response).await;
        assert_eq!(
            finish_body["public_key"].as_str().unwrap(),
            public_key,
            "expected credentials for the same, already-existing store"
        );

        // The two real side effects of attaching a second site to an
        // existing store: the row's site_url reflects the new site...
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap();
        assert_eq!(
            row.site_url, "https://new-wp-site.example.com",
            "expected the row's site_url to move to the newly-attached site"
        );

        // ...and the new site's domain joins the store's domains, waiting
        // for the merchant to verify it.
        let domains = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap();
        assert!(
            domains
                .iter()
                .any(|d| d.domain == "new-wp-site.example.com" && d.verified_at.is_none()),
            "expected the new site's domain added, got: {domains:?}"
        );
    }

    /// The "added alongside, not replacing" half of the same behavior: a
    /// store whose first site's domain is already on its list keeps it
    /// after a second site attaches.
    #[tokio::test]
    async fn attaching_a_second_site_adds_its_domain_alongside_the_first() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "preserve-origin@example.com",
            "correct horse battery staple",
        )
        .await;

        // Create the store directly through the JSON API, so it starts with
        // its first site's domain already present.
        let create_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(
                        serde_json::json!({
                            "platform": "custom",
                            "site_url": "https://original-site.example.com",
                            "view_key_hex": TEST_VIEW_KEY_HEX,
                            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
                            "network": "mainnet",
                            "domains": ["https://original-site.example.com"],
                            "base_currency": "XMR",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::CREATED);
        let created = body_json(create_response).await;
        let connection_id = created["connection_id"].as_str().unwrap().to_string();

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://second-site.example.com"),
                    ("return_url", "https://second-site.example.com/settings"),
                    ("nonce", "nonce-preserve"),
                    ("mode", "existing"),
                    ("connection_id", &connection_id),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);

        let domains: Vec<String> = state
            .db
            .lock()
            .list_store_domains(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .into_iter()
            .map(|d| d.domain)
            .collect();
        assert_eq!(
            domains,
            vec![
                "original-site.example.com".to_string(),
                "second-site.example.com".to_string()
            ]
        );
    }

    /// The real security boundary: a signed-in user must not be able to
    /// attach *someone else's* store by guessing/copying its connection id -
    /// that would hand its genuine `sk_...` secret token to their own
    /// `return_url` via `/finish`.
    #[tokio::test]
    async fn connecting_with_a_connection_id_owned_by_a_different_user_is_rejected() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let victim_cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "victim@example.com",
            "correct horse battery staple",
        )
        .await;
        let (victim_connection_id, _victim_public_key) =
            create_a_store(&router, &victim_cookie, "https://victims-shop.example.com").await;

        let attacker_cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "attacker@example.com",
            "correct horse battery staple",
        )
        .await;

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&attacker_cookie),
                &[
                    ("site_url", "https://attacker-site.example.com"),
                    ("return_url", "https://attacker-site.example.com/settings"),
                    ("nonce", "nonce-attack"),
                    ("mode", "existing"),
                    ("connection_id", &victim_connection_id),
                ],
            ))
            .await
            .unwrap();

        // Not a redirect - never hand out a token for a store the caller
        // doesn't own.
        assert_eq!(
            post_response.status(),
            StatusCode::OK,
            "expected the confirm form re-rendered with an error, not a redirect"
        );
        let html = body_text(post_response).await;
        assert!(
            html.contains("class=\"error\""),
            "expected a visible error, got: {html}"
        );
        assert!(
            !html.contains("token="),
            "must never leak a token for a store the caller doesn't own"
        );
    }

    #[tokio::test]
    async fn submitting_existing_mode_with_no_connection_id_chosen_shows_a_clear_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "forgot-to-pick@example.com",
            "correct horse battery staple",
        )
        .await;
        let _ = create_a_store(&router, &cookie, "https://shop.example.com").await;

        let post_response = router
            .oneshot(form_request(
                "/connect/woocommerce",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-nochoice"),
                    ("mode", "existing"),
                    ("connection_id", ""),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::OK);
        let html = body_text(post_response).await;
        assert!(
            html.contains("Choose a store to connect."),
            "expected a clear error, got: {html}"
        );
    }

    /// While no public address is set, the confirm screen explains why
    /// connecting can't work (no form), submitting it does nothing, and
    /// `/finish` answers `503` with a JSON error *without* spending the
    /// token - which then works once the operator sets the address.
    #[tokio::test]
    async fn plugins_cannot_connect_until_the_public_url_is_set_and_are_told_why() {
        let (state, engine) = test_state_with_real_engine().await;
        state.save_setting("public_url", "").await;
        let router = build_router(state.clone());
        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "no-public-url@example.com",
            "correct horse battery staple",
        )
        .await;

        let start = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fcb&nonce=n1")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(start).await;
        assert!(html.contains("hasn't set its public"), "got: {html}");
        assert!(
            !html.contains("<form method=\"post\" action=\"/connect/woocommerce\""),
            "no confirm form, got: {html}"
        );

        let fields = [
            ("site_url", "https://shop.example.com"),
            ("return_url", "https://shop.example.com/cb"),
            ("nonce", "n1"),
            ("view_key_hex", TEST_VIEW_KEY_HEX),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
            ("network", "mainnet"),
            ("base_currency", "XMR"),
        ];
        let submit = router
            .clone()
            .oneshot(form_request("/connect/woocommerce", Some(&cookie), &fields))
            .await
            .unwrap();
        assert_eq!(submit.status(), StatusCode::OK, "no redirect, no token");
        let user_id = signed_in_user_id(&state, "no-public-url@example.com");
        assert!(state
            .db
            .lock()
            .list_store_connections_for_user(&shared::ids::UserId::new(user_id.to_string()))
            .unwrap()
            .is_empty());

        // Get a real token with the address set, then unset it again.
        state.save_setting("public_url", TEST_PUBLIC_URL).await;
        let submit = router
            .clone()
            .oneshot(form_request("/connect/woocommerce", Some(&cookie), &fields))
            .await
            .unwrap();
        assert_eq!(submit.status(), StatusCode::FOUND);
        let token =
            parse_query_params(submit.headers()["location"].to_str().unwrap())["token"].clone();
        state.save_setting("public_url", "").await;

        let finish = |token: String| {
            router.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connect/woocommerce/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": token }).to_string(),
                    ))
                    .unwrap(),
            )
        };
        let response = finish(token.clone()).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(response).await;
        assert!(
            body["error"].as_str().unwrap().contains("public address"),
            "{body}"
        );

        state.save_setting("public_url", TEST_PUBLIC_URL).await;
        let response = finish(token).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the token was not spent by the refused call"
        );
        let body = body_json(response).await;
        assert_eq!(body["endpoint"], TEST_PUBLIC_URL);
        assert!(
            !body.to_string().contains(&engine.addr.to_string()),
            "nothing in the response names the engine: {body}"
        );
    }

    fn signed_in_user_id(state: &AppState, email: &str) -> String {
        state
            .db
            .lock()
            .get_user_by_email(email)
            .unwrap()
            .unwrap()
            .id
            .into_string()
    }

    #[test]
    fn a_connect_target_must_send_credentials_back_to_the_shop_itself() {
        let ok = |site: &str, back: &str| super::ConnectTarget::parse(site, back).is_ok();
        assert!(ok(
            "https://shop.example.com/",
            "https://shop.example.com/wp-admin/admin-post.php"
        ));
        assert!(
            ok("https://Shop.Example.com", "https://shop.example.com/cb"),
            "hosts compare case-insensitively"
        );
        assert!(
            ok(
                "http://shop.example.com/",
                "https://shop.example.com/wp-admin/"
            ),
            "an https admin for an http shop"
        );
        assert!(
            ok("http://127.0.0.1:8080/", "http://127.0.0.1:8080/cb"),
            "plain http on this machine"
        );
        assert!(ok("http://localhost/", "http://localhost/cb"));
        assert!(
            ok("http://abc.onion/", "http://abc.onion/cb"),
            "plain http for an onion service"
        );

        assert!(
            !ok("https://shop.example.com/", "https://evil.example.com/cb"),
            "another host"
        );
        assert!(
            !ok(
                "https://shop.example.com/",
                "https://shop.example.com.evil.example/cb"
            ),
            "a lookalike host"
        );
        assert!(
            !ok(
                "https://shop.example.com/",
                "https://evil.example.com@shop.example.com/cb"
            ),
            "credentials in the address"
        );
        assert!(
            !ok("https://shop.example.com/", "http://shop.example.com/cb"),
            "plain http on the internet"
        );
        assert!(
            !ok("http://shop.example.com/", "http://shop.example.com/cb"),
            "plain http on the internet"
        );
        assert!(!ok("https://shop.example.com/", "javascript:alert(1)"));
        assert!(!ok("https://shop.example.com/", "/relative/path"));
        assert!(!ok("not a url", "https://shop.example.com/cb"));
    }

    /// A crafted link naming the merchant's real shop but a return address
    /// elsewhere: the confirm screen says why and offers no form.
    #[tokio::test]
    async fn a_connect_link_returning_to_another_site_shows_why_and_no_form() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "phished@example.com",
            "correct horse battery staple",
        )
        .await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fevil.example.com%2Fsteal&nonce=n")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(response).await;
        assert!(html.contains("a different website than the shop"), "{html}");
        assert!(
            !html.contains("name=\"view_key_hex\""),
            "no form to submit: {html}"
        );
    }

    /// Submitting the confirm form anyway (it was never shown, but a forged
    /// POST could carry it), in either mode, mints no token and redirects
    /// nowhere - an existing store's credentials included.
    #[tokio::test]
    async fn a_forged_confirm_to_another_site_sends_no_credentials_anywhere() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let cookie = signed_up_and_logged_in_session_cookie(
            &router,
            "forged@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _) = create_a_store(&router, &cookie, "https://shop.example.com").await;
        for fields in [
            vec![
                ("mode", "existing"),
                ("connection_id", connection_id.as_str()),
            ],
            vec![
                ("mode", "new"),
                ("view_key_hex", TEST_VIEW_KEY_HEX),
                ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ("network", "mainnet"),
                ("base_currency", "XMR"),
            ],
        ] {
            let mut fields = fields;
            fields.extend([
                ("site_url", "https://shop.example.com"),
                ("return_url", "https://evil.example.com/steal"),
                ("nonce", "n"),
            ]);
            let response = router
                .clone()
                .oneshot(form_request("/connect/woocommerce", Some(&cookie), &fields))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{fields:?}");
            assert!(response.headers().get("location").is_none());
            let html = body_text(response).await;
            assert!(html.contains("a different website than the shop"), "{html}");
        }
    }
}
