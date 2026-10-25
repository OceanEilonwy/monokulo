//! The API explorer, `/api/`: the whole HTTP API on one long page, laid out
//! like Stripe's or Scalar's, from the OpenAPI document monokulo writes
//! with real recorded exchanges (`crate::api`, `target/api/openapi.json`).
//!
//! - The introduction (`docs/api.md`), with the base URL, the keys and a
//!   first request beside it.
//! - Each resource (an OpenAPI tag, `mk:orders`): its description and its
//!   routes.
//! - Each operation in two columns. The left has its method and path, who
//!   may call it, its description, parameters, body fields and responses by
//!   status; the right stays in view with the request as curl, JavaScript
//!   or raw HTTP, and the recorded responses, every example of every
//!   status.
//! - The onion links and the problems.
//!
//! Everything is rendered here, so the page reads fully without a script:
//! every snippet and every recorded example is on it, under a heading or in
//! a disclosure. The explorer's script (`assets/api/api.js`) adds the
//! language and example switches, Copy, and Try it: a banner saying where
//! requests go, and one dialog (the `ui` package's) holding the reader's
//! Monokulo address and key and an operation's form, which this page
//! renders into a `<template>` per operation. The live answer is drawn
//! into the operation's Response panel, which holds an empty place for it.

use super::api_reference::{fields, list, text, type_name};
use super::docs::markdown_html;
use super::shell::{self, icon, Entry};
use super::site_header::SITE_URL;
use maud::{html, Markup, PreEscaped};
use serde_json::Value;
use ui::assets::icons;

/// The resources in the order the explorer shows them, each with its name:
/// how a client meets them, the entry first and the handshakes and pages
/// for people last. A tag the document has and this doesn't name follows
/// them, under its own name.
const TAGS: [(&str, &str); 12] = [
    ("root", "Entry"),
    ("orders", "Orders"),
    ("order", "One order"),
    ("order-events", "Order events"),
    ("events", "Sale events"),
    ("store", "Store"),
    ("webhook-tests", "Webhook tests"),
    ("app", "Connected app"),
    ("logs", "Plugin logs"),
    ("openapi", "OpenAPI document"),
    ("handshakes", "Handshakes"),
    ("pages", "Pages for people"),
];

/// The keys, by the name the document gives them (`x-monokulo-keys`), with
/// their prefix and a word for each, and what each may do in a line.
const KEYS: [(&str, &str, &str, &str); 3] = [
    (
        "sk",
        "sk_",
        "secret",
        "The shop's server. Everything about the store's orders and events.",
    ),
    (
        "ak",
        "ak_",
        "app",
        "A connected app. The same as the secret key, and its own connection.",
    ),
    (
        "pk",
        "pk_",
        "public",
        "A browser page. Makes and reads orders; proves nothing.",
    ),
];

/// The introduction's Markdown.
const INTRO: &str = include_str!("docs/api.md");

/// The id of the introduction's section, and of the Try it dialog.
const INTRO_ID: &str = "introduction";
const DIALOG: &str = "try-it";

/// The API as the explorer shows it: its introduction, its resources with
/// their operations, and what the operations' bodies refer to.
pub(super) struct Api<'a> {
    /// The introduction, as HTML.
    intro: String,
    resources: Vec<Resource<'a>>,
    /// The onion links' relations, from the reference.
    onion: &'a [Value],
    problems: &'a [Value],
    /// The schemas the operations' bodies refer to.
    schemas: &'a Value,
    /// Where requests go until the reader names their own Monokulo: the
    /// document's server's default.
    server: String,
}

/// A resource: an OpenAPI tag and the operations under it.
struct Resource<'a> {
    /// Its tag, which is its section's id.
    tag: &'a str,
    name: String,
    description: &'a str,
    /// Its relation's page, from the site's root, when it has one
    /// (`rels/orders/`).
    rel: Option<String>,
    operations: Vec<Operation<'a>>,
}

/// A recorded example: its name (`sk`, `pk-with-onion`, a problem's slug),
/// what it shows, and the body.
type Example<'a> = (&'a str, &'a str, &'a Value);

/// A response: its status, its description and its recorded examples.
type Response<'a> = (&'a str, &'a str, Vec<Example<'a>>);

/// One operation: a method on a path.
struct Operation<'a> {
    /// Its section's id: its `operationId` (`orders.create`) as an anchor,
    /// `orders-create`.
    id: String,
    method: &'a str,
    path: &'a str,
    op: &'a Value,
}

