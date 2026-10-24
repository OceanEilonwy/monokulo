//! Monokulo's HTTP surface: the merchant's and payer's pages, the connect
//! flows, and the router the machine API (`crate::api`) mounts on.
//!
//! Mirrors the engine's own `engine::http` module (see its doc
//! comment) in shape, not content: a `Clone`-able `AppState` carrying
//! shared, lock-guarded storage, and a `build_router(state) -> Router`
//! function so any caller — the real binary in `src/main.rs`, or this
//! module's own tests — can construct the exact same router. Tests drive it
//! through `tower::ServiceExt::oneshot` with no bound socket, the same
//! pattern as the engine's `src/http/tests.rs`; that's the right pattern
//! here specifically because these tests only ever need to exercise the
//! monokulo's own router in-process, unlike `engine-test-support`,
//! which exists because *other* crates need a real socket to reach a
//! separately-deployed *engine* instance.
//!
//! `/signup` and `/login` (`auth` module) are plain HTML forms over the account and login logic
//! (`signup`, `login`), ending in a `session` cookie, which the
//! [`AuthedUser`] extractor below resolves back to a user. The machine API
//! (`crate::api`) takes a store's or an app's key instead.
//!
//! `/setup` (`setup` module) makes a store in three steps (Store, Wallet,
//! Done) on top of `connections::create_connection_for_user`.
//!
//! `/plugins/connect/{platform}` and `/plugins/connect/{platform}/finish`
//! (`connect` module) are the generic, platform-agnostic "one-click install" flow
//! for any shop plugin, which names itself (`crate::plugins`): a plugin sends the merchant's
//! browser to `GET /plugins/connect/{platform}`, which redirects to
//! `/login` (carrying a validated `next`, see
//! `auth::login_submit`) if there's no session yet; with one, the
//! store with the shop's site gets the plugin, or setup makes one, and
//! the plugin's `return_url` gets a short-lived, single-use connect token
//! instead of a raw `sk_...`.
//! `POST /plugins/connect/{platform}/finish` is deliberately *not* behind
//! [`AuthedUser`] — it's called server-to-server by the plugin, which has no
//! monokulo session at all — and redeems that token exactly once.

pub mod abuse;
mod account;
mod admin_settings;
mod admin_setup;
pub(crate) mod apps;
mod auth;
mod checkout;
pub(crate) mod connect;
mod connections;
pub mod csrf;
pub mod embed_domains;
pub mod engine_page;
pub mod fx;
mod home;
mod invites;
pub mod key_entry;
mod login;
mod logs_page;
#[cfg(test)]
mod machine_tests;
mod orders;
pub mod paths;
mod pos;
mod pos_logs;
#[cfg(test)]
mod prefix_tests;
pub(crate) mod query_page;
mod setup;
mod signup;
pub mod status_page;
pub mod store_key;
mod store_settings;
mod store_site;
mod telemetry_client;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
#[cfg(test)]
mod tests;
mod wallet_service;
mod wallets;
mod webhooks;

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::{header, request::Parts, HeaderMap};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use axum_extra::extract::CookieJar;
use axum_extra::routing::RouterExt;

use crate::db::{Database, UserRow};
use crate::engine_client::EngineClient;
use shared::auth::{RawToken, TokenHash};

/// A store connection the signed-in user owns: made only by checking the
/// row's owner ([`OwnedStore::check`], which `orders::load_owned_connection`
/// uses), so a dashboard handler can't render or change a store it merely
/// looked up by id. Reads as the row it wraps.
pub struct OwnedStore(crate::db::StoreConnectionRow);

impl OwnedStore {
    /// `row`, if `user_id` owns it. A row that isn't theirs and one that
    /// doesn't exist must look the same to the caller (`None` for both).
    pub fn check(row: crate::db::StoreConnectionRow, user_id: &crate::db::UserId) -> Option<Self> {
        (&row.user_id == user_id).then_some(OwnedStore(row))
    }

    pub fn into_row(self) -> crate::db::StoreConnectionRow {
        self.0
    }
}

impl std::ops::Deref for OwnedStore {
    type Target = crate::db::StoreConnectionRow;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Name of the cookie the browser-facing login flow (`auth::login_submit`)
/// sets and [`AuthedUser`] reads back — a plain constant so the two sides
/// can't drift apart on the name.
pub(crate) const SESSION_COOKIE_NAME: &str = "session";

pub(crate) use orders::decrypt_sk;
pub(crate) use pos::resolve_confirmations_required;
pub(crate) use signup::MIN_PASSWORD_LEN;
pub(crate) use telemetry_client::clip as clip_report;

/// The session cookie as it is set and cleared: `HttpOnly` (never readable
/// from page JS), `SameSite=Lax` (sent on top-level navigation, not on
/// cross-site subrequests; `http::csrf` covers the rest), on the base path
/// (`crate::base`: `Path=/monokulo` behind a proxy serving Monokulo there,
/// so the rest of that host never sees it; `Path=/` at the root and over
/// Tor), and `Secure` when the visitor's connection is `https`
/// (`crate::network::OnNetwork::secure`), so a session is never sent back
/// over plain HTTP once it was made over TLS. Not `Secure` on a plain-HTTP
/// instance or an onion address, where it would never be sent at all.
pub(crate) fn session_cookie(
    secure: bool,
    value: String,
) -> axum_extra::extract::cookie::Cookie<'static> {
    use axum_extra::extract::cookie::{Cookie, SameSite};
    let base = crate::base::path();
    Cookie::build((SESSION_COOKIE_NAME, value))
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .path(if base.is_empty() {
            "/".to_owned()
        } else {
            base
        })
        .build()
}

