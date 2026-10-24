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
    /// For a page: who a link to it is offered to (`who = …` in
    /// `resource!`); `None` for anyone with the link. A resource the API
    /// serves offers its link to whoever its `GET` admits instead.
    pub who: Option<Who>,
    pub file: &'static str,
    pub module: &'static str,
}

/// A route a machine calls outside the API, beside it in the OpenAPI
/// document: a plugin's or app's handshake, which hands out the key the
/// API is then called with. Registered next to its handler.
pub struct Handshake {
    pub method: &'static str,
    pub template: &'static str,
    /// Its operation's id in the OpenAPI document.
    pub operation: &'static str,
    /// What it does: its summary, then the rest.
    pub doc: &'static str,
    /// The schema of what it takes (a reference to it, from the generator).
    pub request: fn(&mut schemars::SchemaGenerator) -> serde_json::Value,
    /// The schema of what it answers.
    pub response: fn(&mut schemars::SchemaGenerator) -> serde_json::Value,
    /// Every problem it answers with.
    pub problems: &'static [super::problem::Problem],
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

impl HandlerInfo {
    /// Every problem it may answer with, by status then slug: the
    /// middleware's in front of it (`http::abuse`, `http::embed_domains`),
    /// a key it doesn't take (`super::auth::Authorized`), its parameters'
    /// and its own (`problems = [...]` in its attribute). The OpenAPI
    /// document lists exactly these; under test an answer with any other
    /// fails (`super::openapi::check_response`), and the recording must
    /// show each one (`super::recording`).
    pub fn problems(&self) -> Vec<super::problem::Problem> {
        use super::auth::Key;
        use super::problem::Problem;
        let method = axum::http::Method::from_bytes(self.method.as_bytes())
            .expect("a handler's method is one");
        let mut problems = crate::http::abuse::api_route_problems(&method, self.template);
        problems.extend(crate::http::embed_domains::api_route_problems(
            &method,
            self.template,
        ));
        if !self.who.admits(None) {
            problems.push(Problem::Unauthorized);
        }
        if [Key::Pk, Key::Sk, Key::Ak]
            .into_iter()
            .any(|key| !self.who.admits(Some(key)))
        {
            problems.push(Problem::Forbidden);
        }
        let mut generator = schemars::SchemaGenerator::default();
        let mut operation = OperationDoc::new(&mut generator);
        (self.describe)(&mut operation);
        problems.extend(operation.problems);
        problems.sort_by_key(|p| (p.status().as_u16(), p.slug()));
        problems.dedup();
        problems
    }
}

/// A resource's body (`#[api::body]`).
pub struct BodyInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub file: &'static str,
    pub module: &'static str,
    pub schema: fn(&mut schemars::SchemaGenerator) -> schemars::Schema,
}

/// A relation of its own for an absolute onion link
/// (`hal::OnionLink`): a body's `Option<OnionLink<T>>` field.
pub struct RelInfo {
    /// `onion-checkout`: the field's name.
    pub rel: &'static str,
    /// The field's doc comment: the relation's page.
    pub doc: &'static str,
    /// The resource it links to.
    pub target: fn() -> &'static ResourceInfo,
    pub file: &'static str,
}

impl RelInfo {
    /// The key a link under it has: `mk:<rel>`.
    pub fn link_key(&self) -> String {
        format!("mk:{}", self.rel)
    }
}

#[linkme::distributed_slice]
pub static RESOURCES: [ResourceInfo];

#[linkme::distributed_slice]
pub static RELS: [RelInfo];

#[linkme::distributed_slice]
pub static HANDLERS: [HandlerInfo];

#[linkme::distributed_slice]
pub static BODIES: [BodyInfo];

#[linkme::distributed_slice]
pub static HANDSHAKES: [Handshake];

/// Every handshake, by template.
pub fn handshakes() -> Vec<&'static Handshake> {
    let mut all: Vec<&Handshake> = HANDSHAKES.iter().collect();
    all.sort_by_key(|h| h.template);
    all
}

/// Every resource, by template.
pub fn resources() -> Vec<&'static ResourceInfo> {
    let mut all: Vec<&ResourceInfo> = RESOURCES.iter().collect();
    all.sort_by_key(|r| r.template);
    all
}

/// Every onion link relation, by name, once each (two bodies may carry
/// the same one: an order in a list and on its own).
pub fn rels() -> Vec<&'static RelInfo> {
    let mut all: Vec<&RelInfo> = RELS.iter().collect();
    all.sort_by_key(|r| r.rel);
    all.dedup_by_key(|r| r.rel);
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

    /// For a page: whether a link to it is offered to a caller with `key`.
    pub fn offered_to(&self, key: Option<super::auth::Key>) -> bool {
        self.who.is_none_or(|who| who.admits(key))
    }
}
