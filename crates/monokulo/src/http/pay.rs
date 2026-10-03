//! Control-plane's own real, public order-creation endpoint
//! (`docs/fx_refactor.md` Phase 1.4) - the production counterpart to
//! `orders::create_order`'s "create a test order" dashboard button, now the
//! actual path a real storefront/plugin calls to create a fiat-priced
//! order. Unauthenticated and addressed by the tenant's own `pk_...` - the
//! same already-public identifier the engine's own equivalent endpoint and
//! checkout page use, not monokulo's internal `connection_id` (which
//! nothing outside this service has ever had a reason to know). Rate-limited
//! per source IP (`http::rate_limit`) - see that module's own doc comment
//! for why monokulo needed a rate limiter at all as of this endpoint.
//!
//! Computes the XMR amount from monokulo's own exchange rate
//! (`AppState.exchange_rate`) and passes that raw `xmr_amount_piconero` to
//! the engine's own (now XMR-only, `docs/fx_refactor.md` Phase 3) public
//! order-creation endpoint - monokulo's computation is the only rate
//! computation in the whole system now. The fiat amount/currency the
//! caller asked for is recorded locally (`Db::create_order_currency_metadata`)
//! for display purposes only; the engine never sees or stores it.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Response};
use axum::Extension;
use serde::{Deserialize, Serialize};

use crate::engine_client::EngineClientError;
use crate::now_unix;

use super::{ApiError, AppState};

#[derive(Deserialize)]
pub struct CreateOrderRequest {
    pub amount: String,
    pub currency: String,
    /// Optional caller-supplied identifier (a storefront's own order/cart
    /// id) - passed straight through to the engine's own `create_order`
    /// (`EngineClient::create_order`'s own `merchant_order_id` parameter)
    /// and shown on the dashboard's order detail page, so a merchant can
    /// match a Monokulo order back to their own records. `None`/omitted
    /// when a caller doesn't have one, same `Option` default-to-`None`
    /// convention this whole codebase already uses for an optional field.
    #[serde(default)]
    pub merchant_order_id: Option<String>,
    /// The caller's key for this one purchase (up to 100 visible ASCII
    /// characters): a retry with the same key gets the order the first
    /// attempt made, never a second one. The WooCommerce plugin sends one
    /// per order and total.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// Mirrors the engine's own `public::CreateOrderResponse` field-for-field -
/// deliberately the same shape a caller integrating against the engine
/// directly today already expects, so migrating a storefront from calling
/// the engine to calling this endpoint instead is a base-URL change, not a
/// response-parsing rewrite.
/// `amount`/`currency` echo back what the caller asked for
/// (`req.amount`/`req.currency`), not anything the engine
/// returned - the engine has no concept of fiat at all any more.
#[derive(Debug, Serialize)]
pub struct CreateOrderResponse {
    pub order_id: crate::db::OrderId,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub amount: String,
    pub currency: String,
    pub merchant_order_id: Option<String>,
    pub expires_at: i64,
}

/// Longest merchant order reference accepted.
pub const MAX_MERCHANT_ORDER_ID_CHARS: usize = 120;

/// `POST /pay/{pk}/orders`. Open to anyone for an unrestricted store; a
/// shop's server may authenticate with `Authorization: Bearer sk_...`
/// (`super::store_key`, checked before this runs), which is recorded on the
/// order (`created_with_key`) and is how a restricted store takes orders
/// from outside a browser (`super::embed_domains::embed_policy_middleware`).
pub async fn create_order(
    State(state): State<AppState>,
    Path(pk): Path<String>,
    key: Option<Extension<super::store_key::StoreKeyAuthenticated>>,
    Json(mut req): Json<CreateOrderRequest>,
) -> Response {
    // The merchant's own order reference, from anyone: trimmed, empty is
    // none, and bounded before it is stored and shown on dashboards.
    req.merchant_order_id = req
        .merchant_order_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    if req
        .merchant_order_id
        .as_deref()
        .is_some_and(|id| id.chars().count() > MAX_MERCHANT_ORDER_ID_CHARS)
    {
        return ApiError::BadRequest(format!(
            "merchant_order_id must be at most {MAX_MERCHANT_ORDER_ID_CHARS} characters"
        ))
        .into_response();
    }
    if req
        .idempotency_key
        .as_deref()
        .is_some_and(|key| !crate::engine_client::valid_idempotency_key(key))
    {
        return ApiError::BadRequest(format!(
            "idempotency_key must be 1 to {} visible ASCII characters",
            crate::engine_client::MAX_CALLER_IDEMPOTENCY_KEY_CHARS
        ))
        .into_response();
    }
    let created_with_key = key.is_some();
    let policy = crate::confirmation_thresholds::lock_policy(&pk).await;
    let (key, currency) = (pk.clone(), req.currency.clone());
    let found = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.get_store_connection_by_public_key(&key)?,
                crate::currencies::is_known_currency(db, &currency),
            ))
        })
        .await;
    let (row, currency_known) = match found {
        Ok((Some(row), known)) => (row, known),
        Ok((None, _)) => return ApiError::NotFound.into_response(),
        Err(_) => return ApiError::Internal.into_response(),
    };

    // Selection-time validation first, entirely independent of whether any
    // provider can actually price it (`crate::currencies`'s own doc comment)
    // - "unknown currency" (this doesn't exist at all) is a genuinely
    // different, clearer error than "unsupported currency" (a real currency
    // this instance just can't get a live rate for right now), so the two
    // get distinct messages rather than being collapsed into one.
    match currency_known {
        Ok(true) => {}
        Ok(false) => {
            return ApiError::BadRequest(format!("unknown currency: {}", req.currency))
                .into_response()
        }
        Err(_) => return ApiError::Internal.into_response(),
    }

    // Control-plane's own exchange rate is the only rate computation left in
    // the whole system (`docs/fx_refactor.md` Phase 3) - a real, fast `400`
    // for an unsupported currency or a malformed amount, before the engine
    // (which has no concept of currency at all) is ever called.
    // `"XMR"` always uses the trivial identity rate regardless of this
    // store's chosen ordered `fx_providers`; every other currency is dispatched by
    // *this store's own* chosen provider, a per-merchant setting, not one
    // shared instance-wide choice.
    let (piconero_per_unit, provider) = match state
        .exchange_rate
        .piconero_per_unit_for(&row, &req.currency)
        .await
    {
        Ok(Some(result)) => result,
        Ok(None) => {
            return ApiError::BadRequest(format!("unsupported currency: {}", req.currency))
                .into_response()
        }
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            // Not a real failure - this store's provider (or no provider at
            // all) simply can't price this currency on this instance, same
            // user-facing meaning as `Ok(None)` above.
            return ApiError::BadRequest(format!("unsupported currency: {}", req.currency))
                .into_response();
        }
        Err(e) => {
            tracing::error!(store.id = %row.id, currency = ?req.currency, error = %e, "exchange rate lookup failed");
            return ApiError::Internal.into_response();
        }
    };
    let xmr_amount_piconero = match shared::exchange_rate::compute_order_amount(
        &req.currency,
        &req.amount,
        piconero_per_unit,
    ) {
        Ok(amount) => amount,
        Err(e) => return ApiError::BadRequest(e.to_string()).into_response(),
    };

    let sk = match crate::http::orders::decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(_) => return ApiError::Internal.into_response(),
    };
    let resolution = match crate::confirmation_thresholds::resolve_for_order(
        &state,
        &row,
        &policy,
        &sk,
        &req.currency,
        piconero_per_unit,
        xmr_amount_piconero,
    )
    .await
    {
        Ok(resolution) => resolution,
        Err(message) => return ApiError::BadRequest(message).into_response(),
    };

    match state
        .engine
        .client
        .create_order(
            &sk,
            shared::xmr_amount::Piconero(xmr_amount_piconero),
            req.merchant_order_id.clone(),
            Some(resolution.confirmations_required),
            req.idempotency_key.as_ref().map(|key| format!("pay:{key}")),
        )
        .await
    {
        Ok(order) => {
            // Best-effort: a failure to record the local metadata row must
            // never fail an order that the engine has *already* genuinely
            // created - the order is real either way, and the customer is
            // already looking at (or about to be redirected to) a real
            // payment address. Losing this one local record is a strictly
            // smaller problem than telling a customer their real order
            // failed when it didn't.
            let source = if created_with_key { "api" } else { "website" };
            let (id, order_id, currency, amount, provider) = (
                row.id.clone(),
                order.order_id.clone(),
                req.currency.clone(),
                req.amount.clone(),
                provider.to_string(),
            );
            let base_currency = resolution.base_currency.clone();
            let (base_rate, confirmations) = (
                resolution.base_currency_piconero_per_unit,
                resolution.confirmations_required,
            );
            let recorded = state
                .db
                .write(move |db| {
                    db.create_order_currency_metadata(
                        &id,
                        &order_id,
                        &currency,
                        &amount,
                        shared::xmr_amount::Piconero(piconero_per_unit),
                        &provider,
                        now_unix(),
                        &base_currency,
                        base_rate,
                        confirmations,
                        created_with_key,
                        Some(source),
                    )
                })
                .await;
            if let Err(e) = recorded {
                tracing::error!(
                    order.id = %order.order_id,
                    store.id = %row.id,
                    error = %e,
                    "failed to record local fiat metadata - the real order still exists on the engine and this \
                     response is still correct, but its fiat display on monokulo's own pages will be missing"
                );
            }
            Json(CreateOrderResponse {
                order_id: order.order_id,
                address: order.address,
                xmr_amount_piconero: order.xmr_amount_piconero,
                amount: req.amount,
                currency: req.currency,
                merchant_order_id: req.merchant_order_id,
                expires_at: order.expires_at,
            })
            .into_response()
        }
        // The engine's own validation (an unconfigured network, its own
        // unsupported-currency check) - surfaced verbatim, same convention
        // every other caller of `EngineClient` in this crate already
        // applies to the engine's real `400`s.
        Err(EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::BAD_REQUEST =>
        {
            ApiError::BadRequest(message).into_response()
        }
        // The key was already used for a different purchase.
        Err(EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::CONFLICT =>
        {
            (
                axum::http::StatusCode::CONFLICT,
                axum::Json(serde_json::json!({ "error": message })),
            )
                .into_response()
        }
        Err(_) => ApiError::Internal.into_response(),
    }
}

/// Scripts and styles: kept, but checked with the server on every use, so
/// a new release's pages never run an old script. A check costs one round
/// trip and no body (`304`), which is what matters over Tor.
const REVALIDATE: &str = "no-cache";
/// Fonts and images change rarely: a week before they are checked again.
const LONG_LIVED: &str = "public, max-age=604800";

/// A file baked into the binary, with `Cache-Control` and an `ETag` of its
/// content: a browser that already has it is answered `304 Not Modified`
/// with no body.
fn static_asset(
    headers: &axum::http::HeaderMap,
    content_type: &'static str,
    cache_control: &'static str,
    body: &'static [u8],
) -> Response {
    use axum::http::header;
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    body.hash(&mut hasher);
    let etag = format!("\"{:016x}\"", hasher.finish());
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|tags| {
            tags.split(',')
                .any(|tag| tag.trim() == etag || tag.trim() == "*")
        });
    let cache = [
        (header::CACHE_CONTROL, cache_control.to_string()),
        (header::ETAG, etag),
    ];
    if fresh {
        return (axum::http::StatusCode::NOT_MODIFIED, cache).into_response();
    }
    (cache, [(header::CONTENT_TYPE, content_type)], body).into_response()
}

