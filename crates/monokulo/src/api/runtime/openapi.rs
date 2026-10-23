//! The API's OpenAPI 3.1 document, built from the catalog: each handler's
//! method, path, who may call it, its doc comment and the schemas of what
//! it takes and gives (the same ones its forms show); the pages the API
//! links (`text/html`); each stream's events; and the plugin and app
//! handshakes beside the API, with every problem they answer. Its examples
//! are real exchanges recorded against a real engine (`super::examples`),
//! in the copy the docs build publishes and `cargo xtask api openapi`
//! writes (`super::reference`). An instance serves it at
//! `/api/v1/openapi.json` without examples: nothing recorded is built in.
//!
//! OpenAPI 3.1 has no schema for a stream's events, so a `text/event-stream`
//! answer lists them in `x-events`: each event's name, what it means and
//! the schema of its `data:` (JSON, but for a plain-text `done`). Its
//! example is a recorded stream, as it's sent.

use std::sync::OnceLock;

use axum::http::StatusCode;
use schemars::SchemaGenerator;
use serde_json::{json, Map, Value};

use super::catalog::{self, Kind};
use super::examples::Exchange;
use super::extract::OperationDoc;
use super::problem::{Problem, ProblemBody, PROBLEM_JSON};
use super::DOCS;

/// The OpenAPI document's media type.
pub const OPENAPI_JSON: &str = "application/openapi+json";

/// The media type of a stream of server-sent events.
const EVENT_STREAM: &str = "text/event-stream";

/// The name an example recorded with `key` has: the key's kind, or
/// `no-key`.
fn key_name(key: Option<&str>) -> String {
    key.map_or_else(|| "no-key".to_owned(), str::to_owned)
}

/// What an example recorded with `key` is about.
fn key_summary(key: Option<&str>) -> &'static str {
    match key {
        Some("sk") => "With the store's secret key (sk_)",
        Some("ak") => "With a connected app's key (ak_)",
        Some("pk") => "With the store's public key (pk_)",
        Some(_) => "With a key",
        None => "With no key",
    }
}

