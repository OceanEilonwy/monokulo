//! The landing page, the dashboard home page, and the "add a store" picker +
//! guided-flow instructional page - the pages a merchant actually lands on
//! first, none of which existed before this task (`dashboard.rs`'s own doc
//! comment on `login_submit` notes exactly this gap: "no real dashboard
//! content page exists yet").

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use crate::views;
use crate::views::dashboard::{DashboardOrderRow, DashboardStoreRow, DashboardViewModel};

use super::dashboard::redirect_302;
use super::orders::{display_name_for, health_of_tenant_lookup};
use super::{resolve_authed_user, AppState, AuthedUser};

/// `GET /` - unauthenticated, explains the product, links to signup/login
/// (or, if the visitor happens to already have a session, "log out" - a
/// real per-request check via [`resolve_authed_user`], not a fixed literal
/// like every other page's `logged_in`, since this is the one truly public
/// page most people actually revisit while already logged in).
///
/// The one gate for the first-run admin setup wizard (`http/admin_setup.rs`):
/// a fresh instance (`Db::is_setup_complete` still false) redirects here to
/// `/admin/setup` instead of ever rendering the landing page - "when you
/// open monokulo it should open to an admin setup flow" is exactly the
/// front door this page is. No other route is gated on this flag; a direct
/// link to `/dashboard/login` or the plain `/signup` API still works even
/// pre-setup; only the very first thing a fresh install's operator sees when
/// they actually load the site.
pub async fn landing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (setup_complete, signup_mode) = state
        .db
        .read(|db| {
            Ok::<_, crate::db::DbError>((
                db.is_setup_complete().unwrap_or(true),
                crate::settings::signup_mode(db),
            ))
        })
        .await
        .unwrap_or((true, crate::settings::SignupMode::InviteOnly));
    if !setup_complete {
        return redirect_302("/admin/setup");
    }
    let authed = resolve_authed_user(&state, &headers).await;
    let chrome = super::page_chrome(&state, authed.as_ref().map(|(user, _)| user), "/").await;
    let signup_public = signup_mode == crate::settings::SignupMode::Public;
    views::landing::page(&chrome, signup_public).into_response()
}

/// `GET /dashboard/stores/new` - the picker between the two connect
/// flows (WBS follow-up: "custom (advanced)" is the existing
/// `/dashboard/connect` form; "simple -> woocommerce" is the guided page
/// below). Behind [`AuthedUser`] like every other `/dashboard/*` route.
pub async fn new_store_picker(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
) -> Response {
    let chrome = super::page_chrome(&state, Some(&user), "/dashboard/stores/new").await;
    views::connect::new_store_picker_page(&chrome).into_response()
}

/// `GET /dashboard/stores/new/woocommerce` - a real live connect *form*
/// can't be rendered here: the generic `/connect/{platform}` flow needs a
/// `site_url`/`return_url`/`nonce` that only the WooCommerce plugin itself
/// can supply (see `http/connect.rs`'s own module doc comment) - the
/// dashboard has no way to manufacture a legitimate `return_url` back into
/// someone else's WordPress admin. So this is instructions, not a form; see
/// this page's own template for the reasoning restated for the merchant.
pub async fn woocommerce_instructions(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
) -> Response {
    let chrome = super::page_chrome(&state, Some(&user), "/dashboard/stores/new/woocommerce").await;
    views::store_detail::woocommerce_instructions_page(&chrome).into_response()
}

