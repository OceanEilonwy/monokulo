//! `cargo xtask pages api --site DIR`: the HTTP API's reference, generated
//! from the code and from real exchanges (`crate::api`, which runs
//! monokulo's `api_reference` example into `target/api/`; nothing is
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
//! request really made against a test engine, so the pages can't drift
//! from the API.

use super::docs::markdown_html;
use super::shell::{self, icon, Entry};
use super::site_assets;
use super::site_header::{self, Section};
use crate::support::{at, Exit};
use maud::{html, Markup};
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
    for rel in list(&reference, "rels") {
        put(
            site.join("rels").join(text(rel, "rel")).join("index.html"),
            onion_rel_page(rel, &reference).into_string(),
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
    shell::write_index(site, "api", &search(&reference))?;
    site_assets::write(site)?;
    Ok(pages)
}

/// What search finds in the reference: each resource and each of its
/// operations, each onion link, each handshake and each problem.
fn search(reference: &Value) -> Vec<Entry> {
    let mut entries = Vec::new();
    for resource in list(reference, "resources") {
        let rel = text(resource, "rel");
        entries.push(Entry {
            title: text(resource, "key").to_owned(),
            sub: text(resource, "title").to_owned(),
            kind: "Resource",
            href: format!("rels/{rel}/"),
            method: None,
        });
        for method in list(resource, "methods") {
            entries.push(Entry {
                title: first_sentence(text(method, "doc")).to_owned(),
                sub: format!("{} {}", text(method, "method"), text(resource, "template")),
                kind: "Endpoint",
                href: format!("rels/{rel}/#{}", text(method, "name")),
                method: Some(text(method, "method").to_owned()),
            });
        }
    }
    for rel in list(reference, "rels") {
        entries.push(Entry {
            title: text(rel, "key").to_owned(),
            sub: text(rel, "title").to_owned(),
            kind: "Resource",
            href: format!("rels/{}/", text(rel, "rel")),
            method: None,
        });
    }
    for handshake in list(reference, "handshakes") {
        entries.push(Entry {
            title: first_sentence(text(handshake, "title")).to_owned(),
            sub: format!(
                "{} {}",
                text(handshake, "method"),
                text(handshake, "template")
            ),
            kind: "Endpoint",
            href: "api/reference/#handshakes".to_owned(),
            method: Some(text(handshake, "method").to_owned()),
        });
    }
    for problem in list(reference, "problems") {
        entries.push(Entry {
            title: text(problem, "title").to_owned(),
            sub: format!("{} · {}", problem["status"], text(problem, "slug")),
            kind: "Problem",
            href: format!("problems/{}/", text(problem, "slug")),
            method: None,
        });
    }
    entries
}

/// A doc comment's first sentence: "Read the entry." of "Read the entry.
/// Open without a key, …".
fn first_sentence(doc: &str) -> &str {
    let doc = doc.trim();
    let line = doc.split('\n').next().unwrap_or(doc);
    match line.find(". ") {
        Some(end) => &line[..=end],
        None => line,
    }
}

/// The `mk` curie and each problem's `type` point at the pages this site
/// publishes: a reference that names another site isn't published.
fn points_here(reference: &Value) -> io::Result<()> {
    let wrong = |what: &str, found: &str, expected: String| {
        io::Error::other(format!(
            "{what} points at {found}, not {expected} where this site publishes it"
        ))
    };
    let resources = list(reference, "resources").iter();
    for resource in resources.chain(list(reference, "rels")) {
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

/// Which page of the reference a page is, for its map to mark.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Here<'a> {
    /// `api/reference/`.
    Index,
    /// `rels/{rel}/`.
    Rel(&'a str),
    /// `problems/{slug}/`.
    Problem,
}

/// The reference's map, from a page `up` from the site's root: the API's
/// overview, every resource (its methods beside it) and onion link, and
/// the problems.
fn map(reference: &Value, up: &str, here: Here) -> Vec<shell::Group> {
    let link = |label: Markup, href: String, current: bool| shell::Link {
        label,
        href,
        current: shell::Mark::page_if(current),
        file: false,
    };
    let rel = |resource: &Value| {
        let rel = text(resource, "rel");
        let label = html! {
            span.map-key { (text(resource, "key")) }
            @for method in list(resource, "methods") {
                span.method data-method=(text(method, "method")) { (text(method, "method")) }
            }
        };
        link(label, format!("{up}rels/{rel}/"), here == Here::Rel(rel))
    };
    vec![
        shell::Group {
            title: "Overview".to_owned(),
            icon: icon::BOOK,
            links: vec![
                link(html! { "Introduction" }, format!("{up}api/"), false),
                link(
                    html! { "All resources" },
                    format!("{up}api/reference/"),
                    here == Here::Index,
                ),
                shell::Link {
                    file: true,
                    ..link(
                        html! { "OpenAPI document" },
                        format!("{up}api/v1/openapi.json"),
                        false,
                    )
                },
            ],
        },
        shell::Group {
            title: "Resources".to_owned(),
            icon: icon::BRACES,
            links: list(reference, "resources")
                .iter()
                .chain(list(reference, "rels"))
                .map(rel)
                .collect(),
        },
        shell::Group {
            title: "Errors".to_owned(),
            icon: icon::ALERT,
            // Not a problem's page, but the table that lists them: where
            // a problem's page is.
            links: vec![shell::Link {
                current: (here == Here::Problem).then_some(shell::Mark::Location),
                ..link(
                    html! { "Problems" },
                    format!("{up}api/reference/#problems"),
                    false,
                )
            }],
        },
    ]
}

/// A page of the reference, in the shell with the reference's map. Every
/// page is two folders below the site's root.
fn page(reference: &Value, here: Here, title: &str, description: &str, body: Markup) -> Markup {
    let up = "../../";
    shell::render(shell::Page {
        up,
        section: Section::Api,
        title: format!("{title} · Monokulo API"),
        description,
        map_label: "API reference",
        map: map(reference, up, here),
        article: body,
        previous: None,
        next: None,
        edit: None,
    })
}

/// The keys a page's link is offered to, in words.
fn offered(keys: &[Value]) -> String {
    keys.iter()
        .filter_map(Value::as_str)
        .map(|key| match key {
            "session" => "the merchant's own session".to_owned(),
            key => format!("the {key}_ key"),
        })
        .collect::<Vec<_>>()
        .join(" and ")
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
        h2 id="resources" { "Resources" }
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
        @if !list(reference, "rels").is_empty() {
            h2 id="onion-links" { "Onion links" }
            p {
                "While a Monokulo has an onion address, these say where a resource is on it, as absolute URLs, "
                "whichever address the client called. Without one they're absent."
            }
            div.tbl { table {
                thead { tr { th { "Relation" } th { "URL" } th { "What it is" } } }
                tbody {
                    @for rel in list(reference, "rels") {
                        tr {
                            td { a href=(format!("{up}rels/{}/", text(rel, "rel"))) { code { (text(rel, "key")) } } }
                            td { code { (text(rel, "template")) } }
                            td { (text(rel, "title")) }
                        }
                    }
                }
            } }
        }
        @if !list(reference, "handshakes").is_empty() {
            h2 id="handshakes" { "Handshakes" }
            p {
                "Beside the API: where a " a href=(format!("{up}plugins/")) { "shop plugin" } " or a "
                a href=(format!("{up}apps/")) { "connected app" } " gets its key, once. They answer with the problems listed."
            }
            div.tbl { table {
                thead { tr { th { "Route" } th { "What it does" } th { "Problems" } } }
                tbody {
                    @for handshake in list(reference, "handshakes") {
                        tr {
                            td { code { (text(handshake, "method")) " " (text(handshake, "template")) } }
                            td { (text(handshake, "title")) }
                            td {
                                @for slug in list(handshake, "problems").iter().filter_map(Value::as_str) {
                                    a href=(format!("{up}problems/{slug}/")) { code { (slug) } } " "
                                }
                            }
                        }
                    }
                }
            } }
        }
        h2 id="problems" { "Problems" }
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
        reference,
        Here::Index,
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
        h2 id="url" { "URL" }
        p { code { (text(resource, "template")) } " · " code { (text(resource, "media_type")) } }
        @if list(resource, "methods").is_empty() {
            h2 id="who-gets-the-link" { "Who gets the link" }
            @if let Some(keys) = resource["offered_to"].as_array() {
                p { "Only " (offered(keys)) ". Anyone else's answer leaves the link out." }
            } @else {
                p { "Anyone who reads something that links it: the page is open to whoever has its URL." }
            }
        } @else {
            h2 id="methods" { "Methods" }
        }
        @for method in list(resource, "methods") {
            h3 id=(text(method, "name")) { code { (text(method, "method")) } " · " (who(method)) }
            (markdown_html(text(method, "doc")))
            @let problems = list(method, "problems");
            @if !problems.is_empty() {
                p {
                    "Problems it may answer: "
                    @for slug in problems.iter().filter_map(Value::as_str) {
                        a href=(format!("{up}problems/{slug}/")) { code { (slug) } } " "
                    }
                }
            }
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
        @let events = list(resource, "events");
        @if !events.is_empty() {
            h2 id="events" { "Events" }
            p { "Each event is an " code { "event:" } " line with its name, then a " code { "data:" } " line." }
            div.tbl { table {
                thead { tr { th { "Event" } th { "Data" } th { "What it means" } } }
                tbody {
                    @for event in events {
                        tr {
                            td { code { (text(event, "name")) } }
                            td { (type_name(&event["data"], definitions)) }
                            td { (markdown_html(text(event, "doc"))) }
                        }
                    }
                }
            } }
        }
        @if body.is_object() {
            h2 id="body" { "Body" }
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
            h2 id="examples" { "Examples" }
            p { "Real exchanges, recorded against a test engine (ids, keys, addresses and times made stable)." }
            @for example in examples {
                h3 {
                    code { (text(example, "method")) " " (text(example, "path")) }
                    @if let Some(key) = example["key"].as_str() { " with a " code { (key) "_" } " key" } @else { " with no key" }
                    @if example["onion"] == true { ", on a Monokulo with an onion address" }
                    " → " (example["status"])
                }
                @if !example["request"].is_null() { (json_block(&example["request"])) }
                @if !example["response"].is_null() { (json_block(&example["response"])) }
            }
        }
    };
    page(
        reference,
        Here::Rel(text(resource, "rel")),
        text(resource, "key"),
        text(resource, "title"),
        markup,
    )
}

/// The page of an onion link's relation (`mk:onion-…`).
fn onion_rel_page(rel: &Value, reference: &Value) -> Markup {
    let up = "../../";
    let target = &rel["target"];
    let markup = html! {
        h1 { code { (text(rel, "key")) } }
        p.lede { (text(rel, "title")) }
        (markdown_html(text(rel, "description").split_once("\n\n").map_or("", |(_, rest)| rest)))
        h2 id="url" { "URL" }
        p {
            code { (text(rel, "template")) } " · " code { (text(rel, "media_type")) }
            ": the same resource as "
            a href=(format!("{up}rels/{}/", text(target, "rel"))) { code { (text(target, "key")) } }
            " (" code { (text(target, "template")) } "), absolute, on the onion address."
        }
        p { "Present only while this Monokulo has an onion address; absent otherwise. A link the caller's key may not follow is left out." }
    };
    page(
        reference,
        Here::Rel(text(rel, "rel")),
        text(rel, "key"),
        text(rel, "title"),
        markup,
    )
}

fn problem_page(problem: &Value, reference: &Value) -> Markup {
    let slug = text(problem, "slug");
    let example = list(reference, "resources")
        .iter()
        .chain(list(reference, "handshakes"))
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
            h2 id="example" { "Example" }
            p { code { (text(example, "method")) " " (text(example, "path")) } }
            (json_block(&example["response"]))
        }
    };
    page(
        reference,
        Here::Problem,
        text(problem, "title"),
        text(problem, "title"),
        markup,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;
    use serde_json::json;

    /// A reference shaped as monokulo writes it: API resources, a page, a
    /// stream, an onion link's relation, a handshake and problems.
    /// Monokulo's own recording checks the real one has every resource and
    /// problem; this one checks each gets its page.
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
                        "problems": ["invalid-amount"],
                        "template": {"properties": [{"name": "amount", "required": true, "prompt": "How much."}]}
                    }],
                    "body": null,
                    "examples": []
                },
                {
                    "rel": "dashboard-order",
                    "key": "mk:dashboard-order",
                    "template": "/stores/{id}/orders/{order_id}",
                    "media_type": "text/html",
                    "title": "The order's page on the dashboard.",
                    "description": "The order's page on the dashboard.\n\nFor the merchant.",
                    "page": format!("{site}/rels/dashboard-order"),
                    "methods": [],
                    "offered_to": ["sk", "session"],
                    "events": [],
                    "body": null,
                    "examples": []
                },
                {
                    "rel": "order-events",
                    "key": "mk:order-events",
                    "template": "/api/v1/orders/{order_id}/events",
                    "media_type": "text/event-stream",
                    "title": "One order's changes.",
                    "description": "One order's changes.",
                    "page": format!("{site}/rels/order-events"),
                    "methods": [{
                        "method": "GET", "name": "get", "keys": ["sk"],
                        "anyone": false, "doc": "Follow it.", "form": null, "template": null
                    }],
                    "offered_to": null,
                    "events": [{"name": "order", "doc": "The order as it is now.", "data": {"$ref": "#/$defs/Wallet"}}],
                    "body": null,
                    "examples": []
                }
            ],
            "handshakes": [{
                "method": "POST",
                "template": "/apps/connect",
                "title": "Connect an app.",
                "doc": "Connect an app.",
                "problems": ["invalid-connect-code"],
                "examples": [{
                    "method": "POST", "route": "/apps/connect", "path": "/apps/connect", "status": 401,
                    "response": {"type": format!("{site}/problems/invalid-connect-code"), "status": 401}
                }]
            }],
            "definitions": {"Wallet": {"type": "object"}},
            "rels": [{
                "rel": "onion-order",
                "key": "mk:onion-order",
                "template": "http://….onion/api/v1/orders/{order_id}",
                "media_type": "application/prs.hal-forms+json",
                "title": "The order on the onion address.",
                "description": "The order on the onion address.\n\nFor a client on Tor.",
                "page": format!("{site}/rels/onion-order"),
                "target": {"rel": "order", "key": "mk:order", "template": "/api/v1/orders/{order_id}"}
            }],
            "problems": [{
                "slug": "not-found",
                "type": format!("{site}/problems/not-found"),
                "status": 404,
                "title": "Not found",
                "doc": "Nothing is there."
            }, {
                "slug": "invalid-connect-code",
                "type": format!("{site}/problems/invalid-connect-code"),
                "status": 401,
                "title": "Connect code not valid",
                "doc": "The code is spent."
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
        assert_eq!(pages, 8);
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
        let onion = fs::read_to_string(site.join("rels/onion-order/index.html")).unwrap();
        assert!(onion.contains("For a client on Tor."), "{onion}");
        assert!(onion.contains(r#"href="../../rels/order/""#), "{onion}");
        // A method lists the problems it may answer.
        let orders = fs::read_to_string(site.join("rels/orders/index.html")).unwrap();
        assert!(
            orders.contains(r#"href="../../problems/invalid-amount/""#),
            "{orders}"
        );
        let order = fs::read_to_string(site.join("rels/order/index.html")).unwrap();
        assert!(order.contains("Wallet"), "{order}");
        assert!(order.contains("Real exchanges"), "{order}");
        let problem = fs::read_to_string(site.join("problems/not-found/index.html")).unwrap();
        assert!(problem.contains("/api/v1/orders/order_2"), "{problem}");
        // A problem only a handshake answers has its example too.
        let code =
            fs::read_to_string(site.join("problems/invalid-connect-code/index.html")).unwrap();
        assert!(code.contains("POST /apps/connect"), "{code}");
        // A page says who gets its link; a stream, its events.
        let page = fs::read_to_string(site.join("rels/dashboard-order/index.html")).unwrap();
        assert!(page.contains("Only the sk_ key and the merchant"), "{page}");
        let stream = fs::read_to_string(site.join("rels/order-events/index.html")).unwrap();
        assert!(stream.contains("The order as it is now."), "{stream}");
        assert!(stream.contains("<code>order</code>"), "{stream}");
        let openapi: Value =
            serde_json::from_str(&fs::read_to_string(site.join("api/v1/openapi.json")).unwrap())
                .unwrap();
        assert_eq!(openapi["openapi"], "3.1.0");
        let index = fs::read_to_string(site.join("api/reference/index.html")).unwrap();
        assert!(index.contains(r#"href="../../rels/order/""#), "{index}");
        assert!(
            index.contains(r#"href="../../rels/onion-order/""#),
            "{index}"
        );
        assert!(index.contains("POST /apps/connect"), "{index}");
        assert!(
            index.contains(r#"href="../../problems/invalid-connect-code/""#),
            "{index}"
        );
    }

    /// Every page is in the shell with the reference's map: every resource
    /// and onion link in it, the page's own marked (a problem's marks the
    /// problems), and "On this page" listing each of its sections.
    #[test]
    fn every_page_has_the_map_and_its_sections() {
        let reference = reference(site_header::SITE_URL);
        let scratch = Scratch::new("api-reference-shell");
        let site = scratch.path().join("site");
        write(&generated(&scratch, &reference), &site).unwrap();
        let rels: Vec<&str> = list(&reference, "resources")
            .iter()
            .chain(list(&reference, "rels"))
            .map(|r| text(r, "rel"))
            .collect();
        let current =
            regex::Regex::new(r#"<a href="([^"]+)" aria-current="(page|location)""#).unwrap();
        let h2 = regex::Regex::new(r"<h2[ >]").unwrap();
        let mut pages = vec![(
            "api/reference/index.html".to_owned(),
            "../../api/reference/".to_owned(),
            "page",
        )];
        for rel in &rels {
            pages.push((
                format!("rels/{rel}/index.html"),
                format!("../../rels/{rel}/"),
                "page",
            ));
        }
        for problem in list(&reference, "problems") {
            pages.push((
                format!("problems/{}/index.html", text(problem, "slug")),
                "../../api/reference/#problems".to_owned(),
                // The table that lists it, not the page itself.
                "location",
            ));
        }
        for (file, marked, how) in pages {
            let html = fs::read_to_string(site.join(&file)).unwrap();
            let map = html.split(r#"<nav class="map""#).nth(1).unwrap();
            let map = map.split(r#"<div class="map-foot">"#).next().unwrap();
            for rel in &rels {
                assert!(
                    map.contains(&format!(r#"href="../../rels/{rel}/""#)),
                    "{file}: {rel}"
                );
            }
            let marks: Vec<(&str, &str)> = current
                .captures_iter(map.split_once(r#"<div class="map-group">"#).unwrap().1)
                .map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
                .collect();
            assert_eq!(marks, [(marked.as_str(), how)], "{file}");
            // No breadcrumbs: the map says where the page is.
            assert!(!html.contains(" › "), "{file}");
            let article = html.split(r#"<article class="prose">"#).nth(1).unwrap();
            let article = article.split("</article>").next().unwrap();
            let sections = shell::sections(article);
            assert_eq!(
                h2.find_iter(article).count(),
                sections.len(),
                "{file}: an h2 without an id"
            );
            for section in &sections {
                assert!(
                    html.contains(&format!(r##"<li><a href="#{}">"##, section.id)),
                    "{file}: {}",
                    section.id
                );
            }
        }
    }

    /// Search finds every resource, every operation, every onion link, every
    /// handshake and every problem, each at a page the build wrote.
    #[test]
    fn the_search_index_lists_every_resource_operation_and_problem() {
        let reference = reference(site_header::SITE_URL);
        let scratch = Scratch::new("api-reference-search");
        let site = scratch.path().join("site");
        write(&generated(&scratch, &reference), &site).unwrap();
        let index: Vec<Value> =
            serde_json::from_str(&fs::read_to_string(site.join("search/api.json")).unwrap())
                .unwrap();
        let has =
            |kind: &str, href: &str| index.iter().any(|e| e["kind"] == kind && e["href"] == href);
        for resource in list(&reference, "resources") {
            let rel = text(resource, "rel");
            assert!(has("Resource", &format!("rels/{rel}/")), "{rel}");
            for method in list(resource, "methods") {
                let href = format!("rels/{rel}/#{}", text(method, "name"));
                assert!(has("Endpoint", &href), "{href}");
                let entry = index.iter().find(|e| e["href"] == href).unwrap();
                assert_eq!(entry["method"], method["method"], "{href}");
                // The operation's anchor is on its page.
                let html = fs::read_to_string(site.join(format!("rels/{rel}/index.html"))).unwrap();
                assert!(
                    html.contains(&format!(r#"id="{}""#, text(method, "name"))),
                    "{href}"
                );
            }
        }
        for rel in list(&reference, "rels") {
            assert!(has("Resource", &format!("rels/{}/", text(rel, "rel"))));
        }
        let handshakes = index
            .iter()
            .filter(|e| e["href"] == "api/reference/#handshakes")
            .count();
        assert_eq!(handshakes, list(&reference, "handshakes").len());
        for problem in list(&reference, "problems") {
            assert!(has(
                "Problem",
                &format!("problems/{}/", text(problem, "slug"))
            ));
        }
        for entry in &index {
            let href = entry["href"].as_str().unwrap();
            let path = href.split('#').next().unwrap();
            assert!(site.join(path).join("index.html").is_file(), "{href}");
        }
    }

    #[test]
    fn an_operations_first_sentence_names_it() {
        assert_eq!(
            first_sentence("Read the entry. Open without a key."),
            "Read the entry."
        );
        assert_eq!(
            first_sentence("Make an order.\n\nWith the key."),
            "Make an order."
        );
        assert_eq!(
            first_sentence("Connect an app: its own key."),
            "Connect an app: its own key."
        );
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
