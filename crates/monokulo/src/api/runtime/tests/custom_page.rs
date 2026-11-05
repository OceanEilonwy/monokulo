//! A store's custom page through the API: who may touch it, what a refusal says, and
//! that the secret key alone can't change what customers see.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::super::recording::{fixture, Fixture, Recorder, PUBLIC_URI};
use crate::http::test_support::{api_request, body_json};
use crate::http::{build_router, AppState};
use domain_proof::test_support::{proofs, FakeDns};

const PAGE: &str = "/api/v1/checkout-page";
const DRAFT: &str = "/api/v1/checkout-page/draft";
const CHECK: &str = "/api/v1/checkout-page/draft/check";
const PUBLISH: &str = "/api/v1/checkout-page/draft/publish";
const TEMPLATE: &str = "<h1>Ours</h1>{{status}}{{if awaiting}}{{qr}}{{amount}}{{fiat}}{{address}}{{end}}{{if !awaiting}}<p>Done</p>{{end}}";

/// A store with a verified `domain`, and DNS a test can put proofs in.
struct Shop {
    router: axum::Router,
    state: AppState,
    fixture: Fixture,
    dns: Arc<FakeDns>,
    _engine: engine_test_support::TestEngineHandle,
}

impl Shop {
    async fn new(domain: Option<&str>) -> Shop {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
        let dns = Arc::new(FakeDns::default());
        let state = AppState {
            proofs: proofs(dns.clone(), Arc::default()),
            ..fixture.state.clone()
        };
        // Only the domain this test says: the fixture's own isn't verified.
        let at = crate::now_unix();
        {
            let db = state.db.lock();
            let conn = db.conn_for_test();
            conn.execute(
                "DELETE FROM store_domains WHERE connection_id = ?1",
                [&fixture.store_id],
            )
            .unwrap();
            if let Some(domain) = domain {
                conn.execute(
                    "INSERT INTO store_domains (id, connection_id, domain, token, created_at_utc, verified_at_utc)
                     VALUES (?1, ?2, ?3, 't', ?4, ?4)",
                    rusqlite::params![domain, fixture.store_id, domain, at],
                )
                .unwrap();
            }
        }
        Shop {
            router: build_router(state.clone()),
            state,
            fixture,
            dns,
            _engine: engine,
        }
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(api_request(method, uri, key, body))
            .await
            .unwrap()
    }

    async fn sk(&self, method: &str, uri: &str, body: Option<Value>) -> axum::response::Response {
        self.call(method, uri, Some(&self.fixture.sk), body).await
    }

    /// Saves `template`, and answers with the draft.
    async fn save(&self, template: &str) -> Value {
        let saved = self
            .sk("POST", DRAFT, Some(json!({"template": template})))
            .await;
        assert_eq!(saved.status(), StatusCode::OK);
        body_json(saved).await
    }

    fn approve(&self, draft: &Value) {
        let approval = &draft["approval"];
        self.dns.publish(
            approval["name"].as_str().unwrap(),
            approval["value"].as_str().unwrap(),
        );
    }

    fn slug_of(body: &Value) -> &str {
        body["type"].as_str().unwrap().rsplit('/').next().unwrap()
    }
}

