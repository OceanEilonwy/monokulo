//! The API's committed reference, written from the code so it can't drift:
//!
//! - `docs/api/examples.json`: one recorded exchange of each kind
//!   (`super::examples`);
//! - `docs/api/openapi.json`: the OpenAPI document with those examples
//!   (`super::openapi`);
//! - `docs/api/reference.json`: every resource (its description, methods,
//!   who may call them, body schema, forms and examples) and every
//!   problem, which `cargo xtask pages docs` turns into the docs site's
//!   `/api/reference/`, `/rels/{rel}/` and `/problems/{slug}/` pages.
//!
//! A test makes the exchanges and checks all three are what this code
//! writes. When a change is meant, write them again with
//! `MONOKULO_WRITE_API=1 cargo nextest run -p monokulo api::runtime::reference`
//! (twice when the examples change: the served document reads them when
//! it's built).

use serde_json::{json, Value};

use super::catalog::{self, Kind};
use super::examples::Exchange;
use super::problem::Problem;
use super::DOCS;

/// Where the committed files are, from this crate.
pub const EXAMPLES_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/examples.json");
pub const OPENAPI_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/openapi.json");
pub const REFERENCE_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/reference.json");

/// Every resource and problem, with what its page says.
pub fn catalog(examples: &[Exchange]) -> Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.meta_schema = None)
        .into_generator();
    let resources: Vec<Value> = catalog::resources()
        .into_iter()
        .map(|resource| {
            let body = catalog::body_of(resource).map(|body| (body.schema)(&mut generator).to_value());
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
                    })
                })
                .collect();
            let recorded: Vec<&Exchange> = examples
                .iter()
                .filter(|e| e.route == resource.template)
                .collect();
            json!({
                "rel": resource.rel,
                "key": if resource.kind == Kind::Page { resource.rel.to_owned() } else { resource.link_key() },
                "template": resource.template,
                "media_type": resource.kind.media_type(),
                "title": catalog::title(resource),
                "description": catalog::description(resource),
                "page": format!("{DOCS}/rels/{}", resource.rel),
                "methods": methods,
                "body": body,
                "examples": recorded,
            })
        })
        .collect();
    let definitions = generator.take_definitions(true);
    json!({
        "resources": resources,
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

/// `value` as committed: pretty, with a final newline.
pub fn to_text(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests;