impl<'a> Api<'a> {
    /// The API, from the OpenAPI document and the reference (for the onion
    /// links and the problems).
    pub(super) fn new(openapi: &'a Value, reference: &'a Value) -> Api<'a> {
        let mut operations: Vec<(&str, Operation)> = Vec::new();
        if let Some(paths) = openapi["paths"].as_object() {
            for (path, methods) in paths {
                for (method, op) in methods.as_object().into_iter().flatten() {
                    let Some(id) = op["operationId"].as_str() else {
                        continue;
                    };
                    let tag = op["tags"][0].as_str().unwrap_or("other");
                    operations.push((
                        tag,
                        Operation {
                            id: id.replace('.', "-"),
                            method: method_name(method),
                            path,
                            op,
                        },
                    ));
                }
            }
        }
        let rank = |method: &str| ["GET", "POST", "DELETE"].iter().position(|m| *m == method);
        operations.sort_by_key(|(_, o)| (rank(o.method), o.path));
        let tags = list(openapi, "tags");
        let mut order: Vec<&str> = TAGS.iter().map(|(tag, _)| *tag).collect();
        for tag in tags.iter().map(|t| text(t, "name")) {
            if !order.contains(&tag) {
                order.push(tag);
            }
        }
        for (tag, _) in &operations {
            if !order.contains(tag) {
                order.push(tag);
            }
        }
        let rels = format!("{SITE_URL}/rels/");
        let resources = order
            .into_iter()
            .filter_map(|tag| {
                let about = tags.iter().find(|t| text(t, "name") == tag);
                let ops: Vec<Operation> = operations
                    .iter()
                    .filter(|(t, _)| *t == tag)
                    .map(|(_, o)| Operation {
                        id: o.id.clone(),
                        ..*o
                    })
                    .collect();
                if ops.is_empty() {
                    return None;
                }
                let name = TAGS
                    .iter()
                    .find(|(t, _)| *t == tag)
                    .map_or_else(|| tag.to_owned(), |(_, name)| (*name).to_owned());
                let rel = about
                    .and_then(|t| t["externalDocs"]["url"].as_str())
                    .and_then(|url| url.strip_prefix(&rels))
                    .map(|rel| format!("rels/{rel}/"));
                Some(Resource {
                    tag,
                    name,
                    description: about.map(|t| text(t, "description")).unwrap_or_default(),
                    rel,
                    operations: ops,
                })
            })
            .collect();
        let server = openapi["servers"][0]["variables"]["monokulo"]["default"]
            .as_str()
            .unwrap_or("https://www.monokulo.com")
            .trim_end_matches('/')
            .to_owned();
        Api {
            intro: markdown_html(&local(INTRO, "../")).into_string(),
            resources,
            onion: list(reference, "rels"),
            problems: list(reference, "problems"),
            schemas: &openapi["components"]["schemas"],
            server,
        }
    }

    /// Every operation, in the explorer's order.
    fn operations(&self) -> impl Iterator<Item = &Operation<'a>> {
        self.resources.iter().flat_map(|r| &r.operations)
    }

    /// The introduction's sections (its `h2`s).
    fn sections(&self) -> Vec<shell::Heading> {
        shell::sections(&self.intro)
    }

    /// The section of the explorer a relation's page belongs to: the
    /// resource it is, the operation that serves it (a page for people),
    /// or the onion links.
    pub(super) fn anchor_of(&self, rel: &str) -> String {
        if self.resources.iter().any(|r| r.tag == rel) {
            return rel.to_owned();
        }
        let get = format!("{rel}-get");
        if self.operations().any(|o| o.id == get) {
            return get;
        }
        "onion-links".to_owned()
    }
}

/// `get` as the document names it, `GET` as a request does.
fn method_name(method: &str) -> &'static str {
    match method {
        "get" => "GET",
        "post" => "POST",
        "delete" => "DELETE",
        "put" => "PUT",
        "patch" => "PATCH",
        _ => "OTHER",
    }
}

/// The `ui` package's method tag for `method`.
fn method_tag(method: &str) -> Markup {
    let method = match method {
        "POST" => ui::HttpMethod::Post,
        "DELETE" => ui::HttpMethod::Delete,
        _ => ui::HttpMethod::Get,
    };
    html! { (ui::HttpMethodTag::new(method)) }
}

