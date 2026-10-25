//! `cargo xtask pages api --site DIR`: the HTTP API's section of the site,
//! generated from the code and from real exchanges (`crate::api`, which
//! runs monokulo's `api_reference` example into `target/api/`; nothing is
//! committed):
//!
//! - `api/`: the explorer (`api_explorer`), every resource and operation
//!   with its recorded examples, and Try it; `api/reference/`, where the
//!   resources' index was, sends the reader there;
//! - `rels/{rel}/`: one page per resource, which the API's `mk:` link
//!   relations point at (`https://docs.monokulo.com/rels/{rel}`): what it
//!   is, its URL, its methods, its body's fields, the forms it offers and
//!   recorded examples, and a link to its section of the explorer;
//! - `problems/{slug}/`: one page per problem, which an error's `type`
//!   points at;
//! - `api/v1/openapi.json`: the OpenAPI document.
//!
//! Every word comes from monokulo's doc comments, every example from a
//! request really made against a test engine, so the pages can't drift
//! from the API.

use super::api_explorer::{self, Api, Here};
use super::docs::{self, markdown_html};
use super::shell::{self, Entry};
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

/// Where the resources' index was, which now sends the reader to the
/// explorer (keeping the link's `#section`).
const OLD_INDEX: &str = "api/reference/index.html";

/// `cargo xtask pages api --site DIR`.
pub(crate) fn build(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let (site, from) = match args {
        ["--site", dir] => (PathBuf::from(dir), None),
        // A reference generated already (the browser tests' fixture,
        // `api_fixture/`): no recording.
        ["--site", dir, "--from", from] => (PathBuf::from(dir), Some(PathBuf::from(from))),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pages api needs --site DIR [--from DIR]",
            ))
        }
    };
    let generated = match from {
        Some(from) => from,
        None => {
            let generated = root.join(GENERATED);
            let exit = crate::api::generate(root, &generated)?;
            if !exit.succeeded() {
                return Ok(exit);
            }
            generated
        }
    };
    let written = write(&generated, &site)?;
    eprintln!("API reference in {}: {written} page(s)", site.display());
    Ok(Exit::SUCCESS)
}

fn read_json(path: &Path) -> io::Result<Value> {
    let text = fs::read_to_string(path).map_err(|e| at(path, e))?;
    serde_json::from_str(&text).map_err(|e| at(path, io::Error::other(e)))
}

/// Writes the explorer, every page and the OpenAPI document, from the
/// `generated` reference and document, into `site`; returns how many
/// pages.
pub(super) fn write(generated: &Path, site: &Path) -> io::Result<usize> {
    let reference = read_json(&generated.join("reference.json"))?;
    points_here(&reference)?;
    let openapi_path = generated.join("openapi.json");
    let openapi = read_json(&openapi_path)?;
    let api = Api::new(&openapi, &reference);
    let put = |path: PathBuf, contents: String| -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
        }
        fs::write(&path, contents).map_err(|e| at(&path, e))
    };
    put(site.join("api/index.html"), explorer(&api).into_string())?;
    put(
        site.join(OLD_INDEX),
        docs::redirect(OLD_INDEX, "api/").into_string(),
    )?;
    let mut pages = 1;
    for resource in list(&reference, "resources") {
        let rel = text(resource, "rel");
        put(
            site.join("rels").join(rel).join("index.html"),
            resource_page(resource, &reference, &api).into_string(),
        )?;
        pages += 1;
    }
    for rel in list(&reference, "rels") {
        put(
            site.join("rels").join(text(rel, "rel")).join("index.html"),
            onion_rel_page(rel, &api).into_string(),
        )?;
        pages += 1;
    }
    for problem in list(&reference, "problems") {
        let slug = text(problem, "slug");
        put(
            site.join("problems").join(slug).join("index.html"),
            problem_page(problem, &reference, &api).into_string(),
        )?;
        pages += 1;
    }
    let dir = site.join("api/v1");
    fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
    fs::copy(&openapi_path, dir.join("openapi.json")).map_err(|e| at(&openapi_path, e))?;
    shell::write_index(site, "api", &search(&api, &reference))?;
    site_assets::write(site)?;
    Ok(pages)
}