/// The engine as monokulo talks to it.
#[derive(Clone)]
pub struct Engine {
    pub client: EngineClient,
    /// Short-TTL cache of the engine's own `GET /status` response, shared by
    /// every viewer - see `http::status_page`'s own module doc comment for
    /// why this exists (a real incident: the nav bar's status dot alone
    /// turned "one user browsing the dashboard" into enough engine requests
    /// to trip its own rate limiter).
    pub status_cache: status_page::StatusCache,
    /// The engine page's link to the engine's activity: one poller per
    /// watched network.
    pub activity: Arc<crate::engine_view::relay::Relay>,
    /// Bundles handed out with key entry forms, for `key-custody-cli`
    /// (`key_entry`).
    pub key_bundles: key_entry::KeyBundles,
}

impl Engine {
    /// The engine at `client`, with nothing cached yet.
    pub fn new(client: EngineClient) -> Self {
        Engine {
            activity: Arc::new(crate::engine_view::relay::Relay::new(client.clone())),
            client,
            status_cache: status_page::new_status_cache(),
            key_bundles: key_entry::KeyBundles::default(),
        }
    }
}

/// Everything the handlers share. A handler takes just the parts it uses
/// (`State<Database>`, `State<Engine>`, ...: see the `FromRef` derive), so
/// its signature says what it can touch.
#[derive(Clone, axum::extract::FromRef)]
pub struct AppState {
    /// The database (`db::Database`): `read` and `write` jobs.
    pub db: Database,
    /// The engine: its client and the cached `GET /status`.
    pub engine: Engine,
    /// AES-256-GCM key used to encrypt the engine's `sk_...`
    /// secret token before it's stored in `store_connections` — see
    /// `crate::crypto` and `http/connections.rs`. Sourced from an
    /// environment variable in the real binary (`main.rs`); tests just
    /// construct a fixed key directly.
    pub encryption_key: crate::crypto::AtRestKey,
    /// Fiat-to-XMR conversion for monokulo's own order creation - the
    /// engine has no concept of fiat, so monokulo computes the XMR amount
    /// itself before ever calling the engine. Dispatches per-request to whichever
    /// provider the *store* (not this instance globally) has chosen - see
    /// `exchange_rate_config::ExchangeRateProviders` and
    /// `db::StoreConnectionRow::fx_providers`.
    pub exchange_rate: Arc<crate::exchange_rate_config::ExchangeRateProviders>,
    /// Rate limits, the open-stream cap and (later) challenges, per client
    /// (`crate::abuse`, `http::abuse`).
    pub abuse: Arc<crate::abuse::AbuseProtection>,
    /// How verified embed domains are proved (`crate::embed_domains`): TXT
    /// lookups for a web domain, an onion shop's file fetched over Tor
    /// inside monokulo - the real ones in the binary, fakes in tests.
    pub proofs: crate::embed_domains::Proofs,
    /// monokulo's settings registry (admin_settings_v2.md part 1): saves from
    /// the admin page go through it and apply to the running process.
    pub settings: Arc<crate::settings::MonokuloSettings>,
    /// This process's log store, for the Logs page (structured_logging.md
    /// part 5). `None` in tests and when it couldn't be opened.
    pub log_store: Option<telemetry::store::LogStore>,
    /// Wakes the webhook delivery worker (`crate::webhooks`) when a
    /// merchant sends a delivery again.
    pub webhooks: Arc<crate::webhooks::Webhooks>,
    /// Sale events for connected apps' event streams (`crate::apps`),
    /// announced by the order API and the order-event subscriber.
    pub apps: Arc<crate::apps::AppEvents>,
    /// Tor inside monokulo (`crate::tor`): its status and onion address.
    pub tor: crate::tor::Tor,
}

impl AppState {
    /// The network a request with these extensions arrived on
    /// (`crate::network`), reading this state's settings.
    pub fn network(&self, extensions: &axum::http::Extensions) -> crate::network::Reached<'_> {
        self.network_of(crate::network::NetworkKind::of(extensions))
    }

    /// This instance's onion address (`http://….onion`, no trailing `/`)
    /// while it has one (`crate::network::onion_uri`), whichever network a
    /// request came on: what absolute onion links (`mk:onion-checkout`,
    /// `onion_api`) are built on.
    pub fn onion_uri(&self) -> Option<String> {
        crate::network::onion_uri(&self.settings, &self.tor)
    }

    /// The network `kind`, reading this state's settings.
    pub fn network_of(&self, kind: crate::network::NetworkKind) -> crate::network::Reached<'_> {
        use crate::network::{NetworkKind, Onion, Reached, Web};
        match kind {
            NetworkKind::Web => Reached::Web(Web {
                settings: &self.settings,
                tor: &self.tor,
                trusted_proxies: self.abuse.config().trusted_proxies,
            }),
            NetworkKind::Onion => Reached::Onion(Onion {
                settings: &self.settings,
                tor: &self.tor,
            }),
        }
    }
}

/// The encryption key [`AppState::for_tests`] uses.
#[cfg(any(test, feature = "test-support"))]
pub const TEST_ENCRYPTION_KEY: crate::crypto::AtRestKey = crate::crypto::AtRestKey::new([7u8; 32]);

#[cfg(any(test, feature = "test-support"))]
impl AppState {
    /// A state for tests: an in-memory db with the test admin seeded
    /// (`Db::seed_test_admin`), an engine that hangs up on every request
    /// (`shared::unreachable`, quick to fail on every system), a fixed
    /// encryption key, XMR-only exchange rates, default abuse limits and
    /// settings except that anyone may sign up ([`AppState::test_settings`]),
    /// no DNS and no log store. A test that needs something else
    /// overrides just that field with struct update syntax:
    /// `AppState { log_store, ..AppState::for_tests() }`.
    pub fn for_tests() -> Self {
        let db =
            crate::db::Db::open_in_memory().expect("opening an in-memory db for a test AppState");
        db.seed_test_admin();
        Self::for_tests_with_db(db.into_shared())
    }