/// Markdown with this site's own addresses made relative, from a page `up`
/// from the site's root, so a reader stays on the site they're reading.
fn local(markdown: &str, up: &str) -> String {
    markdown.replace(&format!("{SITE_URL}/"), up)
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

/// A doc comment's first paragraph, and the rest.
fn split_doc(doc: &str) -> (&str, &str) {
    let doc = doc.trim();
    doc.split_once("\n\n").unwrap_or((doc, ""))
}

impl Operation<'_> {
    /// What it does, in a few words: its summary's first sentence, without
    /// the full stop.
    fn title(&self) -> &str {
        let summary = first_sentence(text(self.op, "summary"));
        summary.strip_suffix('.').unwrap_or(summary)
    }

    /// The keys that may call it (`sk`, `ak`, `pk`), not a session.
    fn keys(&self) -> Vec<&str> {
        list(self.op, "x-monokulo-keys")
            .iter()
            .filter_map(Value::as_str)
            .filter(|k| *k != "session")
            .collect()
    }

    /// Whether anyone may call it, with no key.
    fn anyone(&self) -> bool {
        list(self.op, "security")
            .iter()
            .any(|s| s.as_object().is_some_and(|s| s.is_empty()))
    }

    /// Whether the merchant, signed in to the dashboard, may.
    fn session(&self) -> bool {
        let named = list(self.op, "x-monokulo-keys")
            .iter()
            .any(|k| k == "session");
        named
            || list(self.op, "security")
                .iter()
                .any(|s| s.get("session").is_some())
    }

    /// Its successful answers' content types.
    fn answers(&self) -> Vec<&str> {
        self.op["responses"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(status, _)| status.starts_with('2'))
            .flat_map(|(_, r)| r["content"].as_object().into_iter().flatten())
            .map(|(kind, _)| kind.as_str())
            .collect()
    }

    /// Whether it answers with a stream of server-sent events.
    fn stream(&self) -> bool {
        self.answers().contains(&"text/event-stream")
    }

    /// Whether it's a page for people, which a browser opens.
    fn page(&self) -> bool {
        let answers = self.answers();
        !answers.is_empty() && answers.iter().all(|a| *a == "text/html")
    }

    /// Whether a page on another site can send it: the API under `/api/`
    /// answers CORS (`http::embed_cors_layer`); the handshakes and the
    /// pages don't.
    fn triable(&self) -> bool {
        self.path.starts_with("/api/") && !self.page()
    }

    /// The key its examples are shown with first: the secret key, an
    /// app's, the public one, in that order of what they may do.
    fn preferred_key(&self) -> Option<&'static str> {
        let keys = self.keys();
        KEYS.iter()
            .map(|(name, ..)| *name)
            .find(|k| keys.contains(k))
    }

    fn parameters(&self) -> &[Value] {
        list(self.op, "parameters")
    }

    /// Its body's content type, schema and recorded example.
    fn body(&self) -> Option<(&str, &Value, &Value)> {
        let (kind, content) = self.op["requestBody"]["content"]
            .as_object()?
            .iter()
            .next()?;
        Some((kind, &content["schema"], &content["example"]))
    }

    /// Its responses: each status, its description, and its recorded
    /// examples (name, summary, value).
    fn responses(&self) -> Vec<Response<'_>> {
        self.op["responses"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(status, response)| {
                let mut examples = Vec::new();
                for (_, content) in response["content"].as_object().into_iter().flatten() {
                    for (name, example) in content["examples"].as_object().into_iter().flatten() {
                        examples.push((name.as_str(), text(example, "summary"), &example["value"]));
                    }
                    if !content["example"].is_null() {
                        examples.push(("recorded", "", &content["example"]));
                    }
                }
                (status.as_str(), text(response, "description"), examples)
            })
            .collect()
    }
}

/// `value` as pretty JSON (as `serde_json` writes it), its keys, strings,
/// numbers and the rest each in a span the stylesheet colours; a string
/// that isn't JSON (an event stream) as it is.
fn json(value: &Value) -> Markup {
    fn escape(text: &str, out: &mut String) {
        for c in text.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                c => out.push(c),
            }
        }
    }
    fn span(class: &str, token: &str, out: &mut String) {
        out.push_str(&format!(r#"<span class="{class}">"#));
        escape(token, out);
        out.push_str("</span>");
    }
    fn write(value: &Value, indent: usize, out: &mut String) {
        let pad = |n: usize| "  ".repeat(n);
        match value {
            Value::Object(map) if !map.is_empty() => {
                out.push_str("{\n");
                for (i, (key, value)) in map.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    span("j-k", &Value::String(key.clone()).to_string(), out);
                    out.push_str(": ");
                    write(value, indent + 1, out);
                    out.push_str(if i + 1 < map.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(indent));
                out.push('}');
            }
            Value::Array(items) if !items.is_empty() => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    write(item, indent + 1, out);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(indent));
                out.push(']');
            }
            Value::String(_) => span("j-s", &value.to_string(), out),
            Value::Number(_) => span("j-n", &value.to_string(), out),
            Value::Bool(_) | Value::Null => span("j-b", &value.to_string(), out),
            _ => out.push_str(&value.to_string()),
        }
    }
    let mut out = String::new();
    match value {
        Value::String(raw) => escape(raw, &mut out),
        value => write(value, 0, &mut out),
    }
    PreEscaped(out)
}

/// The path with its parameters marked: `/api/v1/orders/<b>{order_id}</b>`.
fn path_markup(path: &str) -> Markup {
    html! {
        @for (i, part) in path.split('{').enumerate() {
            @if i == 0 {
                (part)
            } @else if let Some((name, rest)) = part.split_once('}') {
                b { "{" (name) "}" } (rest)
            } @else {
                "{" (part)
            }
        }
    }
}

