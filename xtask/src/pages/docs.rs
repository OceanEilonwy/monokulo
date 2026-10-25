//! `cargo xtask pages docs`: the guides for adding Monokulo to a site, from
//! the Markdown in `xtask/src/pages/docs/`, beside the landing page and the
//! coverage report. Each page is rendered with maud around pulldown-cmark's
//! HTML, in the shell the docs and the API reference share (`shell`): the
//! map of every page down the left, grouped as `PAGES` groups them, and the
//! page's sections beside it. A fenced `diff` block becomes the
//! walkthrough's added and kept lines.
//!
//! Every page is a folder at the site's top level (docs.monokulo.com):
//! `/guide/` is the front page, `/webhooks/`, `/plugins/` and the rest
//! beside it. The sample shop the walkthrough builds, `examples/geomart/`,
//! is copied to `/geomart/` so its pages run there. The docs were under
//! `/docs/` until the site moved to its own domain; every old address keeps
//! a small page that sends the reader on (`redirects`).
//!
//! `/api/` is the HTTP API's section: its front page is `api.md`, here, and
//! the reference generated from the API goes beside it, from its own build
//! step into the same site folder: its pages under `api/reference/`, the
//! OpenAPI document at `api/v1/openapi.json`, and the link relations and
//! problem types its responses name under `rels/` and `problems/`. No docs
//! page is published in those folders (`API_OUTPUT` in the tests).
//!
//! Search finds every page and each of its sections, from the index the
//! build writes at `search/docs.json`.

use super::shell::{self, icon, Group as MapGroup, Link, Mark};
use super::site_assets;
use super::site_header::{Section, GITHUB_URL, SITE_URL};
use crate::support::{at, Exit};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// A group of the docs' map, in the order the map shows them. A group with
/// no page and no link isn't shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Group {
    GetStarted,
    TakePayments,
    Integrations,
    Reference,
}

impl Group {
    const ALL: [Group; 4] = [
        Group::GetStarted,
        Group::TakePayments,
        Group::Integrations,
        Group::Reference,
    ];

    fn title(self) -> &'static str {
        match self {
            Group::GetStarted => "Get started",
            Group::TakePayments => "Take payments",
            Group::Integrations => "Integrations",
            Group::Reference => "Reference",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Group::GetStarted => icon::FLAG,
            Group::TakePayments => icon::COIN,
            Group::Integrations => icon::PLUG,
            Group::Reference => icon::BOOK,
        }
    }
}

/// One page of the docs: its Markdown file in `xtask/src/pages/docs/`, the
/// folder it is published as at the site's top level, its name in the map,
/// the map's group it is in, the part of the site the header marks, and the
/// folder it was published as before the docs moved to their own domain
/// (`docs/…`), which keeps a redirect to it; none for a page added since.
pub(super) struct Page {
    pub(super) source: &'static str,
    pub(super) dir: &'static str,
    pub(super) label: &'static str,
    pub(super) group: Group,
    pub(super) section: Section,
    pub(super) old: Option<&'static str>,
}

/// The docs, in the map's order, which is also Previous and Next's: each
/// group's pages together, the groups in `Group::ALL`'s order. The first is
/// where the bar's Docs and Get started go.
pub(super) const PAGES: [Page; 8] = [
    Page {
        source: "index.md",
        dir: "guide",
        label: "Accept your first payment",
        group: Group::GetStarted,
        section: Section::Docs,
        old: Some("docs"),
    },
    Page {
        source: "walkthrough-geomart.md",
        dir: "walkthrough-geomart",
        label: "Walkthrough: Geomart",
        group: Group::GetStarted,
        section: Section::Docs,
        old: Some("docs/walkthrough-geomart"),
    },
    Page {
        source: "js-library.md",
        dir: "js-library",
        label: "JS library",
        group: Group::TakePayments,
        section: Section::Docs,
        old: Some("docs/js-library"),
    },
    Page {
        source: "paid.md",
        dir: "paid",
        label: "When is it paid?",
        group: Group::TakePayments,
        section: Section::Docs,
        old: Some("docs/paid"),
    },
    Page {
        source: "webhooks.md",
        dir: "webhooks",
        label: "Webhooks",
        group: Group::TakePayments,
        section: Section::Docs,
        old: Some("docs/webhooks"),
    },
    Page {
        source: "plugins.md",
        dir: "plugins",
        label: "Shop plugins",
        group: Group::Integrations,
        section: Section::Docs,
        old: Some("docs/plugins"),
    },
    Page {
        source: "apps.md",
        dir: "apps",
        label: "Connected apps",
        group: Group::Integrations,
        section: Section::Docs,
        old: Some("docs/apps"),
    },
    Page {
        source: "api.md",
        dir: "api",
        label: "API reference",
        group: Group::Reference,
        section: Section::Api,
        old: None,
    },
];