/// It's the merchant's server's alone: a page in a shop holds the public key, and an
/// app has no business changing the checkout.
#[tokio::test]
async fn only_the_secret_key_has_the_custom_page() {
    let shop = Shop::new(Some("shop.example")).await;
    let body = || Some(json!({"template": TEMPLATE}));
    for (method, uri, body) in [
        ("GET", PAGE, None),
        ("DELETE", PAGE, None),
        ("GET", DRAFT, None),
        ("POST", DRAFT, body()),
        ("DELETE", DRAFT, None),
        ("POST", CHECK, None),
        ("POST", PUBLISH, None),
    ] {
        for key in [&shop.fixture.pk, &shop.fixture.ak] {
            let answer = shop.call(method, uri, Some(key), body.clone()).await;
            assert_eq!(answer.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        }
        let answer = shop.call(method, uri, None, body.clone()).await;
        assert_eq!(answer.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
    assert_eq!(
        shop.sk("GET", DRAFT, None).await.status(),
        StatusCode::NOT_FOUND,
        "nothing above saved anything"
    );
}

/// A refusal says what and where, and keeps what was there.
#[tokio::test]
async fn a_refused_draft_says_what_and_where_and_saves_nothing() {
    let shop = Shop::new(Some("shop.example")).await;
    let kept = shop.save(TEMPLATE).await;

    let refused = shop
        .sk(
            "POST",
            DRAFT,
            Some(json!({"template": "{{status}}\n{{nope}}", "css": "h1{color:red}"})),
        )
        .await;
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(refused).await;
    assert_eq!(Shop::slug_of(&body), "checkout-page-refused");
    let errors = body["errors"].as_array().unwrap();
    let on_line_two = errors
        .iter()
        .find(|error| error["line"] == 2)
        .unwrap_or_else(|| panic!("{errors:?}"));
    assert_eq!(on_line_two["place"], "template");
    assert!(on_line_two["message"].as_str().unwrap().contains("nope"));

    let draft = body_json(shop.sk("GET", DRAFT, None).await).await;
    assert_eq!(draft["version"], kept["version"]);
}

/// The whole way: the key saves and asks, but new words reach customers only once the
/// proof of that very version is on the domain.
#[tokio::test]
async fn the_key_alone_cant_put_new_words_in_front_of_customers() {
    let shop = Shop::new(Some("shop.example")).await;
    let first = shop.save(TEMPLATE).await;
    assert_eq!(first["approval"]["kind"], "dns");
    assert_eq!(first["approval"]["domain"], "shop.example");
    assert_eq!(first["approval"]["name"], "_monokulo-template.shop.example");

    let check = body_json(shop.sk("POST", CHECK, None).await).await;
    assert_eq!(
        (&check["found"], &check["answered"]),
        (&json!(false), &json!(true))
    );
    let refused = shop.sk("POST", PUBLISH, None).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(
        Shop::slug_of(&body_json(refused).await),
        "checkout-page-not-approved"
    );
    assert_eq!(
        shop.sk("GET", PAGE, None).await.status(),
        StatusCode::NOT_FOUND
    );

    shop.approve(&first);
    let check = body_json(shop.sk("POST", CHECK, None).await).await;
    assert_eq!(check["found"], true);
    let published = shop.sk("POST", PUBLISH, None).await;
    assert_eq!(published.status(), StatusCode::OK);
    let page = body_json(published).await;
    assert_eq!(page["status"], "working");
    assert_eq!(page["version"], first["version"]);

    // Another version's proof doesn't approve the next, and what's in use stays.
    let second = shop.save(&format!("{TEMPLATE}<p>Now with more</p>")).await;
    assert_ne!(second["version"], first["version"]);
    assert_eq!(
        shop.sk("POST", PUBLISH, None).await.status(),
        StatusCode::CONFLICT
    );
    let page = body_json(shop.sk("GET", PAGE, None).await).await;
    assert_eq!(page["version"], first["version"]);
}

/// An onion address has no DNS, so a store with only that approves with a file.
#[tokio::test]
async fn an_onion_store_approves_with_a_file() {
    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    let shop = Shop::new(Some(ONION)).await;
    let draft = shop.save(TEMPLATE).await;
    assert_eq!(draft["approval"]["kind"], "file");
    assert_eq!(
        draft["approval"]["name"],
        format!("http://{ONION}/.well-known/monokulo-template")
    );
}

/// Without a verified domain nobody could approve a draft, and its preview would be
/// anyone's words at this Monokulo's address.
#[tokio::test]
async fn a_store_with_no_domain_cant_save_a_draft() {
    let shop = Shop::new(None).await;
    let refused = shop
        .sk("POST", DRAFT, Some(json!({"template": TEMPLATE})))
        .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(
        Shop::slug_of(&body_json(refused).await),
        "checkout-page-needs-domain"
    );
    for (method, uri) in [("GET", DRAFT), ("POST", CHECK), ("POST", PUBLISH)] {
        assert_eq!(
            shop.sk(method, uri, None).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}

/// What the dashboard can save, the API can save again, escapes and all, and no more.
#[tokio::test]
async fn a_page_at_its_limits_can_be_saved_and_a_larger_body_cant() {
    let shop = Shop::new(Some("shop.example")).await;
    let css = format!("p{{color:red}}\n{}", "/*\"\"*/".repeat(5000));
    assert!(css.len() < checkout_template::css::MAX_CSS_BYTES);
    let body = json!({"template": TEMPLATE, "css": css});
    assert!(body.to_string().len() > crate::api::runtime::MAX_BODY_BYTES);
    let saved = shop.sk("POST", DRAFT, Some(body)).await;
    assert_eq!(saved.status(), StatusCode::OK);
    let huge = json!({"template": "x".repeat(crate::api::runtime::MAX_CUSTOM_PAGE_BODY_BYTES)});
    let refused = shop.sk("POST", DRAFT, Some(huge)).await;
    assert_eq!(refused.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// The link goes to someone who isn't signed in, so it's whole.
#[tokio::test]
async fn a_drafts_preview_link_can_be_sent_as_it_is() {
    let shop = Shop::new(Some("shop.example")).await;
    let draft = shop.save(TEMPLATE).await;
    let link = draft["preview_url"].as_str().unwrap();
    assert!(
        link.starts_with(&format!("{PUBLIC_URI}/checkout-preview/")),
        "{link}"
    );
    assert_eq!(
        draft["expires_at"].as_i64().unwrap() - draft["created_at"].as_i64().unwrap(),
        crate::db::DRAFT_SECS
    );
}

/// Each limit, and how long to wait, as a client reads it. Saves and lookups count
/// apart, and publishing shares the lookups'.
#[tokio::test]
async fn saves_and_lookups_are_limited_per_minute() {
    let shop = Shop::new(Some("shop.example")).await;
    let body = || Some(json!({"template": TEMPLATE}));
    for _ in 0..crate::http::checkout_pages::SAVES_PER_MINUTE {
        assert_eq!(
            shop.sk("POST", DRAFT, body()).await.status(),
            StatusCode::OK
        );
    }
    let past = shop.sk("POST", DRAFT, body()).await;
    assert_eq!(past.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(past.headers()["retry-after"], "60");
    assert_eq!(body_json(past).await["retry_after"], 60);

    for _ in 0..crate::http::checkout_pages::CHECKS_PER_MINUTE {
        assert_eq!(shop.sk("POST", CHECK, None).await.status(), StatusCode::OK);
    }
    for uri in [CHECK, PUBLISH] {
        let past = shop.sk("POST", uri, None).await;
        assert_eq!(past.status(), StatusCode::TOO_MANY_REQUESTS, "{uri}");
        assert_eq!(past.headers()["retry-after"], "60");
    }
}

/// A draft saved by an earlier release can't go live on its old approval once it
/// fails today's checks, and the refusal says so rather than asking for a proof.
#[tokio::test]
async fn a_draft_that_no_longer_passes_is_refused_as_such() {
    let shop = Shop::new(Some("shop.example")).await;
    let store = crate::db::ConnectionId::new(shop.fixture.store_id.clone());
    shop.state
        .db
        .lock()
        .save_checkout_page_draft(&store, "{{status}}", "", "", "older", crate::now_unix())
        .unwrap();
    let refused = shop.sk("POST", PUBLISH, None).await;
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(refused).await;
    assert_eq!(Shop::slug_of(&body), "checkout-page-refused");
    assert!(!body["errors"].as_array().unwrap().is_empty());
}

/// An operator's decision isn't the merchant's to undo, however they ask.
#[tokio::test]
async fn a_turned_off_page_stays_off() {
    let shop = Shop::new(Some("shop.example")).await;
    let draft = shop.save(TEMPLATE).await;
    shop.approve(&draft);
    assert_eq!(
        shop.sk("POST", PUBLISH, None).await.status(),
        StatusCode::OK
    );
    let store = crate::db::ConnectionId::new(shop.fixture.store_id.clone());
    assert!(shop
        .state
        .db
        .lock()
        .set_checkout_page_turned_off(&store, Some((crate::now_unix(), "Wrong address")))
        .unwrap());

    let page = body_json(shop.sk("GET", PAGE, None).await).await;
    assert_eq!(page["status"], "turned-off");
    assert_eq!(page["turned_off_reason"], "Wrong address");
    let stopping = shop.sk("DELETE", PAGE, None).await;
    assert_eq!(stopping.status(), StatusCode::FORBIDDEN);
    // Told at once, with no proof to go and add.
    shop.save(&format!("{TEMPLATE}<p>Honest</p>")).await;
    let publishing = shop.sk("POST", PUBLISH, None).await;
    assert_eq!(publishing.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        Shop::slug_of(&body_json(publishing).await),
        "checkout-page-turned-off"
    );
}
