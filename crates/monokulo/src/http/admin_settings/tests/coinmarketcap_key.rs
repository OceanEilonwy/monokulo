//! The CoinMarketCap API key on the admin settings page: the one secret the
//! page may set (`settings::EXCHANGE_RATE_COINMARKETCAP_API_KEY`). It is
//! asked for, never shown back; kept encrypted; checked at CoinMarketCap
//! before it is saved; used by the provider (keyed base and header) from
//! the save on; replaced and removed from the page; locked by its
//! environment variable; and never written to a page or a log line.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};

use super::*;
use crate::settings::{EXCHANGE_RATE_COINMARKETCAP_API_KEY, EXCHANGE_RATE_COINMARKETCAP_BASE_URL};

const KEY: &str = "b54bcf4d-1bca-4e8e-9a24-22ff2c3d462c";
const OTHER_KEY: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
const API_KEY: &str = EXCHANGE_RATE_COINMARKETCAP_API_KEY.key;

/// Each request the stand-in saw: its path and its `X-CMC_PRO_API_KEY`.
type Seen = Arc<parking_lot::Mutex<Vec<(String, Option<String>)>>>;

/// A local server standing in for CoinMarketCap, keyless (`/public-api/…`)
/// and keyed (`/…`, `/v1/key/info`): it records each request and answers
/// with `answer(path, key)`. Returns the keyless base, as configured.
async fn spawn_cmc<F>(answer: F) -> (String, Seen)
where
    F: Fn(&str, Option<&str>) -> Response + Send + Sync + 'static,
{
    let seen: Seen = Arc::default();
    let answer = Arc::new(answer);
    let record = seen.clone();
    let app = Router::new().fallback(move |request: axum::extract::Request| {
        let answer = answer.clone();
        let record = record.clone();
        async move {
            let path = request.uri().path().to_string();
            let key = request
                .headers()
                .get(shared::coinmarketcap::API_KEY_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            record.lock().push((path.clone(), key.clone()));
            answer(&path, key.as_deref())
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/public-api"), seen)
}

fn json(status: u16, body: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

const KEY_INFO: &str =
    r#"{"data":{"plan":{"credit_limit_monthly":10000}},"status":{"error_code":0}}"#;
const USD_2: &str =
    r#"{"data":{"id":328,"symbol":"XMR","quote":{"USD":{"price":2.0}}},"status":{"error_code":0}}"#;
const INVALID: &str =
    r#"{"status":{"error_code":1001,"error_message":"This API Key is invalid."}}"#;

/// CoinMarketCap as it is for a working `KEY` or `OTHER_KEY`: key info,
/// and $2 a monero, keyed or keyless; any other key is refused.
fn working(path: &str, key: Option<&str>) -> Response {
    match (path, key) {
        (_, Some(key)) if key != KEY && key != OTHER_KEY => json(401, INVALID),
        ("/v1/key/info", Some(_)) => json(200, KEY_INFO),
        ("/v2/tools/price-conversion", Some(_))
        | ("/public-api/v2/tools/price-conversion", None) => json(200, USD_2),
        _ => json(
            404,
            r#"{"status":{"error_code":404,"error_message":"no such path"}}"#,
        ),
    }
}

/// An instance whose CoinMarketCap is at `base` (the stand-in), with
/// `env` as its environment.
async fn state_with(base: &str, env: live_settings::Env) -> AppState {
    let db = Db::open_in_memory().unwrap();
    db.seed_test_admin();
    let db = db.into_shared();
    let engine_client =
        EngineClient::for_tests(format!("http://{}", shared::unreachable::address()));
    let exchange_rate = test_exchange_rate_provider();
    let abuse: std::sync::Arc<crate::abuse::AbuseProtection> = Default::default();
    let options = live_settings::OptionsFile::in_memory(format!(
        "[exchange_rate]\ncoinmarketcap_base_url = \"{base}\"\n"
    ));
    let settings = crate::settings::MonokuloSettings::load(
        crate::settings::DbSettings::new(
            crate::db::Database::inline(db.clone()),
            crate::http::TEST_ENCRYPTION_KEY,
        ),
        engine_client.clone(),
        exchange_rate.clone(),
        abuse.clone(),
        None,
        env,
        options,
    )
    .await
    .unwrap();
    AppState {
        engine: crate::http::Engine::new(engine_client),
        exchange_rate,
        abuse,
        settings,
        ..AppState::for_tests_with_db(db)
    }
}

/// `test_secrets` with the key's variable set to `key`.
fn env_with_key(key: &str) -> live_settings::Env {
    live_settings::Env::fixed([
        (
            crate::settings::CRYPTO_ENCRYPTION_KEY.env_var.to_string(),
            "07".repeat(32),
        ),
        (
            crate::settings::ENGINE_TOKEN.env_var.to_string(),
            "t".repeat(credentials::auth::MIN_ENGINE_TOKEN_LEN),
        ),
        (
            EXCHANGE_RATE_COINMARKETCAP_API_KEY.env_var.to_string(),
            key.to_string(),
        ),
    ])
}

/// A store pricing with CoinMarketCap alone.
fn cmc_store() -> crate::db::StoreConnectionRow {
    crate::db::StoreConnectionRow {
        id: engine_protocol::ids::ConnectionId::new("conn-1".to_string()),
        user_id: engine_protocol::ids::UserId::new("user-1".to_string()),
        name: "Shop".to_string(),
        site: None,
        tenant_public_key: "pk_test".to_string(),
        tenant_secret_token_encrypted: "sk_test".to_string(),
        created_at: 0,
        fx_providers: vec!["coinmarketcap".to_string()],
        base_currency: "XMR".to_string(),
        fx_provider_settings: Default::default(),
        wallet_id: None,
        payment_description: String::new(),
    }
}

async fn payments_page(router: &Router, cookie: &str) -> String {
    body_text(
        get(
            router,
            &crate::views::admin::SettingsTab::Payments.href(),
            Some(cookie),
        )
        .await,
    )
    .await
}

/// The key's encrypted row, as stored.
fn stored_row(state: &AppState) -> Option<String> {
    state
        .db
        .lock()
        .list_encrypted_settings()
        .unwrap()
        .remove(API_KEY)
}

/// What the tests' log lines say, at every level.
#[derive(Clone, Default)]
struct Lines(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Lines {
    fn capture() -> (Lines, tracing::subscriber::DefaultGuard) {
        let lines = Lines::default();
        let writer = lines.clone();
        let (_telemetry, subscriber) = telemetry::build(
            "monokulo",
            telemetry::Format::Json,
            false,
            "trace",
            move || writer.clone(),
        );
        (lines, tracing::subscriber::set_default(subscriber))
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

/// The whole life of a key set from the page: asked for in the
/// CoinMarketCap part of the exchange rates card, checked at CoinMarketCap,
/// stored encrypted, used by the provider at once (keyed base, its
/// header), shown only as set, left alone by a save that doesn't give a
/// new one, replaced, and removed (keyless again). Never on a page or in
/// a log line.
#[tokio::test]
async fn a_key_set_on_the_page_is_checked_stored_encrypted_used_replaced_and_removed() {
    let (lines, _guard) = Lines::capture();
    let (base, seen) = spawn_cmc(working).await;
    let state = state_with(&base, crate::settings::test_secrets()).await;
    let router = build_router(state.clone());
    let cookie = admin_session_cookie(&router).await;

    // Unset: an empty password box in the CoinMarketCap part of the card,
    // after its address.
    let page = payments_page(&router, &cookie).await;
    let card = &page[page
        .find("<h4>CoinMarketCap</h4>")
        .expect("the CoinMarketCap part")..];
    let card = &card[..card.find("RetoSwap (Haveno)").unwrap()];
    assert!(
        card.find(EXCHANGE_RATE_COINMARKETCAP_BASE_URL.key) < card.find(API_KEY),
        "{card}"
    );
    assert!(
        card.contains(&format!(
            r#"type="password" name="{API_KEY}" id="setting-{API_KEY}""#
        )),
        "{card}"
    );
    assert!(card.contains("No key is set."), "{card}");
    assert!(
        !card.contains(&crate::views::admin::secret_remove_name(API_KEY)),
        "nothing to remove"
    );

    // Set: checked at CoinMarketCap first, then saved.
    let html = follow(
        &router,
        &cookie,
        post_settings(&router, &cookie, &[("tab", "payments"), (API_KEY, KEY)]).await,
    )
    .await;
    assert!(is_saved(&html), "{html}");
    assert!(!html.contains(KEY), "the key is on the page after its save");
    assert_eq!(
        seen.lock().as_slice(),
        [("/v1/key/info".to_string(), Some(KEY.to_string()))]
    );
    let stored = stored_row(&state).expect("stored");
    assert!(!stored.contains(KEY), "stored in the clear");
    assert_eq!(state.db.lock().get_setting(API_KEY).unwrap(), None);
    assert_eq!(
        monokulo_value(&state.settings, API_KEY),
        (
            live_settings::MASK.to_string(),
            live_settings::SettingSource::Database
        )
    );

    // Used at once: the keyed base, with the key in its header.
    seen.lock().clear();
    let (rate, provider) = state
        .exchange_rate
        .piconero_per_unit_for(&cmc_store(), "USD")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((rate, provider), (500_000_000_000, "coinmarketcap"));
    assert_eq!(
        seen.lock().as_slice(),
        [(
            "/v2/tools/price-conversion".to_string(),
            Some(KEY.to_string())
        )]
    );

    // Shown as set, never shown: a Remove box, an empty box to replace it.
    let page = payments_page(&router, &cookie).await;
    assert!(page.contains("A key is set."), "{page}");
    assert!(!page.contains(KEY), "the key is on the page");
    assert!(
        page.contains(&format!(
            r#"name="{}""#,
            crate::views::admin::secret_remove_name(API_KEY)
        )),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"name="{API_KEY}" value="#)),
        "{page}"
    );

    // A save that doesn't give a new one (its box left empty, another
    // setting changed) leaves it as it is.
    let html = follow(
        &router,
        &cookie,
        post_settings(
            &router,
            &cookie,
            &[
                ("tab", "payments"),
                (API_KEY, ""),
                ("exchange_rate.cache_seconds", "45"),
            ],
        )
        .await,
    )
    .await;
    assert!(is_saved(&html), "{html}");
    assert_eq!(stored_row(&state), Some(stored.clone()), "untouched");

    // Replaced: the new key is checked, stored and used.
    seen.lock().clear();
    let html = follow(
        &router,
        &cookie,
        post_settings(
            &router,
            &cookie,
            &[("tab", "payments"), (API_KEY, OTHER_KEY)],
        )
        .await,
    )
    .await;
    assert!(is_saved(&html), "{html}");
    assert_ne!(stored_row(&state), Some(stored));
    state
        .exchange_rate
        .piconero_per_unit_for(&cmc_store(), "USD")
        .await
        .unwrap();
    assert_eq!(
        seen.lock().as_slice(),
        [
            ("/v1/key/info".to_string(), Some(OTHER_KEY.to_string())),
            (
                "/v2/tools/price-conversion".to_string(),
                Some(OTHER_KEY.to_string())
            ),
        ]
    );

    // Removed: gone from the database, keyless again.
    seen.lock().clear();
    let remove = crate::views::admin::secret_remove_name(API_KEY);
    let html = follow(
        &router,
        &cookie,
        post_settings(
            &router,
            &cookie,
            &[("tab", "payments"), (API_KEY, ""), (&remove, "on")],
        )
        .await,
    )
    .await;
    assert!(is_saved(&html), "{html}");
    assert_eq!(stored_row(&state), None);
    assert_eq!(
        monokulo_value(&state.settings, API_KEY),
        (String::new(), live_settings::SettingSource::Default)
    );
    state
        .exchange_rate
        .piconero_per_unit_for(&cmc_store(), "USD")
        .await
        .unwrap();
    assert_eq!(
        seen.lock().as_slice(),
        [("/public-api/v2/tools/price-conversion".to_string(), None)]
    );
    assert!(payments_page(&router, &cookie)
        .await
        .contains("No key is set."));

    let logged = lines.text();
    assert!(
        !logged.is_empty(),
        "nothing was logged, so this proves nothing"
    );
    for key in [KEY, OTHER_KEY] {
        assert!(!logged.contains(key), "a key was logged: {logged}");
    }
}

/// A key CoinMarketCap refuses is refused on save, with its reason beside
/// it, and nothing is saved; one that isn't shaped like a key is refused
/// without asking CoinMarketCap. Neither is shown again or logged.
#[tokio::test]
async fn a_key_coinmarketcap_refuses_or_a_malformed_one_is_not_saved_and_says_why() {
    let (lines, _guard) = Lines::capture();
    let (base, seen) = spawn_cmc(|_, _| json(401, INVALID)).await;
    let state = state_with(&base, crate::settings::test_secrets()).await;
    let router = build_router(state.clone());
    let cookie = admin_session_cookie(&router).await;

    let html = unescaped(
        &follow(
            &router,
            &cookie,
            post_settings(&router, &cookie, &[("tab", "payments"), (API_KEY, KEY)]).await,
        )
        .await,
    );
    assert!(!is_saved(&html), "{html}");
    assert!(
        html.contains("CoinMarketCap refused this key (HTTP 401: This API Key is invalid.)"),
        "{html}"
    );
    assert!(!html.contains(KEY), "the refused key is on the page");
    assert_eq!(stored_row(&state), None);
    assert_eq!(seen.lock().len(), 1, "asked once");

    seen.lock().clear();
    let malformed = "not a key at all";
    let html = unescaped(
        &follow(
            &router,
            &cookie,
            post_settings(
                &router,
                &cookie,
                &[("tab", "payments"), (API_KEY, malformed)],
            )
            .await,
        )
        .await,
    );
    assert!(!is_saved(&html), "{html}");
    assert!(
        html.contains("A CoinMarketCap API key is 32 to 64 letters"),
        "{html}"
    );
    assert!(!html.contains(malformed), "{html}");
    assert!(
        seen.lock().is_empty(),
        "a malformed key isn't sent anywhere"
    );
    assert_eq!(stored_row(&state), None);

    let logged = lines.text();
    assert!(
        logged.contains("CoinMarketCap refused the API key"),
        "{logged}"
    );
    assert!(
        !logged.contains(KEY) && !logged.contains(malformed),
        "{logged}"
    );
}

/// A saved key CoinMarketCap refuses later (revoked, its plan lapsed):
/// rates fall back as for any other failure, the log says so, and the
/// settings page says so beside the key, without the key.
#[tokio::test]
async fn a_saved_key_refused_later_falls_back_and_is_shown_beside_it() {
    let (lines, _guard) = Lines::capture();
    let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = revoked.clone();
    let (base, _) = spawn_cmc(move |path, key| {
        if flag.load(std::sync::atomic::Ordering::SeqCst) && key.is_some() {
            json(401, INVALID)
        } else {
            working(path, key)
        }
    })
    .await;
    let state = state_with(&base, crate::settings::test_secrets()).await;
    let router = build_router(state.clone());
    let cookie = admin_session_cookie(&router).await;
    let html = follow(
        &router,
        &cookie,
        post_settings(&router, &cookie, &[("tab", "payments"), (API_KEY, KEY)]).await,
    )
    .await;
    assert!(is_saved(&html), "{html}");

    revoked.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = state
        .exchange_rate
        .piconero_per_unit_for(&cmc_store(), "USD")
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("refused its API key (HTTP 401: This API Key is invalid.)"),
        "{error}"
    );
    let page = unescaped(&payments_page(&router, &cookie).await);
    assert!(
        page.contains("CoinMarketCap refused this key (HTTP 401: This API Key is invalid.)"),
        "{page}"
    );
    assert!(!page.contains(KEY), "{page}");
    let logged = lines.text();
    assert!(
        logged.contains("CoinMarketCap refused the API key"),
        "{logged}"
    );
    assert!(!logged.contains(KEY), "{logged}");
}

/// Set by its environment variable: the provider uses it, and the page
/// shows it locked (dots, the variable named), with no box to change or
/// remove it; a form sending one anyway is refused, and nothing is stored.
#[tokio::test]
async fn a_key_from_the_environment_is_used_and_locked_on_the_page() {
    let (base, seen) = spawn_cmc(working).await;
    let state = state_with(&base, env_with_key(KEY)).await;
    let router = build_router(state.clone());
    let cookie = admin_session_cookie(&router).await;

    state
        .exchange_rate
        .piconero_per_unit_for(&cmc_store(), "USD")
        .await
        .unwrap();
    assert_eq!(
        seen.lock().as_slice(),
        [(
            "/v2/tools/price-conversion".to_string(),
            Some(KEY.to_string())
        )]
    );

    let page = payments_page(&router, &cookie).await;
    assert!(
        page.contains(&format!(
            r#"<input class="mk-input" type="password" value="locked" id="setting-{API_KEY}""#
        )),
        "{page}"
    );
    assert!(
        page.contains(EXCHANGE_RATE_COINMARKETCAP_API_KEY.env_var),
        "{page}"
    );
    assert!(!page.contains(&format!(r#"name="{API_KEY}""#)), "{page}");
    assert!(
        !page.contains(&crate::views::admin::secret_remove_name(API_KEY)),
        "{page}"
    );
    assert!(!page.contains(KEY), "{page}");

    seen.lock().clear();
    let html = unescaped(
        &follow(
            &router,
            &cookie,
            post_settings(
                &router,
                &cookie,
                &[("tab", "payments"), (API_KEY, OTHER_KEY)],
            )
            .await,
        )
        .await,
    );
    assert!(!is_saved(&html), "{html}");
    assert!(
        html.contains(EXCHANGE_RATE_COINMARKETCAP_API_KEY.env_var),
        "{html}"
    );
    assert!(!html.contains(OTHER_KEY), "{html}");
    assert!(seen.lock().is_empty(), "a locked key isn't checked");
    assert_eq!(stored_row(&state), None);
}