/// The map's links that aren't docs pages, after their group's pages: the
/// API reference's own, which its build writes (`api_reference`). Each is
/// (group, label, link from the site's root, whether it's a file rather
/// than a page).
const LINKS: [(Group, &str, &str, bool); 2] = [
    (
        Group::Reference,
        "Problems",
        "api/reference/#problems",
        false,
    ),
    (
        Group::Reference,
        "OpenAPI document",
        "api/v1/openapi.json",
        true,
    ),
];

/// The sample shop's files, from `examples/geomart/` to `geomart/`, each
/// redirected from `docs/geomart/` too.
const GEOMART: [&str; 2] = ["index.html", "done.html"];

/// The webhook test vectors plugins test against (monokulo's
/// `webhooks::vectors` keeps them true), beside the plugins page.
const VECTORS: &str = "webhook-vectors.json";

/// `cargo xtask pages docs --site DIR`.
pub(crate) fn build(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let site = match args {
        ["--site", dir] => PathBuf::from(dir),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pages docs needs --site DIR",
            ))
        }
    };
    write(root, &site)?;
    eprintln!("docs in {}: {} page(s)", site.display(), PAGES.len());
    Ok(Exit::SUCCESS)
}

/// Writes every page, the sample shop and the old addresses' redirects into
/// the site's folder `site`, and the assets they use into its `assets/`.
pub(super) fn write(root: &Path, site: &Path) -> io::Result<()> {
    let put = |path: PathBuf, contents: String| {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
        }
        fs::write(&path, contents).map_err(|e| at(&path, e))
    };
    let mut found = Vec::new();
    for page in &PAGES {
        let source = root.join("xtask/src/pages/docs").join(page.source);
        let markdown = fs::read_to_string(&source).map_err(|e| at(&source, e))?;
        put(
            site.join(page.dir).join("index.html"),
            render(page, &markdown).0,
        )?;
        found.extend(index(page, &markdown));
    }
    shell::write_index(site, "docs", &found)?;
    for (old, new) in redirects() {
        put(site.join(&old), redirect(&old, &new).0)?;
    }
    site_assets::write(site)?;
    let geomart = site.join("geomart");
    fs::create_dir_all(&geomart).map_err(|e| at(&geomart, e))?;
    for name in GEOMART {
        let from = root.join("examples/geomart").join(name);
        fs::copy(&from, site.join("geomart").join(name)).map_err(|e| at(&from, e))?;
    }
    // The vectors are data a plugin's tests may fetch, which a redirect
    // page would not answer: their old address keeps a copy.
    let vectors = root.join("xtask/src/pages/docs").join(VECTORS);
    for dir in ["plugins", "docs/plugins"] {
        fs::copy(&vectors, site.join(dir).join(VECTORS)).map_err(|e| at(&vectors, e))?;
    }
    Ok(())
}

/// Every address the docs had under `docs/`, as a file in the site, and the
/// path from the site's root it is at now: each page with an `old` folder
/// (`docs/webhooks/index.html` to `webhooks/`), and the sample shop's
/// pages (`docs/geomart/done.html` to `geomart/done.html`).
fn redirects() -> Vec<(String, String)> {
    let pages = PAGES.iter().filter_map(|page| {
        let old = page.old?;
        Some((format!("{old}/index.html"), format!("{}/", page.dir)))
    });
    let shop = GEOMART.iter().map(|&name| {
        let new = match name {
            "index.html" => "geomart/".to_string(),
            _ => format!("geomart/{name}"),
        };
        (format!("docs/geomart/{name}"), new)
    });
    pages.chain(shop).collect()
}

