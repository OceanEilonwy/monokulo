//! The machine API: what shop plugins, connected apps and the embed
//! library (`monokulo-client.js`) call, at `/api/v1`. The key in
//! `Authorization: Bearer` names the store (`http::store_key`): its secret
//! key (`sk_`), a connected app's (`ak_`) or its public key (`pk_`).
//!
//! # One resource per file
//!
//! Each file under `api/` (but `mod.rs` files and `runtime/`) is one
//! resource, written with the macros from `api-macros`:
//!
//! ```ignore
//! // api/v1/order.rs
//! use crate::api::{self, prelude::*};
//!
//! api::resource!("/api/v1/orders/{order_id}", rel = "order", params(order_id: OrderId));
//!
//! /// One order: what to pay, where, and how far the payment has got.
//! #[api::body]
//! struct Body {
//!     /// The order's id.
//!     order_id: OrderId,
//!     // ...
//!     events: Link<order_events::Path>,
//! }
//!
//! /// Read one order.
//! #[get(any_key)]
//! async fn get(path: Path, orders: Orders) -> api::Result<Body> { ... }
//! ```
//!
//! - `resource!` makes the file's `Path` (its URL, printed with
//!   `Display`; nothing else spells it) and registers the resource.
//!   A second one in the same file doesn't compile
//!   (`ONLY_ONE_API_RESOURCE_PER_FILE` is defined twice). A page for
//!   people the API links (`kind = page`) may say who a link to it is
//!   offered to (`who = sk | session`); anyone, without.
//! - `#[get(who)]`, `#[post(who, form = "name")]`, `#[delete(...)]`
//!   register a handler on the file's `Path`, behind its auth check: `who`
//!   is `pk`, `sk`, `ak`, `session`, or `any_key` (pk, sk, ak), `server`
//!   (sk, ak), `app` (ak) or `anyone` (with no key too). A key it doesn't
//!   admit gets `403`, no key `401`; a key this Monokulo doesn't know is
//!   `401` before any handler runs (`http::abuse`, `runtime::auth`). A
//!   handler with a `form` is a HAL-FORMS template on its resource, and a
//!   type (`Form<orders::create>`) another body can offer. It answers a
//!   body, `Created`, `NoContent`, `Cached` (a body its caller may keep) or
//!   an `EventStream` of the events its type lists.
//! - `#[api::body]` is the resource's body: `Link<T>` fields become
//!   `_links` (`mk:<T's rel>`), `Form<H>` fields and the resource's own
//!   forms `_templates`, each only when the caller's key may use it, and
//!   `self` is the body's own `Path`. An `Option<OnionLink<T>>` field
//!   named `onion_…` is `T` absolute on the onion address, under its own
//!   relation (`mk:onion-…`) that the field's doc comment describes.
//!   `#[api::object]` is a part of a body or an event's data (its `Link<T>`
//!   fields become `_links` too, without `self`), `#[api::input]` what a
//!   form sends, and `#[api::scalar]` a checked string
//!   (`runtime::scalars`).
//! - A missing doc comment on a body, object, input, scalar, field,
//!   handler or problem doesn't compile: the docs are written from them.
//!
//! The router (`runtime::router`), the OpenAPI document (`runtime::openapi`)
//! and the docs' reference (`runtime::reference`) read the catalog the
//! macros fill (`runtime::catalog`); there's no list of resources anywhere
//! else. Every link is a path on this host, relative to it, so a client
//! that came in over the onion address stays there, and starts with the
//! path this Monokulo is served under (`crate::base::for_request`); while the
//! instance has an onion address, `mk:onion-…` links say where the same
//! thing is on it, absolute, for a client on the web address serving
//! someone on Tor.

pub(crate) mod checkout;
pub(crate) mod dashboard_order;
pub mod runtime;
pub(crate) mod v1;

pub use api_macros::{body, input, object, resource, scalar};

/// What a handler answers: a resource, or a problem.
pub type Result<T> = std::result::Result<T, runtime::problem::ProblemDetails>;

/// What a resource's file uses.
pub mod prelude {
    pub use super::runtime::extract::{
        Apps, Caller, Client, ConnectedApp, Instance, Orders, Page, Store, StreamSlot,
    };
    pub use super::runtime::hal::{
        Cached, Created, EventStream, Events, Form, Href, Link, NoContent, OnionLink, StreamEvent,
    };
    pub use super::runtime::problem::{internal, not_found, Problem};
    pub use super::runtime::scalars::{
        Amount, CurrencyCode, Description, IdempotencyKey, MerchantRef,
    };
    pub use crate::db::OrderId;
    pub use api_macros::{delete, get, post};
}

/// What the macros' code refers to.
#[doc(hidden)]
pub mod __private {
    pub use super::runtime::auth::{respond, Authorized, Who, WhoMarker};
    pub use super::runtime::catalog::{
        BodyInfo, HandlerInfo, Kind, RelInfo, ResourceInfo, BODIES, HANDLERS, RELS, RESOURCES,
    };
    pub use super::runtime::extract::{json_body, ApiParam, Input, OperationDoc};
    pub use super::runtime::hal::{
        described, encode_segment, hal_parts, link_key, link_title, object_links, object_schema,
        schema_name, Body, FormTarget, LinksBuilder, Resource, Respond, TemplatesBuilder,
    };
    pub use super::runtime::problem::{not_found, ProblemDetails};
    pub use super::runtime::scalars::ScalarRules;
    pub use crate::http::AppState;
    pub use {axum, linkme, schemars, serde, serde_json};
}
