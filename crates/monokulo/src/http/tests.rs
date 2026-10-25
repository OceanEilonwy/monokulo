//! HTTP-layer integration tests for signing up, logging in and setting up a
//! store, driven through the real `Router` via `tower::ServiceExt::oneshot`
//! — no bound socket needed, same pattern as `engine::http::tests`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::engine_client::EngineClient;

use super::{build_router, AppState};

fn test_router() -> Router {
    build_router(AppState::for_tests())
}

/// `GET /account` (a page behind `AuthedUser`) with `cookie` as the
/// `Cookie` header, as a script asks: a `401` without a session.
fn account_request(cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri("/account");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::empty()).unwrap()
}

use crate::http::test_support::signed_up_and_logged_in_session_token;

#[tokio::test]
async fn the_stored_password_hash_is_a_real_argon2_hash_not_the_plaintext_password() {
    let state = AppState::for_tests();
    let router = build_router(state.clone());

    let plaintext = "correct horse battery staple";
    signed_up_and_logged_in_session_token(&router, "carol@example.com", plaintext).await;

    let row = state
        .db
        .lock()
        .get_user_by_email("carol@example.com")
        .unwrap()
        .unwrap();
    assert_ne!(
        row.password_hash, plaintext,
        "the plaintext password must never be stored as-is"
    );
    assert!(
        row.password_hash.starts_with("$argon2"),
        "expected a PHC-format Argon2 hash, got: {}",
        row.password_hash
    );
    let stored = row.password_hash.clone();
    assert_eq!(
        credentials::password::run(move |hasher| hasher.verify(plaintext, &stored)).await,
        Some(true)
    );
}

/// A session is a cookie: it reaches a protected page as its own user,
/// and the same token sent as a bearer header is not a session at all.
#[tokio::test]
async fn a_session_cookie_reaches_a_protected_page_as_the_right_user_and_a_bearer_does_not() {
    let router = test_router();
    let token = signed_up_and_logged_in_session_token(
        &router,
        "grace@example.com",
        "correct horse battery staple",
    )
    .await;

    let response = router
        .clone()
        .oneshot(account_request(Some(&format!("session={token}"))))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("grace@example.com"));

    let bearer = router
        .oneshot(
            Request::builder()
                .uri("/account")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bearer.status(), StatusCode::UNAUTHORIZED);
}

/// No session, or one nobody issued: a protected page answers `401`.
#[tokio::test]
async fn a_protected_page_refuses_no_session_and_an_unknown_one() {
    let router = test_router();
    for cookie in [None, Some("session=garbage-token-nobody-issued")] {
        let response = router
            .clone()
            .oneshot(account_request(cookie))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{cookie:?}");
    }
}

// -- The sign-up and log-in pages ---------------------------------------------

