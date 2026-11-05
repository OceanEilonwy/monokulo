use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use domain_proof::test_support::FakeDns;
use tower::ServiceExt;

use super::*;
use crate::http::test_support::{
    body_text, form_post, signed_up_and_logged_in_session_token, store_with_keys,
};
use crate::http::{build_router, Engine};

const GOOD: &str = "{{status}}{{qr}}<p>{{amount}} {{fiat}}</p>{{address}}<div class=\"pay\"></div>";

struct Wizard {
    state: AppState,
    router: Router,
    cookie: String,
    store: ConnectionId,
    _engine: engine_test_support::TestEngineHandle,
}

impl Wizard {
    fn page(&self) -> String {
        format!("/stores/{}/settings/checkout-page", self.store)
    }

    async fn get(&self, uri: &str) -> (StatusCode, String) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", self.cookie.as_str())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        (status, body_text(response).await)
    }

    async fn frame(&self, uri: &str, dest: &str, site: &str) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("sec-fetch-dest", dest)
                    .header("sec-fetch-site", site)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn save(&self, template: &str, css: &str) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(form_post(
                &self.page(),
                Some(&self.cookie),
                &[("template", template), ("css", css)],
            ))
            .await
            .unwrap()
    }

    fn draft(&self) -> Option<crate::db::CheckoutPageDraft> {
        self.state
            .db
            .lock()
            .checkout_page_draft(&self.store, crate::now_unix())
            .unwrap()
    }
}

async fn wizard(email: &str, verified: bool) -> Wizard {
    let dns = Arc::new(FakeDns::default());
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[monero::Network::Mainnet])
        .spawn()
        .await;
    let state = AppState {
        proofs: domain_proof::test_support::proofs(dns.clone(), Arc::default()),
        engine: Engine::new(crate::engine_client::EngineClient::embedded_for_tests(
            engine.router(),
        )),
        ..AppState::for_tests()
    };
    let router = build_router(state.clone());
    let session =
        signed_up_and_logged_in_session_token(&router, email, "correct horse battery staple").await;
    let store = store_with_keys(&router, &state, &session, "shop.example", "XMR").await;
    let store = ConnectionId::new(store.id);
    if verified {
        let row = state
            .db
            .lock()
            .list_store_domains(&store)
            .unwrap()
            .remove(0);
        dns.publish(
            &domain_proof::VERIFY.record_name(&row.domain),
            &domain_proof::VERIFY.value(&row.token),
        );
        crate::embed_domains::check_and_record(&state.db, &state.proofs, &row, crate::now_unix())
            .await
            .unwrap();
    }
    Wizard {
        state,
        router,
        cookie: format!("session={session}"),
        store,
        _engine: engine,
    }
}

#[tokio::test]
async fn a_custom_page_needs_a_verified_domain() {
    let wizard = wizard("no-domain@example.com", false).await;
    let (status, _) = wizard.get(&wizard.page()).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(wizard.save(GOOD, "").await.status(), StatusCode::SEE_OTHER);
    assert_eq!(wizard.draft(), None, "nothing saved");
}

