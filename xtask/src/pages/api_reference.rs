//! `cargo xtask pages api --site DIR`: the HTTP API's reference, generated
//! from the code and from real exchanges (`crate::api`, which runs
//! monokulo's `api::runtime::reference` test into `target/api/`; nothing is
//! committed), into the site beside the docs' `/api/` page:
//!
//! - `api/reference/`: every resource, with its methods and who may call
//!   them, and every problem;
//! - `rels/{rel}/`: one page per resource, which the API's `mk:` link
//!   relations point at (`https://docs.monokulo.com/rels/{rel}`): what it
//!   is, its URL, its methods, its body's fields, the forms it offers and
//!   recorded examples;
//! - `problems/{slug}/`: one page per problem, which an error's `type`
//!   points at;
//! - `api/v1/openapi.json`: the OpenAPI document.
//!
//! Every word comes from monokulo's doc comments, every example from a
//! request the tests really made, so the pages can't drift from the API.

use super::docs::markdown_html;
use super::site_assets;
use super::site_header::{self, Section, GITHUB_URL};
use crate::support::{at, Exit};
use maud::{html, Markup, DOCTYPE};
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Where the reference is generated, from the repository's root.
const GENERATED: &str = "target/api";

/// `cargo xtask pages api --site DIR`.
pub(crate) fn build(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let site = match args {
        ["--site", dir] => PathBuf::from(dir),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pages api needs --site DIR",
            ))
        }
    };
    let generated = root.join(GENERATED);
    let exit = crate::api::generate(root, &generated)?;
    if !exit.succeeded() {
        return Ok(exit);
    }
    let written = write(&generated, &site)?;
    eprintln!("API reference in {}: {written} page(s)", site.display());
    Ok(Exit::SUCCESS)
}

fn read_json(path: &Path) -> io::Result<Value> {
    let text = fs::read_to_string(path).map_err(|e| at(path, e))?;
    serde_json::from_str(&text).map_err(|e| at(path, io::Error::other(e)))
}

/// Writes every page and the OpenAPI document, from the `generated`
/// reference, into `site`; returns how many pages.
pub(super) fn write(generated: &Path, site: &Path) -> io::Result<usize> {
    let reference = read_json(&generated.join("reference.json"))?;
    points_here(&reference)?;
    let put = |path: PathBuf, contents: String| -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
        }
        fs::write(&path, contents).map_err(|e| at(&path, e))
    };
    let mut pages = 0;
    put(
        site.join("api/reference/index.html"),
        index(&reference).into_string(),
    )?;
    pages += 1;
    for resource in list(&reference, "resources") {
        let rel = text(resource, "rel");
        put(
            site.join("rels").join(rel).join("index.html"),
            resource_page(resource, &reference).into_string(),
        )?;
        pages += 1;
    }
    for problem in list(&reference, "problems") {
        let slug = text(problem, "slug");
        put(
            site.join("problems").join(slug).join("index.html"),
            problem_page(problem, &reference).into_string(),
        )?;
        pages += 1;
    }
    let openapi = generated.join("openapi.json");
    let dir = site.join("api/v1");
    fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
    fs::copy(&openapi, dir.join("openapi.json")).map_err(|e| at(&openapi, e))?;
    site_assets::write(site)?;
    Ok(pages)
}

/// The `mk` curie and each problem's `type` point at the pages this site
/// publishes: a reference that names another site isn't published.
fn points_here(reference: &Value) -> io::Result<()> {
    let wrong = |what: &str, found: &str, expected: String| {
        io::Error::other(format!(
            "{what} points at {found}, not {expected} where this site publishes it"
        ))
    };
    for resource in list(reference, "resources") {
        let expected = format!("{}/rels/{}", site_header::SITE_URL, text(resource, "rel"));
        if text(resource, "page") != expected {
            return Err(wrong(
                text(resource, "rel"),
                text(resource, "page"),
                expected,
            ));
        }
    }
    for problem in list(reference, "problems") {
        let expected = format!(
            "{}/problems/{}",
            site_header::SITE_URL,
            text(problem, "slug")
        );
        if text(problem, "type") != expected {
            return Err(wrong(
                text(problem, "slug"),
                text(problem, "type"),
                expected,
            ));
        }
    }
    Ok(())
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or_default()
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// A page of the reference, `up` from the site's root.
fn page(up: &str, title: &str, description: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Monokulo API" }
                meta name="description" content=(description);
                (site_assets::head(up))
            }
            body {
                (site_header::render(Section::Api, up, GITHUB_URL))
                main.docs-page {
                    nav aria-label="API" {
                        a href=(format!("{up}api/")) { "HTTP API" } " › "
                        a href=(format!("{up}api/reference/")) { "Reference" }
                    }
                    article { (body) }
                }
            }
        }
    }
}