const CLIENT_LIBRARY_JS: &str = include_str!("../../static/monokulo-client.js");

/// `GET /static/monokulo-client.js` - the thin embed library a merchant's
/// static site `<script src>`s (`docs/fx_refactor.md` decision 3 / Phase
/// 4.3). Moved here from the engine, which no longer has any checkout UI or
/// fiat concept for it to talk to - this version's `createOrder`/`mount`
/// call monokulo's own `/pay/{pk}/orders` and
/// `/pay/{pk}/orders/{order_id}` instead. Served from this binary rather
/// than a CDN so a self-hoster's static site has no third-party dependency
/// in its payment path, same reasoning the engine's original had.
pub async fn client_library(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        CLIENT_LIBRARY_JS.as_bytes(),
    )
}

pub async fn checkout_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/checkout.js").as_bytes(),
    )
}

pub async fn pos_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!(concat!(env!("OUT_DIR"), "/pos-ui/pos-app.js")).as_bytes(),
    )
}

pub async fn pos_style(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/css; charset=utf-8",
        REVALIDATE,
        include_str!(concat!(env!("OUT_DIR"), "/pos-ui/pos-app.css")).as_bytes(),
    )
}

/// `GET /static/challenge.js` - solves the abuse-protection challenge on
/// the "Checking your connection" page (`views::challenge`).
pub async fn challenge_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/challenge.js").as_bytes(),
    )
}

/// `GET /static/fixi.js`, `/static/ssexi.js` and `/static/fx-glue.js` -
/// partial page updates and server-sent events on the server-rendered
/// pages (`http::fx`). fixi and ssexi are vendored, pinned copies (see
/// their `.SOURCE` files), served from here like `jsQR.js`: no CDN, so the
/// pages work offline and over Tor.
pub async fn fixi_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/fixi.js").as_bytes(),
    )
}