/// The three ways to write an operation's request: curl, JavaScript and raw
/// HTTP, as (name, label, markup). The address and the key are spans the
/// explorer's script fills with the reader's own (`data-base`, `data-host`,
/// `data-prefix`, `data-key`), the key only as its prefix.
fn snippets(api: &Api, op: &Operation) -> [(&'static str, &'static str, Markup); 3] {
    let key = op.preferred_key().filter(|_| !op.keys().is_empty());
    let key_span = |key: &str| html! { span data-key { (key) "_…" } };
    let base = html! { span data-base { (api.server) } };
    let body = op.body().map(|(_, _, example)| {
        serde_json::to_string_pretty(if example.is_null() {
            &Value::Null
        } else {
            example
        })
        .unwrap_or_default()
    });
    let stream = op.stream();
    let indent = |text: &str, by: &str| text.replace('\n', &format!("\n{by}"));
    let curl = html! {
        "curl"
        @if op.method != "GET" { " -X " (op.method) }
        @if stream { " -N" }
        " " (base) (op.path)
        @if let Some(key) = key { " \\\n  -H \"Authorization: Bearer " (key_span(key)) "\"" }
        @if let Some(body) = &body {
            " \\\n  -H \"Content-Type: application/json\" \\\n  -d '" (indent(&body.replace('\'', "'\\''"), "  ")) "'"
        }
    };
    let js = html! {
        "const res = await fetch(\"" (base) (op.path) "\""
        @if op.method != "GET" || key.is_some() || body.is_some() {
            ", {"
            @if op.method != "GET" { "\n  method: \"" (op.method) "\"," }
            @if key.is_some() || body.is_some() {
                "\n  headers: {"
                @if let Some(key) = key { "\n    Authorization: \"Bearer " (key_span(key)) "\"," }
                @if body.is_some() { "\n    \"Content-Type\": \"application/json\"," }
                "\n  },"
            }
            @if let Some(body) = &body { "\n  body: JSON.stringify(" (indent(body, "  ")) ")," }
            "\n}"
        }
        ");\n"
        @if stream {
            "// Server-sent events: read them as they arrive.\nconst reader = res.body.pipeThrough(new TextDecoderStream()).getReader();"
        } @else if op.page() {
            "const page = await res.text();"
        } @else {
            "const data = await res.json();"
        }
    };
    let accept = if stream {
        "text/event-stream"
    } else {
        op.answers().first().copied().unwrap_or("application/json")
    };
    let http = html! {
        (op.method) " " span data-prefix {} (op.path) " HTTP/1.1\nHost: "
        span data-host { (api.server.split_once("://").map_or(api.server.as_str(), |(_, host)| host)) }
        @if let Some(key) = key { "\nAuthorization: Bearer " (key_span(key)) }
        "\nAccept: " (accept)
        @if let Some(body) = &body { "\nContent-Type: application/json\n\n" (body) }
    };
    [
        ("curl", "curl", curl),
        ("js", "JavaScript", js),
        ("http", "HTTP", http),
    ]
}

/// The explorer's page: its sections, then the Try it dialog.
pub(super) fn page(api: &Api) -> Markup {
    html! {
        (banner(api))
        (intro(api))
        // Where the resources start, which the old index's `#resources`
        // links (`api/reference/` now sends its readers here).
        div id="resources" {}
        @for resource in &api.resources {
            (resource_section(resource))
            @for op in &resource.operations {
                (operation(api, op))
            }
        }
        @if !api.onion.is_empty() {
            (onion_links(api))
        }
        (problems(api))
        (dialog(api))
    }
}

/// The banner across the top: where Try it sends requests, and the button
/// that changes it. Only with a script, which keeps its words true.
fn banner(api: &Api) -> Markup {
    let settings = ui::Button::new(ui::Slot::from(html! {
        (PreEscaped(icons::SETTINGS)) "Settings"
    }))
    .size(ui::ControlSize::Compact)
    .attr("data-opens-dialog", DIALOG)
    .attr("data-try", "");
    html! {
        div.api-banner data-only="script" role="region" aria-label="Try it" {
            (ui::StatusBanner::new(
                format!("Try it sends requests to {} with no key yet", api.server),
                ui::Tone::Notice,
            )
            .icon(ui::Slot::from(PreEscaped(icons::GLOBE.to_owned())))
            .action(settings)
            .attr("id", "try-it-banner"))
        }
    }
}

/// A panel beside a section: a titled box.
fn panel(title: &str, head: Markup, body: Markup) -> Markup {
    html! {
        div.api-panel {
            div.api-panel-head {
                span.api-panel-title { (title) }
                (head)
            }
            (body)
        }
    }
}

fn intro(api: &Api) -> Markup {
    let entry = api
        .operations()
        .find(|o| o.path == "/api/v1" && o.method == "GET");
    html! {
        section.api-sec.api-intro id=(INTRO_ID) {
            div.api-main.prose {
                p.crumbs { span { "API" } span { "v1" } }
                (PreEscaped(&api.intro))
            }
            div.api-side {
                (panel("Base URL", html! {}, html! {
                    pre { code { span data-base { (api.server) } "/api/v1" } }
                    p.api-note { "Your Monokulo's own address, with its path if it's served under one." }
                }))
                (panel("Key prefixes", html! {}, html! {
                    ul.api-fields.api-keys-list {
                        @for (_, prefix, word, what) in KEYS {
                            li {
                                p.api-field-head { code { (prefix) } span.api-field-type { (word) } }
                                p.api-field-text { (what) }
                            }
                        }
                    }
                }))
                @if let Some(entry) = entry {
                    (panel(
                        "First request",
                        html! {
                            (ui::Button::new("Try the entry")
                                .variant(ui::ButtonVariant::Primary)
                                .size(ui::ControlSize::Compact)
                                .attr("data-only", "script")
                                .attr("data-opens-dialog", DIALOG)
                                .attr("data-try", entry.id.as_str()))
                        },
                        html! { pre { code { (snippets(api, entry)[0].2) } } },
                    ))
                }
            }
        }
    }
}

