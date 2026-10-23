//! HAL-FORMS (`application/prs.hal-forms+json`): every resource carries
//! `_links`, and `_templates` for what the caller may do.
//!
//! A body's `Link<T>` fields go into `_links` under `mk:<T's rel>` (or the
//! field's name, when that is a registered relation such as `next`), its
//! `Form<H>` fields and its resource's own forms into `_templates`; each is
//! left out when the caller's key may not use it ([`super::auth`]). The
//! `mk` curie points at each relation's page in the docs.

use std::marker::PhantomData;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::auth;
use super::catalog::{self, HandlerInfo, Kind, ResourceInfo};
use super::extract::OperationDoc;
use super::DOCS;

/// The media type of every API resource.
pub const HAL_FORMS: &str = "application/prs.hal-forms+json";

/// A resource's path type (`resource!`'s `Path`).
pub trait Resource: std::fmt::Display {
    fn info() -> &'static ResourceInfo;
}

/// A resource's body (`#[api::body]`).
pub trait Body: Serialize + JsonSchema {
    type Path: Resource;
    /// The path of the resource this body is.
    fn self_path(&self) -> Self::Path;
}

/// A handler, as a form (`Form<orders::create>`).
pub trait FormTarget {
    fn info() -> &'static HandlerInfo;
}

/// A link to the resource at `T`.
pub struct Link<T> {
    href: String,
    target: PhantomData<T>,
}

impl<T: Resource> Link<T> {
    pub fn new(path: T) -> Self {
        Link {
            href: path.to_string(),
            target: PhantomData,
        }
    }

    /// A link to `path` with `query` (such as a page of a list).
    pub fn with_query(path: T, query: &impl Serialize) -> Self {
        let query = serde_urlencoded::to_string(query).unwrap_or_default();
        let href = if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        };
        Link {
            href,
            target: PhantomData,
        }
    }

    pub fn href(&self) -> &str {
        &self.href
    }
}

/// A link outside the API (the docs), under a registered relation.
pub struct Href {
    pub href: String,
    pub media_type: &'static str,
}

/// A form a body offers: the handler `H`.
pub struct Form<H>(PhantomData<H>);

impl<H> Default for Form<H> {
    fn default() -> Self {
        Form(PhantomData)
    }
}

impl<H> Form<H> {
    pub fn new() -> Self {
        Form(PhantomData)
    }
}

/// Whether the caller may follow a link to `resource`: its `GET` admits
/// their key.
fn may_follow(resource: &ResourceInfo) -> bool {
    match resource.kind {
        // A page for people: anyone with the link.
        Kind::Page => true,
        _ => catalog::get_of(resource.template).is_some_and(|get| get.who.admits(auth::current())),
    }
}

/// The key a link to `T` has: `name` when it's a registered relation,
/// else `mk:<T's rel>`.
pub fn link_key<T: Resource>(registered: Option<&str>) -> String {
    registered.map_or_else(|| T::info().link_key(), str::to_owned)
}

/// The title of a link to `T`: its body's first doc line.
pub fn link_title<T: Resource>() -> String {
    catalog::title(T::info())
}

/// One link as written.
#[derive(Serialize)]
struct LinkObject {
    href: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    title: String,
    #[serde(rename = "type", skip_serializing_if = "str::is_empty")]
    type_: &'static str,
}

/// A body's `_links` while it's written.
pub struct LinksBuilder {
    links: Vec<(String, LinkObject)>,
}

impl LinksBuilder {
    pub fn new<P: Resource>(self_path: &P) -> Self {
        LinksBuilder {
            links: vec![(
                "self".to_owned(),
                LinkObject {
                    href: self_path.to_string(),
                    title: String::new(),
                    type_: "",
                },
            )],
        }
    }

    /// Adds `link`, when the caller may follow it.
    pub fn add<T: Resource>(&mut self, link: &Link<T>, registered: Option<&str>) {
        let info = T::info();
        if !may_follow(info) {
            return;
        }
        self.links.push((
            link_key::<T>(registered),
            LinkObject {
                href: link.href.clone(),
                title: catalog::title(info),
                type_: if info.kind == Kind::Hal {
                    ""
                } else {
                    info.kind.media_type()
                },
            },
        ));
    }

    pub fn add_href(&mut self, href: &Href, rel: &str) {
        self.links.push((
            rel.to_owned(),
            LinkObject {
                href: href.href.clone(),
                title: String::new(),
                type_: href.media_type,
            },
        ));
    }
}

impl Serialize for LinksBuilder {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if self.links.iter().any(|(key, _)| key.starts_with("mk:")) {
            map.serialize_entry(
                "curies",
                &[json!({"name": "mk", "href": format!("{DOCS}/rels/{{rel}}"), "templated": true})],
            )?;
        }
        for (key, link) in &self.links {
            map.serialize_entry(key, link)?;
        }
        map.end()
    }
}

