//! The landing page, the dashboard home page, and the "add a store" picker +
//! guided-flow instructional page - the pages a merchant actually lands on
//! first, none of which existed before this task (`dashboard.rs`'s own doc
//! comment on `login_submit` notes exactly this gap: "no real dashboard
//! content page exists yet").

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};

use crate::templates::{DashboardOrderRow, DashboardStoreRow, DashboardViewModel};

use super::orders::{display_name_for, health_of_tenant_lookup};
use super::{resolve_authed_user, AppState, AuthedUser};

/// `GET /` - unauthenticated, explains the product, links to signup/login
/// (or, if the visitor happens to already have a session, "log out" - a
/// real per-request check via [`resolve_authed_user`], not a fixed literal
/// like every other page's `logged_in`, since this is the one truly public
/// page most people actually revisit while already logged in).
pub async fn landing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let logged_in = resolve_authed_user(&state, &headers).is_some();
    let html = state.templates.render_landing(logged_in).expect("the built-in landing template must always render");
    Html(html).into_response()
}

/// `GET /dashboard/connections/new` - the picker between the two connect
/// flows (WBS follow-up: "custom (advanced)" is the existing
/// `/dashboard/connect` form; "simple -> woocommerce" is the guided page
/// below). Behind [`AuthedUser`] like every other `/dashboard/*` route.
pub async fn new_store_picker(State(state): State<AppState>, AuthedUser(_user, _): AuthedUser) -> Response {
    let html = state
        .templates
        .render_new_store_picker(true)
        .expect("the built-in new-store-picker template must always render");
    Html(html).into_response()
}

/// `GET /dashboard/connections/new/woocommerce` - a real live connect *form*
/// can't be rendered here: the generic `/connect/{platform}` flow needs a
/// `site_url`/`return_url`/`nonce` that only the WooCommerce plugin itself
/// can supply (see `http/connect.rs`'s own module doc comment) - the
/// dashboard has no way to manufacture a legitimate `return_url` back into
/// someone else's WordPress admin. So this is instructions, not a form; see
/// this page's own template for the reasoning restated for the merchant.
pub async fn woocommerce_instructions(State(state): State<AppState>, AuthedUser(_user, _): AuthedUser) -> Response {
    let html = state
        .templates
        .render_woocommerce_instructions(true)
        .expect("the built-in woocommerce-instructions template must always render");
    Html(html).into_response()
}

/// `GET /dashboard` - the real dashboard home page: every store the user
/// has connected, a merged recent-orders feed across all of them, and a
/// total-received figure. There is no dedicated "dashboard summary" engine
/// endpoint to call - this is real aggregation over each connection's own
/// `EngineClient::get_tenant`/`list_orders` calls, done sequentially here
/// (the expected number of stores per user is small; this is not the place
/// to add concurrency complexity for a case with no evidence it matters
/// yet).
pub async fn dashboard_home(State(state): State<AppState>, AuthedUser(user, _): AuthedUser) -> Response {
    let rows = match state.db.lock().unwrap().list_store_connections_for_user(&user.id) {
        Ok(rows) => rows,
        Err(_) => return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let mut stores = Vec::with_capacity(rows.len());
    let mut all_orders: Vec<DashboardOrderRow> = Vec::new();
    let mut total_received_piconero: u128 = 0;

    for row in rows {
        let sk = match crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted) {
            Ok(sk) => sk,
            // A row this service itself encrypted failing to decrypt with
            // its own key is an internal-consistency problem, not this
            // store's fault - skip it from the listing rather than failing
            // the whole dashboard for every other store the user has.
            Err(_) => continue,
        };

        let tenant_result = state.engine_client.get_tenant(&sk).await;
        let (health, health_label) = health_of_tenant_lookup(&tenant_result);
        let display_name = display_name_for(&row.site_url);

        stores.push(DashboardStoreRow {
            connection_id: row.id.clone(),
            display_name: display_name.clone(),
            platform: row.platform.clone(),
            site_url: row.site_url.clone(),
            public_key: row.tenant_public_key.clone(),
            health,
            health_label,
        });

        if let Ok(orders) = state.engine_client.list_orders(&sk).await {
            for o in orders {
                total_received_piconero += o.amount_received_piconero as u128;
                all_orders.push(DashboardOrderRow {
                    connection_id: row.id.clone(),
                    display_name: display_name.clone(),
                    payment_id: o.payment_id,
                    status: o.status,
                    fiat_amount: o.fiat_amount,
                    fiat_currency: o.fiat_currency,
                    created_at: o.created_at,
                });
            }
        }
    }

    all_orders.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    all_orders.truncate(10);

    let view_model = DashboardViewModel {
        has_stores: !stores.is_empty(),
        stores,
        recent_orders: all_orders,
        total_received_xmr: format_piconero_as_xmr(total_received_piconero),
        logged_in: true,
    };
    let html = state
        .templates
        .render_dashboard_home(&view_model)
        .expect("the built-in dashboard home template must always render");
    Html(html).into_response()
}

