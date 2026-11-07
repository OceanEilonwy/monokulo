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

const GOOD: &str = "<h1>Fern and Wire's own page</h1>{{status}}{{qr}}<p>{{amount}} {{fiat}}</p>{{address}}<div class=\"pay\"></div>";
const CUSTOM: &str = "Fern and Wire's own page";
use crate::checkout_pages::TEMPLATE;

struct Wizard {
    state: AppState,
    router: Router,
    cookie: String,
    store: ConnectionId,
    public_key: String,
    dns: Arc<FakeDns>,
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
                    .header("referer", "https://store-home.example/cart")
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

    async fn post(&self, uri: &str, cookie: Option<&str>) -> (StatusCode, String) {
        let response = self
            .router
            .clone()
            .oneshot(form_post(uri, cookie.or(Some(&self.cookie)), &[]))
            .await
            .unwrap();
        let status = response.status();
        (status, body_text(response).await)
    }

    /// Publishes `GOOD`, with its record in DNS.
    async fn publish(&self) -> String {
        self.save(GOOD, ".pay{color:red}").await;
        let token = self.publish_record();
        let (status, _) = self.post(&format!("{}/publish", self.page()), None).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(self.published().is_some());
        token
    }

    fn publish_record(&self) -> String {
        let draft = self.draft().unwrap();
        let token =
            checkout_template::version_token(self.store.as_str(), &draft.template, &draft.css);
        self.dns.publish(
            &TEMPLATE.record_name("store-home.example"),
            &TEMPLATE.value(&token),
        );
        token
    }

    fn published(&self) -> Option<crate::db::PublishedCheckoutPage> {
        self.state
            .db
            .lock()
            .published_checkout_page(&self.store)
            .unwrap()
    }