/// A recorded stream (`[{"event", "data"}]`) as it's sent.
fn stream_text(events: &Value) -> Option<String> {
    let events = events.as_array()?;
    let mut text = String::new();
    for event in events {
        let name = event.get("event")?.as_str()?;
        let data = match event.get("data")? {
            Value::String(text) => text.clone(),
            data => data.to_string(),
        };
        text.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    Some(text)
}

/// The content of an answer `media` with `schema`, and the examples
/// `recorded` for it: one, or one per key when several keys were recorded.
fn content_entry(media: &str, schema: Option<Value>, recorded: &[&Exchange]) -> Value {
    let mut entry = json!({"schema": schema.unwrap_or_else(|| json!({"type": "string"}))});
    let with_body: Vec<&&Exchange> = recorded.iter().filter(|e| e.response.is_some()).collect();
    if media == EVENT_STREAM {
        if let Some(text) = with_body
            .iter()
            .find_map(|e| stream_text(e.response.as_ref()?))
        {
            entry["example"] = Value::String(text);
        }
        return entry;
    }
    let mut seen = std::collections::BTreeMap::new();
    for exchange in &with_body {
        seen.entry(key_name(exchange.key.as_deref()))
            .or_insert(*exchange);
    }
    match seen.len() {
        0 => {}
        1 => {
            entry["example"] = seen
                .into_values()
                .next()
                .and_then(|e| e.response.clone())
                .unwrap_or_default()
        }
        _ => {
            let examples: Map<String, Value> = seen
                .into_iter()
                .map(|(name, exchange)| {
                    (
                        name,
                        json!({"summary": key_summary(exchange.key.as_deref()), "value": exchange.response}),
                    )
                })
                .collect();
            entry["examples"] = Value::Object(examples);
        }
    }
    entry
}

/// The `x-events` of a stream's answer: each event's meaning and data.
fn events_entry(events: Vec<(&'static str, &'static str, Value)>) -> Value {
    let events: Map<String, Value> = events
        .into_iter()
        .map(|(name, doc, data)| (name.to_owned(), json!({"description": doc, "data": data})))
        .collect();
    Value::Object(events)
}

/// Each problem `recorded` met, by its slug: examples of an error answer.
fn problem_examples(recorded: &[&Exchange]) -> Map<String, Value> {
    let mut examples = Map::new();
    for exchange in recorded.iter().filter(|e| e.status >= 400) {
        if let Some(slug) = exchange
            .response
            .as_ref()
            .and_then(|r| r.get("type"))
            .and_then(Value::as_str)
            .and_then(|t| t.rsplit('/').next())
        {
            examples
                .entry(slug.to_owned())
                .or_insert_with(|| json!({"value": exchange.response}));
        }
    }
    examples
}

/// The path parameters of `template`, as OpenAPI parameters.
fn path_parameters(names: &[&str]) -> Vec<Value> {
    names
        .iter()
        .map(|name| json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"}}))
        .collect()
}

/// The `GET` of a page the API links: HTML for people.
fn page_operation(resource: &catalog::ResourceInfo) -> Value {
    let (security, who) = match resource.who {
        // Offered only to callers acting for the merchant, who opens it
        // signed in.
        Some(who) => (json!([{"session": []}]), who.keys()),
        None => (json!([{}]), Vec::new()),
    };
    let mut entry = json!({
        "operationId": format!("{}.get", resource.rel),
        "tags": ["pages"],
        "summary": catalog::title(resource),
        "description": catalog::description(resource),
        "security": security,
        "externalDocs": {"url": format!("{DOCS}/rels/{}", resource.rel)},
        "responses": {
            "200": {"description": "The page.", "content": {"text/html": {"schema": {"type": "string"}}}},
            "404": {"description": "No such page, or not for this merchant.", "content": {"text/html": {"schema": {"type": "string"}}}},
        },
    });
    if !who.is_empty() {
        entry["x-monokulo-offered-to"] = json!(who);
    }
    if !resource.params.is_empty() {
        entry["parameters"] = Value::Array(path_parameters(resource.params));
    }
    entry
}

/// The operation of a handshake: what it takes, what it gives, and each
/// problem it answers, by status, with those `recorded`.
fn handshake_operation(
    generator: &mut SchemaGenerator,
    handshake: &catalog::Handshake,
    problem: &Value,
    recorded: &[&Exchange],
) -> Value {
    let request = (handshake.request)(generator);
    let response = (handshake.response)(generator);
    let ok: Vec<&Exchange> = recorded
        .iter()
        .copied()
        .filter(|e| e.status == 200)
        .collect();
    let mut responses = Map::new();
    responses.insert(
        "200".into(),
        json!({"description": "OK", "content": {"application/json": content_entry("application/json", Some(response), &ok)}}),
    );
    let mut problems: Vec<Problem> = handshake.problems.to_vec();
    problems.sort_by_key(|p| p.status().as_u16());
    let examples = problem_examples(recorded);
    for status in problems
        .iter()
        .map(|p| p.status())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let slugs: Vec<String> = problems
            .iter()
            .filter(|p| p.status() == status)
            .map(|p| format!("[`{}`]({})", p.slug(), p.type_uri()))
            .collect();
        let mut content = json!({"schema": problem});
        let here: Map<String, Value> = problems
            .iter()
            .filter(|p| p.status() == status)
            .filter_map(|p| {
                examples
                    .get(p.slug())
                    .map(|e| (p.slug().to_owned(), e.clone()))
            })
            .collect();
        if !here.is_empty() {
            content["examples"] = Value::Object(here);
        }
        responses.insert(
            status.as_u16().to_string(),
            json!({
                "description": format!("A problem: {}.", slugs.join(", ")),
                "content": {PROBLEM_JSON: content},
            }),
        );
    }
    let mut request_content = json!({"schema": request});
    if let Some(example) = ok.iter().find_map(|e| e.request.clone()) {
        request_content["example"] = example;
    }
    let names: Vec<&str> = handshake
        .template
        .split('/')
        .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
        .collect();
    let mut entry = json!({
        "operationId": handshake.operation,
        "tags": ["handshakes"],
        "summary": catalog::summary(handshake.doc),
        "description": handshake.doc,
        "security": [{}],
        "parameters": path_parameters(&names),
        "requestBody": {"required": true, "content": {"application/json": request_content}},
        "responses": responses,
    });
    if names.is_empty() {
        entry
            .as_object_mut()
            .expect("an operation")
            .remove("parameters");
    }
    entry
}

/// The document, with `examples`.
pub fn build(examples: &[Exchange]) -> Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.definitions_path = "/components/schemas".into();
            settings.meta_schema = None;
        })
        .into_generator();
    let problem = generator.subschema_for::<ProblemBody>().to_value();
    let mut paths = Map::new();
    let mut tags = Vec::new();
    fn insert(paths: &mut Map<String, Value>, template: &str, method: &str, entry: Value) {
        paths
            .entry(template.to_owned())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("a path item is an object")
            .insert(method.to_ascii_lowercase(), entry);
    }
    for resource in catalog::resources() {
        if resource.kind == Kind::Page {
            insert(
                &mut paths,
                resource.template,
                "GET",
                page_operation(resource),
            );
            continue;
        }
        tags.push(json!({
            "name": resource.rel,
            "description": catalog::description(resource),
            "externalDocs": {"url": format!("{DOCS}/rels/{}", resource.rel)},
        }));
    }
    tags.push(json!({
        "name": "pages",
        "description": "Pages for people the API links to: the payer's checkout, and an order's page on the merchant's dashboard.",
    }));
    tags.push(json!({
        "name": "handshakes",
        "description": "How a shop plugin or a connected app gets its key: once, beside the API.",
        "externalDocs": {"url": format!("{DOCS}/plugins/")},
    }));
    for handler in catalog::handlers() {
        let resource = catalog::resources()
            .into_iter()
            .find(|r| r.template == handler.template)
            .expect("a handler's resource is registered");
        let mut operation = OperationDoc::new(&mut generator);
        (handler.describe)(&mut operation);
        let OperationDoc {
            path_params,
            query,
            body,
            answers,
            events,
            ..
        } = operation;
        let mut parameters: Vec<Value> = path_parameters(&path_params);
        if let Some(query) = query {
            let name = query
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.rsplit('/').next())
                .unwrap_or_default()
                .to_owned();
            let resolved = generator.definitions().get(&name).cloned().unwrap_or(query);
            if let Some(fields) = resolved.get("properties").and_then(Value::as_object) {
                for (name, schema) in fields {
                    parameters.push(
                        json!({"name": name, "in": "query", "required": false, "schema": schema}),
                    );
                }
            }
        }
        let recorded: Vec<&Exchange> = examples
            .iter()
            .filter(|e| e.method == handler.method && e.route == handler.template)
            .collect();
        let cached = answers
            .iter()
            .any(|(status, ..)| *status == StatusCode::NOT_MODIFIED);
        let mut events = Some(events);
        let mut responses = Map::new();
        for (status, media, schema) in answers {
            let mut content = Map::new();
            if !media.is_empty() {
                let here: Vec<&Exchange> = recorded
                    .iter()
                    .copied()
                    .filter(|e| e.status == status.as_u16())
                    .collect();
                let mut entry = content_entry(media, schema, &here);
                if media == EVENT_STREAM {
                    if let Some(events) = events.take().filter(|e| !e.is_empty()) {
                        entry["x-events"] = events_entry(events);
                    }
                }
                content.insert(media.to_owned(), entry);
            }
            let mut response =
                json!({"description": status.canonical_reason().unwrap_or_default()});
            if !content.is_empty() {
                response["content"] = Value::Object(content);
            }
            if status == StatusCode::CREATED {
                response["headers"] = json!({"Location": {"description": "The new resource.", "schema": {"type": "string"}}});
            }
            if cached && (status == StatusCode::OK || status == StatusCode::NOT_MODIFIED) {
                response["headers"] = json!({
                    "ETag": {"description": "The body's version: send it back in `If-None-Match`.", "schema": {"type": "string"}},
                    "Cache-Control": {"description": "`private, max-age=…`: keep it that long.", "schema": {"type": "string"}},
                    "Vary": {"description": "One header naming `Authorization` (each key sees its own), and `Origin` when another site asks.", "schema": {"type": "string"}},
                });
            }
            responses.insert(status.as_u16().to_string(), response);
        }
        if cached {
            parameters.push(json!({
                "name": "If-None-Match",
                "in": "header",
                "required": false,
                "description": "The `ETag` of the copy the client holds: `304 Not Modified` while it's current.",
                "schema": {"type": "string"},
            }));
        }
        let mut problem_content = json!({"schema": problem});
        let problems = problem_examples(&recorded);
        if !problems.is_empty() {
            problem_content["examples"] = Value::Object(problems);
        }
        responses.insert(
            "default".into(),
            json!({"description": "A problem (RFC 9457): its `type` is its page in the docs.", "content": {PROBLEM_JSON: problem_content}}),
        );
        let keys = handler.who.keys();
        let security = if handler.who.admits(None) {
            json!([{}, {"bearer": []}])
        } else {
            json!([{"bearer": []}])
        };
        let mut entry = json!({
            "operationId": format!("{}.{}", resource.rel, handler.name),
            "tags": [resource.rel],
            "summary": catalog::summary(handler.doc),
            "description": handler.doc,
            "security": security,
            "x-monokulo-keys": keys,
            "responses": responses,
        });
        if let Some(form) = handler.form {
            entry["x-hal-forms-template"] = form.into();
        }
        if !parameters.is_empty() {
            entry["parameters"] = Value::Array(parameters);
        }
        if let Some(body) = body {
            let mut content = json!({"schema": body});
            if let Some(example) = recorded
                .iter()
                .find(|e| e.status < 400)
                .and_then(|e| e.request.clone())
            {
                content["example"] = example;
            }
            entry["requestBody"] =
                json!({"required": true, "content": {"application/json": content}});
        }
        insert(&mut paths, handler.template, handler.method, entry);
    }
    for handshake in catalog::handshakes() {
        let recorded: Vec<&Exchange> = examples
            .iter()
            .filter(|e| e.method == handshake.method && e.route == handshake.template)
            .collect();
        let entry = handshake_operation(&mut generator, handshake, &problem, &recorded);
        insert(&mut paths, handshake.template, handshake.method, entry);
    }
    let schemas = generator.take_definitions(true);
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Monokulo API",
            "version": "1",
            "description": "Monokulo's machine API. Resources are HAL-FORMS (`application/prs.hal-forms+json`): follow `_links`, use the forms in `_templates`. Errors are `application/problem+json`. The key in `Authorization: Bearer` names the store: its secret key (`sk_`), a connected app's key (`ak_`) or its public key (`pk_`, which only makes and reads orders). A plugin or app gets its key from its handshake (`/plugins/connect/{slug}/finish`, `/apps/connect`).",
        },
        "externalDocs": {"url": format!("{DOCS}/api/")},
        "servers": [{"url": "{monokulo}", "description": "Your Monokulo's address, with its path prefix if it has one.", "variables": {"monokulo": {"default": "https://www.monokulo.com"}}}],
        "tags": tags,
        "paths": paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": {
                "bearer": {"type": "http", "scheme": "bearer", "description": "The store's key: `sk_…`, `ak_…` or `pk_…`."},
                "session": {"type": "apiKey", "in": "cookie", "name": "session", "description": "The merchant, signed in to the dashboard."},
            },
        },
    })
}