    /// [`AppState::for_tests`] around a db the test prepared itself, so a
    /// test that needs its own db doesn't also open and seed one it throws
    /// away.
    pub fn for_tests_with_db(db: crate::db::SharedDb) -> Self {
        AppState {
            db: Database::inline(db),
            encryption_key: TEST_ENCRYPTION_KEY,
            exchange_rate: Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only()),
            abuse: Default::default(),
            proofs: crate::embed_domains::Proofs {
                dns: Arc::new(crate::embed_domains::UnavailableDns(
                    "DNS is not available in tests".to_string(),
                )),
                onion: Arc::new(crate::embed_domains::UnavailableTor(
                    "Tor is not available in tests".to_string(),
                )),
            },
            settings: Self::test_settings(None),
            log_store: None,
            webhooks: Arc::default(),
            apps: Arc::default(),
            tor: crate::tor::Tor::default(),
            engine: crate::http::Engine::new(EngineClient::for_tests(format!(
                "http://{}",
                shared::unreachable::address()
            ))),
        }
    }

    /// Settings for tests: no registry, and anyone may sign up (signup is
    /// invite-only by default, which would stop every test's ordinary
    /// signups; the invite tests ask for it themselves), with `public_uri`
    /// set or not.
    pub fn test_settings(public_uri: Option<&str>) -> Arc<crate::settings::MonokuloSettings> {
        crate::settings::MonokuloSettings::fixed(crate::settings::PerRequest {
            signup_mode: crate::settings::SignupMode::Public,
            public_uri: public_uri.unwrap_or_default().to_string(),
        })
    }
}

#[cfg(any(test, feature = "test-support"))]
impl AppState {
    /// This state with a real settings registry, over an options file in
    /// memory holding `options` (TOML), so a test can save settings as the
    /// admin page does ([`AppState::save_setting`]).
    pub async fn with_options(self, options: &str) -> Self {
        let settings = crate::settings::MonokuloSettings::load(
            self.db.clone(),
            self.engine.client.clone(),
            self.exchange_rate.clone(),
            self.abuse.clone(),
            None,
            crate::settings::test_secrets(),
            live_settings::OptionsFile::in_memory(options),
        )
        .await
        .expect("loading the test settings");
        AppState { settings, ..self }
    }

    /// Saves one setting through the registry, as the admin page does.
    pub async fn save_setting(&self, key: &str, value: &str) {
        self.settings
            .registry
            .as_ref()
            .expect("a state from `with_options`")
            .save(vec![(key.to_string(), Some(value.to_string()))])
            .await
            .expect("saving a test setting");
    }
}

