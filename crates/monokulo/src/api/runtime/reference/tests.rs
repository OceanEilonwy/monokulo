//! The reference is written from exchanges really made, and is sound; and
//! the API keeps to its own rules: one resource per file, every link and
//! form names a route.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::api::runtime::recording::{self, Reference};
use crate::api::runtime::{catalog, openapi};

/// The examples, OpenAPI document and reference, written from real
/// exchanges as `cargo xtask api openapi` writes them, and sound
/// (`Reference::of` checks them): the document is OpenAPI 3.1 and every
/// example in it fits its schema, every link names a route, and every
/// resource, relation, problem and handshake has its page.
#[tokio::test]
async fn the_reference_is_written_from_real_exchanges() {
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let written = Reference::of(recording::record(engine.router()).await);
    // What's written reads back as written.
    for (name, text) in written.files() {
        let _: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let document = &written.openapi;
    // The entry and the store, as each key sees them.
    let entry = &document["paths"]["/api/v1"]["get"]["responses"]["200"]["content"]
        ["application/prs.hal-forms+json"]["examples"];
    // On a Monokulo without an onion address (the common case) and on
    // one with: only the second has the onion links.
    for key in ["no-key", "pk", "sk", "ak"] {
        let plain = &entry[key]["value"]["_links"];
        assert!(plain.is_object(), "no {key} entry");
        assert!(plain["mk:onion-api"].is_null(), "{key}: {plain}");
        let onion = &entry[format!("{key}-with-onion")]["value"]["_links"];
        assert!(
            onion["mk:onion-api"].is_object(),
            "{key}-with-onion: {onion}"
        );
    }
    let order = &document["paths"]["/api/v1/orders/{order_id}"]["get"]["responses"]["200"]
        ["content"]["application/prs.hal-forms+json"]["examples"];
    assert!(order["sk"]["value"]["_links"]["mk:onion-checkout"].is_null());
    assert!(order["sk-with-onion"]["value"]["_links"]["mk:onion-checkout"].is_object());
    let store = &document["paths"]["/api/v1/store"]["get"]["responses"]["200"]["content"]
        ["application/prs.hal-forms+json"]["examples"];
    for key in ["sk", "ak"] {
        assert!(store[key]["value"]["name"].is_string(), "no {key} store");
    }
    // Order ids look like real ones: `order_` and 32 hex digits.
    let examples = serde_json::to_string(&written.examples).unwrap();
    let ids = regex::Regex::new(r"order_[0-9a-fA-F-]{8,}").unwrap();
    let mut seen = 0;
    for id in ids.find_iter(&examples) {
        let hex = &id.as_str()["order_".len()..];
        assert!(
            hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()),
            "{} isn't an order id",
            id.as_str()
        );
        seen += 1;
    }
    assert!(seen > 5, "only {seen} order ids");
    // Each handshake has a recorded success and problems.
    for handshake in catalog::handshakes() {
        let responses = &document["paths"][handshake.template]
            [handshake.method.to_ascii_lowercase()]["responses"];
        let answers = &responses["200"]["content"]["application/json"]["examples"];
        assert!(
            answers["no-key"]["value"]["onion_api"].is_null()
                && answers["no-key-with-onion"]["value"]["onion_api"].is_string(),
            "{} has no recorded answer with and without an onion address: {answers}",
            handshake.template
        );
        for status in ["400", "401", "413", "415", "429", "500", "503"] {
            assert!(
                responses[status]["content"]["application/problem+json"]["examples"]
                    .as_object()
                    .is_some_and(|e| !e.is_empty()),
                "{} {status} has no recorded problem",
                handshake.template
            );
        }
    }
    // The pages the API links are in it, as HTML.
    for page in ["/checkout/{order_id}", "/stores/{id}/orders/{order_id}"] {
        assert!(
            !document["paths"][page]["get"]["responses"]["200"]["content"]["text/html"].is_null(),
            "{page}"
        );
    }
}

/// The served document is the one built from the code, without examples.
#[test]
fn the_served_document_has_no_recorded_examples() {
    let served = openapi::document();
    assert_eq!(served, &openapi::build(&[]));
    assert_eq!(recording::examples_fit_their_schemas(served), 0);
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