/// The page left at the old address `old` (a file in the site) for `new` (a
/// path from the site's root): it refreshes there at once, names it as the
/// canonical address, and links it for a reader whose browser does not
/// follow the refresh. It needs no script; a browser with one keeps the old
/// link's `#section` (and query), which a refresh drops.
fn redirect(old: &str, new: &str) -> Markup {
    let to = format!("{}{new}", "../".repeat(old.matches('/').count()));
    let canonical = format!("{SITE_URL}/{new}");
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Moved · Monokulo docs" }
                link rel="canonical" href=(canonical);
                meta name="robots" content="noindex";
                script { (PreEscaped(format!(r#"location.replace("{to}" + location.search + location.hash);"#))) }
                meta http-equiv="refresh" content=(format!("0; url={to}"));
            }
            body {
                p { "This page has moved to " a href=(to) { (canonical) } "." }
            }
        }
    }
}

/// The docs' map, from a page `up` from the site's root, `current` marked.
pub(super) fn map(up: &str, current: Option<&str>) -> Vec<MapGroup> {
    Group::ALL
        .into_iter()
        .map(|group| {
            let pages = PAGES.iter().filter(|p| p.group == group).map(|p| Link {
                label: html! { (p.label) },
                href: format!("{up}{}/", p.dir),
                current: Mark::page_if(current == Some(p.dir)),
                file: false,
            });
            let links = LINKS
                .iter()
                .filter(|(g, ..)| *g == group)
                .map(|(_, label, href, file)| Link {
                    label: html! { (label) },
                    href: format!("{up}{href}"),
                    current: None,
                    file: *file,
                });
            MapGroup {
                title: group.title().to_owned(),
                icon: group.icon(),
                links: pages.chain(links).collect(),
            }
        })
        .filter(|group| !group.links.is_empty())
        .collect()
}

/// A page's title: its Markdown's `# ` heading, or its name in the map.
fn title<'a>(page: &'a Page, markdown: &'a str) -> &'a str {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .unwrap_or(page.label)
}

/// A page, in the shell, with Previous and Next in the map's order. Every
/// page is a folder at the site's top level, so the site's root is `../`
/// from each.
pub(super) fn render(page: &Page, markdown: &str) -> Markup {
    let up = "../";
    let at = PAGES.iter().position(|p| p.dir == page.dir);
    let neighbour = |i: Option<usize>| {
        i.and_then(|i| PAGES.get(i))
            .map(|p| (p.label.to_owned(), format!("{up}{}/", p.dir)))
    };
    let article = html! {
        p.crumbs { span { "Docs" } span { (page.group.title()) } }
        (markdown_html(markdown))
    };
    shell::render(shell::Page {
        up,
        section: page.section,
        title: format!("{} · Monokulo docs", title(page, markdown)),
        description: "How to add Monokulo, self-hosted Monero payments, to your own website.",
        map_label: "Docs",
        map: map(up, Some(page.dir)),
        article,
        previous: neighbour(at.and_then(|i| i.checked_sub(1))),
        next: neighbour(at.map(|i| i + 1)),
        edit: Some(format!(
            "{GITHUB_URL}/edit/main/xtask/src/pages/docs/{}",
            page.source
        )),
    })
}

/// What search finds on a page: the page, and each of its sections.
fn index(page: &Page, markdown: &str) -> Vec<shell::Entry> {
    let guide = shell::Entry {
        title: page.label.to_owned(),
        sub: format!("{} · {}", page.group.title(), title(page, markdown)),
        kind: "Guide",
        href: format!("{}/", page.dir),
        method: None,
    };
    let sections = shell::sections(&markdown_html(markdown).0)
        .into_iter()
        .map(|section| shell::Entry {
            title: section.plain(),
            sub: page.label.to_owned(),
            kind: "Section",
            href: format!("{}/#{}", page.dir, section.id),
            method: None,
        });
    std::iter::once(guide).chain(sections).collect()
}

/// `text` as an anchor: `Step 6 · Verify, test, go live` is `step-6`
/// (headings name their anchor by what comes before a `·`).
fn anchor(text: &str) -> String {
    let lead = text.split('·').next().unwrap_or(text);
    let mut slug = String::new();
    for c in lead.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// A fenced `diff` block: a `# file` line names the file, `+` lines are
/// added, the rest are kept.
fn diff_block(code: &str) -> Markup {
    html! {
        pre.diff {
            @for line in code.lines() {
                @if let Some(file) = line.strip_prefix("# ") {
                    span.file { (file) }
                } @else if line.starts_with('+') {
                    span.add { (line) }
                } @else {
                    span.ctx { (line) }
                }
            }
        }
    }
}

/// The Markdown as HTML: headings get anchors (`#step-6`), diff blocks
/// their own drawing, tables a scrolling box.
pub(super) fn markdown_html(markdown: &str) -> Markup {
    let options = Options::ENABLE_TABLES;
    let mut events = Vec::new();
    let mut parser = Parser::new_ext(markdown, options).peekable();
    while let Some(event) = parser.next() {
        match event {
            Event::Start(Tag::Heading { level, .. }) if level != HeadingLevel::H1 => {
                let mut inner = Vec::new();
                let mut text = String::new();
                for event in parser.by_ref() {
                    match event {
                        Event::End(TagEnd::Heading(_)) => break,
                        Event::Text(t) | Event::Code(t) => {
                            text.push_str(&t);
                            inner.push(Event::Text(t));
                        }
                        other => inner.push(other),
                    }
                }
                let mut html_inner = String::new();
                pulldown_cmark::html::push_html(&mut html_inner, inner.into_iter());
                let tag = level.to_string();
                events.push(Event::Html(
                    format!(
                        "<{tag} id=\"{}\">{}</{tag}>",
                        anchor(&text),
                        html_inner.trim()
                    )
                    .into(),
                ));
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) if &*lang == "diff" => {
                let mut code = String::new();
                for event in parser.by_ref() {
                    match event {
                        Event::End(TagEnd::CodeBlock) => break,
                        Event::Text(t) => code.push_str(&t),
                        _ => {}
                    }
                }
                events.push(Event::Html(diff_block(&code).0.into()));
            }
            Event::Start(Tag::Table(_)) => {
                events.push(Event::Html("<div class=\"tbl\">".into()));
                events.push(event);
            }
            Event::End(TagEnd::Table) => {
                events.push(event);
                events.push(Event::Html("</div>".into()));
            }
            other => events.push(other),
        }
    }
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events.into_iter());
    PreEscaped(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    /// The folders the generated API reference is written to, beside
    /// `api/index.html`.
    const API_OUTPUT: [&str; 4] = ["api/reference", "api/v1", "rels", "problems"];

    #[test]
    fn every_page_and_the_sample_shop_are_written() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new("docs");
        let site = scratch.path();
        write(root, site).unwrap();
        for page in &PAGES {
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            assert!(
                html.contains(r#"<article class="prose">"#),
                "{}",
                page.source
            );
        }
        let walkthrough = fs::read_to_string(site.join("walkthrough-geomart/index.html")).unwrap();
        assert!(
            walkthrough.contains(r#"id="step-6""#),
            "the Done page links here"
        );
        assert!(site.join("geomart/index.html").is_file());
        assert!(site.join("geomart/done.html").is_file());
        // The plugins page links the webhook vectors beside it, which their
        // old address keeps too.
        let plugins = fs::read_to_string(site.join("plugins/index.html")).unwrap();
        assert!(
            plugins.contains(r#"href="webhook-vectors.json""#),
            "{plugins}"
        );
        for dir in ["plugins", "docs/plugins"] {
            let vectors = fs::read_to_string(site.join(dir).join(VECTORS)).unwrap();
            assert!(
                serde_json::from_str::<serde_json::Value>(&vectors).unwrap()["deliveries"]
                    .is_array()
            );
        }
        // The API page is the API's section in the header, and the docs
        // write nothing where the generated reference goes.
        let api = fs::read_to_string(site.join("api/index.html")).unwrap();
        assert!(
            api.contains(r#"<a href="../api/" aria-current="page">API reference</a>"#),
            "{api}"
        );
        for dir in API_OUTPUT {
            assert!(!site.join(dir).exists(), "{dir}");
        }
        // Every page links its files from the site's assets/.
        for page in &PAGES {
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            let linked = site_assets::assert_links_published(&html, &site.join("assets"));
            assert!(linked.contains(&"styles.css"), "{}", page.source);
            assert!(linked.contains(&"site.js"), "{}", page.source);
        }
    }

    /// The links in a page's map, in order: (link, whether it's marked as
    /// the page).
    fn map_links(html: &str) -> Vec<(String, bool)> {
        let map = html.split(r#"<nav class="map""#).nth(1).unwrap();
        let map = map.split("</nav>").next().unwrap();
        let groups = map.split_once(r#"<div class="map-group">"#).unwrap().1;
        let groups = groups.split(r#"<div class="map-foot">"#).next().unwrap();
        let link = regex::Regex::new(r#"<a href="([^"]+)"( aria-current="page")?"#).unwrap();
        link.captures_iter(groups)
            .map(|c| (c[1].to_owned(), c.get(2).is_some()))
            .collect()
    }

    /// Every page's map lists every page, in `PAGES`' order, grouped as
    /// `PAGES` says, and marks the page itself; Previous and Next go to the
    /// pages either side; "On this page" lists the page's sections, which
    /// are every `h2` it has.
    #[test]
    fn every_page_is_in_the_map_and_lists_its_sections() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new("docs-shell");
        let site = scratch.path();
        write(root, site).unwrap();
        let h2 = regex::Regex::new(r"<h2[ >]").unwrap();
        let toc_link = regex::Regex::new(r##"<li><a href="#([^"]+)">"##).unwrap();
        for (i, page) in PAGES.iter().enumerate() {
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            let links = map_links(&html);
            let pages: Vec<&(String, bool)> = links
                .iter()
                .filter(|(href, _)| PAGES.iter().any(|p| *href == format!("../{}/", p.dir)))
                .collect();
            assert_eq!(pages.len(), PAGES.len(), "{}: {links:?}", page.dir);
            for (listed, other) in pages.iter().zip(&PAGES) {
                assert_eq!(listed.0, format!("../{}/", other.dir), "{}", page.dir);
                assert_eq!(
                    listed.1,
                    other.dir == page.dir,
                    "{}: {}",
                    page.dir,
                    other.dir
                );
            }
            // The map's groups in their order, each page under its own.
            let map = html.split(r#"<nav class="map""#).nth(1).unwrap();
            let map = map.split(r#"<div class="map-foot">"#).next().unwrap();
            let groups: Vec<(&str, &str)> = map
                .split(r#"<div class="map-group">"#)
                .skip(1)
                .map(|group| {
                    let heading = group.split("</svg>").nth(1).unwrap();
                    (heading.split("</p>").next().unwrap(), group)
                })
                .collect();
            let titles: Vec<&str> = groups.iter().map(|(title, _)| *title).collect();
            let expected: Vec<&str> = Group::ALL
                .iter()
                .filter(|g| {
                    PAGES.iter().any(|p| p.group == **g) || LINKS.iter().any(|l| l.0 == **g)
                })
                .map(|g| g.title())
                .collect();
            assert_eq!(titles, expected, "{}", page.dir);
            for other in &PAGES {
                let (_, group) = groups
                    .iter()
                    .find(|(t, _)| *t == other.group.title())
                    .unwrap();
                assert!(
                    group.contains(&format!(r#"href="../{}/""#, other.dir)),
                    "{}: {}",
                    page.dir,
                    other.dir
                );
            }
            // Previous and Next.
            let pager = html
                .split(r#"<nav class="pager""#)
                .nth(1)
                .unwrap_or_default();
            if let Some(before) = i.checked_sub(1).map(|i| &PAGES[i]) {
                assert!(
                    pager.contains(&format!(r#"class="previous" href="../{}/""#, before.dir)),
                    "{}",
                    page.dir
                );
            }
            match PAGES.get(i + 1) {
                Some(after) => assert!(
                    pager.contains(&format!(r#"class="next" href="../{}/""#, after.dir)),
                    "{}",
                    page.dir
                ),
                None => assert!(!pager.contains(r#"class="next""#), "{}", page.dir),
            }
            // On this page: every h2 of the article, by its id, in order.
            let article = html.split(r#"<article class="prose">"#).nth(1).unwrap();
            let article = article.split(r#"<nav class="pager""#).next().unwrap();
            let sections = shell::sections(article);
            assert_eq!(
                h2.find_iter(article).count(),
                sections.len(),
                "{}: an h2 without an id",
                page.dir
            );
            let toc = html
                .split(r#"<aside class="toc""#)
                .nth(1)
                .unwrap_or_default();
            let listed: Vec<&str> = toc_link
                .captures_iter(toc)
                .map(|c| c.get(1).unwrap().as_str())
                .collect();
            let ids: Vec<&str> = sections.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(listed, ids, "{}", page.dir);
            assert!(
                toc.contains(&format!("/edit/main/xtask/src/pages/docs/{}", page.source)),
                "{}",
                page.dir
            );
        }
    }

    /// Search finds every page and every section of each, at its address.
    #[test]
    fn the_search_index_lists_every_page_and_section() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new("docs-search");
        let site = scratch.path();
        write(root, site).unwrap();
        let index: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(site.join("search/docs.json")).unwrap())
                .unwrap();
        let has =
            |kind: &str, href: &str| index.iter().any(|e| e["kind"] == kind && e["href"] == href);
        for page in &PAGES {
            assert!(has("Guide", &format!("{}/", page.dir)), "{}", page.dir);
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            let article = html.split(r#"<article class="prose">"#).nth(1).unwrap();
            let sections = shell::sections(article.split("</article>").next().unwrap());
            for section in sections {
                let href = format!("{}/#{}", page.dir, section.id);
                assert!(has("Section", &href), "{href}");
                // Its words as the page shows them, unescaped.
                let entry = index.iter().find(|e| e["href"] == href).unwrap();
                assert_eq!(entry["title"], section.plain(), "{href}");
            }
        }
        // Every address it gives is a published page.
        for entry in &index {
            let href = entry["href"].as_str().unwrap();
            let path = href.split('#').next().unwrap();
            assert!(site.join(path).join("index.html").is_file(), "{href}");
        }
    }

    /// Every address the docs were published at under /docs/ before they
    /// moved to their own domain. A page keeps its old address's redirect
    /// for as long as it is in `PAGES`.
    const PUBLISHED_UNDER_DOCS: [&str; 9] = [
        "docs/index.html",
        "docs/walkthrough-geomart/index.html",
        "docs/js-library/index.html",
        "docs/paid/index.html",
        "docs/webhooks/index.html",
        "docs/plugins/index.html",
        "docs/apps/index.html",
        "docs/geomart/index.html",
        "docs/geomart/done.html",
    ];

    #[test]
    fn every_old_docs_address_sends_the_reader_to_its_page() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new("docs-redirects");
        let site = scratch.path();
        write(root, site).unwrap();
        let olds: Vec<String> = redirects().into_iter().map(|(old, _)| old).collect();
        assert_eq!(olds, PUBLISHED_UNDER_DOCS);
        for (old, new) in redirects() {
            let html = fs::read_to_string(site.join(&old)).unwrap();
            let to = format!("{}{new}", "../".repeat(old.matches('/').count()));
            assert!(
                html.contains(&format!(
                    r#"<meta http-equiv="refresh" content="0; url={to}">"#
                )),
                "{old}: {html}"
            );
            assert!(
                html.contains(&format!(
                    r#"<link rel="canonical" href="https://docs.monokulo.com/{new}">"#
                )),
                "{old}: {html}"
            );
            assert!(
                html.contains(&format!(
                    r#"<a href="{to}">https://docs.monokulo.com/{new}</a>"#
                )),
                "{old}: {html}"
            );
            // A browser with a script keeps the old link's #section.
            assert!(
                html.contains(&format!(
                    r#"<script>location.replace("{to}" + location.search + location.hash);</script>"#
                )),
                "{old}: {html}"
            );
            // The link, followed from the old address, reaches a published
            // file.
            let mut target = site.join(&old);
            target.pop();
            let target = target.join(&to);
            let file = if new.ends_with('/') {
                target.join("index.html")
            } else {
                target
            };
            assert!(file.is_file(), "{old} -> {}", file.display());
            assert!(
                !fs::read_to_string(&file).unwrap().contains("has moved to"),
                "{old}"
            );
        }
        // A page since the move has no old address, and none is published
        // where the API reference goes.
        let api = PAGES.iter().find(|page| page.dir == "api").unwrap();
        assert_eq!(api.old, None);
        for page in &PAGES {
            assert!(!API_OUTPUT.contains(&page.dir), "{}", page.dir);
        }
    }

    /// The site was at GitHub Pages' own address, then monokulo.com,
    /// which is now a hosted Monokulo: no file in the checkout links either
    /// for the docs, the testing report or the apk repositories (GitHub's
    /// redirect and the pages under /docs/ are for links elsewhere).
    #[test]
    fn nothing_links_the_sites_old_addresses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let listed = std::process::Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(listed.status.success(), "git ls-files");
        // Split, so this file does not name them.
        let github_pages = concat!("oceaneilonwy.github", ".io/monokulo");
        let bare = ["docs", "testing", "openwrt"].map(|dir| format!("//monokulo{}/{dir}", ".com"));
        let mut stale = Vec::new();
        for name in String::from_utf8(listed.stdout).unwrap().split('\0') {
            let Ok(text) = fs::read_to_string(root.join(name)) else {
                continue;
            };
            for (n, line) in text.lines().enumerate() {
                // Unescaped, so a test's regex (`github\.io\/…`) counts too.
                let lower = line.replace('\\', "").to_ascii_lowercase();
                if lower.contains(github_pages) || bare.iter().any(|b| lower.contains(b)) {
                    stale.push(format!("{name}:{}: {line}", n + 1));
                }
            }
        }
        assert!(stale.is_empty(), "{}", stale.join("\n"));
    }
}