pub fn build_router(state: AppState) -> Router {
    let router = Router::new()
        .route("/", axum::routing::get(home::home))
        .route(
            "/admin/setup",
            axum::routing::get(admin_setup::setup_form).post(admin_setup::setup_submit),
        )
        .route(
            "/admin/settings",
            axum::routing::get(admin_settings::page).post(admin_settings::save),
        )
        .route(
            "/admin/settings/reload",
            axum::routing::post(admin_settings::reload),
        )
        .route(
            "/admin/proof/{network}/reanchor",
            axum::routing::post(status_page::take_new_anchor),
        )
        .route(
            "/request-invite",
            axum::routing::get(invites::request_invite_form).post(invites::request_invite_submit),
        )
        .route("/admin/invites", axum::routing::get(invites::invites_page))
        .route(
            "/admin/invites/create-link",
            axum::routing::post(invites::create_invite_link),
        )
        .route(
            "/admin/invites/delete-all",
            axum::routing::post(invites::delete_all_invite_requests),
        )
        .route(
            "/admin/invites/{id}/delete",
            axum::routing::post(invites::delete_invite_request),
        )
        .route("/admin/logs", axum::routing::get(logs_page::page))
        .route("/admin/logs/tail", axum::routing::get(logs_page::tail))
        .route(
            "/admin/logs/syntax",
            axum::routing::get(logs_page::syntax_page),
        )
        .route("/admin/logs/export", axum::routing::get(logs_page::export))
        .route(
            "/admin/logs/trace/{trace_id}",
            axum::routing::get(logs_page::trace_page),
        )
        .route(
            "/admin/logs/row/{cursor}",
            axum::routing::get(logs_page::row_page),
        )
        .route(
            "/admin/logs/orders/{order_id}/pos",
            axum::routing::get(logs_page::pos_session_for_order),
        )
        .route(
            "/admin/logs/pos/{session}",
            axum::routing::get(logs_page::pos_timeline),
        )
        .route("/admin/logs/saved", post(logs_page::save_search))
        .route(
            "/admin/logs/saved/{id}/delete",
            post(logs_page::delete_search),
        )
        .route("/admin/query", axum::routing::get(query_page::page))
        .route(
            "/admin/query/schema",
            axum::routing::get(query_page::schema),
        )
        .route("/admin/query/value", axum::routing::get(query_page::value))
        .route(
            "/admin/query/export",
            axum::routing::get(query_page::export),
        )
        .route("/status", axum::routing::get(status_page::status_page))
        .route(
            "/status/summary",
            axum::routing::get(status_page::status_summary),
        )
        .route(
            "/status/events",
            axum::routing::get(status_page::status_events),
        )
        .route("/status/engine", axum::routing::get(engine_page::page))
        .route(
            "/status/engine/events",
            axum::routing::get(engine_page::events),
        )
        .route("/status/engine/at", axum::routing::get(engine_page::at))
        .route(
            "/status/engine/round",
            axum::routing::get(engine_page::round),
        )
        .route(
            "/status/engine/replay",
            axum::routing::get(engine_page::replay),
        )
        .route(
            "/signup",
            axum::routing::get(auth::signup_form).post(auth::signup_submit),
        )
        .route(
            "/login",
            axum::routing::get(auth::login_form).post(auth::login_submit),
        )
        .route("/logout", axum::routing::post(auth::logout_submit))
        .route("/theme", axum::routing::post(auth::theme_submit))
        .route(
            "/setup",
            axum::routing::get(setup::store_form).post(setup::store_submit),
        )
        .route("/setup/wallet", axum::routing::get(setup::wallet_form))
        .route("/setup/wallet/existing", post(setup::use_existing))
        .route(
            "/setup/wallet/keys",
            axum::routing::get(setup::keys_form).post(setup::keys_submit),
        )
        .route(
            "/setup/wallet/new",
            axum::routing::get(setup::new_form).post(setup::new_submit),
        )
        .route("/setup/done/{id}", axum::routing::get(setup::done))
        .route("/account", axum::routing::get(account::page))
        .route("/account/profile", post(account::save_profile))
        .route("/account/wallets/add", axum::routing::get(wallets::add))
        .route(
            "/account/wallets/name-check",
            axum::routing::get(wallets::name_check),
        )
        .route(
            "/account/wallets/import",
            axum::routing::get(wallets::import_form).post(wallets::import_submit),
        )
        .route(
            "/account/wallets/new",
            axum::routing::get(wallets::create_form).post(wallets::create_submit),
        )
        .route("/account/wallets/{id}", axum::routing::get(wallets::detail))
        .route(
            "/account/wallets/{id}/rename",
            axum::routing::post(wallets::rename),
        )
        .route(
            "/account/wallets/{id}/retire",
            axum::routing::get(wallets::retire_form).post(wallets::retire),
        )
        .route(
            "/account/wallets/{id}/restore",
            axum::routing::get(wallets::restore_form_page).post(wallets::restore),
        )
        .route("/stores/{id}", axum::routing::get(orders::store_detail))
        .route(
            "/stores/{id}/settings",
            axum::routing::get(orders::store_settings).post(store_settings::save),
        )
        .route(
            "/stores/{id}/settings/domains",
            post(embed_domains::add_domain),
        )
        .route(
            "/stores/{id}/settings/domains/new",
            axum::routing::get(embed_domains::add_form),
        )
        .route(
            "/stores/{id}/settings/domains/{domain_id}/record",
            axum::routing::get(embed_domains::record_page),
        )
        .route(
            "/stores/{id}/settings/domains/{domain_id}/check",
            axum::routing::get(embed_domains::check_page).post(embed_domains::check_domain),
        )
        .route(
            "/stores/{id}/settings/domains/{domain_id}/delete",
            post(embed_domains::delete_domain),
        )
        .route(
            "/stores/{id}/settings/embed-restriction",
            post(embed_domains::set_embed_restriction),
        )
        .route(
            "/stores/{id}/settings/embed-restriction/dismiss-warning",
            post(embed_domains::dismiss_embed_warning),
        )
        .route(
            "/stores/{id}/settings/key-custody",
            post(orders::move_key_storage),
        )
        .route(
            "/stores/{id}/settings/wallet",
            post(orders::change_store_wallet),
        )
        .route(
            "/stores/{id}/settings/website",
            axum::routing::get(store_site::website_form).post(store_site::website_submit),
        )
        .route(
            "/stores/{id}/settings/apps/connect",
            axum::routing::get(apps::connect_form).post(apps::connect_submit),
        )
        .route(
            "/stores/{id}/settings/apps/connect/{code_hash}",
            axum::routing::get(apps::connect_status),
        )
        .route(
            "/stores/{id}/settings/apps/{app_id}/disconnect",
            post(apps::disconnect_submit),
        )
        .route(
            "/stores/{id}/settings/plugins/{integration}/disconnect",
            axum::routing::get(store_site::disconnect_form).post(store_site::disconnect_submit),
        )
        .route(
            "/stores/{id}/orders/new",
            axum::routing::get(orders::create_order_page).post(orders::create_order),
        )
        .route(
            "/stores/{id}/settings/webhooks",
            axum::routing::post(webhooks::add),
        )
        .route(
            "/stores/{id}/settings/webhooks/{webhook_id}/delete",
            axum::routing::get(webhooks::delete_page).post(webhooks::delete),
        )
        .route(
            "/stores/{id}/settings/webhooks/{webhook_id}/retry-failed",
            axum::routing::post(webhooks::retry_failed),
        )
        .route(
            "/stores/{id}/settings/webhooks/{webhook_id}/deliveries",
            axum::routing::get(webhooks::all),
        )
        .route(
            "/stores/{id}/settings/webhooks/{webhook_id}/deliveries/{delivery_id}",
            axum::routing::get(webhooks::detail),
        )
        .route(
            "/stores/{id}/settings/webhooks/{webhook_id}/deliveries/{delivery_id}/send-again",
            axum::routing::post(webhooks::send_again),
        )
        .route("/stores/{id}/pos", axum::routing::get(pos::pos_page))
        .route(
            "/stores/{id}/pos/orders",
            axum::routing::get(pos::list_orders).post(pos::create_order),
        )
        .route(
            "/stores/{id}/pos/orders/{order_id}",
            axum::routing::get(pos::order_detail),
        )
        .route(
            "/stores/{id}/pos/orders/{order_id}/background",
            axum::routing::post(pos::background_order),
        )
        .route(
            "/stores/{id}/pos/orders/{order_id}/cancel",
            axum::routing::post(pos::cancel_order),
        )
        .route(
            "/stores/{id}/pos/orders/{order_id}/status",
            axum::routing::get(pos::order_status),
        )
        .route(
            "/stores/{id}/pos/events",
            axum::routing::get(pos::order_events),
        )
        .route(
            "/stores/{id}/pos/logs",
            post(pos_logs::receive).layer(axum::extract::DefaultBodyLimit::max(
                pos_logs::MAX_BODY_BYTES,
            )),
        )
        .route(
            "/stores/{id}/orders",
            axum::routing::get(orders::orders_list),
        )
        .route(
            "/stores/{id}/orders/lookup",
            axum::routing::post(orders::lookup_payment),
        )
        .route(
            crate::api::dashboard_order::Path::TEMPLATE,
            axum::routing::get(orders::order_detail),
        )
        .route(
            "/stores/{id}/orders/{order_id}/events",
            axum::routing::get(orders::order_detail_events),
        )
        .route(
            "/plugins/connect/{slug}",
            axum::routing::get(connect::start).post(connect::confirm_submit),
        )
        .route(connect::FINISH, post(connect::finish))
        .route(
            apps::CONNECT,
            axum::routing::get(apps::landing).post(apps::redeem),
        )
        .route(
            "/telemetry/client",
            post(telemetry_client::client_report).layer(axum::extract::DefaultBodyLimit::max(
                telemetry_client::MAX_BODY_BYTES,
            )),
        );

    // The machine API (`crate::api`): every resource's routes, from the
    // catalog its macros fill.
    let api = crate::api::runtime::router();
    // The payer's pages, keyed by the order alone, and the machine API:
    // the surfaces a shop, an app or a customer's browser reaches, under
    // the embed policy, the per-client budget and the embed CORS rules.
    let api_and_checkout = Router::new()
        .route(
            crate::api::checkout::Path::TEMPLATE,
            axum::routing::get(checkout::checkout_page),
        )
        .typed_get(checkout::checkout_events)
        .typed_post(checkout::set_refund_address)
        // A nav-bearing, shareable page wrapping the (nav-less) checkout page above in an
        // iframe - see `checkout::checkout_share_page`'s own doc comment.
        .typed_get(checkout::checkout_share_page)
        .merge(api)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            embed_domains::embed_policy_middleware,
        ))
        // Identifies the client (and a store's secret key), spends its
        // budget and challenges it past its soft limit before anything else
        // runs (`http::abuse`).
        .layer(middleware::from_fn_with_state(
            state.clone(),
            abuse::api_and_checkout_middleware,
        ))
        // Outside the rate limit, so a preflight never spends budget and a
        // `429` still carries the headers a cross-origin caller needs to read it.
        .layer(embed_cors_layer(&state))
        // A checkout page, its status and its share page carry a payment
        // address and amount, and (once given) a refund address: never
        // kept by a browser or a shared cache.
        .layer(middleware::map_response(
            |mut response: Response| async move {
                response
                    .headers_mut()
                    .entry(header::CACHE_CONTROL)
                    .or_insert(axum::http::HeaderValue::from_static("no-store"));
                response
            },
        ));

    // Every other route (the merchant's pages, login, sign-up, the landing and
    // status pages) is counted too - see `http::abuse::site_middleware`.
    // Static files are added below, outside it, and never counted. A
    // state-changing request to any of them that a browser says came from
    // another site is refused first (`http::csrf`).
    let router = router
        // Baseline headers for the site's own pages: no MIME sniffing. The
        // Referer policy depends on the network (`network_headers`), and
        // the checkout pages set their own framing policy.
        .layer(middleware::map_response(
            |mut response: Response| async move {
                response
                    .headers_mut()
                    .entry(header::X_CONTENT_TYPE_OPTIONS)
                    .or_insert(axum::http::HeaderValue::from_static("nosniff"));
                response
            },
        ))
        .layer(middleware::from_fn(csrf::same_origin_middleware))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            abuse::site_middleware,
        ));

    let router = router.merge(api_and_checkout);

    // A plain static file, not state-changing - no rate limiter needed.
    let router = router.route(
        "/static/monokulo-client.js",
        axum::routing::get(crate::assets::client_library).layer(any_origin_cors_layer()),
    );
    // Every other file baked into the binary (`crate::assets`): scripts and
    // styles at URLs carrying their digest, fonts, marks and modules by name.
    let router = router.route("/static/{*file}", axum::routing::get(crate::assets::serve));
    let router = router.route(
        "/key-custody/bundles/{id}",
        axum::routing::get(key_entry::bundle),
    );

    // Test-only route exercising `AuthedUser` - see its doc comment.
    // Compiled only under `#[cfg(test)]`, so it never exists in the real
    // binary; nothing outside this crate's own tests should ever reach it.
    #[cfg(test)]
    let router = router.route("/_test/whoami", axum::routing::get(test_whoami));

    // Outermost, so every line from the middleware above (abuse, embed
    // policy) carries the request's span (structured_logging.md 2.1), with
    // the store it concerns recorded first.
    let router = router
        .layer(middleware::from_fn_with_state(
            state.clone(),
            network_headers,
        ))
        // The base path every URL this request gives starts with
        // (`crate::base`), kept for everything above and the response body.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::base::layer,
        ))
        .layer(middleware::from_fn(record_store))
        .layer(middleware::from_fn(telemetry::http::server))
        // Outermost: whatever the layers above added, each list header
        // goes out once.
        .layer(middleware::map_response(
            |mut response: Response| async move {
                one_of_each_list_header(response.headers_mut());
                response
            },
        ));
    // Under test, every answer of a route in the OpenAPI document (the
    // API, the handshakes) is checked against it as the client gets it,
    // the middleware's problems too (`api::runtime::examples`).
    #[cfg(test)]
    let router = router.layer(middleware::from_fn(
        crate::api::runtime::examples::check_layer,
    ));
    router.with_state(state)
}