/// 1 XMR = 10^12 piconero (the same fixed-point convention every other
/// piconero value in this codebase uses - see `moneropay_core::status`'s own
/// doc comments at the repo root). Formats with the full 12 fractional
/// digits, trailing zeros trimmed, so a whole-XMR total reads as `"1"` not
/// `"1.000000000000"`, while still showing genuine sub-piconero-rounded
/// precision when present.
fn format_piconero_as_xmr(piconero: u128) -> String {
    const PICONERO_PER_XMR: u128 = 1_000_000_000_000;
    let whole = piconero / PICONERO_PER_XMR;
    let frac = piconero % PICONERO_PER_XMR;
    if frac == 0 {
        return whole.to_string();
    }
    let frac_str = format!("{frac:012}");
    let trimmed = frac_str.trim_end_matches('0');
    format!("{whole}.{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::format_piconero_as_xmr;

    #[test]
    fn formats_a_whole_xmr_amount_with_no_trailing_decimal() {
        assert_eq!(format_piconero_as_xmr(1_000_000_000_000), "1");
        assert_eq!(format_piconero_as_xmr(0), "0");
    }

    #[test]
    fn formats_a_fractional_amount_trimming_trailing_zeros() {
        assert_eq!(format_piconero_as_xmr(1_500_000_000_000), "1.5");
        assert_eq!(format_piconero_as_xmr(335_000_000), "0.000335");
    }

    #[test]
    fn formats_sub_piconero_precision_without_losing_the_last_digit() {
        assert_eq!(format_piconero_as_xmr(1), "0.000000000001");
    }

    mod http_tests {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::Router;
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        use crate::db::Db;
        use crate::engine_client::EngineClient;

        use super::super::super::{build_router, AppState};

        const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
        const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
        const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];
        const TEST_CURRENCY: &str = "USD";
        const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

        /// See `AppState`'s own doc comment on `exchange_rate`.
        fn test_exchange_rate_provider() -> std::sync::Arc<dyn shared::exchange_rate::ExchangeRateProvider> {
            std::sync::Arc::new(shared::exchange_rate::FixedRateProvider::new(std::collections::HashMap::from([(
                TEST_CURRENCY.to_string(),
                TEST_RATE_PICONERO_PER_UNIT,
            )])))
        }

        async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
            let engine = engine_test_support::TestEngineConfig::new()
                .with_networks(&[monero::Network::Mainnet])
                .with_rate(TEST_CURRENCY, TEST_RATE_PICONERO_PER_UNIT)
                .spawn()
                .await;
            let engine_client = EngineClient::new(format!("http://{}", engine.addr));
            let state = AppState {
                db: Db::open_in_memory().unwrap().into_shared(),
                engine_client,
                encryption_key: TEST_ENCRYPTION_KEY,
                templates: std::sync::Arc::new(crate::templates::TemplateEngine::new().unwrap()),
                status_cache: crate::http::status_page::new_status_cache(),
                exchange_rate: test_exchange_rate_provider(),
                rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
            };
            (state, engine)
        }

        async fn body_text(response: axum::response::Response) -> String {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            String::from_utf8(bytes.to_vec()).unwrap()
        }

        async fn body_json(response: axum::response::Response) -> serde_json::Value {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice(&bytes).unwrap()
        }

        async fn signed_up_and_logged_in_session_token(router: &Router, email: &str, password: &str) -> String {
            let signup = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/signup")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(signup.status(), StatusCode::CREATED);

            let login = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/login")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(login.status(), StatusCode::OK);
            body_json(login).await.as_object().unwrap().get("session_token").unwrap().as_str().unwrap().to_string()
        }

        async fn create_connection(router: &Router, session_token: &str) -> (String, String) {
            let body = serde_json::json!({
                "platform": "woocommerce",
                "site_url": "https://shop.example.com",
                "view_key_hex": TEST_VIEW_KEY_HEX,
                "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
                "network": "mainnet",
                "allowed_origins": [],
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
            let body = body_json(response).await;
            let obj = body.as_object().unwrap();
            (
                obj.get("connection_id").unwrap().as_str().unwrap().to_string(),
                obj.get("public_key").unwrap().as_str().unwrap().to_string(),
            )
        }

        async fn seed_real_order(engine_addr: std::net::SocketAddr, public_key: &str) -> String {
            let response = reqwest::Client::new()
                .post(format!("http://{engine_addr}/api/v1/t/{public_key}/orders"))
                .json(&serde_json::json!({ "fiat_amount": "10.00", "fiat_currency": TEST_CURRENCY }))
                .send()
                .await
                .expect("seeding a real order against the engine's public API failed");
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            let body: serde_json::Value = response.json().await.unwrap();
            body.as_object().unwrap().get("payment_id").unwrap().as_str().unwrap().to_string()
        }

        #[tokio::test]
        async fn landing_page_is_reachable_without_any_session() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);
            let response = router.oneshot(Request::builder().method("GET").uri("/").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;
            assert!(html.contains(r#"href="/dashboard/signup""#));
        }

        #[tokio::test]
        async fn dashboard_without_a_session_is_rejected() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);
            let response =
                router.oneshot(Request::builder().method("GET").uri("/dashboard").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn dashboard_with_no_stores_shows_the_empty_state_cta() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);
            let session_token =
                signed_up_and_logged_in_session_token(&router, "empty-dashboard@example.com", "correct horse battery staple").await;

            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard")
                        .header("authorization", format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;
            assert!(html.contains(r#"href="/dashboard/connections/new""#), "expected the add-a-store CTA, got: {html}");
        }

        #[tokio::test]
        async fn dashboard_with_a_connected_store_and_a_real_order_shows_the_real_total_received() {
            let (state, engine) = test_state_with_real_engine().await;
            let router = build_router(state);
            let session_token =
                signed_up_and_logged_in_session_token(&router, "full-dashboard@example.com", "correct horse battery staple").await;
            let (connection_id, public_key) = create_connection(&router, &session_token).await;
            let payment_id = seed_real_order(engine.addr, &public_key).await;

            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard")
                        .header("authorization", format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;

            assert!(html.contains(&public_key), "expected the store's public key listed, got: {html}");
            assert!(html.contains(&format!("/dashboard/connections/{connection_id}")), "expected a link to the store, got: {html}");
            assert!(html.contains(&payment_id), "expected the seeded order in the recent-orders feed, got: {html}");
            assert!(html.contains("tag-ok"), "the engine is genuinely reachable, so health must render as ok, got: {html}");
            assert!(html.contains("Total received"), "expected the total-received summary, got: {html}");
        }

        #[tokio::test]
        async fn new_store_picker_requires_auth_and_links_both_connect_flows() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);

            let unauthed = router
                .clone()
                .oneshot(Request::builder().method("GET").uri("/dashboard/connections/new").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

            let session_token =
                signed_up_and_logged_in_session_token(&router, "picker@example.com", "correct horse battery staple").await;
            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard/connections/new")
                        .header("authorization", format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;
            assert!(html.contains(r#"href="/dashboard/connections/new/woocommerce""#));
            assert!(html.contains(r#"href="/dashboard/connect""#));
        }

        #[tokio::test]
        async fn woocommerce_instructions_page_requires_auth() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);

            let unauthed = router
                .clone()
                .oneshot(Request::builder().method("GET").uri("/dashboard/connections/new/woocommerce").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

            let session_token =
                signed_up_and_logged_in_session_token(&router, "wc-instructions@example.com", "correct horse battery staple").await;
            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard/connections/new/woocommerce")
                        .header("authorization", format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    }
}