fn resource_section(resource: &Resource) -> Markup {
    html! {
        section.api-sec.api-resource id=(resource.tag) {
            div.api-main {
                p.crumbs { span { "Resource" } }
                h2 {
                    (resource.name)
                    @if let Some(rel) = &resource.rel {
                        " " a.api-rel href={ "../" (rel) } { code { "mk:" (resource.tag) } }
                    }
                }
                div.prose { (markdown_html(&local(resource.description, "../"))) }
            }
            div.api-side {
                nav.api-routes aria-label={ (resource.name) " routes" } {
                    @for op in &resource.operations {
                        a href={ "#" (op.id) } {
                            (method_tag(op.method))
                            span.api-route-path { (op.path) }
                        }
                    }
                }
            }
        }
    }
}

/// Who may call an operation, as chips.
fn who(op: &Operation) -> Markup {
    let keys = op.keys();
    html! {
        p.api-who {
            "Who can call it:"
            @if op.anyone() { (ui::Tag::new("No key")) }
            @for (name, prefix, word, _) in KEYS {
                @if keys.contains(&name) {
                    (ui::Tag::new(ui::Slot::from(html! { code { (prefix) } " " (word) })))
                }
            }
            @if op.session() { (ui::Tag::new("Signed-in merchant")) }
        }
    }
}

/// A list of fields: name, type, whether required, what it is.
fn field_list(items: Vec<(String, String, String, bool)>) -> Markup {
    html! {
        ul.api-fields {
            @for (name, kind, description, required) in items {
                li {
                    p.api-field-head {
                        code { (name) }
                        span.api-field-type { (kind) }
                        @if required { (ui::Tag::new("Required").tone(ui::TagTone::Muted)) }
                    }
                    @if !description.is_empty() {
                        div.api-field-text { (markdown_html(&local(&description, "../"))) }
                    }
                }
            }
        }
    }
}

fn operation(api: &Api, op: &Operation) -> Markup {
    // The heading is the first sentence; the rest of the first paragraph
    // and the paragraphs after it follow the chips.
    let (lead, rest) = split_doc(text(op.op, "description"));
    let lead = lead[first_sentence(lead).len()..].trim();
    let about = [lead, rest]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let parameters: Vec<(String, String, String, bool)> = op
        .parameters()
        .iter()
        .map(|p| {
            let description = p["description"]
                .as_str()
                .or(p["schema"]["description"].as_str())
                .unwrap_or_default();
            (
                text(p, "name").to_owned(),
                format!(
                    "{} · {}",
                    text(p, "in"),
                    type_name(&p["schema"], api.schemas)
                ),
                description.to_owned(),
                p["required"] == true,
            )
        })
        .collect();
    let responses = op.responses();
    // What Try it needs to know of it: how to send it, and who may.
    html! {
        section.api-sec.api-op id=(op.id) data-op=(op.id) data-method=(op.method) data-path=(op.path)
            data-keys=(op.keys().join(" ")) data-anyone[op.anyone()] data-stream[op.stream()] {
            div.api-main {
                h3 { (op.title()) }
                div.api-endpoint {
                    (method_tag(op.method))
                    code.api-path { (path_markup(op.path)) }
                    (ui::Button::new("Copy")
                        .size(ui::ControlSize::Compact)
                        .attr("data-only", "script")
                        .attr("data-copy-path", op.path))
                }
                (who(op))
                @if !about.is_empty() {
                    div.prose { (markdown_html(&local(&about, "../"))) }
                }
                @if !parameters.is_empty() {
                    h4.api-block { "Parameters" }
                    (field_list(parameters))
                }
                @if let Some((kind, schema, _)) = op.body() {
                    h4.api-block { "Body " span.api-block-kind { "· " (kind) } }
                    (field_list(body_fields(api, schema)))
                }
                h4.api-block { "Responses" }
                ul.api-responses {
                    @for (status, description, _) in &responses {
                        li {
                            (ui::HttpStatusTag::of(*status))
                            div { (markdown_html(&local(description, "../"))) }
                        }
                    }
                }
            }
            div.api-side {
                (request_panel(api, op))
                (response_panel(op, &responses))
            }
            @if op.triable() {
                (try_form(op))
            }
        }
    }
}

/// A body's fields, from its schema (an object, or a reference to one).
fn body_fields(api: &Api, schema: &Value) -> Vec<(String, String, String, bool)> {
    let schema = match schema["$ref"].as_str() {
        Some(reference) => &api.schemas[reference.rsplit('/').next().unwrap_or(reference)],
        None => schema,
    };
    fields(schema, api.schemas)
        .into_iter()
        .map(|(name, kind, description, required)| {
            (name.to_owned(), kind, description.to_owned(), required)
        })
        .collect()
}