/// The headers whose values are lists, which some layers append to
/// rather than set (the CORS layer adds `Vary: origin, …`, the API
/// `Vary: Authorization`).
const LIST_HEADERS: [header::HeaderName; 2] = [header::VARY, header::CACHE_CONTROL];

/// Adds `token` to the one `Vary` header `headers` has (made when there's
/// none), never as a second `Vary`: a client that keeps only the last
/// header of each name would lose the first.
pub fn add_vary(headers: &mut axum::http::HeaderMap, token: &'static str) {
    headers.append(header::VARY, axum::http::HeaderValue::from_static(token));
    one_of_each_list_header(headers);
}

/// Every header name `headers` has more than once, but `Set-Cookie` (one
/// per cookie, by design): none, in every answer Monokulo gives.
pub fn repeated_headers(headers: &axum::http::HeaderMap) -> Vec<header::HeaderName> {
    headers
        .keys()
        .filter(|name| **name != header::SET_COOKIE && headers.get_all(*name).iter().count() > 1)
        .cloned()
        .collect()
}

/// Merges each [`LIST_HEADERS`] header that appears more than once into
/// one, its entries comma-separated and each once (a `Vary` naming `*`
/// is just `*`).
pub fn one_of_each_list_header(headers: &mut axum::http::HeaderMap) {
    for name in &LIST_HEADERS {
        if headers.get_all(name).iter().count() < 2 {
            continue;
        }
        let mut entries: Vec<String> = Vec::new();
        for value in headers.get_all(name) {
            for entry in value.to_str().unwrap_or_default().split(',') {
                let entry = entry.trim();
                if !entry.is_empty() && !entries.iter().any(|e| e.eq_ignore_ascii_case(entry)) {
                    entries.push(entry.to_owned());
                }
            }
        }
        if *name == header::VARY && entries.iter().any(|e| e == "*") {
            entries = vec!["*".to_owned()];
        }
        match axum::http::HeaderValue::from_str(&entries.join(", ")) {
            Ok(value) => {
                headers.insert(name, value);
            }
            Err(e) => {
                tracing::error!(header = %name, error = %e, "a list header couldn't be merged")
            }
        }
    }
}