/// A body's `_templates` while it's written: the forms the caller may use.
pub struct TemplatesBuilder {
    forms: Vec<&'static HandlerInfo>,
}

impl TemplatesBuilder {
    /// Starts with the resource's own forms (its `post` and `delete`
    /// handlers with a `form`).
    pub fn for_resource<P: Resource>() -> Self {
        let mut builder = TemplatesBuilder { forms: Vec::new() };
        for handler in catalog::handlers_of(P::info()) {
            builder.push(handler);
        }
        builder
    }

    /// Adds the form `H`.
    pub fn add<H: FormTarget>(&mut self) {
        self.push(H::info());
    }

    fn push(&mut self, handler: &'static HandlerInfo) {
        let Some(name) = handler.form else { return };
        if !handler.who.admits(auth::current()) || self.forms.iter().any(|f| f.form == Some(name)) {
            return;
        }
        self.forms.push(handler);
    }

    pub fn is_empty(&self) -> bool {
        self.forms.is_empty()
    }
}

impl Serialize for TemplatesBuilder {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.forms.len()))?;
        for handler in &self.forms {
            if let Some(name) = handler.form {
                map.serialize_entry(name, &template(handler))?;
            }
        }
        map.end()
    }
}

/// A HAL-FORMS template.
#[derive(Clone, Debug, Serialize)]
pub struct Template {
    pub title: String,
    pub method: &'static str,
    #[serde(rename = "contentType", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<&'static str>,
    pub target: String,
    pub properties: Vec<Property>,
}

/// A field of a HAL-FORMS template.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Property {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    #[serde(rename = "minLength", skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(rename = "maxLength", skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

/// The form `handler` is: its title (its doc's first line), method,
/// target and fields.
pub fn template(handler: &HandlerInfo) -> Template {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let mut operation = OperationDoc::new(&mut generator);
    (handler.describe)(&mut operation);
    let body = operation.body.take();
    let definitions = generator.take_definitions(true);
    // A form on a path with parameters isn't offered; there is none.
    let target = handler.template.to_owned();
    Template {
        title: catalog::summary(handler.doc),
        method: handler.method,
        content_type: body.as_ref().map(|_| "application/json"),
        target,
        properties: body
            .map(|schema| properties(&resolve(&schema, &definitions), &definitions))
            .unwrap_or_default(),
    }
}

/// `schema`, with a `$ref` followed into `definitions`.
fn resolve(schema: &Value, definitions: &Map<String, Value>) -> Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.rsplit('/').next())
        .and_then(|name| definitions.get(name))
        .cloned()
        .unwrap_or_else(|| schema.clone())
}