/// What search finds: the explorer's sections, and each onion link's page.
fn search(api: &Api, reference: &Value) -> Vec<Entry> {
    let mut entries = api_explorer::search(api);
    for rel in list(reference, "rels") {
        entries.push(Entry {
            title: text(rel, "key").to_owned(),
            sub: text(rel, "title").to_owned(),
            kind: "Resource",
            href: format!("rels/{}/", text(rel, "rel")),
            method: None,
        });
    }
    entries
}

/// The explorer, `api/`, in the shell: the whole width, the map marking the
/// section being read.
fn explorer(api: &Api) -> Markup {
    shell::render(shell::Page {
        up: "../",
        section: Section::Api,
        title: "API reference · Monokulo".to_owned(),
        description: "Monokulo's HTTP API: every resource and route, with real recorded examples, and Try it.",
        map_label: "API reference",
        map: api_explorer::map(api, Here::Explorer),
        article: api_explorer::page(api),
        previous: None,
        next: None,
        edit: None,
        layout: shell::Layout::Wide,
        script: Some("api.js"),
    })
}

/// A page under the explorer, two folders below the site's root, in the
/// shell with the API's map, `section` marked as where it is.
fn page(api: &Api, section: &str, title: &str, description: &str, body: Markup) -> Markup {
    shell::render(shell::Page {
        up: "../../",
        section: Section::Api,
        title: format!("{title} · Monokulo API"),
        description,
        map_label: "API reference",
        map: api_explorer::map(api, Here::Under(section)),
        article: body,
        previous: None,
        next: None,
        edit: None,
        layout: shell::Layout::Article,
        script: None,
    })
}

