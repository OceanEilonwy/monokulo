//! The API's reference, written from the code so it can't drift, and
//! never committed: `cargo xtask api openapi` and the docs build (`cargo
//! xtask pages api`) make it with monokulo's `api_reference` example
//! (`super::recording`):
//!
//! - `examples.json`: one recorded exchange of each kind
//!   (`super::examples`);
//! - `openapi.json`: the OpenAPI document with those examples
//!   (`super::openapi`);
//! - `reference.json`: every resource (its description, methods, who may
//!   call them, body schema, forms, a stream's events, a page's who it's
//!   offered to, and examples), every onion link's relation (`mk:onion-…`),
//!   the handshakes and every problem, which `cargo xtask pages api` turns
//!   into the docs site's `/api/reference/`, `/rels/{rel}/` and
//!   `/problems/{slug}/` pages.
//!
//! The recording makes the exchanges against a real engine and checks what
//! it writes: the document is OpenAPI 3.1, every example fits its schema,
//! every resource and problem is there with its page, and every link names
//! a route.

use serde_json::{json, Value};

use super::catalog;
use super::examples::Exchange;
use super::extract::OperationDoc;
use super::problem::Problem;
use super::DOCS;

/// Every resource and problem, with what its page says.
pub fn catalog(examples: &[Exchange]) -> Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.meta_schema = None)
        .into_generator();
    let resources: Vec<Value> = catalog::resources()
        .into_iter()
        .map(|resource| {
            let body =
                catalog::body_of(resource).map(|body| (body.schema)(&mut generator).to_value());
            let methods: Vec<Value> = catalog::handlers_of(resource)
                .map(|handler| {
                    let mut template = None;
                    if handler.form.is_some() {
                        template = serde_json::to_value(super::hal::template(handler)).ok();
                    }
                    json!({
                        "method": handler.method,
                        "name": handler.name,
                        "keys": handler.who.keys(),
                        "anyone": handler.who.admits(None),
                        "doc": handler.doc,
                        "form": handler.form,
                        "template": template,
                        "problems": handler.problems().iter().map(|p| p.slug()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            let recorded: Vec<&Exchange> = examples
                .iter()
                .filter(|e| e.route == resource.template)
                .collect();
            // A stream's events: each one's name, meaning and data.
            let events: Vec<Value> = catalog::get_of(resource.template)
                .map(|get| {
                    let mut operation = OperationDoc::new(&mut generator);
                    (get.describe)(&mut operation);
                    operation
                        .events
                        .into_iter()
                        .map(|(name, doc, data)| json!({"name": name, "doc": doc, "data": data}))
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "rel": resource.rel,
                "key": resource.link_key(),
                "template": resource.template,
                "media_type": resource.kind.media_type(),
                "title": catalog::title(resource),
                "description": catalog::description(resource),
                "page": format!("{DOCS}/rels/{}", resource.rel),
                "methods": methods,
                "offered_to": resource.who.map(|who| who.keys()),
                "events": events,
                "body": body,
                "examples": recorded,
            })
        })
        .collect();
    // The handshakes beside the API, with the problems they answer.
    let handshakes: Vec<Value> = catalog::handshakes()
        .into_iter()
        .map(|handshake| {
            let recorded: Vec<&Exchange> = examples
                .iter()
                .filter(|e| e.route == handshake.template && e.method == handshake.method)
                .collect();
            json!({
                "method": handshake.method,
                "template": handshake.template,
                "title": catalog::summary(handshake.doc),
                "doc": handshake.doc,
                "request": (handshake.request)(&mut generator),
                "response": (handshake.response)(&mut generator),
                "problems": handshake.problems.iter().map(|p| p.slug()).collect::<Vec<_>>(),
                "examples": recorded,
            })
        })
        .collect();
    let definitions = generator.take_definitions(true);
    // The onion links' own relations: each a page, like a resource's.
    let rels: Vec<Value> = catalog::rels()
        .into_iter()
        .map(|rel| {
            let target = (rel.target)();
            json!({
                "rel": rel.rel,
                "key": rel.link_key(),
                "template": format!("http://….onion{}", target.template),
                "media_type": target.kind.media_type(),
                "title": catalog::summary(rel.doc),
                "description": rel.doc,
                "page": format!("{DOCS}/rels/{}", rel.rel),
                "target": {
                    "rel": target.rel,
                    "key": target.link_key(),
                    "template": target.template,
                },
            })
        })
        .collect();
    json!({
        "resources": resources,
        "rels": rels,
        "handshakes": handshakes,
        "definitions": definitions,
        "problems": Problem::ALL.iter().map(|problem| json!({
            "slug": problem.slug(),
            "type": problem.type_uri(),
            "status": problem.status().as_u16(),
            "title": problem.title(),
            "doc": problem.doc(),
        })).collect::<Vec<_>>(),
    })
}

/// `value` as written: pretty, with a final newline.
pub fn to_text(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests;