/// Adds the headers every response carries on the network its request
/// arrived on (`crate::network::Network::response_headers`): the Referer
/// policy, and on the web address a page's `Onion-Location`.
async fn network_headers(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let kind = crate::network::NetworkKind::of(request.extensions());
    let target = request
        .uri()
        .path_and_query()
        .map_or("/", |target| target.as_str())
        .to_string();
    let mut response = next.run(request).await;
    state
        .network_of(kind)
        .response_headers(&target, response.headers_mut());
    response
}

/// Resolves the `session` cookie (set by `auth::login_submit`) to the user that session belongs to. `401` for a missing cookie
/// or an unknown/invalid token - never distinguishes the two. A browser
/// loading a page is sent to log in instead of getting the `401`
/// ([`unauthenticated`]). The machine API takes keys, not sessions
/// (`crate::api`).
///
/// Also carries the presented session's `token_hash` (the same hash
/// `Db::find_session`/`Db::delete_session` key on) alongside the resolved
/// user - logging out (`auth::logout_submit`) needs to know exactly
/// which session row to delete.
pub struct AuthedUser(pub UserRow, pub TokenHash);

/// The session `abuse::site_middleware` already resolved for this request,
/// so [`AuthedUser`] doesn't look it up a second time.
#[derive(Clone)]
pub(crate) struct ResolvedSession(pub UserRow, pub TokenHash);

impl FromRequestParts<AppState> for AuthedUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(ResolvedSession(user, hash)) = parts.extensions.get::<ResolvedSession>() {
            return Ok(AuthedUser(user.clone(), hash.clone()));
        }
        match resolve_authed_user(state, &parts.headers).await {
            Some((user, hash)) => Ok(AuthedUser(user, hash)),
            None => Err(unauthenticated(parts)),
        }
    }
}

/// What a request without a valid session gets: a browser loading a page
/// is sent to log in, coming back to the page after (`next`); anything
/// else (a script's fetch, a form post) a plain `401`.
fn unauthenticated(parts: &Parts) -> Response {
    let headers = &parts.headers;
    let page = matches!(
        parts.method,
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && !headers.contains_key(fx::FX_REQUEST)
        && headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"));
    if !page {
        return ApiError::Unauthorized.into_response();
    }
    let here = parts.uri.path_and_query().map_or("/", |p| p.as_str());
    crate::base::redirect(format!(
        "/login?next={}",
        url::form_urlencoded::byte_serialize(here.as_bytes()).collect::<String>()
    ))
}

/// Same credential resolution as [`AuthedUser`], plus a real
/// `user.is_admin` check - the gate for the admin settings page
/// (`http/admin_settings.rs`): the route itself is admin-only, not just
/// the nav link to it. A missing/invalid session still rejects
/// with the same `401` `AuthedUser` would; a *valid* session that just isn't
/// the admin account gets `403`, not `401` - see [`ApiError::Forbidden`]'s
/// own doc comment for why those are kept distinct.
pub struct AuthedAdmin(pub UserRow, pub TokenHash);

impl FromRequestParts<AppState> for AuthedAdmin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let AuthedUser(user, hash) = AuthedUser::from_request_parts(parts, state).await?;
        if !user.is_admin {
            return Err(ApiError::Forbidden.into_response());
        }
        Ok(AuthedAdmin(user, hash))
    }
}

