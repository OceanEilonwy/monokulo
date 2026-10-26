//! The API reference (`reference.json`): every resource, onion link
//! relation, handshake and problem, with what its page says. Monokulo writes
//! it from its code (`api::runtime::reference`), and the docs build
//! (`cargo xtask pages api`) reads it into the site's `/rels/{rel}/` and
//! `/problems/{slug}/` pages, both through these types. A field renamed or
//! dropped on one side no longer compiles on the other, and one missing
//! from, or unknown to, the file fails the docs build instead of publishing
//! a page that says something else (an access rule left out reads as "open
//! to whoever has its URL").
//!
//! Schemas, forms and recorded bodies stay JSON: they are JSON Schema, HAL
//! templates and real answers, whose shape is the API's, not this file's.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub resources: Vec<Resource>,
    /// The onion links' own relations (`mk:onion-…`).
    pub rels: Vec<Rel>,
    /// The handshakes beside the API.
    pub handshakes: Vec<Handshake>,
    /// The schemas the bodies, events and handshakes refer to, by name.
    pub definitions: serde_json::Map<String, Value>,
    pub problems: Vec<Problem>,
}

/// One resource: an API route, a stream or a page for people.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub rel: String,
    /// Its link's key in `_links` (`mk:order`).
    pub key: String,
    pub template: String,
    pub media_type: String,
    pub title: String,
    pub description: String,
    /// Where its page is published.
    pub page: String,
    pub methods: Vec<Method>,
    /// For a page with no methods: the keys its link is offered to, or
    /// `None` when anyone who reads a link to it may follow it.
    pub offered_to: Option<Vec<String>>,
    /// A stream's events.
    pub events: Vec<Event>,
    /// Its body's schema.
    pub body: Option<Value>,
    pub examples: Vec<Exchange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub method: String,
    pub name: String,
    /// The keys that may call it.
    pub keys: Vec<String>,
    /// Whether a request with no key may.
    pub anyone: bool,
    pub doc: String,
    /// The form it is offered as in `_templates`, if any.
    pub form: Option<String>,
    /// That form's HAL template.
    pub template: Option<Value>,
    /// The problems it may answer, by slug.
    pub problems: Vec<String>,
}

/// One event of a stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub name: String,
    pub doc: String,
    /// Its data's schema.
    pub data: Value,
}

/// A handshake beside the API (an app connecting, say).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub method: String,
    pub template: String,
    pub title: String,
    pub doc: String,
    /// The request's schema.
    pub request: Value,
    /// The response's schema.
    pub response: Value,
    pub problems: Vec<String>,
    pub examples: Vec<Exchange>,
}

/// An onion link's relation: the same resource as `target`, absolute, on
/// the onion address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rel {
    pub rel: String,
    pub key: String,
    pub template: String,
    pub media_type: String,
    pub title: String,
    pub description: String,
    pub page: String,
    pub target: RelTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelTarget {
    pub rel: String,
    pub key: String,
    pub template: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    pub slug: String,
    /// The URI an error's `type` names it by: its page.
    #[serde(rename = "type")]
    pub type_uri: String,
    pub status: u16,
    pub title: String,
    pub doc: String,
}

/// One request and its answer, recorded against a real engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exchange {
    pub method: String,
    /// The route's template.
    pub route: String,
    /// The path asked for.
    pub path: String,
    /// The key it carried: `pk`, `sk`, `ak`, or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Whether the Monokulo it was made with has an onion address (its
    /// answers then carry the onion links too). Most have none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub onion: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
    pub status: u16,
    /// The answer's JSON; for a stream, the events read from it, each
    /// `{"event", "data"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}