/// Who may call a method, in words.
fn who(method: &Value) -> String {
    let keys: Vec<&str> = list(method, "keys")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut names: Vec<String> = keys
        .iter()
        .filter(|k| **k != "session")
        .map(|k| format!("{k}_"))
        .collect();
    if method["anyone"].as_bool() == Some(true) {
        names.insert(0, "no key".to_owned());
    }
    names.join(", ")
}

fn index(reference: &Value) -> Markup {
    let up = "../../";
    let body = html! {
        h1 { "API reference" }
        p {
            "Every resource of " code { "/api/v1" } ", what it is and who may use it, and every problem it answers with. "
            "The " a href=(format!("{up}api/v1/openapi.json")) { "OpenAPI document" } " has the same, for tools."
        }
        h2 { "Resources" }
        div.tbl { table {
            thead { tr { th { "Relation" } th { "URL" } th { "Methods" } th { "What it is" } } }
            tbody {
                @for resource in list(reference, "resources") {
                    tr {
                        td { a href=(format!("{up}rels/{}/", text(resource, "rel"))) { code { (text(resource, "key")) } } }
                        td { code { (text(resource, "template")) } }
                        td {
                            @for method in list(resource, "methods") {
                                code { (text(method, "method")) } " "
                            }
                        }
                        td { (text(resource, "title")) }
                    }
                }
            }
        } }
        h2 { "Problems" }
        div.tbl { table {
            thead { tr { th { "Type" } th { "Status" } th { "Title" } } }
            tbody {
                @for problem in list(reference, "problems") {
                    tr {
                        td { a href=(format!("{up}problems/{}/", text(problem, "slug"))) { code { (text(problem, "slug")) } } }
                        td { (problem["status"]) }
                        td { (text(problem, "title")) }
                    }
                }
            }
        } }
    };
    page(
        up,
        "API reference",
        "Every resource and problem of Monokulo's HTTP API.",
        body,
    )
}

/// The fields of an object schema, with `definitions` to follow `$ref`s.
fn fields<'a>(schema: &'a Value, definitions: &'a Value) -> Vec<(&'a str, String, &'a str, bool)> {
    let required: Vec<&str> = list(schema, "required")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let Some(properties) = schema["properties"].as_object() else {
        return Vec::new();
    };
    properties
        .iter()
        .filter(|(name, _)| !name.starts_with('_'))
        .map(|(name, field)| {
            (
                name.as_str(),
                type_name(field, definitions),
                field["description"].as_str().unwrap_or_default(),
                required.contains(&name.as_str()),
            )
        })
        .collect()
}

/// A field's type in a few words: `string`, `integer`, `Wallet or null`.
fn type_name(schema: &Value, definitions: &Value) -> String {
    if let Some(reference) = schema["$ref"].as_str() {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        let target = &definitions[name];
        return match target["type"].as_str() {
            Some("object") | None => name.to_owned(),
            Some(kind) => kind.to_owned(),
        };
    }
    if let Some(any) = schema["anyOf"].as_array() {
        return any
            .iter()
            .map(|s| type_name(s, definitions))
            .collect::<Vec<_>>()
            .join(" or ");
    }
    match &schema["type"] {
        Value::String(kind) if kind == "array" => {
            format!("array of {}", type_name(&schema["items"], definitions))
        }
        Value::String(kind) => kind.clone(),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" or "),
        _ => "any".to_owned(),
    }
}

fn json_block(value: &Value) -> Markup {
    html! { pre { code { (serde_json::to_string_pretty(value).unwrap_or_default()) } } }
}