/// CORS for the public routes a merchant's page uses to embed checkout
/// (order creation from `monokulo-client.js` with the store's public key,
/// the order and the checkout's live stream). A store that hasn't
/// restricted embedding answers any origin - a clearnet shop, a `.onion`
/// one, a sandboxed frame whose origin is `null` - and one that has
/// answers only its verified domains
/// (`embed_domains::EmbedPolicy::allows_origin`). The store is the one the
/// request's public key names, or its checkout's order's; a preflight
/// carries neither (no `Authorization` value), and is answered for any
/// origin, the request itself then being checked. Safe to open this wide by
/// default because none of these routes use cookies or any other ambient
/// credential (credentials are never allowed), and anything they do is
/// equally possible from a script outside a browser. Private-network
/// preflights are answered too, so a public page can embed a monokulo that
/// runs on a local network address.
fn embed_cors_layer(state: &AppState) -> tower_http::cors::CorsLayer {
    use tower_http::cors::AllowOrigin;
    let db = state.db.clone();
    cors_layer_base().allow_origin(AllowOrigin::async_predicate(move |origin, parts| {
        let db = db.clone();
        // The store's public key, when the request carries one; the order,
        // on a checkout page.
        let public_key = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once(' '))
            .map(|(_, key)| key.trim().to_owned())
            .filter(|key| key.starts_with("pk_"));
        let order = paths::checkout_order(parts.uri.path());
        async move {
            let public_key = match (public_key, order) {
                (Some(public_key), _) => public_key,
                (None, Some(order)) => {
                    match db
                        .read(move |db| db.get_store_connection_by_order(&order))
                        .await
                    {
                        Ok(Some(store)) => store.tenant_public_key,
                        Ok(None) => return true,
                        // Not knowing is not permission.
                        Err(_) => return false,
                    }
                }
                (None, None) => return true,
            };
            let Ok(origin) = origin.to_str() else {
                return false;
            };
            match crate::embed_domains::policy_for_public_key(&db, &public_key).await {
                Ok(Some(policy)) => policy.allows_origin(origin, crate::now_unix()),
                Ok(None) => true,
                // Not knowing is not permission.
                Err(_) => false,
            }
        }
    }))
}

/// CORS for the client library itself, for a page that loads it with
/// `crossorigin` or as a module: any origin, since loading the library
/// alone does nothing for any store.
fn any_origin_cors_layer() -> tower_http::cors::CorsLayer {
    cors_layer_base().allow_origin(tower_http::cors::Any)
}

fn cors_layer_base() -> tower_http::cors::CorsLayer {
    tower_http::cors::CorsLayer::new()
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        // `Monokulo-Proof` carries a solved challenge; `Monokulo-Challenge`
        // and `Retry-After` must be readable by the embed library on another
        // site (`http::abuse`).
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            header::HeaderName::from_static(abuse::PROOF_HEADER),
        ])
        .expose_headers([
            header::HeaderName::from_static(abuse::CHALLENGE_HEADER),
            header::RETRY_AFTER,
        ])
        .allow_private_network(true)
        .max_age(std::time::Duration::from_secs(24 * 60 * 60))
}