fn form_request(uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

use crate::http::test_support::urlencoding_encode;

use crate::http::test_support::body_text;

/// A new account is logged in straight away and goes on to set up its
/// first wallet: a store needs one.
#[tokio::test]
async fn signing_up_logs_in_and_goes_on_to_set_up_a_wallet() {
    let router = test_router();
    let response = router
        .oneshot(form_request(
            "/signup",
            &[
                ("email", "form-signup@example.com"),
                ("password", "correct horse battery staple"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FOUND,
        "expected a 302 redirect"
    );
    let location = response
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/setup");
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .starts_with("session="),
        "the new account is logged in"
    );
}

#[tokio::test]
async fn posting_a_duplicate_email_to_dashboard_signup_rerenders_the_form_with_an_error() {
    let router = test_router();

    let first = router
        .clone()
        .oneshot(form_request(
            "/signup",
            &[
                ("email", "dupe-form@example.com"),
                ("password", "first password here"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::FOUND);

    let second = router
        .oneshot(form_request(
            "/signup",
            &[
                ("email", "dupe-form@example.com"),
                ("password", "a different password"),
            ],
        ))
        .await
        .unwrap();
    // Still a 200 re-render of the form, not a redirect and not a bare JSON 409.
    assert_eq!(second.status(), StatusCode::OK);
    let html = body_text(second).await;
    assert!(
        html.contains("already registered"),
        "expected a visible duplicate-email error, got: {html}"
    );
    assert!(
        html.contains("<form"),
        "the signup form must still be present: {html}"
    );
}

/// Logging in redirects home with a session cookie that scripts can't
/// read (`HttpOnly`) and other sites' forms don't carry (`SameSite=Lax`,
/// which `http::csrf` relies on), and that cookie signs the user in.
#[tokio::test]
async fn logging_in_sets_an_httponly_samesite_session_cookie_that_signs_the_user_in() {
    let router = test_router();

    let signup = router
        .clone()
        .oneshot(form_request(
            "/signup",
            &[
                ("email", "form-login@example.com"),
                ("password", "correct horse battery staple"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    let response = router
        .clone()
        .oneshot(form_request(
            "/login",
            &[
                ("email", "form-login@example.com"),
                ("password", "correct horse battery staple"),
            ],
        ))
        .await
        .unwrap();
    // With no `next`, home (`auth::login_submit`).
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers().get("location").unwrap(), "/");
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        set_cookie.starts_with("session="),
        "expected a `session` cookie, got: {set_cookie}"
    );
    assert!(
        set_cookie.to_lowercase().contains("httponly"),
        "expected HttpOnly, got: {set_cookie}"
    );
    assert!(
        set_cookie.to_lowercase().contains("samesite=lax"),
        "expected SameSite=Lax, got: {set_cookie}"
    );

    // What a browser sends back: `session=<value>`, without the attributes.
    let session_pair = set_cookie.split(';').next().unwrap();
    let account = router
        .oneshot(account_request(Some(session_pair)))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::OK);
    assert!(body_text(account).await.contains("form-login@example.com"));
}

/// A browser loading a page it must be signed in for is sent to log in,
/// and comes back to that page after; anything that isn't a page load (the
/// machine API, a script's fetch, a form post) still gets a plain `401`.
/// Signed in, the nav shows the account menu instead of Log in and Sign up.
#[tokio::test]
async fn a_signed_out_browser_is_sent_to_log_in_and_back_while_the_api_gets_401() {
    let router = test_router();
    let page = |uri: &str| {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header(
                "accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .body(Body::empty())
            .unwrap()
    };

    let response = router
        .clone()
        .oneshot(page("/setup?from=nav"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    let to = response.headers()["location"].to_str().unwrap().to_string();
    assert_eq!(to, "/login?next=%2Fsetup%3Ffrom%3Dnav");
    let html = body_text(router.clone().oneshot(page(&to)).await.unwrap()).await;
    assert!(
        html.contains(r#"<input type="hidden" name="next" value="/setup?from=nav">"#),
        "{html}"
    );

    // Not a page load: the plain 401, as before.
    for request in [
        Request::builder()
            .uri("/setup")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri("/setup")
            .header("accept", "text/html")
            .header("fx-request", "true")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .method("POST")
            .uri("/theme")
            .header("accept", "text/html")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("theme=dark"))
            .unwrap(),
    ] {
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // Logged in, the account menu takes the place of Log in and Sign up.
    let signup = router
        .clone()
        .oneshot(form_request(
            "/signup",
            &[
                ("email", "nav-auth-state@example.com"),
                ("password", "correct horse battery staple"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);
    let session_pair = signup.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let html = body_text(
        router
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .header("cookie", session_pair)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("<span>Log out</span></button>"), "{html}");
    assert!(
        html.contains("<strong>nav-auth-state@example.com</strong>"),
        "{html}"
    );
    assert!(!html.contains(r#"href="/login">Log in<"#), "{html}");
    assert!(!html.contains(r#"href="/signup">Sign up<"#), "{html}");
}

#[tokio::test]
async fn logging_out_via_the_dashboard_nav_form_clears_the_session_and_goes_to_log_in() {
    let router = test_router();

    let signup = router
        .clone()
        .oneshot(form_request(
            "/signup",
            &[
                ("email", "dashboard-logout@example.com"),
                ("password", "correct horse battery staple"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);
    let login = router
        .clone()
        .oneshot(form_request(
            "/login",
            &[
                ("email", "dashboard-logout@example.com"),
                ("password", "correct horse battery staple"),
            ],
        ))
        .await
        .unwrap();
    let set_cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let session_pair = set_cookie.split(';').next().unwrap().to_string();

    let logout = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/logout")
                .header("cookie", session_pair.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        logout.status(),
        StatusCode::FOUND,
        "expected a redirect after logging out"
    );
    assert_eq!(logout.headers().get("location").unwrap(), "/login");

    // The same, now-deleted session cookie must no longer authenticate.
    let after = router
        .oneshot(account_request(Some(&session_pair)))
        .await
        .unwrap();
    assert_eq!(
        after.status(),
        StatusCode::UNAUTHORIZED,
        "the logged-out session must no longer authenticate"
    );
}

#[tokio::test]
async fn posting_a_wrong_password_to_dashboard_login_rerenders_the_form_with_a_generic_error() {
    let router = test_router();

    let signup = router
        .clone()
        .oneshot(form_request(
            "/signup",
            &[
                ("email", "wrong-pw-form@example.com"),
                ("password", "the real password"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    let response = router
        .oneshot(form_request(
            "/login",
            &[
                ("email", "wrong-pw-form@example.com"),
                ("password", "not the real password"),
            ],
        ))
        .await
        .unwrap();
    // Still a 200 re-render, not a redirect and not a bare JSON 401.
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get("set-cookie").is_none(),
        "no session cookie on a failed login"
    );
    let html = body_text(response).await;
    assert!(
        html.contains("Invalid email or password"),
        "expected a generic login error, got: {html}"
    );

    // An unknown email at dashboard login gets the same generic error as a
    // wrong password.
    {
        let router = test_router();
        let response = router
            .oneshot(form_request(
                "/login",
                &[
                    ("email", "nobody-has-this-account@example.com"),
                    ("password", "whatever"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Invalid email or password"),
            "expected the same generic login error, got: {html}"
        );
    }
}

// -- Setting up a store (`/setup`) -----------------------------------------
//
// Unlike the rest of this file, these tests need a *real* spawned engine
// (same reason as `connections.rs`'s own tests: setup really provisions a
// tenant) - so they get their own `AppState` helper instead of
// `AppState::for_tests()`'s dummy, never-dialed engine URL.

/// Same fixed-scalar construction `connections.rs`'s and `engine_client.rs`'s
/// own tests use - see those modules for why these particular values pass
/// the engine's real wallet-material validation.
const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const TEST_SPEND_PUBKEY_HEX: &str =
    "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let engine_client = EngineClient::embedded_for_tests(engine.router());
    let state = AppState {
        engine: crate::http::Engine::new(engine_client),
        ..AppState::for_tests()
    };
    (state, engine)
}

/// Signs up and logs in a fresh user through the browser form flow, returning
/// the `session=<value>` pair (no `HttpOnly`/`SameSite`/`Path` attributes) a
/// browser would send back as a `Cookie` header - same extraction
/// `logging_in_sets_an_httponly_samesite_session_cookie_that_signs_the_user_in`
/// above uses.
async fn signed_up_and_logged_in_session_cookie(
    router: &Router,
    email: &str,
    password: &str,
) -> String {
    let signup = router
        .clone()
        .oneshot(form_request(
            "/signup",
            &[("email", email), ("password", password)],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    let login = router
        .clone()
        .oneshot(form_request(
            "/login",
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

/// A form post as the browser holding `cookie` sends it.
fn cookie_form_request(uri: &str, cookie: &str, fields: &[(&str, &str)]) -> Request<Body> {
    crate::http::test_support::form_post(uri, Some(cookie), fields)
}

async fn cookie_get(router: &Router, uri: &str, cookie: &str) -> axum::response::Response {
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

/// Brings in a wallet with the test keys ("Bring your own wallet"), as a
/// merchant does from the Account page; its id.
async fn add_test_wallet(router: &Router, cookie: &str) -> String {
    let response = router
        .clone()
        .oneshot(cookie_form_request(
            "/account/wallets/import",
            cookie,
            &[
                ("name", "Test wallet"),
                ("view_key_hex", TEST_VIEW_KEY_HEX),
                ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ("network", "mainnet"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "the wallet is added"
    );
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    location
        .trim_start_matches("/account/wallets/")
        .split(['/', '?'])
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn setup_without_a_session_sends_a_page_load_to_log_in() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/setup")
                .header("accept", "text/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers()["location"], "/login?next=%2Fsetup");
}

/// The whole of setup without JavaScript: the store step, the wallet
/// step's choice (no wallet yet, so nothing to reuse), bringing a wallet in
/// with its keys, and Done. The store keeps its name apart from its site,
/// and its site is the host of what was pasted.
#[tokio::test]
async fn setting_up_a_website_store_brings_a_wallet_in_and_ends_on_done() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let cookie = signed_up_and_logged_in_session_cookie(
        &router,
        "bakery@example.com",
        "correct horse battery staple",
    )
    .await;

    let html = body_text(cookie_get(&router, "/setup", &cookie).await).await;
    assert!(html.contains("<h1>Set up a store</h1>"), "{html}");
    assert!(
        html.contains(r#"<li aria-current="step"><span>Store<small>In progress</small></span>"#)
    );
    // XMR: no language names a region, and this instance has no exchange
    // rates, so an Australian browser gets XMR too (`new_store_default`).
    assert!(html.contains(r#"<option value="XMR" selected"#), "{html}");
    let australian = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/setup")
                .header("cookie", &cookie)
                .header("accept-language", "en-AU,en;q=0.9")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let australian = body_text(australian).await;
    assert!(
        australian.contains(r#"<option value="XMR" selected"#),
        "{australian}"
    );

    let store_step = [
        ("store_name", "  Bakery "),
        ("store_site", "https://Bakery.example/checkout?x=1"),
        ("base_currency", "AUD"),
    ];
    let response = router
        .clone()
        .oneshot(cookie_form_request("/setup", &cookie, &store_step))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let wallet_step = response.headers()["location"].to_str().unwrap().to_owned();
    assert_eq!(
        wallet_step,
        "/setup/wallet?store_name=Bakery&store_site=bakery.example&base_currency=AUD"
    );

    let html = body_text(cookie_get(&router, &wallet_step, &cookie).await).await;
    assert!(
        html.contains("Where should the money for Bakery go?"),
        "{html}"
    );
    assert!(
        html.contains(r#"value="Bakery""#),
        "a name is offered: {html}"
    );
    assert!(
        !html.contains("Use a wallet you already added"),
        "none yet: {html}"
    );
    assert!(html.contains(r#"<li class="done"><span>Store<small>Complete</small></span>"#));
    assert!(
        !html.contains("Base currency"),
        "defaults, not asked: {html}"
    );

    let keys = body_text(
        cookie_get(
            &router,
            "/setup/wallet/keys?store_name=Bakery&store_site=bakery.example&base_currency=AUD&name=Bakery&network=mainnet",
            &cookie,
        )
        .await,
    )
    .await;
    assert!(keys.contains("<h1>Bring your own wallet</h1>"), "{keys}");
    assert!(
        keys.contains(r#"<input type="hidden" name="store_site" value="bakery.example">"#)
            && keys.contains(r#"<input type="hidden" name="base_currency" value="AUD">"#),
        "{keys}"
    );

    let mut fields = vec![
        ("store_name", "Bakery"),
        ("store_site", "bakery.example"),
        ("base_currency", "AUD"),
        ("name", "Bakery"),
        ("network", "mainnet"),
        ("view_key_hex", TEST_VIEW_KEY_HEX),
        ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
    ];
    let made = router
        .clone()
        .oneshot(cookie_form_request("/setup/wallet/keys", &cookie, &fields))
        .await
        .unwrap();
    let store_id = crate::http::test_support::store_made(&made);

    let row = state
        .db
        .lock()
        .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(store_id.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(row.name, "Bakery");
    assert_eq!(
        row.site.as_ref().map(crate::stores::Site::as_str),
        Some("bakery.example")
    );
    assert_eq!(
        row.base_currency, "AUD",
        "the currency chosen on the store step"
    );
    let wallet = state
        .db
        .lock()
        .get_wallet(&row.user_id, row.wallet_id.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(wallet.name, "Bakery");
    // The site's domain waits to be verified.
    let domains = state.db.lock().list_store_domains(&row.id).unwrap();
    assert_eq!(domains[0].domain, "bakery.example");

    let done = body_text(
        cookie_get(
            &router,
            made.headers()["location"].to_str().unwrap(),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(done.contains("Bakery is set up"), "{done}");
    assert!(done.contains("Not taking payments yet"));
    assert!(done.contains("Open the guide"));

    // The dashboard calls it by its name.
    let dashboard = body_text(cookie_get(&router, "/", &cookie).await).await;
    assert!(dashboard.contains("Bakery"), "{dashboard}");

    // The same keys again make no second wallet: they're refused on the
    // keys screen, which says which wallet has them.
    fields[0] = ("store_name", "Second");
    fields[1] = ("store_site", "second.example");
    let again = router
        .clone()
        .oneshot(cookie_form_request("/setup/wallet/keys", &cookie, &fields))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::OK);
    assert!(body_text(again).await.contains("already added this wallet"));
}

/// A store's Store card names it and gives it a site, or takes it away: a
/// new site waits to be verified, and one another store has is refused,
/// saying which.
#[tokio::test]
async fn a_stores_name_and_site_are_changed_in_its_settings() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let cookie = signed_up_and_logged_in_session_cookie(
        &router,
        "rename@example.com",
        "correct horse battery staple",
    )
    .await;
    let made = crate::http::test_support::set_up_store_with_keys(
        &router,
        &cookie,
        &[
            ("store_name", "Bakery"),
            ("store_site", "bakery.example"),
            ("view_key_hex", TEST_VIEW_KEY_HEX),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
        ],
    )
    .await;
    let bakery = crate::http::test_support::store_made(&made);
    let wallet = state
        .db
        .lock()
        .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(bakery.clone()))
        .unwrap()
        .unwrap()
        .wallet_id
        .unwrap();
    let stall = crate::http::test_support::store_made(
        &crate::http::test_support::set_up_store_on_wallet(
            &router,
            &cookie,
            "Market stall",
            "",
            wallet.as_str(),
        )
        .await,
    );
    let settings = format!("/stores/{stall}/settings");
    let page = body_text(cookie_get(&router, &settings, &cookie).await).await;
    assert!(
        page.contains(r#"name="store_name" id="store-name" value="Market stall""#),
        "{page}"
    );
    assert!(
        page.contains(r#"name="store_site" id="store-site" value="" placeholder"#),
        "{page}"
    );
    assert!(page.contains("Your website (optional)"), "{page}");

    let taken = router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[
                ("store_name", "Market stall"),
                ("store_site", "https://bakery.example/"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(taken.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(taken).await;
    assert!(
        html.contains("Your store Bakery already uses bakery.example."),
        "{html}"
    );

    let saved = router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[
                ("store_name", " Market  stall & café "),
                ("store_site", "https://Stall.example/menu"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    assert!(saved.headers()["location"]
        .to_str()
        .unwrap()
        .ends_with("?saved=store"));
    let row = state
        .db
        .lock()
        .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(stall.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(row.name, "Market stall & café");
    assert_eq!(
        row.site.as_ref().map(crate::stores::Site::as_str),
        Some("stall.example")
    );
    let domains = state.db.lock().list_store_domains(&row.id).unwrap();
    assert_eq!(domains[0].domain, "stall.example", "waits to be verified");

    // Emptying it in the form saves nothing of it: the save goes on to ask
    // (B1), the name typed; then it's gone, and its domain with it.
    let cleared = router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[("store_site", "")],
        ))
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::SEE_OTHER);
    let website = format!("{settings}/website");
    assert_eq!(
        cleared.headers()["location"],
        format!("{website}?remove=1").as_str()
    );
    let site_of = |state: &AppState| {
        state
            .db
            .lock()
            .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(stall.clone()))
            .unwrap()
            .unwrap()
            .site
            .map(|site| site.to_string())
            .unwrap_or_default()
    };
    assert_eq!(site_of(&state), "stall.example", "not yet");
    let page = body_text(cookie_get(&router, &format!("{website}?remove=1"), &cookie).await).await;
    assert!(
        page.contains("Remove the website of \u{201c}Market stall &amp; café\u{201d}?"),
        "{page}"
    );
    assert!(
        page.contains("Checkouts embedded on stall.example stop loading"),
        "{page}"
    );
    assert!(
        page.contains("Its verified domain stall.example is dropped"),
        "{page}"
    );
    let wrong = router
        .clone()
        .oneshot(cookie_form_request(
            &website,
            &cookie,
            &[("remove", "1"), ("confirm", "market stall")],
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(site_of(&state), "stall.example");
    let removed = router
        .clone()
        .oneshot(cookie_form_request(
            &website,
            &cookie,
            &[("remove", "1"), ("confirm", "Market stall & café")],
        ))
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::SEE_OTHER);
    assert_eq!(site_of(&state), "");
    assert!(state
        .db
        .lock()
        .list_store_domains(&engine_protocol::ids::ConnectionId::new(stall.clone()))
        .unwrap()
        .is_empty());
    // Changing works the same way, to the new site.
    router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[("store_site", "first.example")],
        ))
        .await
        .unwrap();
    let asked = router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[("store_site", "second.example")],
        ))
        .await
        .unwrap();
    assert_eq!(
        asked.headers()["location"],
        format!("{website}?site=second.example").as_str()
    );
    let changed = router
        .clone()
        .oneshot(cookie_form_request(
            &website,
            &cookie,
            &[
                ("site", "https://Second.example/x"),
                ("confirm", "Market stall & café"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::SEE_OTHER);
    assert_eq!(site_of(&state), "second.example");
}

/// A site is optional: a store with none has a name only. Done still lists
/// the checkout (add a site first), the plugin and the till, the till its
/// one primary; the dashboard's Site column is empty for it.
#[tokio::test]
async fn a_store_with_no_site_has_a_name_only() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let cookie = signed_up_and_logged_in_session_cookie(
        &router,
        "market@example.com",
        "correct horse battery staple",
    )
    .await;
    let made = crate::http::test_support::set_up_store_with_keys(
        &router,
        &cookie,
        &[
            ("store_name", "Saturday market stall"),
            ("store_site", "  "),
            ("view_key_hex", TEST_VIEW_KEY_HEX),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
        ],
    )
    .await;
    let store_id = crate::http::test_support::store_made(&made);
    let row = state
        .db
        .lock()
        .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(store_id.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(row.site, None);
    assert!(
        state
            .db
            .lock()
            .list_store_domains(&row.id)
            .unwrap()
            .is_empty(),
        "no domain to verify"
    );
    let done = body_text(
        cookie_get(
            &router,
            made.headers()["location"].to_str().unwrap(),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(done.contains("Saturday market stall is set up"), "{done}");
    assert!(done.contains(&format!(
        r#"<a class="mk-btn" href="/stores/{store_id}/pos" data-variant="primary">Open the till</a>"#
    )), "{done}");
    assert!(
        done.contains("add your site in the store's settings first"),
        "{done}"
    );
    assert!(!done.contains("Not taking payments yet"), "{done}");
    let dashboard = body_text(cookie_get(&router, "/", &cookie).await).await;
    assert!(
        dashboard.contains(&format!(
            r#"<h3 class="mk-card-title"><a href="/stores/{store_id}" data-store-link>Saturday market stall</a></h3><p class="mk-card-sub">No website</p>"#
        )),
        "no website: {dashboard}"
    );
}

/// The Store card's payment description: shown after the website, refused
/// with a line break (the card says why and keeps what was typed), saved
/// trimmed, and cleared by emptying it.
#[tokio::test]
async fn a_stores_payment_description_is_saved_from_its_settings() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let cookie = signed_up_and_logged_in_session_cookie(
        &router,
        "described-store@example.com",
        "correct horse battery staple",
    )
    .await;
    let made = crate::http::test_support::set_up_store_with_keys(
        &router,
        &cookie,
        &[
            ("store_name", "Corner shop"),
            ("store_site", "corner.example"),
            ("view_key_hex", TEST_VIEW_KEY_HEX),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
        ],
    )
    .await;
    let store = crate::http::test_support::store_made(&made);
    let settings = format!("/stores/{store}/settings");
    let page = body_text(cookie_get(&router, &settings, &cookie).await).await;
    let website = page.find("Your website (optional)").unwrap();
    let description = page.find("Payment description (optional)").unwrap();
    assert!(website < description, "{page}");
    assert!(
        page.contains("when an order has no description of its own"),
        "{page}"
    );

    let saved_description = |state: &AppState| {
        state
            .db
            .lock()
            .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(store.clone()))
            .unwrap()
            .unwrap()
            .payment_description
    };
    let refused = router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[("payment_description", "Coffee\nand cake")],
        ))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(refused).await;
    assert!(
        html.contains("A payment description must be one line of plain text"),
        "{html}"
    );
    assert!(html.contains("Coffee\nand cake"), "kept: {html}");
    assert_eq!(saved_description(&state), "");

    let saved = router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[("payment_description", "  Coffee and cake  ")],
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    assert!(saved.headers()["location"]
        .to_str()
        .unwrap()
        .ends_with("?saved=store"));
    assert_eq!(saved_description(&state), "Coffee and cake");
    let page = body_text(cookie_get(&router, &settings, &cookie).await).await;
    assert!(
        page.contains(
            r#"name="payment_description" id="payment-description" value="Coffee and cake""#
        ),
        "{page}"
    );

    let cleared = router
        .clone()
        .oneshot(cookie_form_request(
            &settings,
            &cookie,
            &[("payment_description", "")],
        ))
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::SEE_OTHER);
    assert_eq!(saved_description(&state), "");
}

/// The store step says what's wrong and keeps what was typed; a site any
/// store on the instance has is refused, linking to it when it's yours.
#[tokio::test]
async fn the_store_step_refuses_a_missing_name_a_bad_site_and_a_site_already_used() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);
    let cookie = signed_up_and_logged_in_session_cookie(
        &router,
        "corner@example.com",
        "correct horse battery staple",
    )
    .await;

    let response = router
        .clone()
        .oneshot(cookie_form_request(
            "/setup",
            &cookie,
            &[("store_name", ""), ("store_site", "ftp://x")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("Give the store a name."), "{html}");
    assert!(html.contains(r#"value="ftp://x""#), "kept: {html}");

    let made = crate::http::test_support::set_up_store_with_keys(
        &router,
        &cookie,
        &[
            ("store_name", "Corner shop"),
            ("store_site", "shop.example"),
            ("view_key_hex", TEST_VIEW_KEY_HEX),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
        ],
    )
    .await;
    let corner = crate::http::test_support::store_made(&made);

    let again = [
        ("store_name", "Another"),
        ("store_site", "https://shop.example/other-page"),
    ];
    let html = body_text(
        router
            .clone()
            .oneshot(cookie_form_request("/setup", &cookie, &again))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        html.contains(&format!(
            r#"Your store <a href="/stores/{corner}">Corner shop</a> already uses shop.example."#
        )),
        "{html}"
    );

    let other = signed_up_and_logged_in_session_cookie(
        &router,
        "someone-else@example.com",
        "correct horse battery staple",
    )
    .await;
    let html = body_text(
        router
            .clone()
            .oneshot(cookie_form_request("/setup", &other, &again))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        html.contains("shop.example is already connected to Monokulo here."),
        "{html}"
    );
    assert!(!html.contains(&corner), "nothing about whose it is: {html}");
}

/// A bad view key is refused on the keys screen, with the store's answers
/// kept, and nothing is made.
#[tokio::test]
async fn an_invalid_view_key_is_refused_on_the_keys_screen_and_nothing_is_made() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let cookie = signed_up_and_logged_in_session_cookie(
        &router,
        "bad-view-key@example.com",
        "correct horse battery staple",
    )
    .await;
    let response = crate::http::test_support::set_up_store_with_keys(
        &router,
        &cookie,
        &[
            ("store_name", "Shop"),
            ("store_site", "shop.example.com"),
            ("view_key_hex", "0707"),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("class=\"error\""), "{html}");
    assert!(
        html.contains(r#"name="store_site" value="shop.example.com""#),
        "{html}"
    );
    let user = state
        .db
        .lock()
        .get_user_by_email("bad-view-key@example.com")
        .unwrap()
        .unwrap();
    assert!(state
        .db
        .lock()
        .list_store_connections_for_user(&user.id)
        .unwrap()
        .is_empty());
    assert!(state.db.lock().list_wallets(&user.id).unwrap().is_empty());
}

/// A wallet already added comes first on the wallet step and makes the
/// store with one click; someone else's (or none) is refused.
#[tokio::test]
async fn a_store_can_use_a_wallet_already_added_but_only_your_own() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);
    let cookie = signed_up_and_logged_in_session_cookie(
        &router,
        "reuse-wallet@example.com",
        "correct horse battery staple",
    )
    .await;
    let wallet = add_test_wallet(&router, &cookie).await;

    let html = body_text(
        cookie_get(
            &router,
            "/setup/wallet?store_name=Shop&store_site=shop.example.com",
            &cookie,
        )
        .await,
    )
    .await;
    let already = html
        .find("Use a wallet you already added")
        .expect("offered first");
    assert!(already < html.find("or-divider").unwrap());
    assert!(html.find("or-divider").unwrap() < html.find(r#"name="name""#).unwrap());
    assert!(html.contains(&format!(r#"value="{wallet}""#)));

    let refused = crate::http::test_support::set_up_store_on_wallet(
        &router,
        &cookie,
        "Shop",
        "shop.example.com",
        "w_not_mine",
    )
    .await;
    assert_eq!(refused.status(), StatusCode::OK);
    assert!(body_text(refused)
        .await
        .contains("Choose one of your wallets."));

    let made = crate::http::test_support::set_up_store_on_wallet(
        &router,
        &cookie,
        "Shop",
        "shop.example.com",
        &wallet,
    )
    .await;
    crate::http::test_support::store_made(&made);
}

// -- `next`-redirect support on auth::login_submit ---------------------------
//
// The pure open-redirect-rejection proof lives in `auth.rs`'s own
// `#[cfg(test)] mod tests` (`is_safe_redirect_path`'s direct unit tests) -
// these are the HTTP-level complement: a real login request carrying a
// malicious `next` must not redirect anywhere attacker-controlled, and a
// real login with no `next` at all must behave exactly as it always has
// (the existing tests above already cover that implicitly, since none of
// them ever send a `next` field and all still pass unchanged).

#[tokio::test]
async fn a_successful_login_with_a_malicious_next_falls_back_to_the_default_confirmation_not_a_redirect(
) {
    let router = test_router();

    let email = "malicious-next@example.com";
    let password = "correct horse battery staple";
    let signup = router
        .clone()
        .oneshot(form_request(
            "/signup",
            &[("email", email), ("password", password)],
        ))
        .await
        .unwrap();
    assert_eq!(signup.status(), StatusCode::FOUND);

    for malicious_next in [
        "//evil.example.com",
        "https://evil.example.com",
        "/\\evil.example.com",
    ] {
        let response = router
            .clone()
            .oneshot(form_request(
                "/login",
                &[
                    ("email", email),
                    ("password", password),
                    ("next", malicious_next),
                ],
            ))
            .await
            .unwrap();
        // A malicious `next` must be silently ignored, falling back to the
        // exact same `/` redirect a `next`-less login gets - never
        // the attacker-controlled value. The response *is* a redirect
        // either way now that a real dashboard exists (see
        // `auth::login_submit`'s doc comment) - the load-bearing check
        // is the `Location` value, not whether a redirect happened at all.
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get("location").unwrap(),
            "/",
            "a malicious next ({malicious_next}) must never appear in Location - only the safe default"
        );
    }
}

/// The sign-up form, posted with `invite` as its invite token field.
fn invite_token_signup_request(
    email: &str,
    password: &str,
    invite_token: Option<&str>,
) -> Request<Body> {
    form_request(
        "/signup",
        &[
            ("email", email),
            ("password", password),
            ("invite", invite_token.unwrap_or("")),
        ],
    )
}

/// Whether a sign-up response made the account: a redirect with a session.
fn signed_up(response: &axum::response::Response) -> bool {
    response.status() == StatusCode::FOUND
        && response
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().starts_with("session="))
}

/// `signup.mode` gating at the sign-up form. The real single-use guarantee
/// through a `/request-invite` + admin "create invite link" flow is covered
/// end to end in `http::invites::tests`.
#[tokio::test]
async fn public_mode_signup_needs_no_invite_token_at_all() {
    // `test_app_state`'s own `seed_test_admin` already defaults to
    // `"public"` - this is the harness default every other test in this
    // file already relies on, asserted explicitly here as documentation.
    let router = test_router();
    let response = router
        .oneshot(invite_token_signup_request(
            "nobody-needs-an-invite@example.com",
            "correct horse battery staple",
            None,
        ))
        .await
        .unwrap();
    assert!(signed_up(&response), "{}", response.status());
}

#[tokio::test]
async fn invite_only_mode_rejects_a_signup_with_no_token() {
    // Invite-only is the default.
    let state = AppState {
        settings: crate::settings::MonokuloSettings::defaults(),
        ..AppState::for_tests()
    };
    let db = state.db.clone();
    let router = build_router(state);

    let response = router
        .oneshot(invite_token_signup_request(
            "hopeful@example.com",
            "correct horse battery staple",
            None,
        ))
        .await
        .unwrap();
    // The form again, in its "you need an invite" state; no account made.
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!signed_up(&response));
    let html = body_text(response).await;
    assert!(html.contains("invite"), "{html}");
    assert!(db
        .lock()
        .get_user_by_email("hopeful@example.com")
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn invite_only_mode_accepts_a_valid_token_exactly_once() {
    let state = AppState {
        settings: crate::settings::MonokuloSettings::defaults(),
        ..AppState::for_tests()
    };
    let db = state.db.clone();
    let router = build_router(state);
    let raw_token = credentials::auth::generate_invite_token();
    db.lock()
        .create_invite_link("link-1", &raw_token.hash(), None, None, crate::now_unix())
        .unwrap();

    let first = router
        .clone()
        .oneshot(invite_token_signup_request(
            "first@example.com",
            "correct horse battery staple",
            Some(raw_token.expose()),
        ))
        .await
        .unwrap();
    assert!(signed_up(&first), "{}", first.status());

    // The literal ask: "more than one account cannot be registered using
    // the same link".
    let second = router
        .oneshot(invite_token_signup_request(
            "second@example.com",
            "correct horse battery staple",
            Some(raw_token.expose()),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert!(!signed_up(&second));
    let html = body_text(second).await;
    assert!(
        html.contains("already been used"),
        "expected a clear already-used error, got: {html}"
    );
}

/// A state-changing request that a browser says came from another site is
/// refused on every route of the site's own, login included (login CSRF
/// would sign the victim into the attacker's account); the site's own
/// forms and requests carrying an `Authorization` header are untouched.
#[tokio::test]
async fn cross_site_posts_to_the_sites_own_routes_are_refused() {
    let state = AppState::for_tests();
    let router = build_router(state);
    let post = |uri: &str, headers: &[(&str, &str)]| {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "pay.example")
            .header("content-type", "application/x-www-form-urlencoded");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder
            .body(Body::from("email=a%40b.example&password=x"))
            .unwrap()
    };
    for uri in ["/login", "/logout", "/theme", "/admin/settings"] {
        let response = router
            .clone()
            .oneshot(post(uri, &[("origin", "https://evil.example")]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri} by Origin");
        let response = router
            .clone()
            .oneshot(post(uri, &[("sec-fetch-site", "cross-site")]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{uri} by Sec-Fetch-Site"
        );
    }
    // The site's own form reaches the handler (which answers for itself).
    let response = router
        .clone()
        .oneshot(post(
            "/logout",
            &[
                ("origin", "https://pay.example"),
                ("sec-fetch-site", "same-origin"),
            ],
        ))
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
    // The site takes no bearer tokens, so an `Authorization` header buys a
    // cross-site request nothing.
    let response = router
        .clone()
        .oneshot(post(
            "/logout",
            &[
                ("origin", "https://evil.example"),
                ("authorization", "Bearer nope"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // The machine API is for other sites by design: without a key it is
    // refused for that, not for where it came from.
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/orders")
                .header("host", "pay.example")
                .header("origin", "https://shop.example")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// An embed works on any site - clearnet or `.onion` - so the machine API
/// and the checkout pages of a store that hasn't restricted embedding
/// answer CORS for any origin (echoing it back), preflight included, and
/// never allow credentials. The dashboard stays same-origin only.
#[tokio::test]
async fn public_embed_routes_allow_any_origin_and_the_dashboard_does_not() {
    let router = test_router();
    let onion = "http://shopexampleabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion";

    let preflight = router
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/orders")
                .header("origin", onion)
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "content-type, authorization",
                )
                .header("access-control-request-private-network", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        preflight.status().is_success(),
        "got {}",
        preflight.status()
    );
    let headers = preflight.headers();
    assert_eq!(headers["access-control-allow-origin"], onion);
    assert!(headers["access-control-allow-methods"]
        .to_str()
        .unwrap()
        .contains("POST"));
    assert!(headers["access-control-allow-headers"]
        .to_str()
        .unwrap()
        .contains("content-type"));
    assert_eq!(headers["access-control-allow-private-network"], "true");
    assert!(!headers.contains_key("access-control-allow-credentials"));

    // Error responses carry it too, so a caller can read why it failed.
    for (uri, allowed) in [
        ("/api/v1/orders/order_x", "https://shop.example"),
        ("/checkout/order_x/events", "https://shop.example"),
        ("/static/monokulo-client.js", "*"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("origin", "https://shop.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some(allowed),
            "{uri}"
        );
    }

    let dashboard = router
        .oneshot(
            Request::builder()
                .uri("/setup")
                .header("origin", "https://shop.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!dashboard
        .headers()
        .contains_key("access-control-allow-origin"));
}

// -- Alerts for stores that can't be scanned (admin_settings_v2.md task 3.7) --

/// A store row becomes an `OwnedStore` only for its own owner.
#[test]
fn a_store_is_owned_only_by_the_user_it_belongs_to() {
    let (state, owner, other) = state_with_owner_and_store("pk_owned");
    let row = || {
        state
            .db
            .lock()
            .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new("c1"))
            .unwrap()
            .unwrap()
    };
    let owned = super::OwnedStore::check(row(), &owner.id).expect("the owner owns it");
    assert_eq!(owned.id, "c1");
    assert!(super::OwnedStore::check(row(), &other.id).is_none());
}

fn state_with_owner_and_store(
    tenant_public_key: &str,
) -> (AppState, crate::db::UserRow, crate::db::UserRow) {
    let state = AppState::for_tests();
    {
        let db = state.db.lock();
        db.create_user(
            &engine_protocol::ids::UserId::new("u_owner"),
            "owner@example.com",
            "x",
            false,
            1,
        )
        .unwrap();
        db.create_user(
            &engine_protocol::ids::UserId::new("u_other"),
            "other@example.com",
            "x",
            false,
            1,
        )
        .unwrap();
        crate::db::test_rows::store_on_wallet(
            &db,
            &engine_protocol::ids::ConnectionId::new("c1"),
            &engine_protocol::ids::UserId::new("u_owner"),
            "shop.example.com",
            tenant_public_key,
            "enc",
            1,
            "XMR",
        )
        .unwrap();
    }
    let owner = state
        .db
        .lock()
        .get_user_by_email("owner@example.com")
        .unwrap()
        .unwrap();
    let other = state
        .db
        .lock()
        .get_user_by_email("other@example.com")
        .unwrap()
        .unwrap();
    (state, owner, other)
}

fn status_with(
    unserved: Vec<crate::engine_client::UnservedTenant>,
) -> crate::engine_client::EngineStatusResponse {
    crate::engine_client::EngineStatusResponse {
        networks: vec![],
        poll_interval_secs: 1,
        generated_at: 0,
        unserved_tenants: unserved,
        key_custody: vec![],
        key_custody_default: None,
        key_custody_snp_trust: None,
        resources: None,
    }
}

#[tokio::test]
async fn the_owner_of_a_store_on_a_network_without_a_node_is_alerted_on_every_page_but_nobody_else_is(
) {
    let (state, owner, other) = state_with_owner_and_store("pk_shop");
    crate::http::status_page::seed_status_for_tests(
        &state.engine,
        status_with(vec![crate::engine_client::UnservedTenant {
            public_key: "pk_shop".into(),
            network: "stagenet".into(),
            reason: "no_reachable_node".into(),
            blocks_behind: None,
        }]),
    );

    let chrome = super::page_chrome(&state, Some(&owner), "/").await;
    assert_eq!(chrome.alerts.len(), 1);
    assert!(
        chrome.alerts[0].contains("shop.example.com") && chrome.alerts[0].contains("stagenet"),
        "{:?}",
        chrome.alerts
    );

    assert!(
        super::page_chrome(&state, Some(&other), "/")
            .await
            .alerts
            .is_empty(),
        "not someone else's store"
    );
    assert!(
        super::page_chrome(&state, None, "/")
            .await
            .alerts
            .is_empty(),
        "never for a visitor"
    );

    // Rendered under the nav on normal pages, never in the bare POS layout.
    let page = crate::views::Page::new("t", ui::PageHeader::new("t"))
        .render(&chrome, maud::html! { p { "body" } })
        .into_string();
    assert!(
        page.contains(r#"<p class="error" role="alert">shop.example.com"#),
        "{page}"
    );
    let pos = crate::views::layout_bare_with_head(
        &chrome,
        "t",
        "width=device-width",
        maud::html! {},
        maud::html! {},
    )
    .into_string();
    assert!(
        !pos.contains("shop.example.com"),
        "the POS terminal shows no alerts"
    );
}

#[tokio::test]
async fn a_store_catching_up_gets_a_gentler_alert_and_it_goes_away_once_it_has() {
    let (state, owner, _) = state_with_owner_and_store("pk_shop");
    crate::http::status_page::seed_status_for_tests(
        &state.engine,
        status_with(vec![crate::engine_client::UnservedTenant {
            public_key: "pk_shop".into(),
            network: "mainnet".into(),
            reason: "catching_up".into(),
            blocks_behind: Some(12),
        }]),
    );
    let alerts = super::page_chrome(&state, Some(&owner), "/").await.alerts;
    assert!(alerts[0].contains("catching up 12 block"), "{alerts:?}");

    crate::http::status_page::seed_status_for_tests(&state.engine, status_with(vec![]));
    assert!(super::page_chrome(&state, Some(&owner), "/")
        .await
        .alerts
        .is_empty());
}

#[tokio::test]
async fn a_store_whose_key_storage_is_off_or_down_gets_an_alert_saying_so() {
    let (state, owner, _) = state_with_owner_and_store("pk_shop");
    for (reason, expected) in [
        ("custody_disabled", "turned off"),
        ("custody_unavailable", "isn't answering"),
    ] {
        crate::http::status_page::seed_status_for_tests(
            &state.engine,
            status_with(vec![crate::engine_client::UnservedTenant {
                public_key: "pk_shop".into(),
                network: "mainnet".into(),
                reason: reason.into(),
                blocks_behind: None,
            }]),
        );
        let alerts = super::page_chrome(&state, Some(&owner), "/").await.alerts;
        assert_eq!(alerts.len(), 1);
        assert!(
            alerts[0].contains("shop.example.com") && alerts[0].contains(expected),
            "{alerts:?}"
        );
    }
}

#[tokio::test]
async fn browser_reports_are_accepted_up_to_a_small_size_and_only_the_sites_own_pages_send_them() {
    let router = test_router();
    let report = |body: String| {
        Request::builder()
            .method("POST")
            .uri("/telemetry/client")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    let small = r#"{"kind":"error","message":"x is undefined","page":"/"}"#.to_string();
    assert_eq!(
        router
            .clone()
            .oneshot(report(small))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let big = format!(r#"{{"kind":"error","message":"{}"}}"#, "x".repeat(20_000));
    assert_eq!(
        router.clone().oneshot(report(big)).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let script = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(crate::assets::url("telemetry.js"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::OK);

    let chrome = crate::views::PageChrome::from_user(None, "/");
    let page = crate::views::Page::new("t", ui::PageHeader::new("t"))
        .render(&chrome, maud::html! {})
        .into_string();
    assert!(
        page.contains(&format!(r#"src="{}""#, crate::assets::url("telemetry.js"))),
        "{page}"
    );
    let bare = crate::views::layout_bare(&chrome, "t", maud::html! {}).into_string();
    assert!(
        !bare.contains("telemetry.js"),
        "not on the checkout, challenge or POS pages: {bare}"
    );
}

#[tokio::test]
async fn store_pages_load_browser_reports_only_once_the_store_opted_in_but_admin_pages_always_do() {
    let (state, owner, _) = state_with_owner_and_store("pk_shop");
    let with_script = async |path: &str| {
        let chrome = super::page_chrome(&state, Some(&owner), path).await;
        crate::views::Page::new("t", ui::PageHeader::new("t"))
            .render(&chrome, maud::html! {})
            .into_string()
            .contains(&crate::assets::url("telemetry.js"))
    };
    assert!(with_script("/admin/logs").await);
    assert!(with_script("/").await);
    assert!(with_script("/setup").await);
    assert!(!with_script("/stores/c1").await);
    assert!(!with_script("/stores/c1/settings").await);
    state
        .db
        .lock()
        .set_client_logging(&engine_protocol::ids::ConnectionId::new("c1"), true)
        .unwrap();
    assert!(with_script("/stores/c1").await);
    assert!(with_script("/stores/c1/orders?page=2").await);
}

#[tokio::test]
async fn the_plugins_forwarded_errors_are_refused_until_the_store_opted_in() {
    let state = AppState::for_tests();
    {
        let db = state.db.lock();
        db.create_user(
            &engine_protocol::ids::UserId::new("u_owner"),
            "owner@example.com",
            "x",
            false,
            1,
        )
        .unwrap();
        let encrypted = crate::crypto::encrypt(
            &state.encryption_key,
            crate::crypto::Binding::StoreSecret("c1"),
            "sk_shop",
        );
        crate::db::test_rows::store_on_wallet(
            &db,
            &engine_protocol::ids::ConnectionId::new("c1"),
            &engine_protocol::ids::UserId::new("u_owner"),
            "https://shop.example.com",
            "pk_shop",
            &encrypted,
            1,
            "XMR",
        )
        .unwrap();
        db.set_secret_key_hash(
            &engine_protocol::ids::ConnectionId::new("c1"),
            &credentials::auth::RawToken::presented("sk_shop").hash(),
        )
        .unwrap();
    }
    let router = build_router(state.clone());
    let forward = || {
        Request::builder()
            .method("POST")
            .uri("/api/v1/logs")
            .header("content-type", "application/json")
            .header("authorization", "Bearer sk_shop")
            .body(Body::from(
                r#"{"entries":[{"level":"error","message":"webhook signature mismatch"}]}"#,
            ))
            .unwrap()
    };
    assert_eq!(
        router.clone().oneshot(forward()).await.unwrap().status(),
        StatusCode::FORBIDDEN,
        "off by default, whatever the plugin says"
    );
    state
        .db
        .lock()
        .set_client_logging(&engine_protocol::ids::ConnectionId::new("c1"), true)
        .unwrap();
    assert_eq!(
        router.clone().oneshot(forward()).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn the_pos_timeline_is_taken_only_from_the_stores_owner_once_it_opted_in() {
    let (state, _, _) = state_with_owner_and_store("pk_shop");
    {
        let db = state.db.lock();
        db.create_session(
            &credentials::auth::RawToken::presented("owner-token").hash(),
            &engine_protocol::ids::UserId::new("u_owner"),
            crate::now_unix(),
        )
        .unwrap();
        db.create_session(
            &credentials::auth::RawToken::presented("other-token").hash(),
            &engine_protocol::ids::UserId::new("u_other"),
            crate::now_unix(),
        )
        .unwrap();
    }
    let router = build_router(state.clone());
    let send = |token: &str, body: String| {
        Request::builder()
            .method("POST")
            .uri("/stores/c1/pos/logs")
            .header("cookie", format!("session={token}"))
            .header("content-type", "text/plain")
            .body(Body::from(body))
            .unwrap()
    };
    let batch = |events: usize| {
        let events: Vec<_> = (1..=events)
            .map(|seq| serde_json::json!({ "seq": seq, "t": 1_790_000_000_000i64 + seq as i64, "kind": "network.offline", "detail": { "online": false } }))
            .collect();
        serde_json::json!({ "session": "0f8e2c1a-3b4d-4e5f-9a6b-7c8d9e0f1a2b", "events": events })
            .to_string()
    };
    let status = |request: Request<Body>| {
        let router = router.clone();
        async move { router.oneshot(request).await.unwrap().status() }
    };
    assert_eq!(
        status(send("owner-token", batch(2))).await,
        StatusCode::FORBIDDEN,
        "Diagnostics is off"
    );
    state
        .db
        .lock()
        .set_client_logging(&engine_protocol::ids::ConnectionId::new("c1"), true)
        .unwrap();
    assert_eq!(
        status(send("owner-token", batch(2))).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(send("owner-token", batch(101))).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        status(send("owner-token", r#"{"session":"x","events":[]}"#.into())).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        status(send("other-token", batch(1))).await,
        StatusCode::NOT_FOUND,
        "someone else's store"
    );
}

#[tokio::test]
async fn the_status_page_names_its_stream_and_the_stream_sends_the_page_content() {
    let router = test_router();
    let page = body_text(
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
    assert!(
        page.contains(r#"fx-action="/status/events" fx-trigger="fx:inited""#),
        "{page}"
    );
    assert!(
        page.contains(r#"<div id="status-live">"#)
            && page.contains(r#"aria-label="Refresh page" title="Refresh page" data-reload"#),
        "{page}"
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri("/status/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    let mut text = String::new();
    while !text.contains("\n\n") {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Some(data) = frame.data_ref() {
            text.push_str(std::str::from_utf8(data).unwrap());
        }
    }
    assert!(
        text.starts_with(r##"event: {"target":"#status-live","swap":"outerHTML"}"##),
        "{text}"
    );
    assert!(text.contains(r#"data: <div id="status-live">"#), "{text}");
}
