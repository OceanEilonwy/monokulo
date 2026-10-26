//! The API's reference, written from the code so it can't drift, and
//! never committed: `cargo xtask api openapi` and the docs build (`cargo
//! xtask pages api`) make it with monokulo's `api_reference` example
//! (`super::recording`):
//!
//! - `examples.json`: one recorded exchange of each kind
//!   (`super::examples`);
//! - `openapi.json`: the OpenAPI document with those examples
//!   (`super::openapi`), which the docs site's API explorer, `/api/`, is
//!   drawn from;
//! - `reference.json`: every resource (its description, methods, who may
//!   call them, body schema, forms, a stream's events, a page's who it's
//!   offered to, and examples), every onion link's relation (`mk:onion-…`),
//!   the handshakes and every problem, which `cargo xtask pages api` turns
//!   into the docs site's `/rels/{rel}/` and `/problems/{slug}/` pages.
//!
//! The recording makes the exchanges against a real engine and checks what
//! it writes: the document is OpenAPI 3.1, every example fits its schema,
//! every resource and problem is there with its page, and every link names
//! a route.

use api_reference::{
    Event, Handshake, Method, Problem as ProblemPage, Reference, Rel, RelTarget, Resource,
};
use serde_json::Value;

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
    let slugs = |problems: &[Problem]| problems.iter().map(|p| p.slug().to_owned()).collect();
    let resources: Vec<Resource> = catalog::resources()
        .into_iter()
        .map(|resource| {
            let body =
                catalog::body_of(resource).map(|body| (body.schema)(&mut generator).to_value());
            let methods: Vec<Method> = catalog::handlers_of(resource)
                .map(|handler| {
                    let mut template = None;
                    if handler.form.is_some() {
                        template = serde_json::to_value(super::hal::template(handler)).ok();
                    }
                    Method {
                        method: handler.method.to_string(),
                        name: handler.name.to_string(),
                        keys: names(handler.who.keys()),
                        anyone: handler.who.admits(None),
                        doc: handler.doc.to_string(),
                        form: handler.form.map(|form| form.to_string()),
                        template,
                        problems: slugs(&handler.problems()),
                    }
                })
                .collect();
            let recorded: Vec<Exchange> = examples
                .iter()
                .filter(|e| e.route == resource.template)
                .cloned()
                .collect();
            // A stream's events: each one's name, meaning and data.
            let events: Vec<Event> = catalog::get_of(resource.template)
                .map(|get| {
                    let mut operation = OperationDoc::new(&mut generator);
                    (get.describe)(&mut operation);
                    operation
                        .events
                        .into_iter()
                        .map(|(name, doc, data)| Event {
                            name: name.to_string(),
                            doc: doc.to_string(),
                            data: serde_json::to_value(data).expect("a schema is JSON"),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Resource {
                rel: resource.rel.to_string(),
                key: resource.link_key(),
                template: resource.template.to_string(),
                media_type: resource.kind.media_type().to_string(),
                title: catalog::title(resource).to_string(),
                description: catalog::description(resource).to_string(),
                page: format!("{DOCS}/rels/{}", resource.rel),
                methods,
                offered_to: resource.who.map(|who| names(who.keys())),
                events,
                body,
                examples: recorded,
            }
        })
        .collect();
    // The handshakes beside the API, with the problems they answer.
    let handshakes: Vec<Handshake> = catalog::handshakes()
        .into_iter()
        .map(|handshake| {
            let recorded: Vec<Exchange> = examples
                .iter()
                .filter(|e| e.route == handshake.template && e.method == handshake.method)
                .cloned()
                .collect();
            Handshake {
                method: handshake.method.to_string(),
                template: handshake.template.to_string(),
                title: catalog::summary(handshake.doc).to_string(),
                doc: handshake.doc.to_string(),
                request: serde_json::to_value((handshake.request)(&mut generator))
                    .expect("a schema is JSON"),
                response: serde_json::to_value((handshake.response)(&mut generator))
                    .expect("a schema is JSON"),
                problems: slugs(handshake.problems),
                examples: recorded,
            }
        })
        .collect();
    let definitions = generator.take_definitions(true);
    // The onion links' own relations: each a page, like a resource's.
    let rels: Vec<Rel> = catalog::rels()
        .into_iter()
        .map(|rel| {
            let target = (rel.target)();
            Rel {
                rel: rel.rel.to_string(),
                key: rel.link_key(),
                template: format!("http://….onion{}", target.template),
                media_type: target.kind.media_type().to_string(),
                title: catalog::summary(rel.doc).to_string(),
                description: rel.doc.to_string(),
                page: format!("{DOCS}/rels/{}", rel.rel),
                target: RelTarget {
                    rel: target.rel.to_string(),
                    key: target.link_key(),
                    template: target.template.to_string(),
                },
            }
        })
        .collect();
    let reference = Reference {
        resources,
        rels,
        handshakes,
        definitions,
        problems: Problem::ALL
            .iter()
            .map(|problem| ProblemPage {
                slug: problem.slug().to_owned(),
                type_uri: problem.type_uri(),
                status: problem.status().as_u16(),
                title: problem.title().to_owned(),
                doc: problem.doc().to_owned(),
            })
            .collect(),
    };
    let mut reference = serde_json::to_value(reference).expect("the reference is JSON");
    // Its schemas as the OpenAPI document has them: no `null`.
    super::openapi::without_null(&mut reference);
    reference
}

/// Keys as the reference names them.
fn names<S: ToString>(keys: impl IntoIterator<Item = S>) -> Vec<String> {
    keys.into_iter().map(|key| key.to_string()).collect()
}

/// `value` as written: pretty, with a final newline.
pub fn to_text(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests;