pub async fn ssexi_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/ssexi.js").as_bytes(),
    )
}

/// The engine page's script (`docs/engine_visualizer.md`).
pub async fn engine_view_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/engine-view.js").as_bytes(),
    )
}

pub async fn fx_glue_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/fx-glue.js").as_bytes(),
    )
}

/// `GET /static/telemetry.js` - browser problem reports (`http::telemetry_client`).
pub async fn telemetry_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/telemetry.js").as_bytes(),
    )
}

pub async fn qr_decoder_script(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        REVALIDATE,
        include_str!("../../static/jsQR.js").as_bytes(),
    )
}

const LOGO_SVG: &str = include_str!("../../static/logo.svg");
const FAVICON_SVG: &str = include_str!("../../static/favicon.svg");

/// `GET /static/logo.svg` - the full Monokulo mark, ink-on-paper, for other
/// sites to link to. monokulo's own pages draw it inline
/// (`views::logo_mark`), in the text colour of either theme.
/// Served the same way as [`client_library`] (a plain, unauthenticated
/// static asset baked into the binary) for the same reason: no third-party
/// CDN dependency in a page real customers may end up on.
pub async fn logo_svg(headers: axum::http::HeaderMap) -> Response {
    static_asset(&headers, "image/svg+xml", LONG_LIVED, LOGO_SVG.as_bytes())
}

/// `GET /static/favicon.svg` - the simplified, small-size version of the
/// same mark, linked from `_styles.html.hbs` (`<link rel="icon">`) so every
/// page that includes the `styles` partial gets a browser-tab icon for
/// free.
pub async fn favicon_svg(headers: axum::http::HeaderMap) -> Response {
    static_asset(
        &headers,
        "image/svg+xml",
        LONG_LIVED,
        FAVICON_SVG.as_bytes(),
    )
}

const MANROPE_500_WOFF2: &[u8] = include_bytes!("../../static/manrope-500.woff2");
const MANROPE_700_WOFF2: &[u8] = include_bytes!("../../static/manrope-700.woff2");
const MANROPE_800_WOFF2: &[u8] = include_bytes!("../../static/manrope-800.woff2");