/// `GET /dashboard` - the real dashboard home page: every store the user
/// has connected, a merged recent-orders feed across all of them, and a
/// total-received figure. There is no dedicated "dashboard summary" engine
/// endpoint to call - this is real aggregation over each connection's own
/// `EngineClient::get_tenant`/`list_orders` calls, done sequentially here
/// (the expected number of stores per user is small; this is not the place
/// to add concurrency complexity for a case with no evidence it matters
/// yet).
pub async fn dashboard_home(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
) -> Response {
    let user_id = user.id.clone();
    let rows = match state
        .db
        .read(move |db| db.list_store_connections_for_user(&user_id))
        .await
    {
        Ok(rows) => rows,
        Err(_) => return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let mut stores = Vec::with_capacity(rows.len());
    let mut all_orders: Vec<DashboardOrderRow> = Vec::new();
    let mut total_received_piconero: u128 = 0;

    for row in rows {
        let sk = match crate::http::orders::decrypt_sk(&state.encryption_key, &row) {
            Ok(sk) => sk,
            // A row this service itself encrypted failing to decrypt with
            // its own key is an internal-consistency problem, not this
            // store's fault - skip it from the listing rather than failing
            // the whole dashboard for every other store the user has.
            Err(()) => {
                tracing::error!(store.id = %row.id, "a store's secret key could not be decrypted; it is left off the dashboard");
                continue;
            }
        };

        let tenant_result = state.engine.client.get_tenant(&sk).await;
        let (health, health_label) = health_of_tenant_lookup(&tenant_result);
        let display_name = display_name_for(&row.site_url);

        stores.push(DashboardStoreRow {
            connection_id: row.id.clone(),
            display_name: display_name.clone(),
            platform: row.platform.clone(),
            public_key: row.tenant_public_key.clone(),
            health,
            health_label,
        });

        if let Ok(orders) = state.engine.client.list_orders(&sk).await {
            for o in orders {
                total_received_piconero += o.amount_received_piconero as u128;
                // Filled in below for the orders that make the list.
                all_orders.push(DashboardOrderRow {
                    connection_id: row.id.clone(),
                    display_name: display_name.clone(),
                    order_id: o.order_id,
                    status: o.status.into(),
                    amount: "—".to_string(),
                    currency: String::new(),
                    created_at: o.created_at,
                });
            }
        }
    }

    all_orders.sort_by_key(|order| std::cmp::Reverse(order.created_at));
    all_orders.truncate(10);
    // The engine has no concept of fiat any more (`docs/fx_refactor.md`
    // Phase 3) - fiat display comes entirely from monokulo's own local
    // `order_currency_metadata`, read for just the orders shown.
    let shown: Vec<(crate::db::ConnectionId, crate::db::OrderId)> = all_orders
        .iter()
        .map(|order| (order.connection_id.clone(), order.order_id.clone()))
        .collect();
    let fiat_metadata = state
        .db
        .read(move |db| {
            let mut found = std::collections::HashMap::new();
            for (connection_id, order_id) in shown {
                if let Some(metadata) = db.get_order_currency_metadata(&connection_id, &order_id)? {
                    found.insert((connection_id, order_id), metadata);
                }
            }
            Ok::<_, crate::db::DbError>(found)
        })
        .await
        .inspect_err(|e| tracing::error!(error = %e, "could not read order currency metadata"))
        .unwrap_or_default();
    for order in &mut all_orders {
        let key = (order.connection_id.clone(), order.order_id.clone());
        if let Some(metadata) = fiat_metadata.get(&key) {
            order.amount = metadata.amount.clone();
            order.currency = metadata.currency.clone();
        }
    }

    let chrome = super::page_chrome(&state, Some(&user), "/dashboard").await;
    let view_model = DashboardViewModel {
        has_stores: !stores.is_empty(),
        stores,
        recent_orders: all_orders,
        total_received_xmr: format_piconero_as_xmr(total_received_piconero),
    };
    views::dashboard::page(&chrome, &view_model).into_response()
}

/// 1 XMR = 10^12 piconero (the same fixed-point convention every other
/// piconero value in this codebase uses - see `scanner::status`'s own
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
        use tower::ServiceExt;

        use crate::engine_client::EngineClient;

        use super::super::super::{build_router, AppState};

        const TEST_VIEW_KEY_HEX: &str =
            "0707070707070707070707070707070707070707070707070707070707070707";
        const TEST_SPEND_PUBKEY_HEX: &str =
            "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
        const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

        async fn test_state_with_real_engine() -> (AppState, scanner_test_support::TestEngineHandle)
        {
            let engine = scanner_test_support::TestEngineConfig::new()
                .with_networks(&[monero::Network::Mainnet])
                .spawn()
                .await;
            let engine_client = EngineClient::new(format!("http://{}", engine.addr));
            let state = AppState {
                engine: crate::http::Engine::new(engine_client),
                ..AppState::for_tests()
            };
            (state, engine)
        }

        use crate::http::test_support::body_text;

        use crate::http::test_support::body_json;

        use crate::http::test_support::signed_up_and_logged_in_session_token;

        async fn create_connection(router: &Router, session_token: &str) -> (String, String) {
            let body = serde_json::json!({
                "platform": "woocommerce",
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

        /// Seeds a real order directly on the spawned engine through its admin
        /// API (`POST /api/v1/admin/tenant/orders`), authenticated with the store's
        /// own `sk_` decrypted from monokulo's database - the same way monokulo
        /// itself reaches the engine. Deliberately bypasses monokulo's own order
        /// creation, so no local currency metadata exists for the order. 10.00 at
        /// `TEST_RATE_PICONERO_PER_UNIT` (1e12 piconero/USD); the engine only knows XMR.
        async fn seed_real_order(
            state: &AppState,
            engine_addr: std::net::SocketAddr,
            public_key: &str,
        ) -> String {
            let row = state
                .db
                .lock()
                .get_store_connection_by_public_key(public_key)
                .unwrap()
                .expect("connection exists");
            let sk = crate::http::orders::decrypt_sk(&state.encryption_key, &row).unwrap();
            let response = reqwest::Client::new()
                .post(format!("http://{engine_addr}/api/v1/admin/tenant/orders"))
                .bearer_auth(sk.expose())
                .json(
                    &serde_json::json!({ "xmr_amount_piconero": 10 * TEST_RATE_PICONERO_PER_UNIT }),
                )
                .send()
                .await
                .expect("seeding a real order against the engine's admin API failed");
            assert_eq!(
                response.status(),
                reqwest::StatusCode::OK,
                "expected the engine to accept the seeded order"
            );
            let body: serde_json::Value = response.json().await.unwrap();
            body.as_object()
                .unwrap()
                .get("order_id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        }

        #[tokio::test]
        async fn landing_page_is_reachable_without_any_session() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);
            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;
            assert!(html.contains(r#"href="/dashboard/signup""#));
        }

        #[tokio::test]
        async fn dashboard_without_a_session_is_rejected() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);
            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn dashboard_with_no_stores_shows_the_empty_state_cta() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);
            let session_token = signed_up_and_logged_in_session_token(
                &router,
                "empty-dashboard@example.com",
                "correct horse battery staple",
            )
            .await;

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
            assert!(
                html.contains(r#"href="/dashboard/stores/new""#),
                "expected the add-a-store CTA, got: {html}"
            );
        }

        #[tokio::test]
        async fn dashboard_with_a_connected_store_and_a_real_order_shows_the_real_total_received() {
            let (state, engine) = test_state_with_real_engine().await;
            let router = build_router(state.clone());
            let session_token = signed_up_and_logged_in_session_token(
                &router,
                "full-dashboard@example.com",
                "correct horse battery staple",
            )
            .await;
            let (connection_id, public_key) = create_connection(&router, &session_token).await;
            let order_id = seed_real_order(&state, engine.addr, &public_key).await;
            let (metadata_connection, metadata_order) = (
                crate::db::ConnectionId::new(connection_id.clone()),
                crate::db::OrderId::new(order_id.clone()),
            );
            state
                .db
                .write(move |db| {
                    db.create_order_currency_metadata(
                        &metadata_connection,
                        &metadata_order,
                        "EUR",
                        "12.34",
                        shared::xmr_amount::Piconero(1_000_000),
                        "fixed",
                        1000,
                        "EUR",
                        None,
                        10,
                        false,
                        None,
                    )
                })
                .await
                .unwrap();

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

            assert!(
                html.contains(&public_key),
                "expected the store's public key listed, got: {html}"
            );
            assert!(
                html.contains(&format!("/dashboard/stores/{connection_id}")),
                "expected a link to the store, got: {html}"
            );
            assert!(
                html.contains(&order_id),
                "expected the seeded order in the recent-orders feed, got: {html}"
            );
            assert!(
                html.contains("12.34"),
                "the order's fiat amount comes from monokulo's own metadata, got: {html}"
            );
            assert!(
                html.contains("tag-ok"),
                "the engine is genuinely reachable, so health must render as ok, got: {html}"
            );
            assert!(
                html.contains("Total received"),
                "expected the total-received summary, got: {html}"
            );
        }

        #[tokio::test]
        async fn new_store_picker_requires_auth_and_links_both_connect_flows() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);

            let unauthed = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard/stores/new")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

            let session_token = signed_up_and_logged_in_session_token(
                &router,
                "picker@example.com",
                "correct horse battery staple",
            )
            .await;
            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard/stores/new")
                        .header("authorization", format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;
            assert!(html.contains(r#"href="/dashboard/stores/new/woocommerce""#));
            assert!(html.contains(r#"href="/dashboard/connect""#));
        }

        #[tokio::test]
        async fn woocommerce_instructions_page_requires_auth() {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state);

            let unauthed = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard/stores/new/woocommerce")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

            let session_token = signed_up_and_logged_in_session_token(
                &router,
                "wc-instructions@example.com",
                "correct horse battery staple",
            )
            .await;
            let response = router
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/dashboard/stores/new/woocommerce")
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