#[tokio::test]
async fn a_draft_is_saved_and_previewed_and_what_is_wrong_is_said_with_nothing_saved() {
    let wizard = wizard("drafts@example.com", true).await;
    let (status, html) = wizard.get(&wizard.page()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("{{merchant}}"),
        "starts from a stock layout: {html}"
    );

    // Several things wrong at once, keeping what was typed, saving nothing.
    let bad = wizard
        .save(
            "{{status}}{{qr}}<p>{{countdown}}</p>",
            ".pay{background:url(x)}",
        )
        .await;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(bad).await;
    assert!(
        html.contains("{{countdown}} can&#39;t go inside")
            || html.contains("{{countdown}} can't go inside"),
        "{html}"
    );
    assert!(html.contains("{{status}}{{qr}}&lt;p&gt;"), "{html}");
    assert_eq!(wizard.draft(), None);

    let good = wizard.save(GOOD, ".pay{color:red}").await;
    assert_eq!(good.status(), StatusCode::SEE_OTHER);
    let draft = wizard.draft().unwrap();
    assert_eq!(draft.template, GOOD);
    assert_eq!(draft.written_css, "#checkout-root .pay{color:red;}");
    let (_, html) = wizard.get(&format!("{}/preview", wizard.page())).await;
    assert!(html.contains(&draft.preview_token), "{html}");

    // A new save means a new link, and an old one stops working.
    let old = draft.preview_token;
    wizard.save(GOOD, "").await;
    let new = wizard.draft().unwrap().preview_token;
    assert_ne!(old, new);
    let (status, _) = wizard.get(&format!("/checkout-preview/{old}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, html) = wizard
        .get(&format!("/checkout-preview/{new}?state=paid"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Preview only"), "{html}");
}

/// A draft's page can only be seen as a sandboxed frame of this Monokulo's own
/// pages: never at its own address, where it would pass for the real thing.
#[tokio::test]
async fn a_drafts_frame_is_only_ever_a_sandboxed_frame_of_this_site() {
    let wizard = wizard("frames@example.com", true).await;
    wizard.save(GOOD, ".pay{color:red}").await;
    let draft = wizard.draft().unwrap();
    let uri = format!("/checkout-preview/{}/frame?state=paid", draft.preview_token);

    for (dest, site) in [
        ("document", "none"),
        ("document", "same-origin"),
        ("iframe", "cross-site"),
        ("iframe", "same-site"),
    ] {
        assert_eq!(
            wizard.frame(&uri, dest, site).await.status(),
            StatusCode::FORBIDDEN,
            "{dest} {site}"
        );
    }
    let bare = wizard
        .router
        .clone()
        .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(bare.status(), StatusCode::FORBIDDEN, "no headers, no draft");
    // Over plain http there's no fetch metadata: a preview's own page says where
    // it's from, and nothing else does.
    let with_referer = |referer: &'static str| {
        Request::builder()
            .uri(&uri)
            .header("host", "shop.test:8080")
            .header("referer", referer)
            .body(Body::empty())
            .unwrap()
    };
    for (referer, status) in [
        ("http://shop.test:8080/checkout-preview/abc", StatusCode::OK),
        (
            "http://shop.test:8080/stores/s/settings/checkout-page/preview",
            StatusCode::OK,
        ),
        (
            "http://evil.example/checkout-preview/abc",
            StatusCode::FORBIDDEN,
        ),
        (
            "http://shop.test:8080/somewhere-else",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = wizard
            .router
            .clone()
            .oneshot(with_referer(referer))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{referer}");
    }

    let framed = wizard.frame(&uri, "iframe", "same-origin").await;
    assert_eq!(framed.status(), StatusCode::OK);
    let policy = framed.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        policy.starts_with("sandbox; default-src 'none'"),
        "{policy}"
    );
    assert!(policy.contains("frame-ancestors 'self'"), "{policy}");
    assert_eq!(framed.headers()["x-robots-tag"], "noindex, nofollow");
    let html = body_text(framed).await;
    assert!(html.contains(&format!(
        "/static-style/{}.css",
        stylesheet_hash(&draft.written_css)
    )));
    assert!(!html.contains("<script"), "a preview runs nothing: {html}");
    assert!(
        !html.contains("checkout-stream"),
        "nothing follows it: {html}"
    );
}

#[tokio::test]
async fn saving_is_limited_per_store_and_a_draft_expires_and_can_be_discarded() {
    let wizard = wizard("limits@example.com", true).await;
    let mut last = StatusCode::OK;
    for _ in 0..=SAVES_PER_MINUTE {
        last = wizard.save(GOOD, "").await.status();
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    // Says so, keeping what was typed.
    let html = body_text(wizard.save("what I typed", "").await).await;
    assert!(html.contains("a lot of saves"), "{html}");
    assert!(html.contains("what I typed"), "{html}");

    let now = crate::now_unix();
    let later = now + crate::db::DRAFT_SECS + 1;
    assert!(wizard.draft().is_some());
    let expired = wizard
        .state
        .db
        .lock()
        .checkout_page_draft(&wizard.store, later)
        .unwrap();
    assert_eq!(expired, None);

    let discarded = wizard
        .router
        .clone()
        .oneshot(form_post(
            &format!("{}/discard", wizard.page()),
            Some(&wizard.cookie),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(discarded.status(), StatusCode::SEE_OTHER);
    assert_eq!(wizard.draft(), None);
}

/// A stylesheet stays while something links it, however long ago it was saved;
/// one nothing links goes after a week.
#[tokio::test]
async fn only_stylesheets_nothing_links_are_pruned() {
    let wizard = wizard("prune@example.com", true).await;
    let (store, day) = (wizard.store.clone(), 24 * 60 * 60);
    let start = crate::now_unix();
    use crate::views::checkout_style::{CheckoutStyle, Roles};
    let replaced = CheckoutStyle {
        transparent: true,
        ..Default::default()
    };
    let style = CheckoutStyle {
        light: Roles {
            accent: Some("#123456".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let old = stylesheet_hash(&replaced.css().unwrap());
    let style_hash = stylesheet_hash(&style.css().unwrap());
    let db = wizard.state.db.lock();
    db.set_store_checkout_style(&store, &replaced, start)
        .unwrap();
    // Replaced a little later: a page may still be open with the old one.
    db.set_store_checkout_style(&store, &style, start + day)
        .unwrap();
    assert!(
        db.checkout_stylesheet(&old).unwrap().is_some(),
        "still open somewhere"
    );
    db.save_checkout_page_draft(
        &store,
        GOOD,
        "",
        "#checkout-root c{}",
        "t3",
        start + 9 * day,
    )
    .unwrap();
    assert_eq!(db.checkout_stylesheet(&old).unwrap(), None);
    assert!(
        db.checkout_stylesheet(&style_hash).unwrap().is_some(),
        "the store's colours are still in use"
    );
}

/// However often a store saves, it holds one draft's stylesheet, and discarding
/// takes it, leaving what else it uses.
#[tokio::test]
async fn a_store_holds_one_drafts_stylesheet() {
    let wizard = wizard("one-sheet@example.com", true).await;
    let store = wizard.store.clone();
    let now = crate::now_unix();
    let db = wizard.state.db.lock();
    let (first, second) = ("#checkout-root a{}", "#checkout-root b{}");
    db.save_checkout_page_draft(&store, GOOD, "", first, "t1", now)
        .unwrap();
    assert!(db
        .checkout_stylesheet(&stylesheet_hash(first))
        .unwrap()
        .is_some());
    db.save_checkout_page_draft(&store, GOOD, "", second, "t2", now)
        .unwrap();
    assert_eq!(
        db.checkout_stylesheet(&stylesheet_hash(first)).unwrap(),
        None
    );
    db.discard_checkout_page_draft(&store).unwrap();
    assert_eq!(
        db.checkout_stylesheet(&stylesheet_hash(second)).unwrap(),
        None
    );
}

#[test]
fn draft_checks_report_the_template_and_css_together() {
    assert!(check_draft(GOOD, "").is_ok());
    let problems = check_draft("{{nope}}", "h1{color:red}").err().unwrap();
    assert_eq!(
        problems.len(),
        1,
        "CSS can't be judged against that: {problems:?}"
    );
    let problems = check_draft("{{status}}", ".pay{color:red}").err().unwrap();
    assert!(problems.len() > 1, "{problems:?}");
    let checked = check_draft(
        "{{status}}{{qr}}<p>{{amount}}{{fiat}}</p>{{address}}\r\n",
        "",
    )
    .ok()
    .unwrap();
    assert!(!checked.template.contains('\r'));
}