fn request_panel(api: &Api, op: &Operation) -> Markup {
    let snippets = snippets(api, op);
    let languages = ui::Slot::many(snippets.iter().enumerate().map(|(i, (name, label, _))| {
        ui::Slot::from(
            ui::Button::new(*label)
                .attr("data-lang", *name)
                .attr("aria-pressed", if i == 0 { "true" } else { "false" }),
        )
    }));
    let head = html! {
        span.api-panel-tools data-only="script" {
            (ui::SegmentedControl::new("Language", languages))
            (ui::Button::new("Copy").size(ui::ControlSize::Compact).attr("data-copy-snippet", ""))
        }
    };
    let body = html! {
        @for (name, label, code) in &snippets {
            figure.api-snippet data-snippet=(name) {
                figcaption data-only="no-script" { (label) }
                pre { code { (code) } }
            }
        }
        div.api-panel-foot {
            @if op.triable() {
                p data-only="script" { "Send it to your Monokulo from here." }
                p data-only="no-script" { "Try it, which sends this to your Monokulo from this page, needs JavaScript." }
                (ui::Button::new("Try it")
                    .variant(ui::ButtonVariant::Primary)
                    .attr("data-only", "script")
                    .attr("data-opens-dialog", DIALOG)
                    .attr("data-try", op.id.as_str()))
            } @else if op.page() {
                p { "A page for people, opened in a browser: nothing to try from here." }
            } @else {
                p { "Called once by a plugin's or an app's own server, not from a page: send it with curl." }
            }
        }
    };
    panel("Request", head, body)
}

/// The recorded responses: a disclosure per status, every example in it;
/// with a script, a switch for the status and one for the example, and the
/// live answer in place of them once there is one.
fn response_panel(op: &Operation, responses: &[Response]) -> Markup {
    let preferred = op.preferred_key();
    let first = responses
        .iter()
        .position(|(_, _, examples)| !examples.is_empty())
        .unwrap_or(0);
    let statuses = ui::Slot::many(responses.iter().enumerate().map(|(i, (status, ..))| {
        ui::Slot::from(
            ui::Button::new(*status)
                .attr("data-status", *status)
                .attr("aria-pressed", if i == first { "true" } else { "false" }),
        )
    }));
    let head = html! {
        (ui::Tag::new("Recorded").tone(ui::TagTone::Muted).attr("data-recorded", ""))
    };
    let body = html! {
        div.api-recorded {
            div.api-panel-bar data-only="script" {
                span { "Status" }
                (ui::SegmentedControl::new("Status", statuses))
            }
            @for (i, (status, description, examples)) in responses.iter().enumerate() {
                @let shown = examples
                    .iter()
                    .position(|(name, ..)| Some(*name) == preferred)
                    .unwrap_or(0);
                details.api-example data-status=(status) open[i == first] {
                    summary {
                        (ui::HttpStatusTag::of(*status))
                        " " (split_doc(description).0.lines().next().unwrap_or_default())
                    }
                    @if examples.len() > 1 {
                        div.api-panel-bar data-only="script" {
                            span { "Example" }
                            (ui::SegmentedControl::new("Example", ui::Slot::many(examples.iter().enumerate().map(|(j, (name, summary, _))| {
                                ui::Slot::from(
                                    ui::Button::new(*name)
                                        .attr("data-example", *name)
                                        .attr("title", *summary)
                                        .attr("aria-pressed", if j == shown { "true" } else { "false" }),
                                )
                            }))))
                        }
                    }
                    @for (j, (name, summary, value)) in examples.iter().enumerate() {
                        figure.api-variant data-example=(name) data-shown[j == shown] {
                            figcaption data-only="no-script" { code { (name) } }
                            pre { code { (json(value)) } }
                            p.api-note {
                                @if !summary.is_empty() { (summary) ". " }
                                "Recorded against a test engine; ids and keys are stand-ins."
                            }
                        }
                    }
                    @if examples.is_empty() {
                        p.api-note { "No body." }
                    }
                }
            }
        }
        div.api-live hidden aria-live="polite" {}
    };
    panel("Response", head, body)
}

/// An operation's form in the Try it dialog, which the script puts there
/// when its Try it opens it: a field per path and query parameter, and the
/// body, prefilled from the recorded example.
fn try_form(op: &Operation) -> Markup {
    let field = |label: &str, id: String, hint: String| {
        ui::Field::new(label, id, label)
            .hint(hint)
            .attr("spellcheck", "false")
    };
    html! {
        template data-try-form=(op.id) {
            form.try-it-form id="try-it-form" {
                h3 { "Request" }
                div.try-it-warnings {}
                @for p in op.parameters() {
                    @let name = text(p, "name");
                    @if text(p, "in") == "path" {
                        (field(name, format!("try-it-path-{name}"), "In the path.".to_owned())
                            .required(true)
                            .placeholder(name)
                            .attr("data-path-param", name))
                    } @else if text(p, "in") == "query" {
                        (field(
                            name,
                            format!("try-it-query-{name}"),
                            p["schema"]["description"].as_str().unwrap_or("Optional.").to_owned(),
                        )
                        .attr("data-query-param", name))
                    }
                }
                @if let Some((kind, _, example)) = op.body() {
                    (field("Body", "try-it-body".to_owned(), format!("{kind}, prefilled from the recorded example."))
                        .multiline(true)
                        .rows(9usize)
                        .value(serde_json::to_string_pretty(example).unwrap_or_default())
                        .attr("data-body", ""))
                }
            }
        }
    }
}