/// A link to a section of the explorer, from a page under it.
fn in_explorer(section: &str, what: &str) -> Markup {
    html! {
        p.api-in-explorer {
            a href={ "../../api/#" (section) } { (what) " in the API explorer" }
        }
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

pub(super) fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or_default()
}

pub(super) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
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

/// The fields of an object schema, with `definitions` to follow `$ref`s.
pub(super) fn fields<'a>(
    schema: &'a Value,
    definitions: &'a Value,
) -> Vec<(&'a str, String, &'a str, bool)> {
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
pub(super) fn type_name(schema: &Value, definitions: &Value) -> String {
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

fn resource_page(resource: &Value, reference: &Value, api: &Api) -> Markup {
    let up = "../../";
    let section = api.anchor_of(text(resource, "rel"));
    let definitions = &reference["definitions"];
    let body = &resource["body"];
    let links: Vec<(&String, &Value)> = body["properties"]["_links"]["properties"]
        .as_object()
        .map(|links| links.iter().filter(|(k, _)| *k != "curies").collect())
        .unwrap_or_default();
    let markup = html! {
        h1 { code { (text(resource, "key")) } }
        p.lede { (text(resource, "title")) }
        (in_explorer(&section, "Its routes, with Try it,"))
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
        api,
        &section,
        text(resource, "key"),
        text(resource, "title"),
        markup,
    )
}

/// The page of an onion link's relation (`mk:onion-…`).
fn onion_rel_page(rel: &Value, api: &Api) -> Markup {
    let up = "../../";
    let target = &rel["target"];
    let section = api.anchor_of(text(rel, "rel"));
    let markup = html! {
        h1 { code { (text(rel, "key")) } }
        p.lede { (text(rel, "title")) }
        (in_explorer(&section, "Every onion link"))
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
    page(api, &section, text(rel, "key"), text(rel, "title"), markup)
}

fn problem_page(problem: &Value, reference: &Value, api: &Api) -> Markup {
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
        (in_explorer(&format!("problem-{slug}"), "Every problem"))
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
        api,
        "problems",
        text(problem, "title"),
        text(problem, "title"),
        markup,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    /// A fixture of `api_fixture/`, its links pointed at `site`.
    fn fixture(json: &str, site: &str) -> Value {
        serde_json::from_str(&json.replace(site_header::SITE_URL, site)).unwrap()
    }

    /// A reference shaped as monokulo writes it (`api_fixture/`, which the
    /// browser tests build the explorer from too): API resources, a page, a
    /// stream, an onion link's relation, a handshake and problems.
    /// Monokulo's own recording checks the real one has every resource and
    /// problem; this one checks each gets its page.
    fn reference(site: &str) -> Value {
        fixture(include_str!("api_fixture/reference.json"), site)
    }

    /// An OpenAPI document shaped as monokulo writes it, for the same API:
    /// tags for the resources and the handshakes, operations with keys,
    /// parameters, a body, a stream and a page, and recorded examples (one
    /// with markup in it, as an answer may have).
    fn openapi(site: &str) -> Value {
        fixture(include_str!("api_fixture/openapi.json"), site)
    }

    fn generated(scratch: &Scratch, reference: &Value) -> PathBuf {
        let dir = scratch.path().join("generated");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("reference.json"), reference.to_string()).unwrap();
        fs::write(
            dir.join("openapi.json"),
            openapi(site_header::SITE_URL).to_string(),
        )
        .unwrap();
        dir
    }

    /// The site, written from the fixtures.
    fn site(name: &str) -> (Scratch, PathBuf) {
        let scratch = Scratch::new(name);
        let site = scratch.path().join("site");
        write(
            &generated(&scratch, &reference(site_header::SITE_URL)),
            &site,
        )
        .unwrap();
        (scratch, site)
    }

    fn read(site: &Path, page: &str) -> String {
        fs::read_to_string(site.join(page)).unwrap()
    }

    /// Every `id` in a page.
    fn ids(html: &str) -> Vec<String> {
        let id = regex::Regex::new(r#"\sid="([^"]+)""#).unwrap();
        id.captures_iter(html).map(|c| c[1].to_owned()).collect()
    }

    /// The links in a page's map, before its foot, with how each is marked.
    fn map_links(html: &str) -> Vec<(String, Option<String>)> {
        let map = html.split(r#"<nav class="map""#).nth(1).unwrap();
        let map = map
            .split(r#"<div class="map-group">"#)
            .skip(1)
            .collect::<String>();
        let map = map
            .split(r#"<div class="map-foot">"#)
            .next()
            .unwrap()
            .to_owned();
        let link = regex::Regex::new(r#"<a href="([^"]+)"(?: aria-current="(\w+)")?"#).unwrap();
        link.captures_iter(&map)
            .map(|c| (c[1].to_owned(), c.get(2).map(|m| m.as_str().to_owned())))
            .collect()
    }

    /// Whether `link`, from the page at `from` (a file in the site), lands
    /// on a file the build wrote, and on an element of it when it names one.
    fn resolves(site: &Path, from: &str, link: &str) -> bool {
        let (path, anchor) = link.split_once('#').unwrap_or((link, ""));
        let file = if path.is_empty() {
            site.join(from)
        } else {
            let mut dir = site.join(from);
            dir.pop();
            let file = dir.join(path);
            if path.ends_with('/') {
                file.join("index.html")
            } else {
                file
            }
        };
        let Ok(html) = fs::read_to_string(&file) else {
            return false;
        };
        anchor.is_empty()
            || !file.extension().is_some_and(|e| e == "html")
            || ids(&html).iter().any(|id| id == anchor)
    }

    /// The ids of the operations in the fixture's document.
    fn operation_ids() -> Vec<String> {
        let openapi = openapi(site_header::SITE_URL);
        let mut ids = Vec::new();
        for (_, methods) in openapi["paths"].as_object().unwrap() {
            for (_, op) in methods.as_object().unwrap() {
                ids.push(text(op, "operationId").replace('.', "-"));
            }
        }
        ids
    }

    /// Every operation of the document has its section in the explorer, by
    /// its anchor, after its resource's, the resources in the explorer's
    /// order; every problem has its row; no two elements share an id.
    #[test]
    fn every_operation_has_its_section_in_the_explorer() {
        let (_scratch, site) = site("api-explorer-operations");
        let html = read(&site, "api/index.html");
        // A template's ids are only in the page once the dialog takes its
        // copy, one at a time.
        let live = regex::Regex::new(r"(?s)<template.*?</template>")
            .unwrap()
            .replace_all(&html, "");
        let ids = ids(&live);
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "an id twice");
        let sections: Vec<&str> =
            regex::Regex::new(r#"<section class="api-sec[^"]*" id="([^"]+)""#)
                .unwrap()
                .captures_iter(&html)
                .map(|c| c.get(1).unwrap().as_str())
                .collect();
        assert_eq!(
            sections,
            [
                "introduction",
                "orders",
                "orders-create",
                "order",
                "order-get",
                "order-events",
                "order-events-get",
                "app",
                "app-delete",
                "handshakes",
                "app-connect-redeem",
                "pages",
                "dashboard-order-get",
                "onion-links",
                "problems",
            ]
        );
        for id in operation_ids() {
            assert!(sections.contains(&id.as_str()), "{id}");
        }
        // The introduction's sections are api.md's.
        assert!(ids.iter().any(|id| id == "keys"));
        for problem in list(&reference(site_header::SITE_URL), "problems") {
            let slug = text(problem, "slug");
            assert!(ids.contains(&format!("problem-{slug}")), "{slug}");
            assert!(
                resolves(&site, "api/index.html", &format!("../problems/{slug}/")),
                "{slug}"
            );
        }
    }

    /// Every recorded example of every status of every operation is on the
    /// page, in its status's disclosure (the first open), so a reader
    /// without a script can open each; a status with no body says so.
    #[test]
    fn every_recorded_example_is_reachable_without_javascript() {
        let (_scratch, site) = site("api-explorer-examples");
        let html = read(&site, "api/index.html");
        let openapi = openapi(site_header::SITE_URL);
        let details = regex::Regex::new(
            r#"(?s)<details class="api-example" data-status="(\w+)"( open)?>(.*?)</details>"#,
        )
        .unwrap();
        let figure =
            regex::Regex::new(r#"<figure class="api-variant" data-example="([^"]+)""#).unwrap();
        for (_, methods) in openapi["paths"].as_object().unwrap() {
            for (_, op) in methods.as_object().unwrap() {
                let id = text(op, "operationId").replace('.', "-");
                let section = html
                    .split(&format!(r#" id="{id}" data-op="{id}""#))
                    .nth(1)
                    .unwrap();
                let section = section.split("</section>").next().unwrap();
                let found: Vec<(String, bool, Vec<String>, bool)> = details
                    .captures_iter(section)
                    .map(|c| {
                        let examples = figure
                            .captures_iter(&c[3])
                            .map(|f| f[1].to_owned())
                            .collect();
                        (
                            c[1].to_owned(),
                            c.get(2).is_some(),
                            examples,
                            c[3].contains("No body."),
                        )
                    })
                    .collect();
                let expected: Vec<(String, Vec<String>)> = op["responses"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(status, response)| {
                        let mut names = Vec::new();
                        for (_, content) in response["content"].as_object().into_iter().flatten() {
                            names.extend(
                                content["examples"]
                                    .as_object()
                                    .into_iter()
                                    .flatten()
                                    .map(|(n, _)| n.clone()),
                            );
                            if !content["example"].is_null() {
                                names.push("recorded".to_owned());
                            }
                        }
                        (status.clone(), names)
                    })
                    .collect();
                let statuses: Vec<(String, Vec<String>)> = found
                    .iter()
                    .map(|(s, _, e, _)| (s.clone(), e.clone()))
                    .collect();
                assert_eq!(statuses, expected, "{id}");
                assert_eq!(found.iter().filter(|f| f.1).count(), 1, "{id}: one open");
                for (status, _, examples, no_body) in &found {
                    assert_eq!(examples.is_empty(), *no_body, "{id} {status}");
                }
            }
        }
    }

    /// Without a script every request reads in each language, and Try it,
    /// which needs one, only shows with one; only the API's own routes
    /// have it (and their form for the dialog), never a handshake or a
    /// page for people.
    #[test]
    fn every_request_reads_in_every_language_without_javascript() {
        let (_scratch, site) = site("api-explorer-snippets");
        let html = read(&site, "api/index.html");
        let snippet =
            regex::Regex::new(r#"<figure class="api-snippet" data-snippet="(\w+)">"#).unwrap();
        let try_it = regex::Regex::new(
            r#"<button [^>]*data-only="(\w+)" data-opens-dialog="try-it" data-try="([^"]+)""#,
        )
        .unwrap();
        let sections: Vec<&str> = html
            .split(r#"<section class="api-sec api-op""#)
            .skip(1)
            .collect();
        assert_eq!(sections.len(), operation_ids().len());
        let mut tried = Vec::new();
        for section in &sections {
            let section = section.split("</section>").next().unwrap();
            let languages: Vec<&str> = snippet
                .captures_iter(section)
                .map(|c| c.get(1).unwrap().as_str())
                .collect();
            assert_eq!(languages, ["curl", "js", "http"]);
            for c in try_it.captures_iter(section) {
                assert_eq!(&c[1], "script");
                tried.push(c[2].to_owned());
            }
        }
        assert_eq!(
            tried,
            [
                "orders-create",
                "order-get",
                "order-events-get",
                "app-delete"
            ]
        );
        let forms: Vec<&str> = regex::Regex::new(r#"<template data-try-form="([^"]+)">"#)
            .unwrap()
            .captures_iter(&html)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(forms, tried);
        let banner = html.split(r#"<div class="api-banner""#).nth(1).unwrap();
        assert!(banner.starts_with(r#" data-only="script""#), "{banner}");
    }

    /// What an example says is shown, never run: markup in a recorded
    /// answer is escaped.
    #[test]
    fn markup_in_a_recorded_answer_is_escaped() {
        let (_scratch, site) = site("api-explorer-escape");
        let html = read(&site, "api/index.html");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<b>no</b>"));
    }

    /// Every resource and problem in the reference has its page, each
    /// links its section of the explorer, the old index sends the reader to
    /// the explorer, and the OpenAPI document is published beside them.
    #[test]
    fn every_relation_and_problem_has_a_page() {
        let reference = reference(site_header::SITE_URL);
        let scratch = Scratch::new("api-reference");
        let site = scratch.path().join("site");
        let written = write(&generated(&scratch, &reference), &site).unwrap();
        assert_eq!(written, 8);
        let into_explorer = regex::Regex::new(r#"href="(\.\./\.\./api/#[^"]+)""#).unwrap();
        let mut pages = Vec::new();
        for resource in list(&reference, "resources")
            .iter()
            .chain(list(&reference, "rels"))
        {
            pages.push(format!("rels/{}/index.html", text(resource, "rel")));
        }
        for problem in list(&reference, "problems") {
            pages.push(format!("problems/{}/index.html", text(problem, "slug")));
        }
        for page in &pages {
            let html = read(&site, page);
            let linked = site_assets::assert_links_published(&html, &site.join("assets"));
            assert!(linked.contains(&"styles.css"), "{page}");
            let article = html.split(r#"<article class="prose">"#).nth(1).unwrap();
            let links: Vec<&str> = into_explorer
                .captures_iter(article)
                .map(|c| c.get(1).unwrap().as_str())
                .collect();
            assert_eq!(links.len(), 1, "{page}: its section of the explorer");
            assert!(resolves(&site, page, links[0]), "{page}: {}", links[0]);
        }
        let onion = read(&site, "rels/onion-order/index.html");
        assert!(onion.contains("For a client on Tor."), "{onion}");
        assert!(resolves(
            &site,
            "rels/onion-order/index.html",
            "../../rels/order/"
        ));
        // A method lists the problems it may answer.
        let orders = read(&site, "rels/orders/index.html");
        assert!(
            orders.contains(r#"href="../../problems/invalid-amount/""#),
            "{orders}"
        );
        let problem = read(&site, "problems/not-found/index.html");
        assert!(problem.contains("/api/v1/orders/order_2"), "{problem}");
        let code = read(&site, "problems/invalid-connect-code/index.html");
        assert!(code.contains("POST /apps/connect"), "{code}");
        let page = read(&site, "rels/dashboard-order/index.html");
        assert!(page.contains("Only the sk_ key and the merchant"), "{page}");
        let stream = read(&site, "rels/order-events/index.html");
        assert!(stream.contains("The order as it is now."), "{stream}");
        let openapi: Value = serde_json::from_str(&read(&site, "api/v1/openapi.json")).unwrap();
        assert_eq!(openapi["openapi"], "3.1.0");
        // The old index sends the reader on to the explorer.
        let old = read(&site, OLD_INDEX);
        assert!(
            old.contains(r#"<meta http-equiv="refresh" content="0; url=../../api/">"#),
            "{old}"
        );
        // Its sections are the explorer's.
        for section in ["resources", "onion-links", "handshakes", "problems"] {
            assert!(
                resolves(&site, OLD_INDEX, &format!("../../api/#{section}")),
                "{section}"
            );
        }
    }

    /// Every page has the API's map: on the explorer its links are its own
    /// sections, every one there and none marked (the script marks what's
    /// read), with no "On this page"; on a page under it, the explorer's,
    /// the page's section marked as where it is, and "On this page" its
    /// sections.
    #[test]
    fn every_page_has_the_map_and_its_links_resolve() {
        let (_scratch, site) = site("api-reference-shell");
        let reference = reference(site_header::SITE_URL);
        let explorer = read(&site, "api/index.html");
        let links = map_links(&explorer);
        for op in operation_ids() {
            assert!(
                links.iter().any(|(href, _)| *href == format!("#{op}")),
                "{op}"
            );
        }
        for (href, current) in &links {
            assert!(resolves(&site, "api/index.html", href), "{href}");
            assert_eq!(*current, None, "{href}");
        }
        assert_eq!(explorer.matches(r#"<aside class="toc""#).count(), 0);
        let mut pages: Vec<(String, String)> = Vec::new();
        for resource in list(&reference, "resources")
            .iter()
            .chain(list(&reference, "rels"))
        {
            let rel = text(resource, "rel");
            let section = match rel {
                "dashboard-order" => "dashboard-order-get",
                "onion-order" => "onion-links",
                rel => rel,
            };
            pages.push((format!("rels/{rel}/index.html"), section.to_owned()));
        }
        for problem in list(&reference, "problems") {
            pages.push((
                format!("problems/{}/index.html", text(problem, "slug")),
                "problems".to_owned(),
            ));
        }
        let h2 = regex::Regex::new(r"<h2[ >]").unwrap();
        for (page, section) in pages {
            let html = read(&site, &page);
            let links = map_links(&html);
            assert_eq!(links.len(), map_links(&explorer).len(), "{page}");
            for (href, _) in &links {
                assert!(resolves(&site, &page, href), "{page}: {href}");
            }
            let marked: Vec<&(String, Option<String>)> =
                links.iter().filter(|(_, c)| c.is_some()).collect();
            assert_eq!(
                marked,
                [&(format!("../../api/#{section}"), Some("location".to_owned()))],
                "{page}"
            );
            let article = html.split(r#"<article class="prose">"#).nth(1).unwrap();
            let article = article.split("</article>").next().unwrap();
            let sections = shell::sections(article);
            assert_eq!(
                h2.find_iter(article).count(),
                sections.len(),
                "{page}: an h2 without an id"
            );
            let toc = html
                .split(r#"<aside class="toc""#)
                .nth(1)
                .unwrap_or_default();
            for s in &sections {
                assert!(
                    toc.contains(&format!(r##"href="#{}""##, s.id)),
                    "{page}: {}",
                    s.id
                );
                assert!(
                    resolves(&site, &page, &format!("#{}", s.id)),
                    "{page}: {}",
                    s.id
                );
            }
        }
    }

    /// Search finds the explorer's introduction and its sections, every
    /// resource, operation and problem at its section of the explorer, and
    /// every onion link at its page: each where the build wrote it.
    #[test]
    fn the_search_index_lists_every_resource_operation_and_problem() {
        let (_scratch, site) = site("api-reference-search");
        let index: Vec<Value> = serde_json::from_str(&read(&site, "search/api.json")).unwrap();
        let has =
            |kind: &str, href: &str| index.iter().any(|e| e["kind"] == kind && e["href"] == href);
        let openapi = openapi(site_header::SITE_URL);
        for tag in list(&openapi, "tags") {
            let href = format!("api/#{}", text(tag, "name"));
            assert!(has("Resource", &href), "{href}");
        }
        for (_, methods) in openapi["paths"].as_object().unwrap() {
            for (method, op) in methods.as_object().unwrap() {
                let href = format!("api/#{}", text(op, "operationId").replace('.', "-"));
                let entry = index
                    .iter()
                    .find(|e| e["href"] == href)
                    .unwrap_or_else(|| panic!("{href}"));
                assert_eq!(entry["kind"], "Endpoint", "{href}");
                assert_eq!(entry["method"], method.to_uppercase(), "{href}");
            }
        }
        let reference = reference(site_header::SITE_URL);
        for rel in list(&reference, "rels") {
            assert!(has("Resource", &format!("rels/{}/", text(rel, "rel"))));
        }
        for problem in list(&reference, "problems") {
            let href = format!("api/#problem-{}", text(problem, "slug"));
            assert!(has("Problem", &href), "{href}");
        }
        assert!(has("Guide", "api/"));
        assert!(has("Section", "api/#keys"));
        for entry in &index {
            let href = entry["href"].as_str().unwrap();
            assert!(resolves(&site, "index.html", href), "{href}");
        }
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
