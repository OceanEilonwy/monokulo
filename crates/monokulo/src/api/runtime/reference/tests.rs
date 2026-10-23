//! The committed reference is what the code writes, from exchanges this
//! test really makes; and the API keeps to its own rules: one resource per
//! file, every link and form names a route.

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::api::runtime::{catalog, examples, openapi};
use crate::http::test_support::{
    admin_session_cookie, api_request, body_text, form_post, store_with_keys,
};
use crate::http::{build_router, AppState};

const PUBLIC_URI: &str = "https://www.monokulo.com";

async fn state() -> (AppState, engine_test_support::TestEngineHandle) {
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let state = AppState {
        engine: crate::http::Engine::new(crate::engine_client::EngineClient::embedded_for_tests(
            engine.router(),
        )),
        ..AppState::for_tests()
    }
    .with_options(&format!("public_uri = \"{PUBLIC_URI}\"\n"))
    .await;
    (state, engine)
}

/// Connects an app to store `id` as the dashboard and the app do: its key.
async fn connect_app(router: &axum::Router, id: &str, cookie: &str) -> String {
    let page = router
        .clone()
        .oneshot(form_post(
            &format!("/dashboard/stores/{id}/apps/connect"),
            Some(cookie),
            &[("device_name", "Front till")],
        ))
        .await
        .unwrap();
    let html = body_text(page).await;
    let start = html.find("/apps/connect#token=").expect("a code") + 20;
    let token: String = html[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let redeemed = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/apps/connect")
                .header("content-type", "application/json")
                .header("monokulo-client", "monokular/1.0.0")
                .body(axum::body::Body::from(
                    json!({"token": token, "slug": "monokular", "name": "Monokular", "platform": "android"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(redeemed.status(), StatusCode::OK);
    let body = crate::http::test_support::body_json(redeemed).await;
    assert_eq!(body, json!({"app_key": body["app_key"], "api": "/api/v1"}));
    body["app_key"].as_str().unwrap().to_owned()
}

/// One of each kind of exchange, against a real engine.
async fn record() -> Vec<examples::Exchange> {
    let (state, _engine) = state().await;
    let router = build_router(state.clone());
    // The seeded admin: this instance keeps sign-up to invites.
    let admin = admin_session_cookie(&router).await;
    let session = admin.strip_prefix("session=").unwrap().to_owned();
    let store = store_with_keys(&router, &state, &session, "bakery.example", "XMR").await;
    let cookie = format!("session={session}");
    let ak = connect_app(&router, &store.id, &cookie).await;
    let (sk, pk) = (store.secret_key.as_str(), store.public_key.as_str());
    let call = |method: &'static str, uri: String, key: Option<&str>, body: Option<Value>| {
        let request = api_request(method, &uri, key, body);
        let router = router.clone();
        async move { router.oneshot(request).await.unwrap() }
    };
    examples::take();
    call("GET", "/api/v1".into(), None, None).await;
    call("GET", "/api/v1".into(), Some(sk), None).await;
    call("GET", "/api/v1".into(), Some(pk), None).await;
    call("GET", "/api/v1".into(), Some(&ak), None).await;
    call("GET", "/api/v1/store".into(), Some(sk), None).await;
    call("GET", "/api/v1/store".into(), None, None).await;
    call("GET", "/api/v1/store".into(), Some(pk), None).await;
    let made = call(
        "POST",
        "/api/v1/orders".into(),
        Some(sk),
        Some(json!({
            "amount": "0.25",
            "currency": "XMR",
            "merchant_order_id": "1042",
            "description": "Order #1042: Blue mug + 2 more",
            "show_merchant_order_id": true,
            "idempotency_key": "order-1042-total-025",
        })),
    )
    .await;
    assert_eq!(made.status(), StatusCode::CREATED);
    let order = crate::http::test_support::body_json(made).await;
    let order_id = order["order_id"].as_str().unwrap().to_owned();
    // Listed while it is the only order: two made in the same second have
    // no fixed order between them.
    call("GET", "/api/v1/orders?limit=1".into(), Some(sk), None).await;
    call(
        "POST",
        "/api/v1/orders".into(),
        Some(pk),
        Some(json!({"amount": "0.1", "currency": "XMR"})),
    )
    .await;
    call(
        "POST",
        "/api/v1/orders".into(),
        Some(sk),
        Some(json!({"amount": "twelve", "currency": "XMR"})),
    )
    .await;
    call(
        "POST",
        "/api/v1/orders".into(),
        Some(sk),
        Some(json!({"amount": "1", "currency": "NOPE"})),
    )
    .await;
    call("GET", format!("/api/v1/orders/{order_id}"), Some(sk), None).await;
    call(
        "GET",
        "/api/v1/orders/order_00000000-0000-4000-8000-000000000000".into(),
        Some(sk),
        None,
    )
    .await;
    call("GET", "/api/v1/app".into(), Some(&ak), None).await;
    call("GET", "/api/v1/app".into(), Some(sk), None).await;
    call(
        "POST",
        "/api/v1/logs".into(),
        Some(sk),
        Some(json!({"entries": [{"level": "error", "message": "Monokulo could not be reached"}]})),
    )
    .await;
    state
        .db
        .lock()
        .set_client_logging(&crate::db::ConnectionId::new(store.id.clone()), true)
        .unwrap();
    call(
        "POST",
        "/api/v1/logs".into(),
        Some(sk),
        Some(json!({"entries": [{"level": "error", "message": "Monokulo could not be reached"}]})),
    )
    .await;
    call("GET", "/api/v1/openapi.json".into(), None, None).await;
    call("DELETE", "/api/v1/app".into(), Some(&ak), None).await;
    call("GET", "/api/v1".into(), Some(&ak), None).await;
    let recorded = examples::take();
    // The first of each method, route, key and status.
    let mut seen = std::collections::HashSet::new();
    let kept: Vec<_> = recorded
        .into_iter()
        .filter(|e| seen.insert((e.method.clone(), e.route.clone(), e.key.clone(), e.status)))
        .collect();
    examples::stable(kept)
}

fn committed(path: &str, expected: &str) {
    if std::env::var_os("MONOKULO_WRITE_API").is_some() {
        if let Some(dir) = Path::new(path).parent() {
            std::fs::create_dir_all(dir).expect("the reference's folder can be made");
        }
        std::fs::write(path, expected).expect("the reference is writable");
    }
    let found = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        found == expected,
        "{path} is not what monokulo writes. If the change is meant, write it again with \
         MONOKULO_WRITE_API=1 cargo nextest run -p monokulo api::runtime::reference (twice when \
         the examples change)"
    );
}

/// The committed examples, OpenAPI document and reference are what this
/// code writes from real exchanges.
#[tokio::test]
async fn the_committed_reference_is_what_the_api_says() {
    let recorded = record().await;
    let examples_text = serde_json::to_string_pretty(&recorded).unwrap() + "\n";
    committed(EXAMPLES_PATH, &examples_text);
    committed(OPENAPI_PATH, &openapi::to_text(&openapi::build(&recorded)));
    committed(REFERENCE_PATH, &to_text(&catalog(&recorded)));
}

/// The served document is the committed one.
#[test]
fn the_served_document_is_the_committed_one() {
    let committed = std::fs::read_to_string(OPENAPI_PATH).unwrap_or_default();
    assert_eq!(openapi::to_text(openapi::document()), committed);
}

/// Each resource is a file of its own, its handlers are in it, and every
/// file under `src/api/` (but `mod.rs` files and the runtime) is one.
#[test]
fn one_resource_per_file() {
    let mut files = std::collections::HashMap::new();
    for resource in catalog::resources() {
        assert!(
            files.insert(resource.file, resource.template).is_none(),
            "{} has two resources",
            resource.file
        );
    }
    for handler in catalog::handlers() {
        let resource = catalog::resources()
            .into_iter()
            .find(|r| r.template == handler.template)
            .expect("a handler's resource is registered");
        assert_eq!(
            handler.file, resource.file,
            "{} {} is in {}, not its resource's file",
            handler.method, handler.template, handler.file
        );
        assert_eq!(handler.module, resource.module, "{}", handler.name);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "runtime") {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "mod.rs")
            {
                out.push(path);
            }
        }
    }
    walk(&root.join("src/api"), &mut sources);
    assert!(!sources.is_empty());
    for source in sources {
        let relative = source
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            catalog::resources()
                .iter()
                .any(|r| r.file.ends_with(&relative)),
            "{relative} declares no resource"
        );
    }
}

