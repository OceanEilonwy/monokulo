//! The plugin connect flow: "OAuth-style one-click install" for any shop
//! plugin, written once for all of them. `platform` is a path parameter
//! naming the plugin, a slug of its own choosing (`crate::plugins`): monokulo
//! keeps no list of plugins, and the store itself has no platform. The
//! plugin may add `name=` to say what to call it ("PrestaShop").
//!
//! 1. `GET /plugins/connect/{platform}?site_url=...&return_url=...&nonce=...[&name=...]`
//!    - the plugin sends the merchant's browser here.
//! 2. No session yet: redirect to `/dashboard/login?next=<this same URL>`
//!    (`next` is checked to be a path on this site before it's followed,
//!    `dashboard::SafePath`); signing up or logging in comes back here.
//! 3. A session. A store is a host and no two stores share one
//!    (`crate::stores`), so the shop's host decides what happens:
//!    - one of the merchant's stores has it: a page to connect the plugin
//!      to that store, with one button;
//!    - another account's store has it: a page saying so, and no form;
//!    - no store has it: store setup (`/setup`, `http::setup`), with the
//!      plugin's request carried through, ending on a Done page whose
//!      button goes back to the plugin.
//! 4. `POST /plugins/connect/{platform}` (behind [`AuthedUser`]): a store of the
//!    merchant's whose site is the shop's; mints a single-use connect token
//!    and redirects to `return_url` with `token`/`nonce` appended (via the
//!    `url` crate, so `return_url`'s own query string is kept).
//! 5. `POST /plugins/connect/{platform}/finish` - deliberately *not* behind
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

use crate::now_unix;
use crate::plugins::{ConnectingPlugin, Plugin, PluginName, PluginSlug};
use crate::stores::Site;
use crate::views::connect::PluginRequestView;

use super::dashboard::redirect_302;
use super::AppState;
use super::AuthedUser;

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
    /// What the plugin calls itself, to show the merchant; optional.
    #[serde(default)]
    pub name: String,
}

/// Why a plugin's request can't be answered. Shown (`Display`) to the
/// merchant on the page that says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnectRefusal {
    /// The shop's address isn't a web address.
    BadSite,
    /// The return address isn't https (or loopback, or onion).
    BadReturn,
    /// The return address is on another website than the shop.
    OtherSite,
    /// This instance has no public address to give the plugin.
    NoPublicUri,
    /// The store posted isn't the merchant's, or isn't the shop's.
    NotTheShopsStore,
    /// The connect token couldn't be stored.
    Failed,
}

impl std::fmt::Display for ConnectRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadSite => "The shop's address isn't a valid web address.",
            Self::BadReturn => "The plugin's return address isn't a valid https address.",
            Self::OtherSite => "The plugin asked for this store's credentials to be sent to a different website than the shop's. Start connecting again from the shop's own settings page.",
            Self::NoPublicUri => "This Monokulo instance can't connect plugins yet: its operator hasn't set its public \
                 address (the public URI setting on the admin settings page). Plugins need it to create orders and send \
                 customers to the checkout. Ask the operator to set it, then try connecting again.",
            Self::NotTheShopsStore => "That store could not be found, or isn't the shop's.",
            Self::Failed => "Something went wrong. Please try again.",
        })
    }
}

/// Where a connecting plugin's store credentials may be sent: its
/// `return_url`, checked against the `site_url` the merchant is shown. Made
/// only by [`ConnectTarget::parse`], so the redirect carrying the connect
/// token can only go back to the site being connected - never to a
/// `return_url` a crafted link pointed somewhere else.
pub(super) struct ConnectTarget {
    pub(super) return_to: Url,
    /// The shop's site, as its store has it.
    pub(super) site: Site,
}

