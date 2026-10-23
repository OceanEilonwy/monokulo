//! The API's OpenAPI 3.1 document, built from the catalog: each handler's
//! method, path, who may call it, its doc comment and the schemas of what
//! it takes and gives (the same ones its forms show). Its examples are
//! real exchanges the tests recorded (`super::examples`). Served at
//! `/api/v1/openapi.json` and committed as `docs/api/openapi.json`
//! (`super::reference`).

use std::sync::OnceLock;

use axum::http::StatusCode;
use serde_json::{json, Map, Value};

use super::catalog;
use super::examples::Exchange;
use super::extract::OperationDoc;
use super::problem::{ProblemBody, PROBLEM_JSON};
use super::DOCS;

/// The OpenAPI document's media type.
pub const OPENAPI_JSON: &str = "application/openapi+json";

/// The recorded exchanges the served document shows, as committed.
const RECORDED: &str = include_str!("../../../../../docs/api/examples.json");

/// The committed examples.
pub fn committed_examples() -> Vec<Exchange> {
    serde_json::from_str(RECORDED).unwrap_or_default()
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
    for resource in catalog::resources() {
        if resource.kind == catalog::Kind::Page {
            continue;
        }
        tags.push(json!({
            "name": resource.rel,
            "description": catalog::description(resource),
            "externalDocs": {"url": format!("{DOCS}/rels/{}", resource.rel)},
        }));
    }
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
            ..
        } = operation;
        let mut parameters: Vec<Value> = path_params
            .iter()
            .map(|name| json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"}}))
            .collect();
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
        let mut responses = Map::new();
        for (status, media, schema) in answers {
            let mut content = Map::new();
            if !media.is_empty() {
                let mut entry =
                    json!({"schema": schema.unwrap_or_else(|| json!({"type": "string"}))});
                if let Some(example) = recorded
                    .iter()
                    .find(|e| e.status == status.as_u16())
                    .and_then(|e| e.response.clone())
                {
                    entry["example"] = example;
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
            responses.insert(status.as_u16().to_string(), response);
        }
        // Each problem a recording met, as an example of the error answer.
        let mut problem_examples = Map::new();
        for exchange in recorded.iter().filter(|e| e.status >= 400) {
            if let Some(slug) = exchange
                .response
                .as_ref()
                .and_then(|r| r.get("type"))
                .and_then(Value::as_str)
                .and_then(|t| t.rsplit('/').next())
            {
                problem_examples
                    .entry(slug.to_owned())
                    .or_insert_with(|| json!({"value": exchange.response}));
            }
        }
        let mut problem_content = json!({"schema": problem});
        if !problem_examples.is_empty() {
            problem_content["examples"] = Value::Object(problem_examples);
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
        paths
            .entry(handler.template.to_owned())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("a path item is an object")
            .insert(handler.method.to_ascii_lowercase(), entry);
    }
    let schemas = generator.take_definitions(true);
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Monokulo API",
            "version": "1",
            "description": "Monokulo's machine API. Resources are HAL-FORMS (`application/prs.hal-forms+json`): follow `_links`, use the forms in `_templates`. Errors are `application/problem+json`. The key in `Authorization: Bearer` names the store: its secret key (`sk_`), a connected app's key (`ak_`) or its public key (`pk_`, which only makes and reads orders).",
        },
        "externalDocs": {"url": format!("{DOCS}/api/")},
        "servers": [{"url": "https://www.monokulo.com"}],
        "tags": tags,
        "paths": paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}},
        },
    })
}

/// The document as served: with the committed examples.
pub fn document() -> &'static Value {
    static DOCUMENT: OnceLock<Value> = OnceLock::new();
    DOCUMENT.get_or_init(|| build(&committed_examples()))
}

/// `document` as text, as served and committed.
pub fn to_text(document: &Value) -> String {
    let mut json = serde_json::to_string_pretty(document).expect("the document is JSON");
    json.push('\n');
    json
}

/// Checks a response the tests saw against the document: its body must
/// fit the schema of its operation for its status, or the problem schema
/// for an error. Panics, failing the test, when it doesn't.
#[cfg(test)]
pub fn check_response(
    method: &str,
    route: &str,
    status: StatusCode,
    content_type: &str,
    body: &Value,
) {
    let document = document();
    let verb = method.to_ascii_lowercase();
    if document["paths"][route][&verb].is_null() {
        return;
    }
    let escape = |s: &str| {
        s.replace('~', "~0")
            .replace('/', "~1")
            .replace('{', "%7B")
            .replace('}', "%7D")
            .replace('+', "%2B")
    };
    let path = escape(route);
    let pointer = if status.is_success() {
        assert!(
            content_type.starts_with(super::hal::HAL_FORMS),
            "{method} {route} answered {status} with {content_type:?}, not HAL-FORMS"
        );
        let code = status.as_u16().to_string();
        assert!(
            !document["paths"][route][&verb]["responses"][&code].is_null(),
            "{method} {route} answered {status}, which the document doesn't list"
        );
        format!(
            "#/paths/{path}/{verb}/responses/{code}/content/{}/schema",
            escape(super::hal::HAL_FORMS)
        )
    } else {
        assert!(
            content_type.starts_with(PROBLEM_JSON),
            "{method} {route} answered {status} with {content_type:?}, not a problem"
        );
        let named = body.get("type").and_then(Value::as_str).unwrap_or_default();
        assert!(
            super::problem::Problem::ALL
                .iter()
                .any(|p| p.type_uri() == named),
            "{method} {route}: problem type {named:?} isn't a documented one"
        );
        format!(
            "#/paths/{path}/{verb}/responses/default/content/{}/schema",
            escape(PROBLEM_JSON)
        )
    };
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