/// Every resource with a body has its body in its file, and every other
/// one says what it is.
#[test]
fn every_resource_is_described() {
    for resource in catalog::resources() {
        assert!(
            !catalog::description(resource).is_empty(),
            "{} has no description",
            resource.template
        );
        if let Some(body) = catalog::body_of(resource) {
            assert_eq!(body.file, resource.file);
        }
    }
}

/// Every link and form target in the recorded answers is a route this
/// router has, or a page outside the API.
#[test]
fn every_link_names_a_route() {
    let committed: Vec<examples::Exchange> =
        serde_json::from_str(&std::fs::read_to_string(EXAMPLES_PATH).unwrap()).unwrap();
    let templates: Vec<&str> = catalog::resources().iter().map(|r| r.template).collect();
    let matches = |href: &str| {
        let path = href.split('?').next().unwrap_or(href);
        templates.iter().any(|template| {
            let (a, b): (Vec<&str>, Vec<&str>) =
                (template.split('/').collect(), path.split('/').collect());
            a.len() == b.len()
                && a.iter()
                    .zip(&b)
                    .all(|(t, p)| t == p || (t.starts_with('{') && !p.is_empty()))
        })
    };
    let mut checked = 0;
    for exchange in &committed {
        let Some(response) = &exchange.response else {
            continue;
        };
        let mut stack = vec![response];
        while let Some(value) = stack.pop() {
            match value {
                Value::Object(map) => {
                    if let Some(Value::Object(links)) = map.get("_links") {
                        for (rel, link) in links {
                            if rel == "curies" {
                                continue;
                            }
                            let href = link["href"].as_str().unwrap();
                            if href.starts_with("https://") {
                                continue;
                            }
                            assert!(matches(href), "{rel}: {href} is no route");
                            checked += 1;
                        }
                    }
                    if let Some(Value::Object(templates)) = map.get("_templates") {
                        for (name, template) in templates {
                            let target = template["target"].as_str().unwrap();
                            assert!(matches(target), "{name}: {target} is no route");
                            checked += 1;
                        }
                    }
                    stack.extend(map.values());
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
    }
    assert!(checked > 10, "only {checked} links checked");
}