    async fn order(&self) -> String {
        let response = self
            .router
            .clone()
            .oneshot(crate::http::test_support::api_request(
                "POST",
                "/api/v1/orders",
                Some(&self.public_key),
                Some(serde_json::json!({ "amount": "0.01", "currency": "XMR" })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        crate::http::test_support::body_json(response).await["order_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// `uri` as a browser asks for it, with what it says about where it's loaded.
    async fn visit(
        &self,
        uri: &str,
        dest: Option<&str>,
        site: Option<&str>,
    ) -> axum::response::Response {
        let mut request = Request::builder().uri(uri);
        if let Some(dest) = dest {
            request = request.header("sec-fetch-dest", dest);
        }
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        request = request.header("referer", "https://store-home.example/cart");
        self.router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
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
    let store = store_with_keys(&router, &state, &session, "store-home.example", "XMR").await;
    let public_key = store.public_key.clone();
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
        public_key,
        dns,
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
        html.contains("{{address short}}"),
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

/// A stock layout's preview is its store's owner's, and only as a frame of the
/// preview page, sandboxed like a draft's.
#[tokio::test]
async fn a_layouts_preview_is_its_owners_and_only_as_a_frame() {
    let wizard = wizard("layout-preview@example.com", true).await;
    let page = format!("/stores/{}/settings/checkout-layout", wizard.store);
    let frame = format!("{page}/frame?layout=tiny&state=paid");
    let request = |cookie: Option<&str>, dest: &str, site: &str| {
        let mut request = Request::builder()
            .uri(&frame)
            .header("sec-fetch-dest", dest)
            .header("sec-fetch-site", site);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.body(Body::empty()).unwrap()
    };

    let (status, _) = wizard.get(&format!("{page}?layout=tiny")).await;
    assert_eq!(status, StatusCode::OK);

    let framed = wizard
        .router
        .clone()
        .oneshot(request(Some(&wizard.cookie), "iframe", "same-origin"))
        .await
        .unwrap();
    assert_eq!(framed.status(), StatusCode::OK);
    let policy = framed.headers()[axum::http::header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap();
    assert!(policy.contains("sandbox"), "{policy}");

    let alone = wizard
        .router
        .clone()
        .oneshot(request(Some(&wizard.cookie), "document", "none"))
        .await
        .unwrap();
    assert_eq!(alone.status(), StatusCode::FORBIDDEN);

    let stranger = signed_up_and_logged_in_session_token(
        &wizard.router,
        "stranger@example.com",
        "correct horse battery staple",
    )
    .await;
    let stranger = format!("{}={stranger}", crate::http::SESSION_COOKIE_NAME);
    let theirs = wizard
        .router
        .clone()
        .oneshot(request(Some(&stranger), "iframe", "same-origin"))
        .await
        .unwrap();
    assert_eq!(theirs.status(), StatusCode::NOT_FOUND);

    let nobody = wizard
        .router
        .clone()
        .oneshot(request(None, "iframe", "same-origin"))
        .await
        .unwrap();
    assert_ne!(nobody.status(), StatusCode::OK);
}

/// The form shows every colour, so saving it as it came changes nothing: a store
/// stays on the theme. Without JavaScript, a box takes every colour back to it.
#[tokio::test]
async fn saving_the_themes_colours_keeps_a_store_on_the_theme() {
    use crate::views::checkout_style::{theme_colour, CheckoutStyle};
    let wizard = wizard("theme-colours@example.com", true).await;
    let settings = format!("/stores/{}/settings", wizard.store);
    let style = || {
        wizard
            .state
            .db
            .lock()
            .get_store_connection_by_id(&wizard.store)
            .unwrap()
            .unwrap()
            .checkout_style
    };
    let save = |fields: Vec<(&'static str, String)>| {
        let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        form_post(&settings, Some(&wizard.cookie), &pairs)
    };

    let own = theme_colour("light", "accent").unwrap().to_string();
    let saved = wizard
        .router
        .clone()
        .oneshot(save(vec![("style_light_accent", own)]))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    assert_eq!(style(), CheckoutStyle::default());

    wizard
        .router
        .clone()
        .oneshot(save(vec![
            ("style_light_accent", "#2f6b4f".to_string()),
            ("style_dark_text", "#eeeeee".to_string()),
        ]))
        .await
        .unwrap();
    assert_eq!(style().light.accent.as_deref(), Some("#2f6b4f"));
    assert_eq!(style().dark.text.as_deref(), Some("#eeeeee"));

    // The box wins over what the swatches still hold, as they can't be emptied.
    wizard
        .router
        .clone()
        .oneshot(save(vec![
            ("style_light_accent", "#2f6b4f".to_string()),
            ("style_theme_colours", "on".to_string()),
        ]))
        .await
        .unwrap();
    assert_eq!(style(), CheckoutStyle::default());
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

/// Nothing is published without its proof: not by skipping the check, not with
/// another version's value, and not during an outage.
#[tokio::test]
async fn a_version_is_published_only_with_its_own_proof_in_place() {
    let wizard = wizard("approve@example.com", true).await;
    wizard.save(GOOD, "").await;
    let publish = format!("{}/publish", wizard.page());
    let approve = format!("{}/approve", wizard.page());

    let (_, html) = wizard.get(&approve).await;
    assert!(html.contains("_monokulo-template"), "{html}");
    assert!(html.contains("store-home.example"), "{html}");

    // Not there.
    let (status, _) = wizard.post(&publish, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(wizard.published(), None);
    let (_, html) = wizard.post(&approve, None).await;
    assert!(html.contains("Not found."), "{html}");
    assert!(!html.contains("Use this template"), "{html}");

    // Another version's value.
    let name = TEMPLATE.record_name("store-home.example");
    wizard.dns.publish(&name, &TEMPLATE.value("an-old-version"));
    wizard.post(&publish, None).await;
    assert_eq!(wizard.published(), None);
    let (_, html) = wizard.post(&approve, None).await;
    assert!(html.contains("holds a different value"), "{html}");

    // An outage says so, rather than that it's missing.
    wizard.dns.fail(&name, "timed out");
    wizard.post(&publish, None).await;
    assert_eq!(wizard.published(), None);
    let (_, html) = wizard.post(&approve, None).await;
    assert!(
        html.contains("Can&#39;t check") || html.contains("Can't check"),
        "{html}"
    );

    // There: found, and it's the button that publishes it.
    let token = wizard.publish_record();
    let (_, html) = wizard.post(&approve, None).await;
    assert!(html.contains("Found the record."), "{html}");
    assert!(html.contains("Use this template"), "{html}");
    assert_eq!(wizard.published(), None, "not until the button");
    wizard.post(&publish, None).await;
    let page = wizard.published().unwrap();
    assert_eq!(page.token, token);
    assert_eq!(wizard.draft(), None, "the draft became the page");
}

/// The value is bound to the store and the version, so neither can be reused.
#[tokio::test]
async fn editing_a_page_keeps_it_showing_until_the_new_version_is_approved() {
    let wizard = wizard("versions@example.com", true).await;
    let first = wizard.publish().await;
    let changed = GOOD.replace("own page", "new page");
    wizard.save(&changed, "").await;
    assert_eq!(
        wizard.published().unwrap().token,
        first,
        "still the old one"
    );
    let (_, _) = wizard
        .post(&format!("{}/publish", wizard.page()), None)
        .await;
    assert_eq!(
        wizard.published().unwrap().token,
        first,
        "the old record doesn't approve it"
    );
    let second = wizard.publish_record();
    assert_ne!(first, second);
    wizard
        .post(&format!("{}/publish", wizard.page()), None)
        .await;
    assert_eq!(wizard.published().unwrap().token, second);
}

/// A custom page shows only as a frame of another site. Anywhere else, the store's
/// layout, so a payment link sent to someone is never a merchant's words at this
/// Monokulo's address, including under its own share page.
#[tokio::test]
async fn a_custom_page_shows_only_in_a_frame_of_another_site() {
    let wizard = wizard("serving@example.com", true).await;
    wizard.publish().await;
    let order = wizard.order().await;
    let uri = format!("/api/v1/checkout/{order}");
    for (dest, site, shown) in [
        (Some("iframe"), Some("cross-site"), true),
        (Some("iframe"), Some("same-site"), true),
        (Some("iframe"), Some("same-origin"), false),
        (Some("document"), Some("none"), false),
        (None, None, false),
        (Some("iframe"), None, false),
    ] {
        let page = wizard.visit(&uri, dest, site).await;
        assert_eq!(page.status(), StatusCode::OK);
        let vary = page.headers()["vary"].to_str().unwrap().to_string();
        assert!(vary.contains("Sec-Fetch-Site"), "{vary}");
        let policy = page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_string();
        let html = body_text(page).await;
        assert_eq!(html.contains(CUSTOM), shown, "{dest:?} {site:?}");
        assert_eq!(
            policy.contains("frame-ancestors"),
            shown,
            "only a custom page is locked to its site: {policy}"
        );
        assert!(policy.starts_with("default-src 'none'"), "{policy}");
    }
    // An embed asking for a layout gets it.
    let asked = wizard
        .visit(
            &format!("{uri}?view=tiny"),
            Some("iframe"),
            Some("cross-site"),
        )
        .await;
    assert!(!body_text(asked).await.contains(CUSTOM));

    // A frame moving itself on says same-origin. Its refresh, forms and links carry
    // a mark that keeps it on the page, and that's no use opened any other way.
    let html = body_text(wizard.visit(&uri, Some("iframe"), Some("cross-site")).await).await;
    assert!(
        html.contains(&format!("content=\"60;url={uri}?framed=true\"")),
        "{html}"
    );
    let marked = format!("{uri}?framed=true");
    for (dest, site, shown) in [
        ("iframe", "same-origin", true),
        ("document", "same-origin", false),
        ("document", "none", false),
    ] {
        let html = body_text(wizard.visit(&marked, Some(dest), Some(site)).await).await;
        assert_eq!(html.contains(CUSTOM), shown, "{dest} {site}");
    }
    // A layout page has no mark to pass on, whatever it was opened with.
    let html = body_text(wizard.visit(&marked, Some("document"), Some("none")).await).await;
    assert!(!html.contains("framed=true"), "{html}");

    let framed = wizard.visit(&uri, Some("iframe"), Some("cross-site")).await;
    let policy = framed.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        policy.contains(
            "frame-ancestors 'self' https://store-home.example https://*.store-home.example"
        ),
        "{policy}"
    );
    let html = body_text(framed).await;
    assert!(
        html.contains(&format!("&amp;page={}", wizard.published().unwrap().token)),
        "{html}"
    );
    assert!(html.contains("/static-style/"), "{html}");
}

/// An answer without the proof starts the grace period, and it shows through it;
/// no answer changes nothing, however long; and it comes back by itself.
#[tokio::test]
async fn a_pages_proof_is_checked_with_a_grace_period_and_no_answer_means_nothing() {
    use crate::checkout_pages::{check_and_record, standing, PageStanding};
    let wizard = wizard("rechecks@example.com", true).await;
    let token = wizard.publish().await;
    let name = TEMPLATE.record_name("store-home.example");
    let domains = wizard
        .state
        .db
        .lock()
        .list_store_domains(&wizard.store)
        .unwrap();
    let (db, proofs) = (wizard.state.db.clone(), wizard.state.proofs.clone());
    let store = wizard.store.clone();
    let check = |at: i64| {
        let (db, proofs, store, token) = (db.clone(), proofs.clone(), store.clone(), token.clone());
        async move {
            check_and_record(
                &db,
                &proofs,
                &store,
                &["store-home.example".to_string()],
                &token,
                at,
            )
            .await
            .unwrap()
        }
    };
    let now = crate::now_unix();
    let at = |wizard: &Wizard, when: i64| standing(&wizard.published().unwrap(), &domains, when);

    // An outage, longer than the grace period: still working.
    wizard.dns.fail(&name, "timed out");
    check(now).await;
    let later = now + domain_proof::GRACE_SECS * 2;
    check(later).await;
    assert_eq!(at(&wizard, later), PageStanding::Working);
    assert!(wizard.published().unwrap().last_error.is_some());

    // Gone: failing, then not showing.
    wizard.dns.remove(&name);
    check(later).await;
    assert_eq!(at(&wizard, later), PageStanding::Failing { since: later });
    assert_eq!(
        at(&wizard, later + domain_proof::GRACE_SECS),
        PageStanding::NotShowing
    );
    // An outage doesn't stop the clock it didn't start.
    wizard.dns.fail(&name, "timed out");
    check(later + 60).await;
    assert_eq!(
        at(&wizard, later + 60),
        PageStanding::Failing { since: later }
    );

    // Back: working, with nothing for the merchant to do.
    wizard.dns.publish(&name, &TEMPLATE.value(&token));
    check(later + domain_proof::GRACE_SECS + 60).await;
    assert_eq!(wizard.published().unwrap().failing_since, None);
    assert_eq!(wizard.published().unwrap().last_error, None);
}

/// A store's domain lapsing takes its page with it.
#[tokio::test]
async fn a_page_stops_showing_when_its_domain_stops_counting() {
    let wizard = wizard("lapsed@example.com", true).await;
    wizard.publish().await;
    let order = wizard.order().await;
    let uri = format!("/api/v1/checkout/{order}");
    let shown = |html: String| html.contains(CUSTOM);
    assert!(shown(
        body_text(wizard.visit(&uri, Some("iframe"), Some("cross-site")).await).await
    ));
    {
        let db = wizard.state.db.lock();
        let domain = db.list_store_domains(&wizard.store).unwrap().remove(0);
        db.delete_store_domain(&wizard.store, &domain.id).unwrap();
    }
    assert!(!shown(
        body_text(wizard.visit(&uri, Some("iframe"), Some("cross-site")).await).await
    ));
}

/// An onion shop approves with a file, fetched only over Tor, with nothing in DNS.
#[tokio::test]
async fn an_onion_only_store_approves_with_a_file_over_tor() {
    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    let wizard = wizard("onion-page@example.com", false).await;
    let onion = Arc::new(domain_proof::test_support::FakeOnion::default());
    let state = AppState {
        proofs: domain_proof::test_support::proofs(wizard.dns.clone(), onion.clone()),
        ..wizard.state.clone()
    };
    let router = build_router(state.clone());
    let id = wizard.store.clone();
    {
        let db = state.db.lock();
        let web = db.list_store_domains(&id).unwrap().remove(0);
        db.delete_store_domain(&id, &web.id).unwrap();
        db.create_store_domain("o", &id, ONION, "t", 1, None)
            .unwrap()
            .unwrap();
        db.record_store_domain_check("o", crate::now_unix(), None)
            .unwrap();
    }
    let wizard = Wizard {
        router,
        state,
        ..wizard
    };
    wizard.save(GOOD, "").await;
    let draft = wizard.draft().unwrap();
    let (_, html) = wizard.get(&format!("{}/approve", wizard.page())).await;
    assert!(html.contains("/.well-known/monokulo-template"), "{html}");
    assert!(html.contains("weaker than DNS"), "{html}");
    let token = checkout_template::version_token(id.as_str(), &draft.template, &draft.css);
    onion.serve(
        ONION,
        &TEMPLATE.value(&crate::checkout_pages::proof_token(&token, ONION)),
    );
    wizard.dns.asked.lock().unwrap().clear();
    wizard
        .post(&format!("{}/publish", wizard.page()), None)
        .await;
    assert_eq!(wizard.published().unwrap().token, token);
    assert_eq!(
        *onion.asked.lock().unwrap(),
        ["/.well-known/monokulo-template"]
    );
    assert!(
        wizard.dns.asked.lock().unwrap().is_empty(),
        "no onion name in DNS"
    );
}

/// The operator turns a page off: it stops showing, a fresh record doesn't bring it
/// back, nor removing it, and the merchant sees why and can still edit a draft.
#[tokio::test]
async fn an_operator_can_turn_a_page_off_and_only_they_can_turn_it_back_on() {
    let wizard = wizard("turned-off@example.com", true).await;
    wizard.publish().await;
    let order = wizard.order().await;
    let uri = format!("/api/v1/checkout/{order}");
    let shown = async || {
        body_text(wizard.visit(&uri, Some("iframe"), Some("cross-site")).await)
            .await
            .contains(CUSTOM)
    };
    let admin = crate::http::test_support::admin_session_cookie(&wizard.router).await;
    let off = format!("/admin/custom-pages/{}/off", wizard.store);

    // Not for anyone else.
    let (status, _) = wizard.get(&off).await;
    assert_ne!(status, StatusCode::OK);
    let refused = wizard
        .router
        .clone()
        .oneshot(form_post(&off, Some(&wizard.cookie), &[("reason", "x")]))
        .await
        .unwrap();
    assert_ne!(refused.status(), StatusCode::SEE_OTHER);
    assert!(shown().await);

    let turned = wizard
        .router
        .clone()
        .oneshot(form_post(
            &off,
            Some(&admin),
            &[("reason", "Impersonating another shop")],
        ))
        .await
        .unwrap();
    assert_eq!(turned.status(), StatusCode::SEE_OTHER);
    assert!(!shown().await);

    // What the merchant sees, and what they can't do about it.
    let (_, html) = wizard
        .get(&format!("/stores/{}/settings?tab=checkout", wizard.store))
        .await;
    assert!(html.contains("Turned off by admin"), "{html}");
    assert!(html.contains("Impersonating another shop"), "{html}");
    wizard
        .save(GOOD.replace("own", "another").as_str(), "")
        .await;
    assert!(wizard.draft().is_some(), "a draft can still be edited");
    wizard.publish_record();
    wizard
        .post(&format!("{}/publish", wizard.page()), None)
        .await;
    assert!(wizard.published().unwrap().turned_off_at.is_some());
    assert!(!shown().await);
    wizard.post(&format!("{}/stop", wizard.page()), None).await;
    assert!(wizard.published().is_some(), "removing it isn't a way back");

    let on = wizard
        .router
        .clone()
        .oneshot(form_post(
            &format!("/admin/custom-pages/{}/on", wizard.store),
            Some(&admin),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(on.status(), StatusCode::SEE_OTHER);
    assert!(shown().await);

    // The operator's list finds it by domain and by order, and nothing by anything else.
    for (query, found) in [
        ("", true),
        ("store-home", true),
        (order.as_str(), true),
        ("nope", false),
    ] {
        let list = wizard
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/custom-pages?q={query}"))
                    .header("cookie", admin.as_str())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(list).await;
        assert_eq!(
            html.contains("store-home.example"),
            found,
            "{query}: {html}"
        );
    }
}

#[tokio::test]
async fn the_card_says_where_a_page_stands() {
    let wizard = wizard("card@example.com", false).await;
    let tab = format!("/stores/{}/settings?tab=checkout", wizard.store);
    assert!(wizard.get(&tab).await.1.contains("Not available yet"));
    let row = wizard
        .state
        .db
        .lock()
        .list_store_domains(&wizard.store)
        .unwrap()
        .remove(0);
    wizard
        .state
        .db
        .lock()
        .record_store_domain_check(&row.id, crate::now_unix(), None)
        .unwrap();
    assert!(wizard.get(&tab).await.1.contains("Create a custom page"));
    wizard.save(GOOD, "").await;
    assert!(wizard.get(&tab).await.1.contains("Waiting for approval"));
    wizard.publish_record();
    wizard
        .post(&format!("{}/publish", wizard.page()), None)
        .await;
    assert!(
        wizard.get(&tab).await.1.contains(">Working<")
            || wizard.get(&tab).await.1.contains("Working")
    );
    {
        let db = wizard.state.db.lock();
        let token = db
            .published_checkout_page(&wizard.store)
            .unwrap()
            .unwrap()
            .token;
        db.record_checkout_page_check(&wizard.store, &token, crate::now_unix(), true, "gone")
            .unwrap();
    }
    assert!(wizard.get(&tab).await.1.contains("Failing"));
    wizard.post(&format!("{}/stop", wizard.page()), None).await;
    assert_eq!(wizard.published(), None);
    assert!(wizard.get(&tab).await.1.contains("Create a custom page"));
}

/// A page on a version that's been replaced loads again, rather than following the
/// order on a template it wasn't written from.
#[tokio::test]
async fn a_stream_on_an_old_version_tells_the_page_to_load_again() {
    let wizard = wizard("reload@example.com", true).await;
    let token = wizard.publish().await;
    let order = wizard.order().await;
    let events = format!("/api/v1/checkout/{order}/events?routed=true&parts=2&page={token}");
    let first_event = |response: axum::response::Response| async move {
        let mut body = response.into_body();
        let (mut pending, mut parser) = (Vec::new(), crate::live::SseTestParser::default());
        crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap()
            .0
    };
    let current = first_event(wizard.visit(&events, None, None).await).await;
    assert_ne!(current, "reload");
    let stale = format!("/api/v1/checkout/{order}/events?routed=true&parts=2&page=an-old-version");
    assert_eq!(
        first_event(wizard.visit(&stale, None, None).await).await,
        "reload"
    );
}

/// What happens while a lookup runs: a draft saved meanwhile stays, and a slow answer
/// about the old version doesn't land on the new one.
#[tokio::test]
async fn nothing_slow_touches_what_replaced_it() {
    let wizard = wizard("races@example.com", true).await;
    let (store, now) = (wizard.store.clone(), crate::now_unix());
    let db = wizard.state.db.lock();
    db.save_checkout_page_draft(&store, GOOD, "", "", "t1", now)
        .unwrap();
    let looked_up = db.checkout_page_draft(&store, now).unwrap().unwrap();
    db.save_checkout_page_draft(&store, GOOD, "p{}", "", "t2", now)
        .unwrap();
    assert!(db
        .publish_checkout_page(&looked_up, "v1", &[], now)
        .unwrap());
    let draft = db.checkout_page_draft(&store, now).unwrap();
    assert_eq!(draft.unwrap().preview_token, "t2", "the newer draft stays");

    db.record_checkout_page_check(&store, "v0", now, true, "gone")
        .unwrap();
    let page = db.published_checkout_page(&store).unwrap().unwrap();
    assert_eq!((page.failing_since, page.last_error), (None, None));
    db.record_checkout_page_check(&store, "v1", now, true, "gone")
        .unwrap();
    let page = db.published_checkout_page(&store).unwrap().unwrap();
    assert_eq!(page.failing_since, Some(now));
}

/// An open page follows its standing, not only its version: when its proof has
/// lapsed or its domain has gone, it's told to load again.
#[tokio::test]
async fn an_open_page_stops_following_a_page_that_stopped_showing() {
    let wizard = wizard("open-page@example.com", true).await;
    wizard.publish().await;
    let token = wizard.published().unwrap().token;
    let (store, now) = (wizard.store.clone(), crate::now_unix());
    let db = wizard.state.db.lock();
    let showing = |at: i64| {
        crate::checkout_pages::page_still_showing(&db, &store, &token, at)
            .unwrap()
            .is_some()
    };
    assert!(showing(now));
    assert!(
        crate::checkout_pages::page_still_showing(&db, &store, "older", now)
            .unwrap()
            .is_none()
    );
    db.record_checkout_page_check(&store, &token, now, true, "gone")
        .unwrap();
    assert!(showing(now), "its grace period");
    assert!(!showing(now + domain_proof::GRACE_SECS));
}

/// A release that fixes the sanitiser reaches pages already live, from what their
/// domain approved.
#[tokio::test]
async fn each_start_writes_a_live_pages_css_again() {
    let wizard = wizard("refresh-page@example.com", true).await;
    let (store, now) = (wizard.store.clone(), crate::now_unix());
    let css = "p{color:red}";
    let written = check_draft(GOOD, css).unwrap().written_css;
    let db = wizard.state.db.lock();
    db.save_checkout_page_draft(&store, GOOD, css, "#checkout-root old{}", "t1", now)
        .unwrap();
    let draft = db.checkout_page_draft(&store, now).unwrap().unwrap();
    assert!(db.publish_checkout_page(&draft, "v1", &[], now).unwrap());

    assert_eq!(db.refresh_checkout_pages(now).unwrap(), 1);
    let page = db.published_checkout_page(&store).unwrap().unwrap();
    assert_eq!(page.written_css, written);
    assert!(db
        .checkout_stylesheet(&stylesheet_hash(&written))
        .unwrap()
        .is_some());
    assert_eq!(
        db.refresh_checkout_pages(now).unwrap(),
        0,
        "already current"
    );
}

/// A store with a web domain and an onion address approves on both, in one step, and
/// the page has one status: either proof missing fails it, on both sites.
#[tokio::test]
async fn a_web_domain_and_an_onion_address_must_both_approve() {
    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    let wizard = wizard("both@example.com", true).await;
    let onion = Arc::new(domain_proof::test_support::FakeOnion::default());
    let proofs = domain_proof::test_support::proofs(wizard.dns.clone(), onion.clone());
    let state = AppState {
        proofs: proofs.clone(),
        ..wizard.state.clone()
    };
    let wizard = Wizard {
        router: build_router(state.clone()),
        state,
        ..wizard
    };
    let (store, now) = (wizard.store.clone(), crate::now_unix());
    {
        let db = wizard.state.db.lock();
        db.create_store_domain("o", &store, ONION, "t", 1, None)
            .unwrap()
            .unwrap();
        db.conn_for_test()
            .execute(
                "UPDATE store_domains SET verified_at_utc = ?1 WHERE id = 'o'",
                [now],
            )
            .unwrap();
    }
    assert_eq!(wizard.save(GOOD, "").await.status(), StatusCode::SEE_OTHER);
    let draft = wizard.draft().unwrap();
    let token = checkout_template::version_token(store.as_str(), &draft.template, &draft.css);
    let approve = format!("{}/approve", wizard.page());

    // Both shown, and neither alone offers the button.
    let (_, html) = wizard.get(&approve).await;
    assert!(
        html.contains("store-home.example") && html.contains(ONION),
        "{html}"
    );
    assert!(html.contains("Check both"), "{html}");
    wizard.dns.publish(
        &TEMPLATE.record_name("store-home.example"),
        &TEMPLATE.value(&token),
    );
    let (_, html) = wizard.post(&approve, None).await;
    assert!(html.contains("Found the record."), "{html}");
    assert!(!html.contains("Use this template"), "{html}");
    let (status, _) = wizard
        .post(&format!("{}/publish", wizard.page()), None)
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(wizard.published(), None, "the file isn't there");

    onion.serve(
        ONION,
        &TEMPLATE.value(&crate::checkout_pages::proof_token(&token, ONION)),
    );
    let (_, html) = wizard.post(&approve, None).await;
    assert!(html.contains("Use this template"), "{html}");
    wizard
        .post(&format!("{}/publish", wizard.page()), None)
        .await;
    assert!(wizard.published().is_some());

    // Either going fails the one page, naming which, and comes back with both.
    let names = ["store-home.example".to_string(), ONION.to_string()];
    let check = |at| {
        crate::checkout_pages::check_and_record(
            &wizard.state.db,
            &proofs,
            &store,
            &names,
            &token,
            at,
        )
    };
    let onion_proof = TEMPLATE.value(&crate::checkout_pages::proof_token(&token, ONION));
    let web_name = TEMPLATE.record_name("store-home.example");
    onion.serve(ONION, "someone else's");
    let found = check(now).await.unwrap();
    assert!(matches!(&found, crate::checkout_pages::Approved::Missing(why) if why.contains(ONION)));
    let page = wizard.published().unwrap();
    assert_eq!(page.failing_since, Some(now));
    assert!(page.last_error.unwrap().contains(ONION));
    onion.serve(ONION, &onion_proof);
    assert_eq!(
        check(now).await.unwrap(),
        crate::checkout_pages::Approved::Yes
    );
    assert_eq!(wizard.published().unwrap().failing_since, None);

    wizard.dns.remove(&web_name);
    let found = check(now + 1).await.unwrap();
    assert!(
        matches!(&found, crate::checkout_pages::Approved::Missing(why) if why.contains("store-home"))
    );
    assert_eq!(wizard.published().unwrap().failing_since, Some(now + 1));

    // Back on the web while Tor says nothing: the clock keeps running, as an answer
    // from one says nothing about the other.
    wizard.dns.publish(&web_name, &TEMPLATE.value(&token));
    onion.fail(ONION, "timed out");
    let found = check(now + 2).await.unwrap();
    assert!(matches!(
        found,
        crate::checkout_pages::Approved::NoAnswer(_)
    ));
    assert_eq!(wizard.published().unwrap().failing_since, Some(now + 1));
}

/// A domain added under a live page shows the layout until it approves, says so on the
/// card, and is looked at soon. A stolen dashboard can't put a thief's page on it, nor
/// the shop's page's words on a domain nobody approved them for.
#[tokio::test]
async fn a_domain_added_under_a_live_page_waits_for_its_own_proof() {
    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    let wizard = wizard("added@example.com", true).await;
    wizard.publish().await;
    let (store, now) = (wizard.store.clone(), crate::now_unix());
    {
        let db = wizard.state.db.lock();
        db.create_store_domain("o", &store, ONION, "t", 1, None)
            .unwrap()
            .unwrap();
        db.conn_for_test()
            .execute(
                "UPDATE store_domains SET verified_at_utc = ?1 WHERE id = 'o'",
                [now],
            )
            .unwrap();
    }
    let page = wizard.published().unwrap();
    assert_eq!(page.approved_domains, ["store-home.example"]);
    let domains = wizard.state.db.lock().list_store_domains(&store).unwrap();
    assert_eq!(
        crate::checkout_pages::waiting_on(&page, &domains, now),
        [ONION]
    );
    let (_, html) = wizard
        .get(&format!("/stores/{store}/settings?tab=checkout"))
        .await;
    assert!(html.contains("until it has this version"), "{html}");
    assert!(html.contains(ONION), "{html}");
}
