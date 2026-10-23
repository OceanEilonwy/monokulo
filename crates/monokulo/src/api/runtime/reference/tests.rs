//! The reference is written from exchanges this test really makes, and is
//! sound; and the API keeps to its own rules: one resource per file, every
//! link and form names a route.

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
            &format!("/stores/{id}/settings/apps/connect"),
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

/// Where the docs build asks for the reference (`cargo xtask pages api`
/// and `cargo xtask api openapi` set it); unset, nothing is written.
const OUT: &str = "MONOKULO_API_OUT";

/// Every example in `document` fits the schema beside it; returns how
/// many there are.
fn examples_fit_their_schemas(document: &Value) -> usize {
    let escape = |s: &str| {
        s.replace('~', "~0")
            .replace('/', "~1")
            .replace('{', "%7B")
            .replace('}', "%7D")
            .replace('+', "%2B")
    };
    let mut checked = 0;
    for (route, operations) in document["paths"].as_object().expect("paths") {
        for (verb, operation) in operations.as_object().expect("operations") {
            let responses = operation["responses"].as_object().expect("responses");
            for (status, response) in responses {
                let Some(content) = response["content"].as_object() else {
                    continue;
                };
                for (media_type, entry) in content {
                    let mut examples: Vec<&Value> = entry.get("example").into_iter().collect();
                    if let Some(named) = entry["examples"].as_object() {
                        examples.extend(named.values().map(|e| &e["value"]));
                    }
                    if examples.is_empty() {
                        continue;
                    }
                    // The schema beside the example, by its place in the
                    // document, as the recording checks answers.
                    let mut schema = document.clone();
                    schema["$ref"] = Value::String(format!(
                        "#/paths/{}/{verb}/responses/{status}/content/{}/schema",
                        escape(route),
                        escape(media_type)
                    ));
                    let validator = jsonschema::validator_for(&schema)
                        .unwrap_or_else(|e| panic!("{verb} {route} {status}: {e}"));
                    for example in examples {
                        let errors: Vec<String> = validator
                            .iter_errors(example)
                            .map(|e| e.to_string())
                            .collect();
                        assert!(
                            errors.is_empty(),
                            "{verb} {route} {status} {media_type}: the example doesn't fit:\n{}",
                            errors.join("\n")
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    checked
}

/// The examples, OpenAPI document and reference, written from real
/// exchanges: the document is OpenAPI 3.1 and every example in it fits its
/// schema, and every resource and problem has its page. With
/// `MONOKULO_API_OUT` set, they're written there for the docs build.
#[tokio::test]
async fn the_reference_is_written_from_real_exchanges() {
    let recorded = record().await;
    every_link_names_a_route(&recorded);
    let document = openapi::build(&recorded);
    let reference = catalog(&recorded);
    let texts = [
        (
            "examples.json",
            serde_json::to_string_pretty(&recorded).unwrap() + "\n",
        ),
        ("openapi.json", openapi::to_text(&document)),
        ("reference.json", to_text(&reference)),
    ];
    // What's written reads back as written.
    for (name, text) in &texts {
        let _: Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    assert_eq!(document["openapi"], "3.1.0");
    assert!(document["paths"].as_object().is_some_and(|p| !p.is_empty()));
    let examples = examples_fit_their_schemas(&document);
    assert!(examples > 10, "only {examples} examples in the document");
    let resources = reference["resources"].as_array().unwrap();
    assert_eq!(resources.len(), catalog::resources().len());
    for resource in catalog::resources() {
        let page = format!("{DOCS}/rels/{}", resource.rel);
        assert!(
            resources
                .iter()
                .any(|r| r["rel"] == resource.rel && r["page"] == page),
            "{} has no page at {page}",
            resource.rel
        );
    }
    let problems = reference["problems"].as_array().unwrap();
    for problem in Problem::ALL {
        assert!(
            problems
                .iter()
                .any(|p| p["slug"] == problem.slug() && p["type"] == problem.type_uri()),
            "{} isn't in the reference",
            problem.slug()
        );
        assert_eq!(
            problem.type_uri(),
            format!("{DOCS}/problems/{}", problem.slug())
        );
    }
    if let Some(dir) = std::env::var_os(OUT) {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("the reference's folder can be made");
        for (name, text) in &texts {
            std::fs::write(dir.join(name), text).expect("the reference is writable");
        }
    }
}

/// The served document is the one built from the code, without examples.
#[test]
fn the_served_document_has_no_recorded_examples() {
    let served = openapi::document();
    assert_eq!(served, &openapi::build(&[]));
    assert_eq!(examples_fit_their_schemas(served), 0);
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
fn every_link_names_a_route(recorded: &[examples::Exchange]) {
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
    for exchange in recorded {
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

/// The entry offers each key the forms it may use, and only those: a shop's
/// server sends its logs through the entry's form, never a path it builds.
#[tokio::test]
async fn the_entry_offers_each_key_its_own_forms() {
    let (state, _engine) = state().await;
    let router = build_router(state.clone());
    let admin = admin_session_cookie(&router).await;
    let session = admin.strip_prefix("session=").unwrap().to_owned();
    let store = store_with_keys(&router, &state, &session, "bakery.example", "XMR").await;
    let ak = connect_app(&router, &store.id, &format!("session={session}")).await;
    for (key, forms) in [
        (None, vec![]),
        (Some(store.public_key.as_str()), vec!["create-order"]),
        (
            Some(store.secret_key.as_str()),
            vec!["create-order", "send-logs"],
        ),
        (Some(ak.as_str()), vec!["create-order", "disconnect"]),
    ] {
        let response = router
            .clone()
            .oneshot(api_request("GET", "/api/v1", key, None))
            .await
            .unwrap();
        let body = crate::http::test_support::body_json(response).await;
        let mut offered: Vec<&str> = body["_templates"]
            .as_object()
            .map(|templates| templates.keys().map(String::as_str).collect())
            .unwrap_or_default();
        offered.sort_unstable();
        assert_eq!(offered, forms, "the forms offered to {key:?}");
    }
    let response = router
        .clone()
        .oneshot(api_request("GET", "/api/v1", Some(&store.secret_key), None))
        .await
        .unwrap();
    let body = crate::http::test_support::body_json(response).await;
    assert_eq!(body["_templates"]["send-logs"]["method"], "POST");
    assert_eq!(body["_templates"]["send-logs"]["target"], "/api/v1/logs");
}