fn onion_links(api: &Api) -> Markup {
    html! {
        section.api-sec.api-wide id="onion-links" {
            div.api-main {
                p.crumbs { span { "Resources" } }
                h2 { "Onion links" }
                p {
                    "While a Monokulo has an onion address, these say where a resource is on it, as absolute URLs, "
                    "whichever address the client called. Without one they're absent."
                }
                div.tbl { table {
                    thead { tr { th { "Relation" } th { "URL" } th { "What it is" } } }
                    tbody {
                        @for rel in api.onion {
                            tr {
                                td { a href={ "../rels/" (text(rel, "rel")) "/" } { code { (text(rel, "key")) } } }
                                td { code { (text(rel, "template")) } }
                                td { (text(rel, "title")) }
                            }
                        }
                    }
                } }
            }
        }
    }
}

fn problems(api: &Api) -> Markup {
    html! {
        section.api-sec.api-wide id="problems" {
            div.api-main {
                p.crumbs { span { "Errors" } }
                h2 { "Problems" }
                p {
                    "Every error is " code { "application/problem+json" } ". Its " code { "type" }
                    " is the problem's page; " code { "detail" } " says what went wrong this time."
                }
                div.tbl { table {
                    thead { tr { th { "Status" } th { "Type" } th { "Title" } } }
                    tbody {
                        @for problem in api.problems {
                            @let slug = text(problem, "slug");
                            tr id={ "problem-" (slug) } {
                                td { (ui::HttpStatusTag::of(problem["status"].to_string())) }
                                td { a href={ "../problems/" (slug) "/" } { code { (slug) } } }
                                td { (text(problem, "title")) }
                            }
                        }
                    }
                } }
            }
        }
    }
}

/// The Try it dialog: the reader's Monokulo and key, the connection's
/// state, then (opened from an operation) the operation's form and its
/// live answer. Its actions say which way it was opened (`data-for`).
fn dialog(api: &Api) -> Markup {
    let address = ui::Field::new("Your Monokulo address", "try-it-address", "address")
        .kind(ui::InputType::Url)
        .placeholder(api.server.as_str())
        .autocomplete("url")
        .hint("Its address, with its path if it's served under one.")
        .attr("spellcheck", "false");
    let show = ui::IconButton::new(
        "Show the key",
        ui::Slot::from(PreEscaped(icons::EYE.to_owned())),
    )
    .attr("id", "try-it-show")
    .attr("aria-pressed", "false");
    let key = html! {
        div.mk-field {
            label for="try-it-key" { "Key" }
            (ui::Input::new("key")
                .id("try-it-key")
                .kind(ui::InputType::Password)
                .placeholder("sk_…, ak_… or pk_…")
                .autocomplete("off")
                .described_by("try-it-key-help")
                .actions(show)
                .attr("spellcheck", "false"))
            p.mk-hint id="try-it-key-help" {
                "Kept in this tab only, and sent only to this address. Use a stagenet store's key."
            }
        }
    };
    let actions = html! {
        (ui::Button::new("Check connection").attr("id", "try-it-check"))
        (ui::Button::new("Close").attr("data-closes-dialog", "").attr("data-for", "operation"))
        (ui::Button::new("Send request")
            .variant(ui::ButtonVariant::Primary)
            .kind(ui::ButtonType::Submit)
            .attr("form", "try-it-form")
            .attr("data-for", "operation"))
        (ui::Button::new("Done")
            .variant(ui::ButtonVariant::Primary)
            .attr("data-closes-dialog", "")
            .attr("data-for", "settings"))
    };
    let title = ui::DialogTitle::new(
        "try-it-title",
        ui::Slot::from(html! { span id="try-it-heading" { "Try it with your Monokulo" } }),
    )
    .aside(
        ui::Tag::new("")
            .attr("id", "try-it-route")
            .attr("hidden", ""),
    );
    let body = html! {
        (title)
        (ui::Hint::new("Requests go from this browser straight to your Monokulo."))
        div.try-it-fields { (address) (key) }
        p.try-it-state {
            "Connection "
            (ui::Tag::new("Not checked").tone(ui::TagTone::Unknown).attr("id", "try-it-state").attr("aria-live", "polite"))
        }
        div id="try-it-operation" {}
        (ui::DialogActions::new(ui::Slot::from(actions)))
    };
    html! {
        (ui::Dialog::new(DIALOG, "try-it-title", ui::Slot::from(body))
            .size(ui::DialogSize::Wide)
            .attr("data-server", api.server.as_str()))
    }
}

