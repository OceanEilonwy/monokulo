//! What the macros register: each resource (`resource!`), handler
//! (`#[get]`, `#[post]`, `#[delete]`) and body (`#[api::body]`), gathered by
//! `linkme` from wherever they are. The router, the OpenAPI document and the
//! docs read these; there is no list of them anywhere else.

use super::auth::Who;
use super::extract::OperationDoc;
use crate::http::AppState;

/// What a resource answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// HAL-FORMS JSON.
    Hal,
    /// Server-sent events.
    Stream,
    /// A page for people (HTML).
    Page,
    /// The OpenAPI document.
    Document,
}

impl Kind {
    /// Its media type, for a link's `type`.
    pub fn media_type(self) -> &'static str {
        match self {
            Kind::Hal => super::hal::HAL_FORMS,
            Kind::Stream => "text/event-stream",
            Kind::Page => "text/html",
            Kind::Document => super::openapi::OPENAPI_JSON,
        }
    }
}

/// A resource: one file's `resource!`.
#[derive(Debug)]
pub struct ResourceInfo {
    pub template: &'static str,
    pub rel: &'static str,
    pub kind: Kind,
    /// Its description, for a resource with no body.
    pub doc: Option<&'static str>,
    pub params: &'static [&'static str],
    pub file: &'static str,
    pub module: &'static str,
}

/// A handler: a method on a resource.
pub struct HandlerInfo {
    pub template: &'static str,
    pub method: &'static str,
    pub name: &'static str,
    pub who: Who,
    /// The form it is, when it changes something.
    pub form: Option<&'static str>,
    pub doc: &'static str,
    pub file: &'static str,
    pub module: &'static str,
    pub route: fn(axum::Router<AppState>) -> axum::Router<AppState>,
    pub describe: fn(&mut OperationDoc<'_>),
}

/// A resource's body (`#[api::body]`).
pub struct BodyInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub file: &'static str,
    pub module: &'static str,
    pub schema: fn(&mut schemars::SchemaGenerator) -> schemars::Schema,
}

#[linkme::distributed_slice]
pub static RESOURCES: [ResourceInfo];

#[linkme::distributed_slice]
pub static HANDLERS: [HandlerInfo];

#[linkme::distributed_slice]
pub static BODIES: [BodyInfo];

/// Every resource, by template.
pub fn resources() -> Vec<&'static ResourceInfo> {
    let mut all: Vec<&ResourceInfo> = RESOURCES.iter().collect();
    all.sort_by_key(|r| r.template);
    all
}

/// Every handler, by template then method.
pub fn handlers() -> Vec<&'static HandlerInfo> {
    let mut all: Vec<&HandlerInfo> = HANDLERS.iter().collect();
    all.sort_by_key(|h| (h.template, h.method));
    all
}

/// `resource`'s handlers.
pub fn handlers_of(resource: &ResourceInfo) -> impl Iterator<Item = &'static HandlerInfo> + '_ {
    HANDLERS
        .iter()
        .filter(move |h| h.template == resource.template)
}

/// The `GET` of the resource at `template`.
pub fn get_of(template: &str) -> Option<&'static HandlerInfo> {
    HANDLERS
        .iter()
        .find(|h| h.template == template && h.method == "GET")
}

/// The body of `resource`, if it has one.
pub fn body_of(resource: &ResourceInfo) -> Option<&'static BodyInfo> {
    BODIES.iter().find(|b| b.module == resource.module)
}

/// What `resource` is, for people: its body's doc comment, or its own.
pub fn description(resource: &ResourceInfo) -> &'static str {
    body_of(resource)
        .map(|b| b.doc)
        .or(resource.doc)
        .unwrap_or_default()
}

/// Its title: the first line of its description.
pub fn title(resource: &ResourceInfo) -> String {
    summary(description(resource))
}

/// The first paragraph of `doc`, on one line: a title or summary.
pub fn summary(doc: &str) -> String {
    doc.split("\n\n")
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl ResourceInfo {
    /// The key a link to it has: `mk:<rel>`.
    pub fn link_key(&self) -> String {
        format!("mk:{}", self.rel)
    }
}