fn resource_page(resource: &Value, reference: &Value) -> Markup {
    let up = "../../";
    let definitions = &reference["definitions"];
    let body = &resource["body"];
    let links: Vec<(&String, &Value)> = body["properties"]["_links"]["properties"]
        .as_object()
        .map(|links| links.iter().filter(|(k, _)| *k != "curies").collect())
        .unwrap_or_default();
    let markup = html! {
        h1 { code { (text(resource, "key")) } }
        p.lede { (text(resource, "title")) }
        (markdown_html(text(resource, "description").split_once("\n\n").map_or("", |(_, rest)| rest)))
        h2 { "URL" }
        p { code { (text(resource, "template")) } " · " code { (text(resource, "media_type")) } }
        h2 { "Methods" }
        @for method in list(resource, "methods") {
            h3 id=(text(method, "name")) { code { (text(method, "method")) } " · " (who(method)) }
            (markdown_html(text(method, "doc")))
            @if let Some(form) = method["form"].as_str() {
                p { "Offered as the form " code { (form) } " in " code { "_templates" } ", to a key that may use it." }
                @if let Some(fields) = method["template"]["properties"].as_array().filter(|f| !f.is_empty()) {
                    div.tbl { table {
                        thead { tr { th { "Field" } th { "Required" } th { "Rules" } th { "What it is" } } }
                        tbody {
                            @for field in fields {
                                tr {
                                    td { code { (text(field, "name")) } }
                                    td { (if field["required"].as_bool() == Some(true) { "yes" } else { "no" }) }
                                    td {
                                        @if let Some(regex) = field["regex"].as_str() { code { (regex) } " " }
                                        @if let Some(max) = field["maxLength"].as_u64() { "at most " (max) " characters" }
                                    }
                                    td { (markdown_html(text(field, "prompt"))) }
                                }
                            }
                        }
                    } }
                }
            }
        }
        @if body.is_object() {
            h2 { "Body" }
            div.tbl { table {
                thead { tr { th { "Field" } th { "Type" } th { "What it is" } } }
                tbody {
                    @for (name, kind, description, _) in fields(body, definitions) {
                        tr { td { code { (name) } } td { (kind) } td { (markdown_html(description)) } }
                    }
                }
            } }
            @if !links.is_empty() {
                h3 { "Links" }
                ul {
                    @for (rel, link) in &links {
                        li { code { (rel) } ": " (link["description"].as_str().unwrap_or_default()) }
                    }
                }
                p { "A link the caller's key may not follow is left out." }
            }
        }
        @let examples = list(resource, "examples");
        @if !examples.is_empty() {
            h2 { "Examples" }
            p { "Real exchanges, recorded by Monokulo's tests (ids, addresses and times made stable)." }
            @for example in examples {
                h3 {
                    code { (text(example, "method")) " " (text(example, "path")) }
                    @if let Some(key) = example["key"].as_str() { " with a " code { (key) "_" } " key" } @else { " with no key" }
                    " → " (example["status"])
                }
                @if !example["request"].is_null() { (json_block(&example["request"])) }
                @if !example["response"].is_null() { (json_block(&example["response"])) }
            }
        }
    };
    page(up, text(resource, "key"), text(resource, "title"), markup)
}