/// A form's fields, from its input type's JSON Schema: each property's
/// name, description, type, pattern, lengths and example, and whether it's
/// required. The one place a schema becomes a form.
pub fn properties(schema: &Value, definitions: &Map<String, Value>) -> Vec<Property> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(fields) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    fields
        .iter()
        .map(|(name, field)| {
            // `Option<T>` is `anyOf: [T, null]`, `T` a `$ref` to a scalar.
            let inner = field
                .get("anyOf")
                .and_then(Value::as_array)
                .and_then(|any| any.iter().find(|s| s.get("type") != Some(&json!("null"))))
                .unwrap_or(field);
            let resolved = resolve(inner, definitions);
            let text = |key: &str| {
                field
                    .get(key)
                    .or_else(|| resolved.get(key))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            let number = |key: &str| resolved.get(key).and_then(Value::as_u64);
            let types: Vec<&str> = match resolved.get("type") {
                Some(Value::String(t)) => vec![t.as_str()],
                Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            let type_ = match types.into_iter().find(|t| *t != "null") {
                Some("boolean") => "checkbox",
                Some("integer" | "number") => "number",
                _ => "text",
            };
            Property {
                name: name.clone(),
                prompt: text("description"),
                required: required.contains(&name.as_str()),
                type_: type_.to_owned(),
                regex: text("pattern"),
                min_length: number("minLength"),
                max_length: number("maxLength"),
                placeholder: resolved
                    .get("examples")
                    .and_then(|e| e.get(0))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        })
        .collect()
}

// -- Schemas the macros build bodies' from ----------------------------------

/// A body's schema name: unique across modules (`order.Body` and
/// `orders.Body` are both `Body`).
pub fn schema_name(module: &str, name: &str) -> String {
    let resource = module.rsplit("::").next().unwrap_or(module);
    let mut out = String::new();
    for part in resource.split('_') {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if name == "Body" {
        out
    } else {
        format!("{out}{name}")
    }
}

/// `schema` with `doc` as its description.
pub fn described(schema: Schema, doc: &str) -> Value {
    let mut value = schema.to_value();
    if let Some(object) = value.as_object_mut() {
        object.insert("description".into(), doc.into());
    }
    value
}

/// An object schema.
pub fn object_schema(doc: &str, properties: Map<String, Value>, required: Vec<&str>) -> Schema {
    let title = catalog::summary(doc);
    Schema::try_from(json!({
        "type": "object",
        "title": title,
        "description": doc,
        "properties": properties,
        "required": required,
    }))
    .expect("an object schema")
}

/// Adds `_links` (self and `links`, each `(key, title)`) and `_templates`
/// to a body's `properties`.
pub fn hal_parts(
    generator: &mut SchemaGenerator,
    properties: &mut Map<String, Value>,
    links: Vec<(String, String)>,
) {
    let _ = generator;
    let link = |title: &str| {
        json!({
            "type": "object",
            "description": title,
            "properties": {
                "href": {"type": "string"},
                "title": {"type": "string"},
                "type": {"type": "string"}
            },
            "required": ["href"]
        })
    };
    let mut link_properties = Map::new();
    link_properties.insert("self".into(), link("This resource."));
    link_properties.insert(
        "curies".into(),
        json!({"type": "array", "description": "Explains the `mk:` relations: each is a page in the docs.", "items": {"type": "object"}}),
    );
    for (key, title) in links {
        link_properties.insert(key, link(&title));
    }
    properties.insert(
        "_links".into(),
        json!({
            "type": "object",
            "description": "Links to what this key may read next, by relation. A link the key may not follow is left out.",
            "properties": link_properties,
            "required": ["self"],
            "additionalProperties": false
        }),
    );
    properties.insert(
        "_templates".into(),
        json!({
            "type": "object",
            "description": "HAL-FORMS templates: the forms this key may use here, by name.",
            "additionalProperties": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "method": {"type": "string"},
                    "contentType": {"type": "string"},
                    "target": {"type": "string"},
                    "properties": {"type": "array", "items": {"type": "object", "required": ["name"]}}
                },
                "required": ["method", "target", "properties"]
            }
        }),
    );
}

/// A path parameter's value, percent-encoded for a URL path.
pub fn encode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// -- Answers -----------------------------------------------------------------

/// What a handler answers with.
pub trait Respond {
    fn respond(self) -> Response;
    /// Its answer, for the OpenAPI document.
    fn describe(operation: &mut OperationDoc<'_>);
}

/// `body` as a HAL-FORMS resource with `status`.
pub fn hal<T: Serialize>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_string(body) {
        Ok(json) => (
            status,
            [(header::CONTENT_TYPE, HeaderValue::from_static(HAL_FORMS))],
            json,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "an API resource could not be written");
            super::problem::internal().into_response()
        }
    }
}

impl<B: Body> Respond for B {
    fn respond(self) -> Response {
        hal(StatusCode::OK, &self)
    }

    fn describe(operation: &mut OperationDoc<'_>) {
        operation.answer::<B>(StatusCode::OK);
    }
}

/// A body just made: `201 Created`, with its `Location`.
pub struct Created<B>(pub B);

impl<B: Body> Respond for Created<B> {
    fn respond(self) -> Response {
        let location = self.0.self_path().to_string();
        let mut response = hal(StatusCode::CREATED, &self.0);
        if let Ok(location) = HeaderValue::from_str(&location) {
            response.headers_mut().insert(header::LOCATION, location);
        }
        response
    }

    fn describe(operation: &mut OperationDoc<'_>) {
        operation.answer::<B>(StatusCode::CREATED);
    }
}

/// Done, with nothing to say: `204 No Content`.
pub struct NoContent;

impl Respond for NoContent {
    fn respond(self) -> Response {
        StatusCode::NO_CONTENT.into_response()
    }

    fn describe(operation: &mut OperationDoc<'_>) {
        operation.answer_empty(StatusCode::NO_CONTENT);
    }
}

/// A stream of server-sent events.
pub struct EventStream(pub Response);

impl Respond for EventStream {
    fn respond(self) -> Response {
        self.0
    }

    fn describe(operation: &mut OperationDoc<'_>) {
        operation.answer_media(StatusCode::OK, "text/event-stream");
    }
}

/// The OpenAPI document.
pub struct Document(pub String);

impl Respond for Document {
    fn respond(self) -> Response {
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(super::openapi::OPENAPI_JSON),
            )],
            self.0,
        )
            .into_response()
    }

    fn describe(operation: &mut OperationDoc<'_>) {
        operation.answer_media(StatusCode::OK, super::openapi::OPENAPI_JSON);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_parameter_is_percent_encoded() {
        assert_eq!(encode_segment("order_1"), "order_1");
        assert_eq!(encode_segment("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn a_body_s_schema_name_is_its_resource_s() {
        assert_eq!(
            schema_name("monokulo::api::v1::order_events", "Body"),
            "OrderEvents"
        );
        assert_eq!(
            schema_name("monokulo::api::v1::store", "Wallet"),
            "StoreWallet"
        );
    }
}