/// `GET /static/manrope-{500,700,800}.woff2` - the UI typeface
/// (`_styles.html.hbs`'s `@font-face`), self-hosted for the same reason as
/// the logo/favicon/client-library assets above: no third-party CDN
/// dependency on any page. This one matters more than most - a Google
/// Fonts `<link>` would leak every visitor's IP to Google on every page
/// load, checkout included, which is the wrong tradeoff for a
/// privacy-focused payment tool. Latin subset only (this UI has no other
/// script), matching what a Google Fonts request for this weight range
/// would itself have served.
pub async fn manrope_500_woff2(headers: axum::http::HeaderMap) -> Response {
    static_asset(&headers, "font/woff2", LONG_LIVED, MANROPE_500_WOFF2)
}
pub async fn manrope_700_woff2(headers: axum::http::HeaderMap) -> Response {
    static_asset(&headers, "font/woff2", LONG_LIVED, MANROPE_700_WOFF2)
}
pub async fn manrope_800_woff2(headers: axum::http::HeaderMap) -> Response {
    static_asset(&headers, "font/woff2", LONG_LIVED, MANROPE_800_WOFF2)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::engine_client::EngineClient;

    use super::super::{build_router, AppState};

    /// Same fixed-scalar construction every other module's own tests use -
    /// see `connections.rs` for why these particular values pass the
    /// engine's real wallet-material validation.
    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    // This module's own tests deliberately exercise a real fiat quote (via a
    // local mock Coingecko server below), not just XMR - `http::pay::create_order`
    // is the real production storefront-facing endpoint, so its own tests are
    // the ones that should prove a real fiat currency actually works end to
    // end, unlike most other modules' tests (see `orders.rs`'s own doc
    // comment on why those use `"XMR"` instead).
    const TEST_CURRENCY: &str = "USD";
    // A mock price of exactly $1.00 makes the resulting piconero-per-unit
    // exactly 1e12 (`1_000_000_000_000.0 / 1.0`), a clean round number to
    // assert against without any floating-point rounding to account for.
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    /// Spins up a real local HTTP server standing in for Coingecko - same
    /// no-mocking-library pattern `shared::exchange_rate`'s own tests use.
    /// Returns the base URL a `CoingeckoRateProvider` can be pointed at.
    async fn spawn_mock_coingecko() -> String {
        async fn price() -> axum::response::Response {
            use axum::response::IntoResponse;
            (
                [("content-type", "application/json")],
                r#"{"monero":{"usd":1.0}}"#,
            )
                .into_response()
        }
        let app = Router::new().route("/api/v3/simple/price", axum::routing::get(price));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn test_exchange_rate_provider(
    ) -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
        let base_url = spawn_mock_coingecko().await;
        std::sync::Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(base_url),
        )
    }

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let engine_client = EngineClient::for_tests(format!("http://{}", engine.addr));
        let state = AppState {
            exchange_rate: test_exchange_rate_provider().await,
            engine: crate::http::Engine::new(engine_client),
            ..AppState::for_tests()
        };
        (state, engine)
    }

    use crate::http::test_support::body_json;

    use crate::http::test_support::body_text;

    use crate::http::test_support::signed_up_and_logged_in_session_token;

    /// Creates a real `store_connections` row (and a real tenant on the
    /// real spawned engine) for the given session, returning its public
    /// key - the identifier this module's own endpoint is addressed by.
    async fn create_connection(router: &Router, session_token: &str) -> String {
        let body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://shop.example.com",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "domains": [],
            "base_currency": "XMR",
        });
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        body_json(response)
            .await
            .as_object()
            .unwrap()
            .get("public_key")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    fn create_order_request(pk: &str, amount: &str, currency: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/pay/{pk}/orders"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "amount": amount, "currency": currency }).to_string(),
            ))
            .unwrap()
    }

    /// A storefront retrying `POST /pay/{pk}/orders` with the same
    /// idempotency key (its first answer was lost) gets the same order and
    /// address; another key, another order; the key reused for a different
    /// total is a 409; a malformed key is a 400.
    #[tokio::test]
    async fn a_retried_order_creation_with_one_key_makes_one_order() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "idempotent@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let post = |body: serde_json::Value| {
            let (router, pk) = (router.clone(), pk.clone());
            async move {
                let response = router
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(format!("/pay/{pk}/orders"))
                            .header("content-type", "application/json")
                            .body(Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                (response.status(), body_json(response).await)
            }
        };
        let order = |key: &str, amount: &str| {
            serde_json::json!({
                "amount": amount,
                "currency": TEST_CURRENCY,
                "merchant_order_id": "wc-9",
                "idempotency_key": key,
            })
        };
        let (status, first) = post(order("wc:9:25.00", "25.00")).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, retry) = post(order("wc:9:25.00", "25.00")).await;
        assert_eq!(status, StatusCode::OK, "{retry}");
        assert_eq!(retry["order_id"], first["order_id"]);
        assert_eq!(retry["address"], first["address"]);

        let (status, other) = post(order("wc:9:30.00", "30.00")).await;
        assert_eq!(status, StatusCode::OK, "{other}");
        assert_ne!(other["order_id"], first["order_id"]);

        let (status, clash) = post(order("wc:9:25.00", "26.00")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{clash}");

        let (status, _) = post(order("has space", "25.00")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Every static file a real page asks for is served, with the type its
    /// extension needs (a font or script with the wrong type is refused by
    /// the browser): the landing and login pages, a customer's checkout, and
    /// the static files those scripts load in turn.
    #[tokio::test]
    async fn every_static_file_the_pages_reference_is_served_with_its_type() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "assets@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let order = body_json(
            router
                .clone()
                .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
                .await
                .unwrap(),
        )
        .await;
        let checkout = format!("/pay/{pk}/orders/{}", order["order_id"].as_str().unwrap());

        let mut referenced = std::collections::BTreeSet::new();
        for page in ["/", "/dashboard/login", checkout.as_str()] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(page).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{page}");
            let html = String::from_utf8(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
            for part in html.split("/static/").skip(1) {
                let name: String = part
                    .chars()
                    .take_while(|c| {
                        c.is_ascii_alphanumeric() || *c == '-' || *c == '.' || *c == '_'
                    })
                    .collect();
                referenced.insert(name.trim_end_matches('.').to_string());
            }
        }
        // checkout.js loads the QR decoder; the POS loads its app and styles.
        referenced.extend(
            ["jsQR.js", "pos-app.js", "pos-app.css", "monokulo-client.js"].map(String::from),
        );
        assert!(
            referenced.iter().any(|name| name.ends_with(".woff2")),
            "{referenced:?}"
        );
        for name in &referenced {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/static/{name}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "/static/{name}");
            let content_type = response.headers()["content-type"]
                .to_str()
                .unwrap()
                .to_string();
            let expected = match name.rsplit('.').next().unwrap() {
                "js" => "javascript",
                "css" => "text/css",
                "svg" => "image/svg+xml",
                "woff2" => "font/woff2",
                other => panic!("unexpected static file type {other} ({name})"),
            };
            assert!(
                content_type.contains(expected),
                "/static/{name} served as {content_type}"
            );
            // Cacheable, and a browser that has it gets no body back.
            let etag = response.headers()["etag"].clone();
            assert!(
                response.headers().contains_key("cache-control"),
                "/static/{name}"
            );
            let again = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/static/{name}"))
                        .header("if-none-match", etag)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "/static/{name}");
            assert!(again
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty());
            assert!(
                !response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .is_empty(),
                "/static/{name} is empty"
            );
        }
    }

    /// Without JavaScript only the checkout embed refreshes by itself
    /// (structured_logging.md D4); every other page is a snapshot with a
    /// Reload button. (The challenge page's continue refresh is covered
    /// by `views::challenge`.)
    #[tokio::test]
    async fn only_the_checkout_refreshes_by_itself() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "no-refresh@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let store_id = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap()
            .id;
        let order = body_json(
            router
                .clone()
                .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
                .await
                .unwrap(),
        )
        .await;
        let order_id = order["order_id"].as_str().unwrap().to_string();

        let get = |path: String| {
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {session_token}"))
                .body(Body::empty())
                .unwrap()
        };
        for path in [
            "/".to_string(),
            "/dashboard/login".into(),
            "/dashboard/signup".into(),
            "/status".into(),
            "/dashboard".into(),
            "/dashboard/stores/new".into(),
            "/dashboard/connect".into(),
            format!("/dashboard/stores/{store_id}"),
            format!("/dashboard/stores/{store_id}/settings"),
            format!("/dashboard/stores/{store_id}/orders"),
            format!("/dashboard/stores/{store_id}/orders/{order_id}"),
            format!("/dashboard/stores/{store_id}/orders/new"),
        ] {
            let response = router.clone().oneshot(get(path.clone())).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let html = String::from_utf8(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
            assert!(
                !html.contains("http-equiv=\"refresh\""),
                "{path} must not refresh by itself: {html}"
            );
        }
        let customer = Request::builder()
            .uri(format!("/pay/{pk}/orders/{order_id}"))
            .body(Body::empty())
            .unwrap();
        let checkout = router.clone().oneshot(customer).await.unwrap();
        let html = String::from_utf8(
            checkout
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(
            html.contains("<noscript><meta http-equiv=\"refresh\""),
            "the checkout keeps its no-JS refresh: {html}"
        );
    }

    #[tokio::test]
    async fn creating_a_real_order_through_the_public_endpoint_returns_a_real_address_and_order_id()
    {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "expected the real engine to accept and create the order"
        );
        let body = body_json(response).await;
        let obj = body.as_object().unwrap();
        assert!(obj
            .get("order_id")
            .unwrap()
            .as_str()
            .unwrap()
            .starts_with("order_"));
        assert!(!obj.get("address").unwrap().as_str().unwrap().is_empty());
        assert_eq!(
            obj.get("currency").unwrap().as_str().unwrap(),
            TEST_CURRENCY
        );
        assert_eq!(obj.get("amount").unwrap().as_str().unwrap(), "25.00");
    }

    #[tokio::test]
    async fn threshold_database_failure_rejects_order_even_with_zero_conf_default() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-db-failure@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let row = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap();
        let sk = crate::http::orders::decrypt_sk(&state.encryption_key, &row).unwrap();
        state
            .engine
            .client
            .set_confirmations_required(&sk, 0)
            .await
            .unwrap();
        state.db.lock().break_confirmation_thresholds_for_test();

        let response = router
            .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        let tenant_id = engine
            .store()
            .lock()
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == pk)
            .unwrap()
            .id;
        assert!(engine
            .store()
            .lock()
            .list_orders(&tenant_id, None, 10, None)
            .unwrap()
            .is_empty());
    }

    /// The plugin's forwarded errors (structured_logging.md 2.4) need the
    /// store's own secret key and its Diagnostics on, and are capped.
    #[tokio::test]
    async fn only_the_store_itself_can_forward_its_plugin_errors() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "plugin-logs@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let row = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap();
        let sk = crate::http::orders::decrypt_sk(&state.encryption_key, &row).unwrap();
        let forward = |key: Option<&str>, entries: usize| {
            let entries: Vec<_> = (0..entries)
                .map(|n| serde_json::json!({ "level": "error", "message": format!("failure {n}"), "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736" }))
                .collect();
            let mut builder = Request::builder()
                .method("POST")
                .uri(format!("/pay/{pk}/logs"))
                .header("content-type", "application/json");
            if let Some(key) = key {
                builder = builder.header("authorization", format!("Bearer {key}"));
            }
            builder
                .body(Body::from(
                    serde_json::json!({ "entries": entries }).to_string(),
                ))
                .unwrap()
        };
        assert_eq!(
            router
                .clone()
                .oneshot(forward(Some(sk.expose()), 2))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "Diagnostics is off"
        );
        state.db.lock().set_client_logging(&row.id, true).unwrap();
        assert_eq!(
            router
                .clone()
                .oneshot(forward(Some(sk.expose()), 2))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            router
                .clone()
                .oneshot(forward(None, 1))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            router
                .clone()
                .oneshot(forward(Some("sk_wrong"), 1))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            router
                .clone()
                .oneshot(forward(Some(sk.expose()), 21))
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn an_order_waits_for_its_stores_policy_edit() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "policy-edit-race@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let guard = crate::confirmation_thresholds::lock_policy(&pk).await;
        let request = create_order_request(&pk, "25.00", TEST_CURRENCY);
        let mut task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut task)
                .await
                .is_err()
        );
        let tenant_id = engine
            .store()
            .lock()
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == pk)
            .unwrap()
            .id;
        assert!(engine
            .store()
            .lock()
            .list_orders(&tenant_id, None, 10, None)
            .unwrap()
            .is_empty());
        drop(guard);
        assert_eq!(task.await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_stored_threshold_cannot_fall_back_to_zero_conf() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "malformed-threshold@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let row = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap();
        let sk = crate::http::orders::decrypt_sk(&state.encryption_key, &row).unwrap();
        state
            .engine
            .client
            .set_confirmations_required(&sk, 0)
            .await
            .unwrap();
        state
            .db
            .lock()
            .create_confirmation_threshold_with_limit(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                "corrupt",
                &row.id,
                "not-an-amount",
                20,
                crate::now_unix(),
            )
            .unwrap();
        let response = router
            .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        let tenant_id = engine
            .store()
            .lock()
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == pk)
            .unwrap()
            .id;
        assert!(engine
            .store()
            .lock()
            .list_orders(&tenant_id, None, 10, None)
            .unwrap()
            .is_empty());
    }

    /// A real, previously-missing capability: `EngineClient::create_order`
    /// used to silently drop `merchant_order_id` no matter what a caller
    /// asked for - every order's own merchant order id always showed as
    /// unset on the dashboard regardless. Proves it's genuinely recorded on
    /// the engine now (not just echoed by monokulo), by reading it
    /// back through the real dashboard order-detail page, not just this
    /// endpoint's own response.
    #[tokio::test]
    async fn creating_a_real_order_with_a_merchant_order_id_records_it_on_the_engine() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-merchant-order-id@example.com",
            "correct horse battery staple",
        )
        .await;

        let connect_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(
                        serde_json::json!({
                            "platform": "custom",
                            "site_url": "https://shop.example.com",
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
        assert_eq!(connect_response.status(), StatusCode::CREATED);
        let connect_body = body_json(connect_response).await;
        let pk = connect_body["public_key"].as_str().unwrap().to_string();
        let connection_id = connect_body["connection_id"].as_str().unwrap().to_string();

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "amount": "25.00", "currency": TEST_CURRENCY, "merchant_order_id": "order-1234" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let order_id = body["order_id"].as_str().unwrap().to_string();
        assert_eq!(
            body["merchant_order_id"], "order-1234",
            "expected the real merchant_order_id echoed back, got: {body}"
        );

        let detail_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/dashboard/stores/{connection_id}/orders/{order_id}"
                    ))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(
            html.contains("order-1234"),
            "expected the real merchant_order_id shown on the dashboard, got: {html}"
        );
    }

    /// The other half of the test above: proves the local fiat-metadata
    /// row was actually recorded (not just the engine's own response
    /// echoed back) by reading it straight out of monokulo's own `Db`.
    #[tokio::test]
    async fn creating_a_real_order_records_local_fiat_metadata() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-metadata@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(create_order_request(&pk, "10.00", TEST_CURRENCY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let order_id = body
            .as_object()
            .unwrap()
            .get("order_id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();

        let connection_id = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap()
            .id;
        let metadata = state
            .db
            .lock()
            .get_order_currency_metadata(
                &connection_id,
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap();
        let metadata =
            metadata.expect("expected a real local fiat-metadata row for the order just created");
        assert_eq!(metadata.currency, TEST_CURRENCY);
        assert_eq!(metadata.amount, "10.00");
        assert_eq!(
            metadata.piconero_per_unit,
            shared::xmr_amount::Piconero(TEST_RATE_PICONERO_PER_UNIT)
        );

        // This store's base currency ("XMR", `create_connection`'s own
        // default) differs from the order's own currency ("USD"), so a
        // real second rate lookup (for "XMR" itself - always the identity
        // rate, regardless of provider, same as
        // `an_xmr_order_is_always_priced_at_the_identity_rate_regardless_of_the_stores_provider`
        // in `exchange_rate_config`) must have run and been snapshotted
        // separately from the order's own USD rate above.
        assert_eq!(metadata.store_base_currency, Some("XMR".to_string()));
        assert_eq!(
            metadata.base_currency_piconero_per_unit,
            Some(shared::xmr_amount::Piconero(TEST_RATE_PICONERO_PER_UNIT))
        );
        assert_eq!(
            metadata.confirmations_required_applied,
            Some(10),
            "no custom threshold exists, so the tenant's own default (10) applies"
        );
    }

    #[tokio::test]
    async fn creating_an_order_for_an_unknown_public_key_returns_404() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let response = router
            .oneshot(create_order_request(
                "pk_nonexistent",
                "25.00",
                TEST_CURRENCY,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// A local server standing in for CoinMarketCap's keyless price
    /// conversion. `usd_price` is what one XMR costs in USD; `None` answers
    /// with a body-level error, as the real API does when it is unavailable.
    async fn spawn_mock_coinmarketcap(usd_price: Option<f64>) -> String {
        let body = match usd_price {
            Some(price) => format!(
                r#"{{"data":{{"id":328,"quote":{{"USD":{{"price":{price}}}}}}},"status":{{"error_code":0}}}}"#
            ),
            None => r#"{"status":{"error_code":"500","error_message":"The system is busy, please try again later!"}}"#.to_string(),
        };
        let app = Router::new().route(
            "/v2/tools/price-conversion",
            axum::routing::get(move || {
                let body = body.clone();
                async move {
                    use axum::response::IntoResponse;
                    ([("content-type", "application/json")], body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Submits the store settings page's provider form (`use_<provider>` and
    /// `position_<provider>` fields) as the store's owner.
    async fn save_provider_settings(
        router: &Router,
        session_token: &str,
        connection_id: &str,
        fields: &[(&str, &str)],
    ) {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/dashboard/stores/{connection_id}/settings/fx-provider"
                    ))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_redirection() || response.status() == StatusCode::OK,
            "saving provider settings failed: {}",
            response.status()
        );
    }

    /// Creates an order and returns (provider recorded, piconero-per-unit
    /// recorded) from monokulo's own database.
    async fn order_pricing(state: &AppState, router: &Router, pk: &str) -> (String, u64) {
        let response = router
            .clone()
            .oneshot(create_order_request(pk, "10.00", TEST_CURRENCY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "order creation failed");
        let order_id = body_json(response).await["order_id"]
            .as_str()
            .unwrap()
            .to_string();
        let connection_id = state
            .db
            .lock()
            .get_store_connection_by_public_key(pk)
            .unwrap()
            .unwrap()
            .id;
        let metadata = state
            .db
            .lock()
            .get_order_currency_metadata(
                &connection_id,
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        (metadata.provider, metadata.piconero_per_unit.get())
    }

    /// Changing a store's providers, or their order, prices the very next
    /// order - nothing between the settings page and order creation caches
    /// the store's choice - and each order records the provider that priced it.
    #[tokio::test]
    async fn a_change_to_the_stores_provider_settings_prices_the_very_next_order() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        let coingecko = spawn_mock_coingecko().await; // $1.00 per XMR
        let coinmarketcap = spawn_mock_coinmarketcap(Some(2.0)).await; // $2.00 per XMR
        state.exchange_rate = std::sync::Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_and_coinmarketcap(
                coingecko,
                coinmarketcap,
            ),
        );
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-provider-change@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let connection_id = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap()
            .id;

        // A new store starts on Coingecko alone.
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coingecko".to_string(), 1_000_000_000_000)
        );

        // CoinMarketCap first, Coingecko second.
        save_provider_settings(
            &router,
            &session_token,
            connection_id.as_str(),
            &[
                ("use_coingecko", "on"),
                ("position_coingecko", "2"),
                ("use_coinmarketcap", "on"),
                ("position_coinmarketcap", "1"),
            ],
        )
        .await;
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coinmarketcap".to_string(), 500_000_000_000)
        );

        // Swapped back.
        save_provider_settings(
            &router,
            &session_token,
            connection_id.as_str(),
            &[
                ("use_coingecko", "on"),
                ("position_coingecko", "1"),
                ("use_coinmarketcap", "on"),
                ("position_coinmarketcap", "2"),
            ],
        )
        .await;
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coingecko".to_string(), 1_000_000_000_000)
        );

        // Coingecko switched off entirely.
        save_provider_settings(
            &router,
            &session_token,
            connection_id.as_str(),
            &[("use_coinmarketcap", "on"), ("position_coinmarketcap", "1")],
        )
        .await;
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coinmarketcap".to_string(), 500_000_000_000)
        );
    }

    /// The first preferred provider being unavailable, or having no rate for
    /// the currency, hands over to the next; and the order records the
    /// provider that actually answered.
    #[tokio::test]
    async fn an_unavailable_or_rateless_provider_falls_through_to_the_next_preferred() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        // Coingecko answers, but has no rate for USD; CoinMarketCap answers.
        async fn no_usd() -> axum::response::Response {
            use axum::response::IntoResponse;
            (
                [("content-type", "application/json")],
                r#"{"monero":{"eur":1.0}}"#,
            )
                .into_response()
        }
        let app = Router::new().route("/api/v3/simple/price", axum::routing::get(no_usd));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let coingecko = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let coinmarketcap = spawn_mock_coinmarketcap(Some(2.0)).await;
        state.exchange_rate = std::sync::Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_and_coinmarketcap(
                coingecko,
                coinmarketcap,
            ),
        );
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-provider-fallback@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let connection_id = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap()
            .id;
        save_provider_settings(
            &router,
            &session_token,
            connection_id.as_str(),
            &[
                ("use_coingecko", "on"),
                ("position_coingecko", "1"),
                ("use_coinmarketcap", "on"),
                ("position_coinmarketcap", "2"),
            ],
        )
        .await;

        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coinmarketcap".to_string(), 500_000_000_000)
        );

        // Now the preferred provider is down altogether (nothing listening).
        state.exchange_rate = std::sync::Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_and_coinmarketcap(
                "http://127.0.0.1:1",
                spawn_mock_coinmarketcap(Some(4.0)).await,
            ),
        );
        let router = build_router(state.clone());
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coinmarketcap".to_string(), 250_000_000_000)
        );
    }

    /// A local server standing in for haveno.markets: a two-sided USD book
    /// whose midpoint is `usd_price`, or (`None`) a one-sided one.
    async fn spawn_mock_haveno(usd_price: Option<f64>) -> String {
        let body = match usd_price {
            Some(p) => format!(
                r#"{{"USD":{{"pair":"XMR_USD","highest_bid":{},"lowest_ask":{}}}}}"#,
                p - 0.05,
                p + 0.05
            ),
            None => r#"{"USD":{"pair":"XMR_USD","highest_bid":1.0,"lowest_ask":null}}"#.to_string(),
        };
        let app = Router::new().route(
            "/api/v1/tickers",
            axum::routing::get(move || {
                let body = body.clone();
                async move {
                    use axum::response::IntoResponse;
                    ([("content-type", "application/json")], body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Haveno slots into the same per-store order: preferred first it prices
    /// the next order and is recorded on it; when its book is one-sided the
    /// next provider takes over.
    #[tokio::test]
    async fn haveno_can_be_ordered_first_and_falls_through_when_its_book_is_one_sided() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        let coingecko = spawn_mock_coingecko().await; // $1.00 per XMR
        let coinmarketcap = spawn_mock_coinmarketcap(Some(2.0)).await;
        let haveno = spawn_mock_haveno(Some(4.0)).await; // midpoint $4.00 per XMR
        state.exchange_rate =
            std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::all(
                coingecko,
                coinmarketcap,
                haveno,
            ));
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-provider-haveno@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let connection_id = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap()
            .id;

        save_provider_settings(
            &router,
            &session_token,
            connection_id.as_str(),
            &[
                ("use_haveno", "on"),
                ("position_haveno", "1"),
                ("use_coinmarketcap", "on"),
                ("position_coinmarketcap", "2"),
                ("use_coingecko", "on"),
                ("position_coingecko", "3"),
            ],
        )
        .await;
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("haveno".to_string(), 250_000_000_000)
        );

        // Same settings, but haveno's book has lost its ask: the next order
        // falls to coinmarketcap without touching the settings.
        state.exchange_rate =
            std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::all(
                spawn_mock_coingecko().await,
                spawn_mock_coinmarketcap(Some(2.0)).await,
                spawn_mock_haveno(None).await,
            ));
        let router = build_router(state.clone());
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coinmarketcap".to_string(), 500_000_000_000)
        );
    }

    /// Haveno's per-store limits apply to the very next order: tightening
    /// the spread limit, or taking the currency off the store's list, sends
    /// the next order to the next provider; loosening restores it.
    #[tokio::test]
    async fn a_change_to_the_stores_haveno_limits_prices_the_very_next_order() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        let haveno = spawn_mock_haveno(Some(4.0)).await; // bid 3.95 / ask 4.05: a 2.5% spread
        state.exchange_rate =
            std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::all(
                spawn_mock_coingecko().await, // $1.00 per XMR
                "http://127.0.0.1:1",
                haveno,
            ));
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-haveno-limits@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let connection_id = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk)
            .unwrap()
            .unwrap()
            .id;
        let save = |spread: &'static str, currencies: &'static str| {
            let router = router.clone();
            let session_token = session_token.clone();
            let connection_id = connection_id.clone();
            async move {
                save_provider_settings(
                    &router,
                    &session_token,
                    connection_id.as_str(),
                    &[
                        ("use_haveno", "on"),
                        ("position_haveno", "1"),
                        ("use_coingecko", "on"),
                        ("position_coingecko", "2"),
                        ("haveno_currencies", currencies),
                        ("haveno_max_spread_pct", spread),
                        ("haveno_min_offers_per_side", "1"),
                        ("haveno_min_depth_xmr_per_side", "0"),
                    ],
                )
                .await
            }
        };

        save("5", "").await;
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("haveno".to_string(), 250_000_000_000)
        );

        save("1", "").await; // 2.5% is now too wide
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coingecko".to_string(), 1_000_000_000_000)
        );

        save("5", "").await;
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("haveno".to_string(), 250_000_000_000)
        );

        save("5", "EUR").await; // USD is no longer on the store's list
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("coingecko".to_string(), 1_000_000_000_000)
        );

        save("5", "EUR,USD").await;
        assert_eq!(
            order_pricing(&state, &router, &pk).await,
            ("haveno".to_string(), 250_000_000_000)
        );
    }

    /// A local haveno.markets whose tickers request is counted.
    async fn spawn_counting_haveno(
        usd_price: f64,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let body = format!(
            r#"{{"USD":{{"pair":"XMR_USD","highest_bid":{},"lowest_ask":{}}}}}"#,
            usd_price - 0.05,
            usd_price + 0.05
        );
        let app = Router::new().route(
            "/api/v1/tickers",
            axum::routing::get(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = body.clone();
                async move {
                    use axum::response::IntoResponse;
                    ([("content-type", "application/json")], body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    /// Two merchants on one instance, sharing its one provider cache, with
    /// different Haveno limits: each store's orders are priced by its own
    /// limits, however their orders interleave, from a single upstream
    /// fetch; and neither merchant can read or change the other's settings.
    #[tokio::test]
    async fn two_merchants_with_different_haveno_limits_are_priced_by_their_own_limits_from_one_shared_cache(
    ) {
        let (mut state, _engine) = test_state_with_real_engine().await;
        let (haveno, tickers_calls) = spawn_counting_haveno(4.0).await; // a 2.5% spread
        state.exchange_rate =
            std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::all(
                spawn_mock_coingecko().await, // $1.00 per XMR
                "http://127.0.0.1:1",
                haveno,
            ));
        let router = build_router(state.clone());

        let token_a = signed_up_and_logged_in_session_token(
            &router,
            "pay-user-a@example.com",
            "correct horse battery staple",
        )
        .await;
        let token_b = signed_up_and_logged_in_session_token(
            &router,
            "pay-user-b@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk_a = create_connection(&router, &token_a).await;
        let pk_b = create_connection(&router, &token_b).await;
        let id_a = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk_a)
            .unwrap()
            .unwrap()
            .id;
        let id_b = state
            .db
            .lock()
            .get_store_connection_by_public_key(&pk_b)
            .unwrap()
            .unwrap()
            .id;

        let haveno_first = |spread: &'static str| {
            vec![
                ("use_haveno", "on"),
                ("position_haveno", "1"),
                ("use_coingecko", "on"),
                ("position_coingecko", "2"),
                ("haveno_currencies", ""),
                ("haveno_max_spread_pct", spread),
                ("haveno_min_offers_per_side", "1"),
                ("haveno_min_depth_xmr_per_side", "0"),
            ]
        };
        save_provider_settings(&router, &token_a, id_a.as_str(), &haveno_first("5")).await; // accepts 2.5%
        save_provider_settings(&router, &token_b, id_b.as_str(), &haveno_first("1")).await; // does not

        let haveno_price = ("haveno".to_string(), 250_000_000_000);
        let coingecko_price = ("coingecko".to_string(), 1_000_000_000_000);
        for round in 0..3 {
            assert_eq!(
                order_pricing(&state, &router, &pk_a).await,
                haveno_price,
                "A, round {round}"
            );
            assert_eq!(
                order_pricing(&state, &router, &pk_b).await,
                coingecko_price,
                "B, round {round}"
            );
            assert_eq!(
                order_pricing(&state, &router, &pk_b).await,
                coingecko_price,
                "B again, round {round}"
            );
            assert_eq!(
                order_pricing(&state, &router, &pk_a).await,
                haveno_price,
                "A again, round {round}"
            );
        }
        assert_eq!(
            tickers_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "both merchants were served from one upstream fetch"
        );

        // B loosening its own limit changes B's next order and nothing of A's.
        save_provider_settings(&router, &token_b, id_b.as_str(), &haveno_first("3")).await;
        assert_eq!(order_pricing(&state, &router, &pk_b).await, haveno_price);
        assert_eq!(order_pricing(&state, &router, &pk_a).await, haveno_price);
        assert_eq!(
            state
                .db
                .lock()
                .get_store_connection_by_id(&id_a)
                .unwrap()
                .unwrap()
                .fx_provider_settings
                .haveno
                .max_spread_pct,
            5.0
        );

        // A merchant cannot change, or see, another merchant's store settings.
        let before_a = state
            .db
            .lock()
            .get_store_connection_by_id(&id_a)
            .unwrap()
            .unwrap();
        let body = "use_coingecko=on&position_coingecko=1&haveno_currencies=EUR&haveno_max_spread_pct=0.5&haveno_min_offers_per_side=9&haveno_min_depth_xmr_per_side=9";
        let forged = |token: &str, uri: String, method: &str, body: &str| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let response = router
            .clone()
            .oneshot(forged(
                &token_b,
                format!("/dashboard/stores/{id_a}/settings/fx-provider"),
                "POST",
                body,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "B may not change A's store"
        );
        let response = router
            .clone()
            .oneshot(forged(
                &token_b,
                format!("/dashboard/stores/{id_a}/settings"),
                "GET",
                "",
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "B may not view A's store settings"
        );
        let after_a = state
            .db
            .lock()
            .get_store_connection_by_id(&id_a)
            .unwrap()
            .unwrap();
        assert_eq!(
            (after_a.fx_providers, after_a.fx_provider_settings),
            (before_a.fx_providers, before_a.fx_provider_settings)
        );
        assert_eq!(
            order_pricing(&state, &router, &pk_a).await,
            haveno_price,
            "A's pricing is unchanged by B's attempt"
        );
    }

    #[tokio::test]
    async fn creating_an_order_with_an_unknown_currency_is_rejected_before_ever_reaching_the_engine(
    ) {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-bad-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(create_order_request(&pk, "25.00", "NOTREAL"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["error"].as_str().unwrap().contains("unknown currency"),
            "got: {body}"
        );
    }

    /// The other half of the same two-stage split (`crate::currencies`'s own
    /// doc comment): `EUR` is a perfectly real, known currency - this test's
    /// own `spawn_mock_coingecko` only ever prices `"usd"` (a fixed stub
    /// response), so a request for `EUR` genuinely has no rate available.
    /// That must surface as a distinct "unsupported", not "unknown",
    /// currency error.
    #[tokio::test]
    async fn creating_an_order_with_a_known_but_provider_unsupported_currency_gets_a_distinct_error(
    ) {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-unsupported-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(create_order_request(&pk, "25.00", "EUR"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("unsupported currency"),
            "got: {body}"
        );
    }

    /// Same `create_connection` shape, but with a caller-chosen
    /// `base_currency` rather than the hardcoded `"XMR"` - needed by the
    /// threshold-resolution tests below, which specifically want a base
    /// currency this test module's own mock Coingecko *can't* price (it
    /// only ever stubs `"usd"`).
    async fn create_connection_with_base_currency(
        router: &Router,
        session_token: &str,
        base_currency: &str,
    ) -> String {
        let body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://shop.example.com",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "domains": [],
            "base_currency": base_currency,
        });
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        body_json(response)
            .await
            .as_object()
            .unwrap()
            .get("public_key")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The store's own base currency ("EUR", a perfectly real known
    /// currency - `crate::currencies`'s selection-time check happily
    /// accepted it when this store was created) turns out to have no
    /// available rate provider at resolution time (this test module's mock
    /// Coingecko only ever prices `"usd"`) - resolving the confirmation
    /// threshold needs a real EUR rate to convert the order's own amount
    /// into base-currency terms, and there isn't one. The order-currency
    /// itself ("USD") is perfectly priceable; it's specifically the base
    /// currency conversion that fails - proving the "usable" check really
    /// is a separate, later concern from "known" (`crate::currencies`'s own
    /// doc comment, and `confirmation_thresholds::resolve_for_order`'s).
    #[tokio::test]
    async fn creating_an_order_is_rejected_when_the_stores_own_base_currency_has_no_available_rate_provider(
    ) {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-unpriceable-base-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection_with_base_currency(&router, &session_token, "EUR").await;

        let response = router
            .clone()
            .oneshot(create_order_request(&pk, "25.00", "USD"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["error"].as_str().unwrap().contains("EUR"),
            "expected a clear error naming the unpriceable base currency, got: {body}"
        );

        // The engine must never have created a real order at all - threshold
        // resolution runs *before* `EngineClient::create_order`, so a
        // failure here must leave no ghost order behind on the engine.
        let store = engine.store().lock();
        let tenant_id = store
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == pk)
            .unwrap()
            .id;
        let orders = store.list_orders(&tenant_id, None, 100, None).unwrap();
        assert!(
            orders.is_empty(),
            "expected no order to have been created on the real engine, got: {orders:?}"
        );
    }

    #[tokio::test]
    async fn creating_an_order_with_a_malformed_amount_is_a_clear_400_not_a_500() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-bad-amount@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(create_order_request(&pk, "not-a-number", TEST_CURRENCY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn client_library_is_served_as_javascript_and_calls_monokulos_own_endpoints() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/static/monokulo-client.js")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/javascript; charset=utf-8"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let js = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(js.contains("Monokulo"));
        assert!(js.contains("createOrder"));
        assert!(js.contains("mount"));
        // The real point of this rewrite (`docs/fx_refactor.md` decision 3):
        // it must call monokulo's own `/pay/{pk}/orders` endpoint, not
        // the engine's old `/api/v1/t/{pk}/orders` - and the mounted iframe
        // must point at monokulo's own checkout page, not the engine's
        // now-deleted `/pay/v1/{pk}/{order_id}`.
        assert!(
            js.contains("/pay/\" + encodeURIComponent(publicKey) + \"/orders"),
            "should call monokulo's own order-creation endpoint, got: {js}"
        );
        assert!(
            js.contains("/orders/\" + encodeURIComponent(orderId)"),
            "should iframe monokulo's own checkout page, got: {js}"
        );
        assert!(
            !js.contains("/api/v1/t/"),
            "must not reference the engine's own API directly: {js}"
        );
        assert!(
            !js.contains("/pay/v1/"),
            "must not reference the engine's own (deleted) checkout route: {js}"
        );
        // The iframe does not post messages back, so mount() drives
        // callbacks through its own status request even when presentation
        // query parameters are present on the iframe URL.
        assert!(
            !js.contains("postMessage"),
            "the embed library must not depend on the iframe posting a message any more, got: {js}"
        );
        assert!(
            js.contains(r#"var statusUrl = checkoutUrl + "/status";"#),
            "expected mount() to poll the status endpoint directly, got: {js}"
        );
        assert!(js.contains("options.refund === false"));
        for asset in ["/static/checkout.js", "/static/jsQR.js"] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(asset).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{asset}");
            assert_eq!(
                response.headers().get("content-type").unwrap(),
                "text/javascript; charset=utf-8"
            );
        }
        // The checkout page and whatever embeds it (the POS terminal, this
        // library, a merchant's own page) stay independent: each follows the
        // order itself, and neither talks to the other.
        let checkout_js = include_str!("../../static/checkout.js");
        assert!(
            !checkout_js.contains("postMessage")
                && !checkout_js.contains("window.parent")
                && !checkout_js.contains("window.top"),
            "checkout.js must not know about its embedder"
        );
    }
}