/// The document as served: the schemas, without examples.
pub fn document() -> &'static Value {
    static DOCUMENT: OnceLock<Value> = OnceLock::new();
    DOCUMENT.get_or_init(|| build(&[]))
}

/// `document` as text, as served and published.
pub fn to_text(document: &Value) -> String {
    let mut json = serde_json::to_string_pretty(document).expect("the document is JSON");
    json.push('\n');
    json
}

/// `s` as one step of a JSON pointer into the document.
pub fn pointer_step(s: &str) -> String {
    s.replace('~', "~0")
        .replace('/', "~1")
        .replace('{', "%7B")
        .replace('}', "%7D")
        .replace('+', "%2B")
}

/// Checks a JSON answer seen under test, or recorded, against the
/// document: its media type must be one its operation lists for its status
/// (or, for an error, a problem), and its body must fit the schema there.
/// Panics, failing the test or the recording, when it doesn't.
#[cfg(any(test, feature = "test-support"))]
pub fn check_response(
    method: &str,
    route: &str,
    status: StatusCode,
    content_type: &str,
    body: &Value,
) {
    let document = document();
    let verb = method.to_ascii_lowercase();
    let operation = &document["paths"][route][&verb];
    if operation.is_null() {
        return;
    }
    let code = status.as_u16().to_string();
    let listed = if operation["responses"][&code].is_null() {
        assert!(
            !status.is_success(),
            "{method} {route} answered {status}, which the document doesn't list"
        );
        "default".to_owned()
    } else {
        code
    };
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    assert!(
        !operation["responses"][&listed]["content"][&media].is_null(),
        "{method} {route} answered {status} with {content_type:?}, which the document doesn't list for it"
    );
    if !status.is_success() {
        let named = body.get("type").and_then(Value::as_str).unwrap_or_default();
        assert!(
            Problem::ALL.iter().any(|p| p.type_uri() == named),
            "{method} {route}: problem type {named:?} isn't a documented one"
        );
    }
    let pointer = format!(
        "#/paths/{}/{verb}/responses/{listed}/content/{}/schema",
        pointer_step(route),
        pointer_step(&media)
    );
    // One validator per schema, for the process: compiling the whole
    // document each time would slow every test down.
    static VALIDATORS: std::sync::Mutex<
        Option<std::collections::HashMap<String, std::sync::Arc<jsonschema::Validator>>>,
    > = std::sync::Mutex::new(None);
    let validator = {
        let mut cache = VALIDATORS.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get_or_insert_with(Default::default)
            .entry(pointer.clone())
            .or_insert_with(|| {
                let mut schema = document.clone();
                schema["$ref"] = Value::String(pointer);
                std::sync::Arc::new(
                    jsonschema::validator_for(&schema).expect("the document's schemas compile"),
                )
            })
            .clone()
    };
    let errors: Vec<String> = validator
        .iter_errors(body)
        .map(|e| format!("{} at {}", e, e.instance_path))
        .collect();
    assert!(
        errors.is_empty(),
        "{method} {route} answered {status} with a body that doesn't fit the document:\n{}\n{body:#}",
        errors.join("\n")
    );
}