fn problem_page(problem: &Value, reference: &Value) -> Markup {
    let up = "../../";
    let slug = text(problem, "slug");
    let example = list(reference, "resources")
        .iter()
        .flat_map(|r| list(r, "examples"))
        .find(|e| {
            e["response"]["type"]
                .as_str()
                .is_some_and(|t| t.ends_with(&format!("/problems/{slug}")))
        });
    let markup = html! {
        h1 { (text(problem, "title")) }
        p { code { (text(problem, "type")) } " · HTTP " (problem["status"]) }
        (markdown_html(text(problem, "doc")))
        p {
            "Answered as " code { "application/problem+json" } " (RFC 9457): "
            code { "type" } ", " code { "title" } ", " code { "status" } " and " code { "detail" } ", which says what went wrong this time."
        }
        @if let Some(example) = example {
            h2 { "Example" }
            p { code { (text(example, "method")) " " (text(example, "path")) } }
            (json_block(&example["response"]))
        }
    };
    page(up, text(problem, "title"), text(problem, "title"), markup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;
    use serde_json::json;

    /// A reference shaped as monokulo writes it: two resources and a
    /// problem. Monokulo's own test checks the real one has every resource
    /// and problem; this one checks each gets its page.
    fn reference(site: &str) -> Value {
        json!({
            "resources": [
                {
                    "rel": "order",
                    "key": "mk:order",
                    "template": "/api/v1/orders/{order_id}",
                    "media_type": "application/prs.hal-forms+json",
                    "title": "One order.",
                    "description": "One order.\n\nWhat the payer was asked for.",
                    "page": format!("{site}/rels/order"),
                    "methods": [{
                        "method": "GET", "name": "show", "keys": ["sk", "ak"],
                        "anyone": false, "doc": "The order.", "form": null, "template": null
                    }],
                    "body": {
                        "type": "object",
                        "required": ["order_id"],
                        "properties": {
                            "order_id": {"type": "string", "description": "Its id."},
                            "wallet": {"$ref": "#/$defs/Wallet"},
                            "_links": {"properties": {"self": {"description": "This order."}}}
                        }
                    },
                    "examples": [{
                        "method": "GET", "route": "/api/v1/orders/{order_id}",
                        "path": "/api/v1/orders/order_1", "key": "sk", "status": 200,
                        "response": {"order_id": "order_1"}
                    }, {
                        "method": "GET", "route": "/api/v1/orders/{order_id}",
                        "path": "/api/v1/orders/order_2", "key": "sk", "status": 404,
                        "response": {"type": format!("{site}/problems/not-found"), "status": 404}
                    }]
                },
                {
                    "rel": "orders",
                    "key": "mk:orders",
                    "template": "/api/v1/orders",
                    "media_type": "application/prs.hal-forms+json",
                    "title": "The store's orders.",
                    "description": "The store's orders.",
                    "page": format!("{site}/rels/orders"),
                    "methods": [{
                        "method": "POST", "name": "create", "keys": ["sk", "pk"],
                        "anyone": false, "doc": "Makes an order.", "form": "create",
                        "template": {"properties": [{"name": "amount", "required": true, "prompt": "How much."}]}
                    }],
                    "body": null,
                    "examples": []
                }
            ],
            "definitions": {"Wallet": {"type": "object"}},
            "problems": [{
                "slug": "not-found",
                "type": format!("{site}/problems/not-found"),
                "status": 404,
                "title": "Not found",
                "doc": "Nothing is there."
            }]
        })
    }

    fn generated(scratch: &Scratch, reference: &Value) -> PathBuf {
        let dir = scratch.path().join("generated");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("reference.json"), reference.to_string()).unwrap();
        fs::write(
            dir.join("openapi.json"),
            json!({"openapi": "3.1.0", "paths": {}}).to_string(),
        )
        .unwrap();
        dir
    }

    /// Every resource and every problem in the reference has its page, and
    /// the OpenAPI document is published beside them.
    #[test]
    fn every_relation_and_problem_has_a_page() {
        let reference = reference(site_header::SITE_URL);
        let scratch = Scratch::new("api-reference");
        let site = scratch.path().join("site");
        let pages = write(&generated(&scratch, &reference), &site).unwrap();
        assert_eq!(pages, 4);
        for resource in list(&reference, "resources") {
            let html = fs::read_to_string(
                site.join("rels")
                    .join(text(resource, "rel"))
                    .join("index.html"),
            )
            .unwrap();
            assert!(html.contains(text(resource, "template")), "{html}");
            let linked = site_assets::assert_links_published(&html, &site.join("assets"));
            assert!(linked.contains(&"styles.css"));
        }
        let order = fs::read_to_string(site.join("rels/order/index.html")).unwrap();
        assert!(order.contains("Wallet"), "{order}");
        assert!(order.contains("Real exchanges"), "{order}");
        let problem = fs::read_to_string(site.join("problems/not-found/index.html")).unwrap();
        assert!(problem.contains("/api/v1/orders/order_2"), "{problem}");
        let openapi: Value =
            serde_json::from_str(&fs::read_to_string(site.join("api/v1/openapi.json")).unwrap())
                .unwrap();
        assert_eq!(openapi["openapi"], "3.1.0");
        let index = fs::read_to_string(site.join("api/reference/index.html")).unwrap();
        assert!(index.contains(r#"href="../../rels/order/""#), "{index}");
    }

    /// A reference whose links point at another site isn't published.
    #[test]
    fn a_reference_naming_another_site_is_refused() {
        let scratch = Scratch::new("api-reference-elsewhere");
        let generated = generated(&scratch, &reference("https://example.com"));
        let error = write(&generated, &scratch.path().join("site")).unwrap_err();
        assert!(
            error.to_string().contains("where this site publishes it"),
            "{error}"
        );
    }
}