/// Page chrome for a page with the site nav (or another status
/// indicator): `views::PageChrome::from_user` plus the engine's last known
/// health (`status_page::known_health`).
pub(crate) async fn page_chrome(
    state: &AppState,
    user: Option<&crate::db::UserRow>,
    current_path: impl Into<String>,
) -> crate::views::PageChrome {
    let current_path = current_path.into();
    let unserved = status_page::known_unserved(&state.engine);
    // One read for all three: the user's stores (only when some store
    // can't be scanned), how many wallets they have (the account menu's
    // count), and whether this page's store reports browser logs.
    let user_id = user.map(|user| user.id.clone());
    let any_unserved = !unserved.is_empty();
    let store = store_of_path(&current_path).map(str::to_string);
    let (stores, wallet_count, browser_reports) = state
        .db
        .read(move |db| {
            let stores = match &user_id {
                Some(user_id) if any_unserved => db
                    .list_store_connections_for_user(user_id)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let wallets = match &user_id {
                Some(user_id) => db.count_wallets(user_id).unwrap_or(0),
                None => 0,
            };
            let reports = store.map(|store| {
                db.client_logging(&crate::db::ConnectionId::new(store))
                    .unwrap_or(false)
            });
            Ok::<_, crate::db::DbError>((stores, wallets, reports))
        })
        .await
        .unwrap_or_default();
    let mut alerts = store_alerts(&unserved, &stores);
    // An operator also hears about a block that is taking too long.
    if user.is_some_and(|user| user.is_admin) {
        alerts.extend(status_page::known_slow_blocks(&state.engine));
        alerts.extend(status_page::known_snp_policy_alert(state));
    }
    let mut chrome = crate::views::PageChrome::from_user(user, current_path)
        .with_health(status_page::known_health(&state.engine))
        .with_alerts(alerts);
    chrome.wallet_count = wallet_count;
    chrome.query_enabled = user.is_some_and(|user| user.is_admin) && state.settings.query_enabled();
    // Only a store's own pages depend on its opt-in; every other page keeps
    // `from_user`'s choice.
    if let Some(reports) = browser_reports {
        chrome.browser_reports = reports;
    }
    chrome
}

/// The store (connection id) a `/stores/{id}/...` path is about.
pub(crate) fn store_of_path(path: &str) -> Option<&str> {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut segments = path.trim_start_matches('/').split('/');
    match (segments.next(), segments.next()) {
        (Some("stores"), Some(id)) if !id.is_empty() => Some(id),
        _ => None,
    }
}

/// One alert per store of `user` that the engine can't scan right now.
/// Only ever the signed-in owner's own stores.
fn store_alerts(
    unserved: &[crate::engine_client::UnservedTenant],
    stores: &[crate::db::StoreConnectionRow],
) -> Vec<String> {
    let mut alerts = Vec::new();
    for store in stores {
        let name = &store.name;
        for problem in unserved
            .iter()
            .filter(|u| u.public_key == store.tenant_public_key)
        {
            alerts.push(match problem.reason.as_str() {
                "catching_up" => format!(
                    "{name}: payments are being checked late - the engine is catching up {} block(s) it couldn't check for this store. They'll show up once it has.",
                    problem.blocks_behind.unwrap_or(0)
                ),
                "custody_disabled" => format!(
                    "{name}: payments aren't being detected - this store's keys are in a key storage backend the instance admin has turned off. Move the store to another one under Key storage in its settings, or ask the admin to turn it back on."
                ),
                "custody_unavailable" => format!(
                    "{name}: payments aren't being detected - the key storage backend holding this store's keys isn't answering. The instance admin needs to check it; payments are found once it's back."
                ),
                _ => format!(
                    "{name}: payments aren't being detected - the {} network has no Monero node that answers. The instance admin needs to set one on the admin settings page.",
                    problem.network
                ),
            });
        }
    }
    alerts
}

/// The actual "resolve a session to its user" logic [`AuthedUser`]'s
/// extractor uses - factored out so a handler that needs to know *whether*
/// the caller has a valid session, without failing the request outright when
/// they don't, can reuse the exact same header/cookie parsing and lookup
/// instead of a parallel reimplementation. `GET /plugins/connect/{platform}`
/// (`http/connect.rs`) is one such caller: an unauthenticated
/// request there is a normal, expected case handled with a redirect to
/// `/login`, not a bare `401` - genuinely different handling from
/// [`AuthedUser`]'s own rejection, but it must resolve a *valid* session
/// identically, or the two code paths could quietly drift apart on what
/// counts as "logged in."
///
/// `None` covers every reason a session doesn't resolve (missing cookie,
/// unknown/invalid token, a database error looking
/// either up) - never distinguished further, same as [`AuthedUser`] itself.
pub(crate) async fn resolve_authed_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<(UserRow, TokenHash)> {
    let jar = CookieJar::from_headers(headers);
    let token = RawToken::presented(jar.get(SESSION_COOKIE_NAME)?.value());
    let token_hash = token.hash();

    let hash = token_hash.clone();
    let mut user = state
        .db
        .read(move |db| {
            let Some(session) = db.find_session(&hash, crate::now_unix())? else {
                return Ok(None);
            };
            db.get_user_by_id(&session.user_id)
        })
        .await
        .ok()
        .flatten()?;
    user.browser_timezone = fx::browser_zone(headers);
    record_identity(user.id.as_str(), &token_hash);
    Some((user, token_hash))
}

/// Puts who's signed in on the request's lines (`user.id`, `session.id`;
/// `telemetry::http::server`), so the Logs page can show every line of one
/// session. Visitors who aren't signed in have neither.
pub(crate) fn record_identity(user_id: &str, token_hash: &TokenHash) {
    tracing::Span::current()
        .record("user.id", user_id)
        .record("session.id", session_log_id(token_hash));
}

/// A session's name in the logs: stable for the session, and no use for
/// finding or presenting it (it is derived from the stored hash, which is
/// itself only a lookup key, and cut short).
pub(crate) fn session_log_id(token_hash: &TokenHash) -> String {
    use sha2::{Digest, Sha256};
    let token_hash = token_hash.as_str();
    hex::encode(&Sha256::digest(format!("monokulo log session:{token_hash}").as_bytes())[..8])
}

/// Puts the store a dashboard store page is about on its lines
/// (`store.id`), first, so the middleware's lines carry it too. The public
/// routes' store is recorded once `abuse::api_and_checkout_middleware` has found it.
async fn record_store(request: axum::extract::Request, next: middleware::Next) -> Response {
    if let Some(store) = request
        .uri()
        .path()
        .strip_prefix("/stores/")
        .and_then(|rest| rest.split('/').next())
        .filter(|id| !id.is_empty())
    {
        tracing::Span::current().record("store.id", store);
    }
    next.run(request).await
}

/// Test-only route behind [`AuthedUser`] and nothing else, so the session
/// tests exercise the extractor alone. Not a real API surface.
#[cfg(test)]
async fn test_whoami(AuthedUser(user, _): AuthedUser) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "user_id": user.id }))
}

/// `Unauthorized` (a missing/invalid session), `BadRequest` (a request
/// the *caller* got wrong in some caller-visible way), or `Internal` for
/// anything else. Every message on `Unauthorized`/`Internal` is fixed and
/// generic — `Internal` never describes *why* the underlying operation
/// failed — so a client can't use error-message differences to
/// fingerprint internals. Each is answered as a problem
/// (`crate::api::runtime::problem`, which converts these).
/// `BadRequest` is the one variant that *does* carry a real message,
/// deliberately: it's surfacing the engine's own validation error back to
/// the caller who made the mistake, not leaking internal state.
pub enum ApiError {
    Unauthorized,
    BadRequest(String),
    /// Missing, or not the caller's: a plain `404` with a generic message
    /// either way, so a caller can't tell the two apart.
    NotFound,
    /// [`AuthedAdmin`]'s own rejection for a real, valid session that simply
    /// isn't the instance's one admin account - deliberately distinct from
    /// `Unauthorized` (no/invalid session at all): a merchant hitting an
    /// admin-only route has a perfectly valid session, they just aren't
    /// allowed here, which is exactly what `403` (not `401`) means.
    Forbidden,
    /// The engine refused (rate limit) or could not be reached; worth
    /// retrying, unlike `Internal`.
    EngineUnavailable,
    Internal,
}

impl IntoResponse for ApiError {
    /// As a problem (`application/problem+json`,
    /// `crate::api::runtime::problem`): every JSON error this site answers
    /// with has the same shape.
    fn into_response(self) -> Response {
        crate::api::runtime::problem::ProblemDetails::from(self).into_response()
    }
}