/// Which page of the API's section a map is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Here<'a> {
    /// The explorer, `api/`.
    Explorer,
    /// A page two folders below the site's root (`rels/order/`,
    /// `problems/not-found/`) that belongs to this section of the explorer.
    Under(&'a str),
}

/// The API's map: the introduction and its sections, every resource with
/// its operations, the onion links and the problems. On the explorer its
/// links are its own sections (`#orders-create`), and the site's script
/// marks the one being read; on a page under it, they're the explorer's,
/// the section the page belongs to marked as where it is.
pub(super) fn map(api: &Api, here: Here) -> Vec<shell::Group> {
    let (up, marks) = match here {
        Here::Explorer => ("../", None),
        Here::Under(section) => ("../../", Some(section)),
    };
    let href = |id: &str| match here {
        Here::Explorer => format!("#{id}"),
        Here::Under(_) => format!("{up}api/#{id}"),
    };
    let link = |label: Markup, id: &str, nested: bool| shell::Link {
        label,
        href: href(id),
        current: (marks == Some(id)).then_some(shell::Mark::Location),
        file: false,
        nested,
    };
    let mut overview = vec![link(html! { "Introduction" }, INTRO_ID, false)];
    overview.extend(
        api.sections()
            .iter()
            .map(|s| link(PreEscaped(s.text.clone()), &s.id, true)),
    );
    overview.push(shell::Link {
        file: true,
        href: format!("{up}api/v1/openapi.json"),
        ..link(html! { "OpenAPI document" }, "", false)
    });
    let mut resources = Vec::new();
    for resource in &api.resources {
        resources.push(link(html! { (resource.name) }, resource.tag, false));
        for op in &resource.operations {
            resources.push(link(
                html! { (method_tag(op.method)) span { (op.title()) } },
                &op.id,
                true,
            ));
        }
    }
    if !api.onion.is_empty() {
        resources.push(link(html! { "Onion links" }, "onion-links", false));
    }
    vec![
        shell::Group {
            title: "Overview".to_owned(),
            icon: icon::BOOK,
            links: overview,
        },
        shell::Group {
            title: "Resources".to_owned(),
            icon: icon::BRACES,
            links: resources,
        },
        shell::Group {
            title: "Errors".to_owned(),
            icon: icon::ALERT,
            links: vec![link(html! { "Problems" }, "problems", false)],
        },
    ]
}

/// What search finds in the explorer, each at its section: the
/// introduction and its sections, each resource, each operation and each
/// problem.
pub(super) fn search(api: &Api) -> Vec<Entry> {
    let mut entries = vec![Entry {
        title: "HTTP API".to_owned(),
        sub: "API reference · Introduction".to_owned(),
        kind: "Guide",
        href: "api/".to_owned(),
        method: None,
    }];
    for section in api.sections() {
        entries.push(Entry {
            title: section.plain(),
            sub: "HTTP API".to_owned(),
            kind: "Section",
            href: format!("api/#{}", section.id),
            method: None,
        });
    }
    for resource in &api.resources {
        entries.push(Entry {
            title: resource.name.clone(),
            sub: split_doc(resource.description).0.to_owned(),
            kind: "Resource",
            href: format!("api/#{}", resource.tag),
            method: None,
        });
        for op in &resource.operations {
            entries.push(Entry {
                title: op.title().to_owned(),
                sub: format!("{} {}", op.method, op.path),
                kind: "Endpoint",
                href: format!("api/#{}", op.id),
                method: Some(op.method.to_owned()),
            });
        }
    }
    for problem in api.problems {
        entries.push(Entry {
            title: text(problem, "title").to_owned(),
            sub: format!("{} · {}", problem["status"], text(problem, "slug")),
            kind: "Problem",
            href: format!("api/#problem-{}", text(problem, "slug")),
            method: None,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The coloured JSON reads as `serde_json` writes it: the same text,
    /// once its spans are taken out and its escapes read.
    #[test]
    fn a_coloured_example_is_the_json_it_colours() {
        let value = json!({
            "a": [1, 2.5, -3], "b": {"c": null, "d": true}, "e": [], "f": {},
            "g": "<tag> & \"quoted\"", "h": [{"i": "j"}]
        });
        let markup = json(&value).into_string();
        let tag = regex::Regex::new(r"<[^>]+>").unwrap();
        let text = tag
            .replace_all(&markup, "")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&amp;", "&");
        assert_eq!(text, serde_json::to_string_pretty(&value).unwrap());
        // An event stream is a string, shown as it is.
        let stream = json(&json!("event: order\ndata: {}\n\n")).into_string();
        assert_eq!(stream, "event: order\ndata: {}\n\n");
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

    /// A path's parameters stand out from the rest of it.
    #[test]
    fn a_paths_parameters_are_marked() {
        assert_eq!(
            path_markup("/api/v1/orders/{order_id}/events").into_string(),
            "/api/v1/orders/<b>{order_id}</b>/events"
        );
        assert_eq!(path_markup("/api/v1").into_string(), "/api/v1");
    }
}