impl ConnectTarget {
    /// The return address, where the credentials travel, must be https
    /// (plain http only for a loopback or `.onion` host) and carry no
    /// credentials of its own; it must name the same host as the shop's
    /// address, which may itself be plain http (WordPress can serve its admin
    /// pages over https while the shop is on http).
    pub(super) fn parse(site_url: &str, return_url: &str) -> Result<Self, ConnectRefusal> {
        let site = Url::parse(site_url.trim())
            .ok()
            .filter(|site| matches!(site.scheme(), "http" | "https"))
            .ok_or(ConnectRefusal::BadSite)?;
        let store_site = Site::parse(site_url).map_err(|_| ConnectRefusal::BadSite)?;
        let return_to = web_url(return_url).ok_or(ConnectRefusal::BadReturn)?;
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
            return Err(ConnectRefusal::OtherSite);
        }
        Ok(ConnectTarget {
            return_to,
            site: store_site,
        })
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

/// This instance's public address, which is what a plugin is given as its
/// `endpoint`. Checked before any form is shown, again when the plugin's
/// key is minted, and in `/finish`, so a plugin is never handed a wrong
/// address.
async fn public_uri_for_plugins(state: &AppState) -> Result<String, ConnectRefusal> {
    state
        .settings
        .public_uri()
        .ok_or(ConnectRefusal::NoPublicUri)
}

/// Where a plugin's request may send the store's key, if it can be
/// answered: a return address that is the shop's, and a public address to
/// give the plugin.
pub(super) async fn check_request(
    state: &AppState,
    site_url: &str,
    return_url: &str,
) -> Result<ConnectTarget, ConnectRefusal> {
    let target = ConnectTarget::parse(site_url, return_url)?;
    public_uri_for_plugins(state).await?;
    Ok(target)
}

/// The plugin and its shop, when `next` is a plugin's connect page (the
/// merchant is logging in on the way to connecting a shop).
pub(crate) fn connecting_site(next: Option<&str>) -> Option<ConnectingPlugin> {
    let (slug, query) = next?.strip_prefix("/plugins/connect/")?.split_once('?')?;
    let field = |name: &str| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    Some(ConnectingPlugin {
        plugin: Plugin::parse(slug, &field("name").unwrap_or_default())?,
        site: Site::parse(&field("site_url")?).ok()?,
    })
}

/// Percent-encodes `s` for safe embedding as one query-string value - the
/// same encoding a browser's own `application/x-www-form-urlencoded`
/// submission uses.
fn encode_query_value(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// `GET /plugins/connect/{platform}` (steps 1-3). No valid session: redirect to
/// `/dashboard/login` carrying a `next` that reconstructs this exact URL.
/// A valid session: the store with the shop's host decides (see the module
/// doc comment). A path that can't name a plugin is not found.
pub async fn start(
    State(state): State<AppState>,
    Path(platform): Path<String>,
    Query(query): Query<ConnectQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(slug) = PluginSlug::parse(&platform) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((user, _)) = super::resolve_authed_user(&state, &headers).await else {
        let mut this_url = format!(
            "/plugins/connect/{}?site_url={}&return_url={}&nonce={}",
            platform,
            encode_query_value(&query.site_url),
            encode_query_value(&query.return_url),
            encode_query_value(&query.nonce),
        );
        if !query.name.is_empty() {
            this_url.push_str(&format!("&name={}", encode_query_value(&query.name)));
        }
        return redirect_302(&format!(
            "/dashboard/login?next={}",
            encode_query_value(&this_url)
        ));
    };
    let plugin = Plugin::new(slug, PluginName::clean(&query.name));
    let chrome =
        super::page_chrome(&state, Some(&user), format!("/plugins/connect/{platform}")).await;
    // A link that can't work says why first.
    let target = match check_request(&state, &query.site_url, &query.return_url).await {
        Ok(target) => target,
        Err(why) => {
            let host = Site::parse(&query.site_url).ok();
            return crate::views::connect::cannot_connect_page(
                &chrome,
                &plugin,
                host.as_ref(),
                &why.to_string(),
            )
            .into_response();
        }
    };
    let host = target.site;
    let site = host.clone();
    let store = state.db.read(move |db| db.find_store_by_site(&site)).await;
    match store {
        Ok(Some(store)) if store.user_id == user.id => {
            let request = PluginRequestView {
                plugin,
                site_url: query.site_url,
                return_url: query.return_url,
                nonce: query.nonce,
                host,
            };
            crate::views::connect::existing_store_page(
                &chrome,
                &request,
                store.id.as_str(),
                &store.name,
                crate::network::onion_uri(&state.settings, &state.tor).as_deref(),
            )
            .into_response()
        }
        Ok(Some(_)) => crate::views::connect::cannot_connect_page(
            &chrome,
            &plugin,
            Some(&host),
            &super::connections::SiteTaken::Someone.message(&host),
        )
        .into_response(),
        Ok(None) => {
            let mut setup = url::form_urlencoded::Serializer::new(String::new());
            setup
                .append_pair("plugin", plugin.slug().as_str())
                .append_pair("site_url", &query.site_url)
                .append_pair("return_url", &query.return_url)
                .append_pair("nonce", &query.nonce);
            if let Some(name) = plugin.name() {
                setup.append_pair("plugin_name", name.as_str());
            }
            redirect_302(&format!("/setup?{}", setup.finish()))
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `POST /plugins/connect/{platform}`'s fields: the plugin's request, carried
/// through, and the merchant's store whose site is the shop's.
#[derive(Deserialize)]
pub struct ConfirmForm {
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    #[serde(default)]
    pub connection_id: String,
    /// What the plugin calls itself, carried from its connect link.
    #[serde(default)]
    pub name: String,
}

/// `POST /plugins/connect/{platform}` (behind [`AuthedUser`], WBS 1.4.1 step 4):
/// gives the plugin its store's key. The store must be the merchant's, and
/// its site the shop's: without both, a signed-in attacker could have any
/// store's `sk_...` delivered to a `return_url` of their own via
/// `/plugins/connect/{platform}/finish`. A store that isn't theirs and one that
/// doesn't exist look the same.
pub async fn confirm_submit(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
    Path(platform): Path<String>,
    Form(form): Form<ConfirmForm>,
) -> Response {
    let Some(plugin) = Plugin::parse(&platform, &form.name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let chrome =
        super::page_chrome(&state, Some(&user), format!("/plugins/connect/{platform}")).await;
    let host = Site::parse(&form.site_url).ok();
    let refuse = |why: ConnectRefusal| {
        crate::views::connect::cannot_connect_page(
            &chrome,
            &plugin,
            host.as_ref(),
            &why.to_string(),
        )
        .into_response()
    };
    let target = match check_request(&state, &form.site_url, &form.return_url).await {
        Ok(target) => target,
        Err(why) => return refuse(why),
    };
    let (id, user_id) = (
        crate::db::ConnectionId::new(form.connection_id.as_str()),
        user.id.clone(),
    );
    let store = state
        .db
        .read(move |db| db.get_store_connection_by_id(&id))
        .await
        .ok()
        .flatten()
        .and_then(|row| super::OwnedStore::check(row, &user_id));
    let Some(store) = store.filter(|s| s.site.as_ref() == Some(&target.site)) else {
        return refuse(ConnectRefusal::NotTheShopsStore);
    };
    mint_token_and_redirect(&state, &store.id, &form.nonce, &target)
        .await
        .unwrap_or_else(|| refuse(ConnectRefusal::Failed))
}

/// Mints a single-use connect token for `connection_id` and redirects to
/// the plugin's `return_url` with `token`/`nonce` appended (parsed and
/// re-serialized via the `url` crate, so a `return_url` that already
/// carries its own query string is handled correctly). `None` when the
/// token couldn't be stored.
async fn mint_token_and_redirect(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    nonce: &str,
    target: &ConnectTarget,
) -> Option<Response> {
    let raw_token = shared::auth::generate_connect_token();
    let token_hash = raw_token.hash();
    let (id, stored_nonce) = (connection_id.clone(), nonce.to_owned());
    state
        .db
        .write(move |db| db.create_connect_token(&token_hash, &id, &stored_nonce, now_unix()))
        .await
        .ok()?;
    let mut redirect_url = target.return_to.clone();
    redirect_url
        .query_pairs_mut()
        .append_pair("token", raw_token.expose())
        .append_pair("nonce", nonce);
    Some(redirect_302(redirect_url.as_str()))
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
    /// What the plugin calls itself ("PrestaShop"), kept on the store's
    /// integration for the dashboard. Optional: without it the dashboard
    /// shows the plugin's slug (`crate::plugins::display_name`).
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Serialize)]
pub struct FinishResponse {
    pub public_key: String,
    pub secret_token: String,
    /// This instance's public address (`crate::settings::public_uri`): where
    /// the plugin creates orders (`POST {endpoint}/pay/{pk}/orders` with the
    /// secret key) and sends customers (`{endpoint}/pay/{pk}/orders/{id}`).
    /// Never the engine's address - the engine is private.
    pub endpoint: String,
    /// This instance's onion address, while it has one
    /// (`crate::network::onion_uri`): where the plugin sends a customer
    /// who is shopping on the shop's own onion address, so they never
    /// leave Tor. Orders are still created at `endpoint`, from the shop's
    /// server. Left out when there's none, so the wire shape is unchanged
    /// for an instance without Tor and a plugin that doesn't know it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onion_endpoint: Option<String>,
    /// Present only when `webhook_url` was supplied and registration succeeded.
    /// `skip_serializing_if` keeps the wire shape for a caller with no webhook
    /// exactly what it always was — a bare `{public_key, secret_token, endpoint}` —
    /// rather than growing a permanent `null` field for every existing caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_signing_secret: Option<String>,
}

/// `POST /plugins/connect/{platform}/finish` (WBS 1.4.1 step 5, extended by 1.4.4) -
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
///
/// Once it succeeds, the store has an active integration
/// (`store_integrations`): the plugin's slug and name, the store's site,
/// the plugin's version (its `Monokulo-Client` header, `<slug>/<version>`)
/// and the webhook it registered. While it's active the store's site is
/// locked; connecting again adds a new row.
pub async fn finish(
    State(state): State<AppState>,
    Path(platform): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<FinishRequest>,
) -> Response {
    let Some(slug) = PluginSlug::parse(&platform) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let endpoint = match public_uri_for_plugins(&state).await {
        Ok(url) => url,
        Err(why) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": why.to_string() })),
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
    // /plugins/connect/{platform}` to mint a fresh one. This was judged the safer default
    // over the alternative (return credentials anyway, with no webhook and no way to
    // signal that clearly in a shape existing callers already parse) rather than
    // because the collapse-to-401 pattern was merely convenient to reuse - flagged
    // here explicitly in case a real deployment prefers "credentials now, webhook
    // registration retried separately" instead.
    let (webhook_id, webhook_signing_secret) = match &req.webhook_url {
        Some(url) => match crate::webhooks::create(
            &state.db,
            &state.encryption_key,
            &row.id,
            url,
            &Default::default(),
        )
        .await
        {
            Ok(created) => (
                Some(created.webhook.id.to_string()),
                Some(created.signing_secret),
            ),
            Err(e) => {
                tracing::warn!(store.id = %row.id, error = %e, "the plugin's webhook could not be added");
                return StatusCode::UNAUTHORIZED.into_response();
            }
        },
        None => (None, None),
    };

    // Connecting while a plugin is still connected (this one again, or
    // another replacing it): the earlier connection's webhook goes, so the
    // shop isn't sent each event twice, and the earlier plugin is closed
    // below (`connect_integration`). A store has one plugin.
    let store_id = row.id.clone();
    let earlier = state
        .db
        .read(move |db| db.active_integration(&store_id))
        .await
        .ok()
        .flatten()
        .and_then(|i| i.webhook_id);
    if let Some(earlier) = earlier {
        let store_id = row.id.clone();
        let earlier = shared::ids::WebhookId::new(earlier);
        if let Err(e) = state
            .db
            .write(move |db| db.delete_webhook(&store_id, &earlier))
            .await
        {
            tracing::warn!(store.id = %row.id, error = %e, "the earlier connection's webhook could not be removed");
        }
    }

    let version = super::pay::client_version(&headers, slug.as_str()).unwrap_or_default();
    let name = req.name.as_deref().and_then(PluginName::clean);
    let (store_id, site, webhook_url) = (row.id.clone(), row.site.clone(), req.webhook_url.clone());
    let recorded = state
        .db
        .write(move |db| {
            // A plugin that gave no name (one built before plugins named
            // themselves) keeps the name it had on this store before.
            let name = match name {
                Some(name) => Some(name),
                None => db.last_integration_name(&store_id, &slug)?,
            };
            db.connect_integration(&crate::db::NewStoreIntegration {
                store_id: &store_id,
                plugin: &Plugin::new(slug, name),
                site: site.as_ref(),
                version: &version,
                webhook_id: webhook_id.as_deref(),
                webhook_url: webhook_url.as_deref(),
                at: now_unix(),
            })
        })
        .await;
    if let Err(e) = recorded {
        // The plugin has its key either way; the store just doesn't show
        // it as connected.
        tracing::error!(store.id = %row.id, error = %e, "a plugin connected but it couldn't be recorded");
    }

    Json(FinishResponse {
        public_key: row.tenant_public_key,
        secret_token: secret_token.expose().to_string(),
        endpoint,
        onion_endpoint: crate::network::onion_uri(&state.settings, &state.tor),
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
    use crate::http::test_support::{body_json, body_text, form_post as form_request};

    use super::super::{build_router, AppState};

    /// Same fixed-scalar construction `connections.rs`'s and
    /// `engine_client.rs`'s own tests use.
    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    /// This test instance's configured public address - what `/finish`
    /// must hand plugins as `endpoint`.
    const TEST_PUBLIC_URI: &str = "https://pay.example.test";
    const PASSWORD: &str = "correct horse battery staple";

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let engine_client = EngineClient::embedded_for_tests(engine.router());
        let state = AppState {
            engine: crate::http::Engine::new(engine_client),
            ..AppState::for_tests()
        }
        .with_options(&format!(
            "public_uri = \"{TEST_PUBLIC_URI}\"\n[signup]\nmode = \"public\"\n"
        ))
        .await;
        (state, engine)
    }

    async fn get(router: &Router, uri: &str, cookie: &str) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn location(response: &axum::response::Response) -> String {
        response.headers()["location"].to_str().unwrap().to_owned()
    }

    /// Signs up through the browser form and returns the `session=<value>`
    /// cookie, with a wallet brought in with the test keys, as a merchant
    /// would have before connecting a shop.
    async fn signed_up_and_logged_in_session_cookie(router: &Router, email: &str) -> String {
        let signup = router
            .clone()
            .oneshot(form_request(
                "/dashboard/signup",
                None,
                &[("email", email), ("password", PASSWORD)],
            ))
            .await
            .unwrap();
        assert_eq!(signup.status(), StatusCode::FOUND);
        let cookie = signup.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let added = router
            .clone()
            .oneshot(form_request(
                "/account/wallets/import",
                Some(&cookie),
                &[
                    ("name", "Shop takings"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(added.status(), StatusCode::SEE_OTHER, "the wallet is added");
        cookie
    }

    fn connect_uri(site_url: &str, return_url: &str, nonce: &str) -> String {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("site_url", site_url)
            .append_pair("return_url", return_url)
            .append_pair("nonce", nonce)
            .append_pair("name", PLUGIN_NAME)
            .finish();
        format!("/plugins/connect/acme-cart?{query}")
    }

    /// What the test plugin calls itself: monokulo knows no plugin, so a
    /// made-up one connects like any real one.
    const PLUGIN_NAME: &str = "Acme Cart";

    fn query_of(location: &str) -> Vec<(String, String)> {
        let query = location.split_once('?').map(|(_, q)| q).unwrap_or("");
        url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect()
    }

    /// The whole plugin flow for a shop with no store yet, as a browser
    /// without JavaScript walks it: the connect link sends the merchant to
    /// setup with the plugin's request; the store step (named "Shop", with
    /// `extra` fields such as the harness's `order_expiry_seconds`); the
    /// wallet already added; Done; and Done's button back to the plugin. The
    /// last response: a `302` to the plugin's `return_url` with its token.
    async fn connect_through_setup(
        router: &Router,
        cookie: &str,
        site_url: &str,
        return_url: &str,
        nonce: &str,
        extra: &[(&str, &str)],
    ) -> axum::response::Response {
        let start = get(router, &connect_uri(site_url, return_url, nonce), cookie).await;
        assert_eq!(
            start.status(),
            StatusCode::FOUND,
            "a shop with no store goes to setup"
        );
        let to_setup = location(&start);
        assert!(to_setup.starts_with("/setup?"), "{to_setup}");
        let mut draft = query_of(&to_setup);
        draft.push(("store_name".into(), "Shop".into()));
        draft.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let fields: Vec<(&str, &str)> = draft
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let store = router
            .clone()
            .oneshot(form_request("/setup", Some(cookie), &fields))
            .await
            .unwrap();
        assert_eq!(
            store.status(),
            StatusCode::SEE_OTHER,
            "{}",
            body_text(store).await
        );
        let wallet_step = location(&store);
        let html = body_text(get(router, &wallet_step, cookie).await).await;
        let wallet_id = html
            .split(r#"<option value=""#)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_else(|| panic!("the wallet already added is offered: {html}"))
            .to_owned();
        let mut existing: Vec<(String, String)> = query_of(&wallet_step);
        existing.push(("wallet_id".into(), wallet_id));
        let fields: Vec<(&str, &str)> = existing
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let made = router
            .clone()
            .oneshot(form_request(
                "/setup/wallet/existing",
                Some(cookie),
                &fields,
            ))
            .await
            .unwrap();
        let store_id = crate::http::test_support::store_made(&made);
        let done = body_text(get(router, &location(&made), cookie).await).await;
        assert!(done.contains("Back to Acme Cart"), "{done}");
        assert!(
            done.contains(r#"<li aria-current="step"><span>Done<small>Complete</small></span>"#)
        );
        router
            .clone()
            .oneshot(form_request(
                "/plugins/connect/acme-cart",
                Some(cookie),
                &[
                    ("connection_id", &store_id),
                    ("site_url", site_url),
                    ("return_url", return_url),
                    ("nonce", nonce),
                    ("name", PLUGIN_NAME),
                ],
            ))
            .await
            .unwrap()
    }

    fn parse_query_params(url: &str) -> std::collections::HashMap<String, String> {
        url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    async fn finish(router: &Router, body: serde_json::Value) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/plugins/connect/acme-cart/finish")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn full_round_trip_through_setup_then_redirect_then_finish_yields_real_working_credentials(
    ) {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "connect-flow@example.com").await;

        // The store step knows the plugin: the kind and the site are the
        // shop's, and the request rides along.
        let start = get(
            &router,
            &connect_uri(
                "https://shop.example.com",
                "https://shop.example.com/settings?page=monero",
                "nonce-xyz",
            ),
            &cookie,
        )
        .await;
        let html = body_text(get(&router, &location(&start), &cookie).await).await;
        assert!(html.contains("Connect your Acme Cart shop"), "{html}");
        assert!(
            html.contains(r#"name="store_site" id="store-site" value="shop.example.com""#),
            "{html}"
        );
        assert!(html.contains(r#"name="nonce" value="nonce-xyz""#));

        let post_response = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/settings?page=monero",
            "nonce-xyz",
            &[],
        )
        .await;
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let location = location(&post_response);
        // The existing `page=monero` query param survives alongside the
        // newly appended ones: the redirect is built by parsing and
        // re-serializing `return_url`, not by concatenating a `?`.
        let params = parse_query_params(&location);
        assert_eq!(params.get("page").map(String::as_str), Some("monero"));
        let token = params.get("token").expect("a token").clone();
        assert_eq!(params.get("nonce").map(String::as_str), Some("nonce-xyz"));

        // The store is the shop's: named as the merchant named it, its site
        // the shop's host, made by the plugin.
        let user = state
            .db
            .lock()
            .get_user_by_email("connect-flow@example.com")
            .unwrap()
            .unwrap();
        let stores = state
            .db
            .lock()
            .list_store_connections_for_user(&user.id)
            .unwrap();
        assert_eq!(stores.len(), 1);
        assert_eq!(stores[0].name, "Shop");
        assert_eq!(
            stores[0].site.as_ref().map(crate::stores::Site::as_str),
            Some("shop.example.com")
        );

        // Step 5: the token at /finish - server-to-server, no session.
        let finish_response = finish(&router, serde_json::json!({ "token": token })).await;
        assert_eq!(finish_response.status(), StatusCode::OK);
        let finish_body = body_json(finish_response).await;
        let obj = finish_body.as_object().unwrap();
        let public_key = obj.get("public_key").unwrap().as_str().unwrap();
        assert!(public_key.starts_with("pk_"));
        let secret_token = obj.get("secret_token").unwrap().as_str().unwrap();
        assert!(secret_token.starts_with("sk_"));
        let endpoint = obj.get("endpoint").unwrap().as_str().unwrap();
        assert_eq!(
            endpoint, TEST_PUBLIC_URI,
            "plugins get monokulo's public address"
        );
        assert_ne!(
            endpoint,
            format!("http://{}", engine.addr),
            "never the engine's"
        );
        assert!(!obj.contains_key("webhook_signing_secret"));

        // The secret token is the tenant's real, working credential.
        let engine_client = EngineClient::embedded_for_tests(engine.router());
        let tenant_view = engine_client
            .get_tenant(&shared::auth::RawToken::presented(secret_token))
            .await
            .expect("the tenant's genuine sk_ credential");
        assert_eq!(tenant_view.public_key, public_key);
    }

    #[tokio::test]
    async fn finishing_the_same_token_twice_only_succeeds_once() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "connect-single-use@example.com").await;
        let post_response = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/settings",
            "nonce-single-use",
            &[],
        )
        .await;
        let token = parse_query_params(&location(&post_response))["token"].clone();
        let first = finish(&router, serde_json::json!({ "token": token })).await;
        assert_eq!(
            first.status(),
            StatusCode::OK,
            "the first finish call succeeds"
        );
        let second = finish(&router, serde_json::json!({ "token": token })).await;
        assert_eq!(
            second.status(),
            StatusCode::UNAUTHORIZED,
            "a spent token fails"
        );
    }

    #[tokio::test]
    async fn finish_with_a_garbage_token_returns_unauthorized() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let response = finish(
            &router,
            serde_json::json!({ "token": "conn_nobody_ever_issued_this" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// WBS 1.4.4: a `webhook_url` supplied to `/finish` registers a webhook
    /// against the real engine. Also carries a harness's
    /// `order_expiry_seconds` through setup to the engine's tenant.
    #[tokio::test]
    async fn finish_with_a_webhook_url_registers_a_real_webhook_and_carries_the_signing_secret() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "webhook-register@example.com").await;
        let post_response = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/settings",
            "nonce-webhook",
            &[("order_expiry_seconds", "1")],
        )
        .await;
        let token = parse_query_params(&location(&post_response))["token"].clone();
        let finish_response = finish(
            &router,
            serde_json::json!({ "token": token, "webhook_url": "https://merchant.example/hook" }),
        )
        .await;
        assert_eq!(finish_response.status(), StatusCode::OK);
        let body = body_json(finish_response).await;
        let secret_token = body["secret_token"].as_str().unwrap().to_string();
        let signing_secret = body["webhook_signing_secret"].as_str().unwrap();
        assert!(signing_secret.starts_with("whsec_"));

        // Monokulo's own webhook, its secret the one the plugin was given,
        // kept encrypted; the integration names it.
        let public_key = body["public_key"].as_str().unwrap().to_string();
        let store = state
            .db
            .lock()
            .get_store_connection_by_public_key(&public_key)
            .unwrap()
            .unwrap();
        let webhooks = state.db.lock().list_webhooks(&store.id).unwrap();
        assert_eq!(webhooks.len(), 1);
        assert_eq!(webhooks[0].url, "https://merchant.example/hook");
        assert_eq!(
            crate::crypto::decrypt(
                &state.encryption_key,
                crate::crypto::Binding::WebhookSecret(webhooks[0].id.as_str()),
                &webhooks[0].signing_secret_encrypted,
            )
            .unwrap(),
            signing_secret
        );
        let integration = state
            .db
            .lock()
            .active_integration(&store.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            integration.webhook_id.as_deref(),
            Some(webhooks[0].id.as_str())
        );

        let engine_client = EngineClient::embedded_for_tests(engine.router());
        let tenant_view = engine_client
            .get_tenant(&shared::auth::RawToken::presented(&secret_token))
            .await
            .unwrap();
        assert_eq!(tenant_view.order_expiry_seconds, 1);
    }

    /// `/finish` with the plugin's `Monokulo-Client` header and a webhook.
    async fn finish_as_plugin(router: &Router, token: &str, version: &str) -> serde_json::Value {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/plugins/connect/acme-cart/finish")
                    .header("content-type", "application/json")
                    .header("monokulo-client", format!("acme-cart/{version}"))
                    .body(Body::from(
                        serde_json::json!({ "token": token, "webhook_url": "https://shop.example.com/?wc-api=monokulo", "name": PLUGIN_NAME })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await
    }

    /// A store has one shop, so one plugin: another plugin connecting
    /// closes the one before and takes the place of its webhook, and a
    /// plugin that gives no name (one built before plugins named
    /// themselves) keeps the name it had on the store.
    #[tokio::test]
    async fn another_plugin_replaces_the_connected_one_and_an_unnamed_one_keeps_its_name() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let cookie = signed_up_and_logged_in_session_cookie(&router, "replace@example.com").await;
        let back = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/settings",
            "nonce-1",
            &[],
        )
        .await;
        let token = parse_query_params(&location(&back))["token"].clone();
        let pk = finish_as_plugin(&router, &token, "0.4.0").await["public_key"]
            .as_str()
            .unwrap()
            .to_owned();
        let id = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap()
            .id;
        // Connects `slug` to the store again, through its confirm form and a
        // `/finish` that names no plugin.
        let connect = |slug: &'static str, nonce: &'static str| {
            let (router, cookie, id) = (router.clone(), cookie.clone(), id.to_string());
            async move {
                let confirmed = router
                    .clone()
                    .oneshot(form_request(
                        &format!("/plugins/connect/{slug}"),
                        Some(&cookie),
                        &[
                            ("site_url", "https://shop.example.com/"),
                            ("return_url", "https://shop.example.com/settings"),
                            ("nonce", nonce),
                            ("connection_id", &id),
                        ],
                    ))
                    .await
                    .unwrap();
                let token = parse_query_params(&location(&confirmed))["token"].clone();
                let finished = router
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(format!("/plugins/connect/{slug}/finish"))
                            .header("content-type", "application/json")
                            .header("monokulo-client", format!("{slug}/1.0.0"))
                            .body(Body::from(
                                serde_json::json!({ "token": token, "webhook_url": "https://shop.example.com/hook" })
                                    .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(finished.status(), StatusCode::OK);
            }
        };
        let active = || {
            let rows = state.db.lock().list_integrations(&id).unwrap();
            let active: Vec<(String, String)> = rows
                .iter()
                .filter(|r| r.disconnected_at.is_none())
                .map(|r| (r.kind.clone(), r.name.clone()))
                .collect();
            (active, state.db.lock().list_webhooks(&id).unwrap().len())
        };

        connect("other-cart", "nonce-2").await;
        assert_eq!(active(), (vec![("other-cart".into(), String::new())], 1));
        connect("acme-cart", "nonce-3").await;
        assert_eq!(
            active(),
            (vec![("acme-cart".into(), PLUGIN_NAME.into())], 1)
        );
    }

    /// A store's plugin, from connecting to disconnecting and back: the
    /// integration is recorded with its version and webhook and locks the
    /// site; its orders say when it was last seen; disconnecting waits while
    /// one of them can be paid, then removes its webhook and changes the
    /// store's key; connecting again adds a new active row.
    #[tokio::test]
    async fn a_plugin_connects_locks_the_site_and_is_disconnected_with_a_new_key() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let cookie = signed_up_and_logged_in_session_cookie(&router, "lifecycle@example.com").await;
        let back = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/settings",
            "nonce-life",
            &[],
        )
        .await;
        let token = parse_query_params(&location(&back))["token"].clone();
        let creds = finish_as_plugin(&router, &token, "0.4.0").await;
        let old_sk = shared::auth::RawToken::presented(creds["secret_token"].as_str().unwrap());
        let pk = creds["public_key"].as_str().unwrap().to_owned();
        let store = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap();
        let id = store.id.clone();
        let rows = state.db.lock().list_integrations(&id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (
                rows[0].kind.as_str(),
                rows[0].site.as_str(),
                rows[0].version.as_str()
            ),
            ("acme-cart", "shop.example.com", "0.4.0")
        );
        assert_eq!(rows[0].name, PLUGIN_NAME, "the plugin's own name is kept");
        assert!(rows[0].webhook_id.is_some() && rows[0].disconnected_at.is_none());
        assert_eq!(
            rows[0].webhook_url.as_deref(),
            Some("https://shop.example.com/?wc-api=monokulo")
        );

        // The site is locked; Connections lists the plugin.
        let settings = format!("/dashboard/stores/{id}/settings");
        let page = body_text(get(&router, &settings, &cookie).await).await;
        assert!(
            page.contains(&format!(
                r##"Set by Acme Cart · <a href="{settings}?tab=connections#card-connections">see Connections</a>"##
            )),
            "{page}"
        );
        assert!(!page.contains(r#"name="store_site""#), "{page}");
        let connections =
            body_text(get(&router, &format!("{settings}?tab=connections"), &cookie).await).await;
        assert!(
            connections.contains("shop.example.com · plugin 0.4.0 · connected "),
            "{connections}"
        );
        assert!(connections.contains("no orders yet"), "{connections}");
        // A site sent anyway changes nothing; the website page refuses.
        router
            .clone()
            .oneshot(form_request(
                &settings,
                Some(&cookie),
                &[("store_site", "other.example")],
            ))
            .await
            .unwrap();
        assert_eq!(
            state
                .db
                .lock()
                .get_store_connection_by_id(&id)
                .unwrap()
                .unwrap()
                .site
                .as_ref()
                .map(crate::stores::Site::as_str),
            Some("shop.example.com")
        );
        let website = format!("{settings}/website");
        let blocked =
            body_text(get(&router, &format!("{website}?site=other.example"), &cookie).await).await;
        assert!(
            blocked.contains("No plugin is connected to it")
                && blocked.contains("disconnect it first"),
            "{blocked}"
        );
        let refused = router
            .clone()
            .oneshot(form_request(
                &website,
                Some(&cookie),
                &[("site", "other.example"), ("confirm", "Shop")],
            ))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Nothing from the shop is open: disconnecting is ready, and says
        // its webhook goes.
        let disconnect = format!("{settings}/connections/{}/disconnect", rows[0].id);
        let ready = body_text(get(&router, &disconnect, &cookie).await).await;
        assert!(ready.contains("The plugin's webhook is removed<span class=\"fix\">https://shop.example.com/?wc-api=monokulo</span>"), "{ready}");
        let wrong = router
            .clone()
            .oneshot(form_request(
                &disconnect,
                Some(&cookie),
                &[("confirm", "shop")],
            ))
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let done = router
            .clone()
            .oneshot(form_request(
                &disconnect,
                Some(&cookie),
                &[("confirm", "Shop")],
            ))
            .await
            .unwrap();
        assert_eq!(done.status(), StatusCode::SEE_OTHER);
        assert!(location(&done).ends_with("?saved=connections#card-connections"));
        let toast =
            body_text(get(&router, &format!("{settings}?saved=connections"), &cookie).await).await;
        assert!(toast.contains("Plugin disconnected"), "{toast}");

        // The plugin's key no longer works; the store's new one does, and
        // the webhook is gone.
        let engine_client = EngineClient::embedded_for_tests(engine.router());
        assert!(
            engine_client.get_tenant(&old_sk).await.is_err(),
            "the old key is refused"
        );
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&id)
            .unwrap()
            .unwrap();
        let new_sk = super::super::orders::decrypt_sk(&state.encryption_key, &row).unwrap();
        assert_ne!(new_sk.expose(), old_sk.expose());
        assert!(state.db.lock().list_webhooks(&id).unwrap().is_empty());
        let rows = state.db.lock().list_integrations(&id).unwrap();
        assert!(rows[0].disconnected_at.is_some());
        // The site unlocks.
        let page = body_text(get(&router, &settings, &cookie).await).await;
        assert!(
            page.contains(r#"name="store_site" id="store-site" value="shop.example.com""#),
            "{page}"
        );
        let connections =
            body_text(get(&router, &format!("{settings}?tab=connections"), &cookie).await).await;
        assert!(connections.contains("<h4>Before</h4>"), "{connections}");

        // Connecting again: a new active row, with the new key.
        let again = router
            .clone()
            .oneshot(form_request(
                "/plugins/connect/acme-cart",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com/"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-again"),
                    ("connection_id", id.as_str()),
                ],
            ))
            .await
            .unwrap();
        let token = parse_query_params(&location(&again))["token"].clone();
        let creds = finish_as_plugin(&router, &token, "0.4.0").await;
        assert_eq!(creds["secret_token"].as_str().unwrap(), new_sk.expose());
        let rows = state.db.lock().list_integrations(&id).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].disconnected_at.is_none() && rows[1].disconnected_at.is_some());
        assert_eq!(state.db.lock().list_webhooks(&id).unwrap().len(), 1);
        // And again while connected: the earlier row closes and its webhook
        // goes, so there's still one.
        let once_more = router
            .clone()
            .oneshot(form_request(
                "/plugins/connect/acme-cart",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com/"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-once-more"),
                    ("connection_id", id.as_str()),
                ],
            ))
            .await
            .unwrap();
        let token = parse_query_params(&location(&once_more))["token"].clone();
        finish_as_plugin(&router, &token, "0.4.0").await;
        let rows = state.db.lock().list_integrations(&id).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().filter(|r| r.disconnected_at.is_none()).count(),
            1
        );
        assert_eq!(state.db.lock().list_webhooks(&id).unwrap().len(), 1);

        // An order from the plugin: seen now, at its new version; and while
        // it can be paid, disconnecting waits.
        let order = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", new_sk.expose()))
                    .header("monokulo-client", "acme-cart/0.4.1")
                    .body(Body::from(
                        serde_json::json!({ "amount": "1.00", "currency": "XMR" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(order.status(), StatusCode::OK);
        let rows = state.db.lock().list_integrations(&id).unwrap();
        assert_eq!(rows[0].version, "0.4.1");
        assert!(rows[0].last_seen_at.is_some());
        let disconnect = format!("{settings}/connections/{}/disconnect", rows[0].id);
        let blocked = body_text(get(&router, &disconnect, &cookie).await).await;
        assert!(blocked.contains("No order from shop.example.com can still be paid<span class=\"fix\">1 can be paid until about "), "{blocked}");
        let refused = router
            .clone()
            .oneshot(form_request(
                &disconnect,
                Some(&cookie),
                &[("confirm", "Shop")],
            ))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(state.db.lock().list_integrations(&id).unwrap()[0]
            .disconnected_at
            .is_none());
    }

    /// A `webhook_url` the engine rejects fails the whole `/finish` call
    /// (WBS 1.4.4's collapse-to-401 policy, see `finish`).
    #[tokio::test]
    async fn finish_with_a_rejected_webhook_url_fails_the_whole_call() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "webhook-reject@example.com").await;
        let post_response = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/settings",
            "nonce-webhook-reject",
            &[],
        )
        .await;
        let token = parse_query_params(&location(&post_response))["token"].clone();
        let response = finish(
            &router,
            serde_json::json!({ "token": token, "webhook_url": "ftp://not-http.example/hook" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_unauthenticated_connect_start_redirects_through_login_and_back_to_the_original_url()
    {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let original_uri = "/plugins/connect/acme-cart?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fsettings&nonce=detour-nonce";
        let get_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(original_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_response.status(), StatusCode::FOUND);
        let login_location = location(&get_response);
        let next_value = login_location
            .strip_prefix("/dashboard/login?next=")
            .expect("a next-carrying login redirect");
        let decoded_next = url::form_urlencoded::parse(format!("x={next_value}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(decoded_next, original_uri);

        // The login page says which shop wants to connect, and sign-up keeps
        // where they were going.
        let html = body_text(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&login_location)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert!(html.contains("shop.example.com wants to connect"), "{html}");
        assert!(html.contains("/dashboard/signup?next="), "{html}");

        // Signing up with `next` goes straight back to the connect link,
        // which sends a merchant with no store to setup.
        let signup = router
            .clone()
            .oneshot(form_request(
                "/dashboard/signup",
                None,
                &[
                    ("email", "connect-detour@example.com"),
                    ("password", PASSWORD),
                    ("next", &decoded_next),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(location(&signup), original_uri);
        let cookie = signup.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let start = get(&router, original_uri, &cookie).await;
        assert!(
            location(&start).starts_with("/setup?plugin=acme-cart&"),
            "{}",
            location(&start)
        );

        // Logging in with `next` lands back on the connect link too.
        let login_response = router
            .clone()
            .oneshot(form_request(
                "/dashboard/login",
                None,
                &[
                    ("email", "connect-detour@example.com"),
                    ("password", PASSWORD),
                    ("next", &decoded_next),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(location(&login_response), original_uri);
    }

    /// The store step from the plugin keeps the plugin's request when it
    /// refuses an answer.
    #[tokio::test]
    async fn a_refused_store_step_from_the_plugin_shows_why_and_keeps_the_plugins_request() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "keep-the-request@example.com").await;
        let response = router
            .oneshot(form_request(
                "/setup",
                Some(&cookie),
                &[
                    ("plugin", "acme-cart"),
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-abc"),
                    ("store_name", "  "),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("Give the store a name."), "{html}");
        assert!(html.contains(r#"name="nonce" value="nonce-abc""#));
        assert!(html.contains(r#"name="return_url" value="https://shop.example.com/settings""#));
    }

    /// Makes a website's store on the merchant's wallet, named after its
    /// site; its id.
    async fn create_a_store(router: &Router, cookie: &str, site: &str) -> String {
        let html = body_text(
            get(
                router,
                &format!("/setup/wallet?store_name=x&store_site={site}"),
                cookie,
            )
            .await,
        )
        .await;
        let wallet_id = html
            .split(r#"<option value=""#)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap()
            .to_owned();
        let response = crate::http::test_support::set_up_store_on_wallet(
            router, cookie, site, site, &wallet_id,
        )
        .await;
        crate::http::test_support::store_made(&response)
    }

    /// A shop whose site one of the merchant's stores already has connects
    /// to that store, with one button: no second store, the same key.
    #[tokio::test]
    async fn a_shop_your_store_already_has_connects_to_it_instead_of_making_another() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "reuse-existing@example.com").await;
        let connection_id = create_a_store(&router, &cookie, "shop.example.com").await;

        let html = body_text(
            get(
                &router,
                &connect_uri(
                    "https://shop.example.com/",
                    "https://shop.example.com/settings",
                    "nonce-existing",
                ),
                &cookie,
            )
            .await,
        )
        .await;
        assert!(html.contains("already uses that site"), "{html}");
        assert!(html.contains(&format!(r#"name="connection_id" value="{connection_id}""#)));

        let post_response = router
            .clone()
            .oneshot(form_request(
                "/plugins/connect/acme-cart",
                Some(&cookie),
                &[
                    ("site_url", "https://shop.example.com/"),
                    ("return_url", "https://shop.example.com/settings"),
                    ("nonce", "nonce-existing"),
                    ("connection_id", &connection_id),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::FOUND);
        let params = parse_query_params(&location(&post_response));
        let token = params.get("token").expect("a real connect token").clone();
        assert_eq!(
            params.get("nonce").map(String::as_str),
            Some("nonce-existing")
        );

        let user = state
            .db
            .lock()
            .get_user_by_email("reuse-existing@example.com")
            .unwrap()
            .unwrap();
        assert_eq!(
            state
                .db
                .lock()
                .list_store_connections_for_user(&user.id)
                .unwrap()
                .len(),
            1,
            "no second store"
        );
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.clone()))
            .unwrap()
            .unwrap();
        let finish_body =
            body_json(finish(&router, serde_json::json!({ "token": token })).await).await;
        assert_eq!(
            finish_body["public_key"].as_str().unwrap(),
            row.tenant_public_key
        );
    }

    /// Another account's store has the shop's site: the page says so, and
    /// offers nothing to submit.
    #[tokio::test]
    async fn a_shop_another_account_has_is_refused_with_no_form() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let owner = signed_up_and_logged_in_session_cookie(&router, "owner@example.com").await;
        create_a_store(&router, &owner, "shop.example.com").await;
        let other = signed_up_and_logged_in_session_cookie(&router, "other@example.com").await;
        let response = get(
            &router,
            &connect_uri(
                "https://shop.example.com",
                "https://shop.example.com/cb",
                "n",
            ),
            &other,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("shop.example.com is already connected to Monokulo here"),
            "{html}"
        );
        assert!(
            !html.contains(r#"action="/plugins/connect/acme-cart""#),
            "{html}"
        );
    }

    /// The real security boundary: a signed-in user must not be able to get
    /// someone else's store's key by sending its id, nor one of their own
    /// stores' keys to another site.
    #[tokio::test]
    async fn a_store_that_isnt_yours_or_isnt_the_shops_gets_no_token() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let victim = signed_up_and_logged_in_session_cookie(&router, "victim@example.com").await;
        let victims_store = create_a_store(&router, &victim, "victims-shop.example.com").await;
        let attacker =
            signed_up_and_logged_in_session_cookie(&router, "attacker@example.com").await;
        let attackers_store = create_a_store(&router, &attacker, "attacker-shop.example.com").await;
        for (store, site) in [
            (victims_store.as_str(), "https://victims-shop.example.com"),
            (
                attackers_store.as_str(),
                "https://attacker-site.example.com",
            ),
            ("", "https://attacker-shop.example.com"),
        ] {
            let return_url = format!("{site}/settings");
            let response = router
                .clone()
                .oneshot(form_request(
                    "/plugins/connect/acme-cart",
                    Some(&attacker),
                    &[
                        ("site_url", site),
                        ("return_url", &return_url),
                        ("nonce", "nonce-attack"),
                        ("connection_id", store),
                    ],
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{store} {site}");
            let html = body_text(response).await;
            assert!(html.contains("class=\"error\""), "{html}");
            assert!(!html.contains("token="), "never a token");
        }
    }

    /// While no public address is set, the connect link explains why it
    /// can't work (no form), setup refuses the plugin's request, and
    /// `/finish` answers `503` *without* spending the token - which then
    /// works once the operator sets the address.
    #[tokio::test]
    async fn plugins_cannot_connect_until_the_public_uri_is_set_and_are_told_why() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "no-public-uri@example.com").await;

        // Get a real token with the address set, then unset it.
        let submit = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/cb",
            "n1",
            &[],
        )
        .await;
        let token = parse_query_params(&location(&submit))["token"].clone();
        state.save_setting("public_uri", "").await;

        let start = get(
            &router,
            &connect_uri(
                "https://other.example.com",
                "https://other.example.com/cb",
                "n2",
            ),
            &cookie,
        )
        .await;
        let html = body_text(start).await;
        assert!(html.contains("hasn't set its public"), "got: {html}");
        assert!(
            !html.contains(r#"action="/plugins/connect/acme-cart""#),
            "no form, got: {html}"
        );
        let setup = router
            .clone()
            .oneshot(form_request(
                "/setup",
                Some(&cookie),
                &[
                    ("plugin", "acme-cart"),
                    ("site_url", "https://other.example.com"),
                    ("return_url", "https://other.example.com/cb"),
                    ("nonce", "n2"),
                    ("store_name", "Other"),
                ],
            ))
            .await
            .unwrap();
        assert!(body_text(setup).await.contains("hasn't set its public"));

        let response = finish(&router, serde_json::json!({ "token": token })).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(response).await;
        assert!(
            body["error"].as_str().unwrap().contains("public address"),
            "{body}"
        );

        state.save_setting("public_uri", TEST_PUBLIC_URI).await;
        let response = finish(&router, serde_json::json!({ "token": token })).await;
        assert_eq!(response.status(), StatusCode::OK, "the token was not spent");
        let body = body_json(response).await;
        assert_eq!(body["endpoint"], TEST_PUBLIC_URI);
        assert!(!body.to_string().contains(&engine.addr.to_string()));
        assert!(body.get("onion_endpoint").is_none(), "no Tor here: {body}");
    }

    /// With an onion address, the plugin is given it too, for customers who
    /// shop on the shop's own onion address; orders are still created at
    /// the web address.
    #[tokio::test]
    async fn a_plugin_is_given_the_onion_address_beside_the_web_one() {
        const ONION: &str = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
        let (state, _engine) = test_state_with_real_engine().await;
        let state = AppState {
            settings: state.settings.with_tor(crate::settings::TorConfig {
                mode: crate::settings::TorMode::External,
                onion_uri: Some(ONION.to_string()),
                ..crate::settings::TorConfig::defaults()
            }),
            ..state
        };
        let router = build_router(state);
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "onion-shop@example.com").await;
        let submit = connect_through_setup(
            &router,
            &cookie,
            "https://shop.example.com",
            "https://shop.example.com/cb",
            "n1",
            &[],
        )
        .await;
        let token = parse_query_params(&location(&submit))["token"].clone();
        let response = finish(&router, serde_json::json!({ "token": token })).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["endpoint"], TEST_PUBLIC_URI);
        assert_eq!(body["onion_endpoint"], ONION);
    }

    #[test]
    fn a_refused_target_says_which_rule_it_broke_and_a_good_one_names_the_site() {
        use super::{ConnectRefusal, ConnectTarget};
        let refused = |site: &str, back: &str| ConnectTarget::parse(site, back).err();
        assert_eq!(
            refused("not a url", "https://shop.example/cb"),
            Some(ConnectRefusal::BadSite)
        );
        assert_eq!(
            refused("https://shop.example/", "http://shop.example/cb"),
            Some(ConnectRefusal::BadReturn)
        );
        assert_eq!(
            refused("https://shop.example/", "https://evil.example/cb"),
            Some(ConnectRefusal::OtherSite)
        );
        let target =
            ConnectTarget::parse("https://Shop.Example/checkout", "https://shop.example/cb")
                .unwrap();
        assert_eq!(target.site.as_str(), "shop.example");
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
        assert!(!ok(
            "https://shop.example.com/",
            "http://shop.example.com/cb"
        ));
        assert!(!ok(
            "http://shop.example.com/",
            "http://shop.example.com/cb"
        ));
        assert!(!ok("https://shop.example.com/", "javascript:alert(1)"));
        assert!(!ok("https://shop.example.com/", "/relative/path"));
        assert!(!ok("not a url", "https://shop.example.com/cb"));
    }

    /// monokulo keeps no list of plugins: any slug a plugin picks reaches
    /// the flow, named as it names itself, and a path that can't be a slug
    /// is not found at every step.
    #[tokio::test]
    async fn any_plugin_connects_under_its_own_slug_and_only_slugs_are_found() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let cookie =
            signed_up_and_logged_in_session_cookie(&router, "any-plugin@example.com").await;
        let query = "site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fback&nonce=n1";
        let start = get(
            &router,
            &format!("/plugins/connect/never-heard-of-it?{query}&name=Never%20Heard"),
            &cookie,
        )
        .await;
        assert_eq!(start.status(), StatusCode::FOUND);
        let setup = location(&start);
        assert!(
            setup.starts_with("/setup?plugin=never-heard-of-it&"),
            "{setup}"
        );
        let html = body_text(get(&router, &setup, &cookie).await).await;
        assert!(
            html.contains("<h1>Connect your Never Heard shop</h1>"),
            "{html}"
        );
        // No name: the slug stands in.
        let start = get(&router, &format!("/plugins/connect/whmcs?{query}"), &cookie).await;
        let html = body_text(get(&router, &location(&start), &cookie).await).await;
        assert!(html.contains("<h1>Connect your Whmcs shop</h1>"), "{html}");

        for slug in ["Acme", "a", "1cart", "my_cart", "-cart"] {
            let start = get(
                &router,
                &format!("/plugins/connect/{slug}?{query}"),
                &cookie,
            )
            .await;
            assert_eq!(start.status(), StatusCode::NOT_FOUND, "{slug}");
            let confirm = router
                .clone()
                .oneshot(form_request(
                    &format!("/plugins/connect/{slug}"),
                    Some(&cookie),
                    &[
                        ("site_url", "https://shop.example.com"),
                        ("return_url", "https://shop.example.com/back"),
                        ("nonce", "n1"),
                    ],
                ))
                .await
                .unwrap();
            assert_eq!(confirm.status(), StatusCode::NOT_FOUND, "{slug}");
            let finish = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/plugins/connect/{slug}/finish"))
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"token":"x"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(finish.status(), StatusCode::NOT_FOUND, "{slug}");
        }
    }

    #[test]
    fn the_connecting_site_is_read_from_a_connect_link_only() {
        let next =
            "/plugins/connect/acme-cart?site_url=https%3A%2F%2Fshop.example.com%2F&return_url=x&nonce=n";
        let plugin_and_site = |next: &str| super::connecting_site(Some(next));
        let shown = |next: &str| {
            plugin_and_site(next).map(|from| (from.plugin.to_string(), from.site.to_string()))
        };
        assert_eq!(
            shown(next),
            Some(("Acme-cart".to_owned(), "shop.example.com".to_owned()))
        );
        // The name the plugin gave, when it gave one.
        assert_eq!(
            shown(&format!("{next}&name=Acme%20Cart")).map(|(name, _)| name),
            Some("Acme Cart".to_owned())
        );
        // A path that can't name a plugin is no connect link.
        assert_eq!(
            plugin_and_site("/plugins/connect/Acme?site_url=https%3A%2F%2Fshop.example.com"),
            None
        );
        assert_eq!(super::connecting_site(Some("/setup")), None);
        assert_eq!(super::connecting_site(None), None);
    }

    /// A crafted link naming the merchant's real shop but a return address
    /// elsewhere: the page says why and offers no form, and a forged post
    /// of the form mints no token.
    #[tokio::test]
    async fn a_connect_link_returning_to_another_site_sends_no_credentials_anywhere() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let cookie = signed_up_and_logged_in_session_cookie(&router, "phished@example.com").await;
        let connection_id = create_a_store(&router, &cookie, "shop.example.com").await;
        let html = body_text(
            get(
                &router,
                &connect_uri(
                    "https://shop.example.com",
                    "https://evil.example.com/steal",
                    "n",
                ),
                &cookie,
            )
            .await,
        )
        .await;
        assert!(html.contains("a different website than the shop"), "{html}");
        assert!(
            !html.contains(r#"action="/plugins/connect/acme-cart""#),
            "{html}"
        );

        let response = router
            .clone()
            .oneshot(form_request(
                "/plugins/connect/acme-cart",
                Some(&cookie),
                &[
                    ("connection_id", &connection_id),
                    ("site_url", "https://shop.example.com"),
                    ("return_url", "https://evil.example.com/steal"),
                    ("nonce", "n"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("location").is_none());
        assert!(body_text(response)
            .await
            .contains("a different website than the shop"));
    }
}
