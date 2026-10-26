//! `cargo xtask pages docs`: the guides for installing Monokulo and adding
//! it to a site, from the Markdown in `xtask/src/pages/docs/`, beside the
//! landing page and the coverage report. Each page is rendered with maud
//! around pulldown-cmark's HTML, or drawn in Rust (`install`: the install,
//! config and backups pages, whose steps use the `ui` package's tabs and
//! copyable commands, and the router's the site's apk repositories), in the
//! shell the docs and the API reference share (`shell`): the
//! map of every page down the left, grouped as `PAGES` groups them, and the
//! page's sections beside it. A fenced `diff` block becomes the
//! walkthrough's added and kept lines.
//!
//! Every page is a folder at the site's top level (docs.monokulo.com):
//! `/introduction/` is the front page, `/guide/`, `/webhooks/` and the rest
//! beside it. The sample shop the walkthrough builds, `examples/geomart/`,
//! is copied to `/geomart/` so its pages run there. The docs were under
//! `/docs/` until the site moved to its own domain; every old address keeps
//! a small page that sends the reader on (`redirects`).
//!
//! `/api/` is the HTTP API's section, written by its own build step into
//! the same site folder (`api_reference`): the explorer at `/api/` (whose
//! introduction is `api.md`, here), the OpenAPI document at
//! `api/v1/openapi.json`, and the link relations and problem types its
//! responses name under `rels/` and `problems/`. The map's Reference group
//! links it. No docs page is published in those folders (`API_OUTPUT` in
//! the tests).
//!
//! Search finds every page and each of its sections, from the index the
//! build writes at `search/docs.json`.
//!
//! The router's page names the apk repositories in the site
//! (`openwrt/<release>/<arch>/`), so the build needs them there: the pages
//! workflow fetches them into the site before it builds any part, and the
//! build publishes their signing key beside them.

use super::install::{self, Site};
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
    RunMonokulo,
    Reference,
}

impl Group {
    const ALL: [Group; 5] = [
        Group::GetStarted,
        Group::TakePayments,
        Group::Integrations,
        Group::RunMonokulo,
        Group::Reference,
    ];

    fn title(self) -> &'static str {
        match self {
            Group::GetStarted => "Get started",
            Group::TakePayments => "Take payments",
            Group::Integrations => "Integrations",
            Group::RunMonokulo => "Run Monokulo",
            Group::Reference => "Reference",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Group::GetStarted => icon::FLAG,
            Group::TakePayments => icon::COIN,
            Group::Integrations => icon::PLUG,
            Group::RunMonokulo => icon::SERVER,
            Group::Reference => icon::BOOK,
        }
    }
}

/// Where a page's words come from.
pub(super) enum Source {
    /// A Markdown file in `xtask/src/pages/docs/`; its `# ` heading is the
    /// page's title.
    Markdown(&'static str),
    /// Drawn in Rust from what the site holds (`install`): its file under
    /// `xtask/src/pages/`, its title, the line under it, and what follows,
    /// each `h2` with an `id` a section.
    Rust {
        file: &'static str,
        title: &'static str,
        lead: &'static str,
        render: fn(&Site) -> Markup,
    },
}

impl Source {
    /// Its file, from the checkout's root: where the page is edited.
    fn file(&self) -> String {
        match self {
            Source::Markdown(file) => format!("xtask/src/pages/docs/{file}"),
            Source::Rust { file, .. } => format!("xtask/src/pages/{file}"),
        }
    }
}

/// One page of the docs: where its words come from, the folder it is
/// published as at the site's top level, its name in the map, the map's
/// group it is in, and the folder it was published as before the docs
/// moved to their own domain (`docs/…`), which keeps a redirect to it; none
/// for a page added since.
pub(super) struct Page {
    pub(super) source: Source,
    pub(super) dir: &'static str,
    pub(super) label: &'static str,
    pub(super) group: Group,
    pub(super) old: Option<&'static str>,
    pub(super) layout: Layout,
}

/// How a page lays out its words.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Layout {
    /// An article at a reading width, "On this page" beside it.
    Guide,
    /// The API explorer's look, for a Markdown page that documents code
    /// (`reference_html`): each section two columns, its words on the left
    /// and its code blocks and tables beside them on the right; a table
    /// headed `Name` is a list of fields. Its sections are under it in the
    /// map, in place of "On this page", an `h2`'s `h3`s under the `h2`.
    Reference,
}

/// The docs, in the map's order, which is also Previous and Next's: each
/// group's pages together, the groups in `Group::ALL`'s order. The first is
/// where the bar's Docs and Get started go.
pub(super) const PAGES: [Page; 12] = [
    Page {
        source: Source::Markdown("introduction.md"),
        dir: "introduction",
        label: "Introduction",
        group: Group::GetStarted,
        old: None,
        layout: Layout::Guide,
    },
    Page {
        source: Source::Rust {
            file: "install/computer.rs",
            title: install::computer::TITLE,
            lead: install::computer::LEAD,
            render: install::computer::render,
        },
        dir: "install-computer",
        label: "Install on a computer",
        group: Group::GetStarted,
        old: None,
        layout: Layout::Guide,
    },
    Page {
        source: Source::Rust {
            file: "install/router.rs",
            title: install::router::TITLE,
            lead: install::router::LEAD,
            render: install::router::render,
        },
        dir: "install-router",
        label: "Install on a router",
        group: Group::GetStarted,
        old: None,
        layout: Layout::Guide,
    },
    Page {
        source: Source::Markdown("index.md"),
        dir: "guide",
        label: "Accept your first payment",
        group: Group::GetStarted,
        old: Some("docs"),
        layout: Layout::Guide,
    },
    Page {
        source: Source::Markdown("walkthrough-geomart.md"),
        dir: "walkthrough-geomart",
        label: "Walkthrough: Geomart",
        group: Group::GetStarted,
        old: Some("docs/walkthrough-geomart"),
        layout: Layout::Guide,
    },
    Page {
        source: Source::Markdown("paid.md"),
        dir: "paid",
        label: "When is it paid?",
        group: Group::TakePayments,
        old: Some("docs/paid"),
        layout: Layout::Guide,
    },
    Page {
        source: Source::Markdown("webhooks.md"),
        dir: "webhooks",
        label: "Webhooks",
        group: Group::TakePayments,
        old: Some("docs/webhooks"),
        layout: Layout::Guide,
    },
    Page {
        source: Source::Markdown("plugins.md"),
        dir: "plugins",
        label: "Shop plugins",
        group: Group::Integrations,
        old: Some("docs/plugins"),
        layout: Layout::Guide,
    },
    Page {
        source: Source::Markdown("apps.md"),
        dir: "apps",
        label: "Connected apps",
        group: Group::Integrations,
        old: Some("docs/apps"),
        layout: Layout::Guide,
    },
    Page {
        source: Source::Markdown("webpages.md"),
        dir: "webpages",
        label: "Webpages",
        group: Group::Integrations,
        old: None,
        layout: Layout::Reference,
    },
    Page {
        source: Source::Rust {
            file: "install/run.rs",
            title: install::run::CONFIG_TITLE,
            lead: install::run::CONFIG_LEAD,
            render: install::run::config,
        },
        dir: "config",
        label: "Config and data",
        group: Group::RunMonokulo,
        old: None,
        layout: Layout::Guide,
    },
    Page {
        source: Source::Rust {
            file: "install/run.rs",
            title: install::run::BACKUPS_TITLE,
            lead: install::run::BACKUPS_LEAD,
            render: install::run::backups,
        },
        dir: "backups",
        label: "Backups and updates",
        group: Group::RunMonokulo,
        old: None,
        layout: Layout::Guide,
    },
];

/// The map's links that aren't docs pages, after their group's pages: the
/// API explorer's, which its build writes (`api_reference`). Each is
/// (group, label, link from the site's root, whether it's a file rather
/// than a page).
const LINKS: [(Group, &str, &str, bool); 3] = [
    (Group::Reference, "API reference", "api/", false),
    (Group::Reference, "Problems", "api/#problems", false),
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

/// `cargo xtask pages docs --site DIR [--site-url URL] [--github-url URL]`.
/// The site's address is where routers fetch the apk repositories from,
/// and the repository's where the install pages download releases from and
/// each page is edited: monokulo's own unless a fork publishes its own.
pub(crate) fn build(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_string());
    let (mut site, mut site_url, mut github_url) =
        (None, SITE_URL.to_string(), GITHUB_URL.to_string());
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| bad(&format!("{flag} needs a value")))?;
        match *flag {
            "--site" => site = Some(PathBuf::from(value)),
            "--site-url" => site_url = value.trim_end_matches('/').to_string(),
            "--github-url" => github_url = value.trim_end_matches('/').to_string(),
            _ => return Err(bad(&format!("unknown option {flag}"))),
        }
    }
    let site = site.ok_or_else(|| bad("pages docs needs --site DIR"))?;
    write(root, &site, &site_url, &github_url)?;
    eprintln!("docs in {}: {} page(s)", site.display(), PAGES.len());
    Ok(Exit::SUCCESS)
}

/// Writes every page, the sample shop and the old addresses' redirects into
/// the site's folder `site`, and the assets they use into its `assets/`;
/// the router's page tells routers to fetch from `site_url`, and the pages
/// link releases and their sources in the repository at `github_url`.
pub(super) fn write(root: &Path, site: &Path, site_url: &str, github_url: &str) -> io::Result<()> {
    let put = |path: PathBuf, contents: String| {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
        }
        fs::write(&path, contents).map_err(|e| at(&path, e))
    };
    let held = Site::load(root, site, site_url, github_url)?;
    let mut found = Vec::new();
    for page in &PAGES {
        let (title, words) = match &page.source {
            Source::Markdown(file) => {
                let source = root.join("xtask/src/pages/docs").join(file);
                let markdown = fs::read_to_string(&source).map_err(|e| at(&source, e))?;
                let markdown = site_assets::linked(&markdown, "../");
                let words = match page.layout {
                    Layout::Guide => markdown_html(&markdown),
                    Layout::Reference => reference_html(&markdown, crumbs(page)),
                };
                (markdown_title(page, &markdown).to_owned(), words)
            }
            Source::Rust {
                title,
                lead,
                render,
                ..
            } => (
                (*title).to_owned(),
                html! { h1 { (title) } p { (lead) } (render(&held)) },
            ),
        };
        put(
            site.join(page.dir).join("index.html"),
            render(page, &title, &words, &held.github_url).0,
        )?;
        found.extend(index(page, &title, &words));
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
pub(super) fn redirect(old: &str, new: &str) -> Markup {
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

/// The docs' map, from a page `up` from the site's root, `current` marked
/// with `sections` (its own, a reference page's) under it.
pub(super) fn map(up: &str, current: Option<&str>, mut sections: Vec<Link>) -> Vec<MapGroup> {
    Group::ALL
        .into_iter()
        .map(|group| {
            let pages = PAGES.iter().filter(|p| p.group == group).map(|p| {
                let here = current == Some(p.dir);
                Link {
                    label: html! { (p.label) },
                    href: format!("{up}{}/", p.dir),
                    current: Mark::page_if(here),
                    file: false,
                    nested: false,
                    children: if here {
                        std::mem::take(&mut sections)
                    } else {
                        Vec::new()
                    },
                }
            });
            let links = LINKS
                .iter()
                .filter(|(g, ..)| *g == group)
                .map(|(_, label, href, file)| Link {
                    label: html! { (label) },
                    href: format!("{up}{href}"),
                    current: None,
                    file: *file,
                    nested: false,
                    children: Vec::new(),
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

/// A Markdown page's title: its `# ` heading, or its name in the map.
fn markdown_title<'a>(page: &'a Page, markdown: &'a str) -> &'a str {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .unwrap_or(page.label)
}

/// A page called `title`, its words `words` (its `h1` and what follows), in
/// the shell, with Previous and Next in the map's order. Every page is a
/// folder at the site's top level, so the site's root is `../` from each.
/// Its source is edited in the repository at `github_url`.
pub(super) fn render(page: &Page, title: &str, words: &Markup, github_url: &str) -> Markup {
    let up = "../";
    let at = PAGES.iter().position(|p| p.dir == page.dir);
    let neighbour = |i: Option<usize>| {
        i.and_then(|i| PAGES.get(i))
            .map(|p| (p.label.to_owned(), format!("{up}{}/", p.dir)))
    };
    // A reference page draws its crumbs in its first section.
    let (article, sections, layout) = match page.layout {
        Layout::Guide => (
            html! { (crumbs(page)) (words) },
            Vec::new(),
            shell::Layout::Article,
        ),
        Layout::Reference => (words.clone(), outline(&words.0), shell::Layout::Wide),
    };
    shell::render(shell::Page {
        up,
        section: Section::Docs,
        title: format!("{title} · Monokulo docs"),
        description:
            "How to install Monokulo, self-hosted Monero payments, and add it to your own website.",
        map_label: "Docs",
        map: map(up, Some(page.dir), sections),
        article,
        previous: neighbour(at.and_then(|i| i.checked_sub(1))),
        next: neighbour(at.map(|i| i + 1)),
        edit: Some(format!("{github_url}/edit/main/{}", page.source.file())),
        layout,
        script: None,
    })
}

/// Where a page is: the docs, then its group.
fn crumbs(page: &Page) -> Markup {
    html! { p.crumbs { span { "Docs" } span { (page.group.title()) } } }
}

/// A reference page's sections as the map lists them under it: each `h2`,
/// the `h3`s after it under it.
fn outline(html: &str) -> Vec<Link> {
    static HEADING: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let heading = HEADING.get_or_init(|| {
        regex::Regex::new(r#"(?s)<h([23]) id="([^"]+)"[^>]*>(.*?)</h[23]>"#).unwrap()
    });
    let mut sections: Vec<Link> = Vec::new();
    for c in heading.captures_iter(html) {
        let link = Link {
            label: PreEscaped(strip_tags(&c[3])),
            href: format!("#{}", &c[2]),
            current: None,
            file: false,
            nested: true,
            children: Vec::new(),
        };
        match (&c[1], sections.last_mut()) {
            ("3", Some(parent)) => parent.children.push(link),
            _ => sections.push(link),
        }
    }
    sections
}

/// `html` without its tags: a heading's words, still escaped.
fn strip_tags(html: &str) -> String {
    static TAG: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let tag = TAG.get_or_init(|| regex::Regex::new(r"<[^>]*>").unwrap());
    tag.replace_all(html, "").trim().to_owned()
}

/// What search finds on a page: the page, and each of its sections.
fn index(page: &Page, title: &str, words: &Markup) -> Vec<shell::Entry> {
    let guide = shell::Entry {
        title: page.label.to_owned(),
        sub: format!("{} · {title}", page.group.title()),
        kind: "Guide",
        href: format!("{}/", page.dir),
        method: None,
    };
    let sections = shell::sections(&words.0)
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

/// A heading's words, read from `events` up to its end, as HTML with its
/// anchor.
fn heading_html<'a>(level: HeadingLevel, events: &mut impl Iterator<Item = Event<'a>>) -> String {
    let mut inner = Vec::new();
    let mut text = String::new();
    for event in events {
        match event {
            Event::End(TagEnd::Heading(_)) => break,
            Event::Text(t) => {
                text.push_str(&t);
                inner.push(Event::Text(t));
            }
            Event::Code(t) => {
                text.push_str(&t);
                inner.push(Event::Code(t));
            }
            other => inner.push(other),
        }
    }
    let mut html_inner = String::new();
    pulldown_cmark::html::push_html(&mut html_inner, inner.into_iter());
    let tag = level.to_string();
    format!(
        "<{tag} id=\"{}\">{}</{tag}>",
        anchor(&text),
        html_inner.trim()
    )
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
                events.push(Event::Html(heading_html(level, &mut parser).into()));
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

/// A reference page's Markdown, laid out as the API explorer lays out its
/// own: what comes before the first `h2` (the title and the line under it,
/// after `crumbs`) across the width, then a section per `h2` and per `h3`,
/// its words on the left and its code blocks and tables in panels beside
/// them. An `h2` with `h3`s after it heads them, as a resource heads its
/// operations. A code block's info string after its language is its
/// panel's title (```` ```js Example ````). A table headed `Name` (`Name`,
/// `Type`, `Default`, `Description`, `Note`) is a list of fields in the
/// words: its `Default` says `required`, `optional` or the value.
pub(super) fn reference_html(markdown: &str, crumbs: Markup) -> Markup {
    struct Section<'a> {
        level: u8,
        heading: String,
        words: Vec<Event<'a>>,
        panels: Vec<Markup>,
    }
    let mut sections = vec![Section {
        level: 1,
        heading: String::new(),
        words: Vec::new(),
        panels: Vec::new(),
    }];
    let mut parser = Parser::new_ext(markdown, Options::ENABLE_TABLES);
    while let Some(event) = parser.next() {
        let here = sections.last_mut().unwrap();
        match event {
            Event::Start(Tag::Heading { level, .. })
                if matches!(level, HeadingLevel::H2 | HeadingLevel::H3) =>
            {
                sections.push(Section {
                    level: if level == HeadingLevel::H2 { 2 } else { 3 },
                    heading: heading_html(level, &mut parser),
                    words: Vec::new(),
                    panels: Vec::new(),
                });
            }
            Event::Start(Tag::Heading { level, .. }) if level != HeadingLevel::H1 => {
                here.words
                    .push(Event::Html(heading_html(level, &mut parser).into()));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let mut code = String::new();
                for event in parser.by_ref() {
                    match event {
                        Event::End(TagEnd::CodeBlock) => break,
                        Event::Text(t) => code.push_str(&t),
                        _ => {}
                    }
                }
                let (lang, title) = info.split_once(' ').unwrap_or((&info, ""));
                let title = if title.trim().is_empty() {
                    "Example"
                } else {
                    title.trim()
                };
                here.panels.push(html! {
                    div.api-panel {
                        div.api-panel-head { span.api-panel-title { (title) } }
                        pre { code class=[(!lang.is_empty()).then(|| format!("language-{lang}"))] { (code) } }
                    }
                });
            }
            Event::Start(Tag::Table(_)) => {
                let mut table = vec![event];
                for event in parser.by_ref() {
                    let end = matches!(event, Event::End(TagEnd::Table));
                    table.push(event);
                    if end {
                        break;
                    }
                }
                match fields(&table) {
                    Some(list) => here.words.push(Event::Html(list.0.into())),
                    None => {
                        let mut out = String::new();
                        pulldown_cmark::html::push_html(&mut out, table.into_iter());
                        here.panels.push(html! {
                            div.api-panel { div.tbl { (PreEscaped(out)) } }
                        });
                    }
                }
            }
            other => here.words.push(other),
        }
    }
    let heads =
        |i: usize| sections[i].level == 2 && sections.get(i + 1).is_some_and(|s| s.level == 3);
    let mut classes = Vec::new();
    for (i, section) in sections.iter().enumerate() {
        let mut class = vec!["api-sec"];
        if section.panels.is_empty() {
            class.push("api-wide");
        }
        match section.level {
            1 => class.push("api-intro"),
            2 if heads(i) => class.push("api-resource"),
            3 => class.push("api-op"),
            _ => {}
        }
        classes.push(class.join(" "));
    }
    html! {
        article.reference {
            @for (section, class) in sections.into_iter().zip(classes) {
                @let words = {
                    let mut out = String::new();
                    pulldown_cmark::html::push_html(&mut out, section.words.into_iter());
                    out
                };
                section class=(class) {
                    div.api-main.prose {
                        @if section.level == 1 { (crumbs) }
                        (PreEscaped(&section.heading))
                        (PreEscaped(words))
                    }
                    @if !section.panels.is_empty() {
                        div.api-side {
                            @for panel in &section.panels { (panel) }
                        }
                    }
                }
            }
        }
    }
}

/// A table headed `Name`, as a list of fields (`reference_html`); none for
/// any other table.
fn fields(table: &[Event]) -> Option<Markup> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cell: Option<Vec<Event>> = None;
    for event in table {
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(Vec::new()),
            Event::Start(Tag::TableCell) => cell = Some(Vec::new()),
            Event::End(TagEnd::TableCell) => {
                let mut out = String::new();
                pulldown_cmark::html::push_html(&mut out, cell.take()?.into_iter());
                rows.last_mut()?.push(out.trim().to_owned());
            }
            event => {
                if let Some(cell) = &mut cell {
                    cell.push(event.clone());
                }
            }
        }
    }
    let (head, rows) = rows.split_first()?;
    let column = |name: &str| head.iter().position(|h| strip_tags(h) == name);
    if column("Name")? != 0 {
        return None;
    }
    let (kind, default, description, note) = (
        column("Type")?,
        column("Default")?,
        column("Description")?,
        column("Note"),
    );
    let at = |row: &Vec<String>, i: usize| row.get(i).cloned().unwrap_or_default();
    Some(html! {
        ul.api-fields {
            @for row in rows {
                @let presence = at(row, default);
                @let note = note.map(|i| at(row, i)).filter(|n| !n.is_empty());
                li {
                    p.api-field-head {
                        (PreEscaped(at(row, 0)))
                        span.api-field-type { (PreEscaped(at(row, kind))) }
                        @match strip_tags(&presence).as_str() {
                            "required" => { span.api-field-required { "required" } }
                            "" | "optional" => { span.api-field-type { "optional" } }
                            _ => { span.api-field-type { "default " (PreEscaped(&presence)) } }
                        }
                    }
                    div.api-field-text { (PreEscaped(at(row, description))) }
                    @if let Some(note) = note {
                        p.api-field-note { (PreEscaped(note)) }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    /// The folders the API's own build writes (`api_reference`).
    const API_OUTPUT: [&str; 3] = ["api", "rels", "problems"];

    /// The docs, written into a site that holds an apk repository, as the
    /// pages workflow's does.
    fn built(name: &str) -> Scratch {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new(name);
        install::tests::repo(
            scratch.path(),
            "25.12",
            "aarch64_cortex-a53",
            "monokulo-0.1.0-r1.apk",
        );
        write(root, scratch.path(), SITE_URL, GITHUB_URL).unwrap();
        scratch
    }

    #[test]
    fn every_page_and_the_sample_shop_are_written() {
        let scratch = built("docs");
        let site = scratch.path();
        for page in &PAGES {
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            assert!(html.contains(article_tag(page)), "{}", page.dir);
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
        // Each plugin's shop shows its mark, at the name it's published at.
        let linked = site_assets::assert_links_published(&plugins, &site.join("assets"));
        for mark in [
            "plugin-woocommerce.svg",
            "plugin-prestashop.svg",
            "plugin-opencart.png",
            "plugin-whmcs.png",
        ] {
            assert!(linked.contains(&mark), "{mark}");
        }
        for dir in ["plugins", "docs/plugins"] {
            let vectors = fs::read_to_string(site.join(dir).join(VECTORS)).unwrap();
            assert!(
                serde_json::from_str::<serde_json::Value>(&vectors).unwrap()["deliveries"]
                    .is_array()
            );
        }
        // The docs write nothing where the API's own build writes (the
        // explorer at api/, which the map links).
        for dir in API_OUTPUT {
            assert!(!site.join(dir).exists(), "{dir}");
        }
        // Every page links its files from the site's assets/.
        for page in &PAGES {
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            let linked = site_assets::assert_links_published(&html, &site.join("assets"));
            assert!(linked.contains(&"styles.css"), "{}", page.dir);
            assert!(linked.contains(&"site.js"), "{}", page.dir);
        }
    }

    /// Where a page's words start: a guide's article, or a reference
    /// page's sections.
    fn article_tag(page: &Page) -> &'static str {
        match page.layout {
            Layout::Guide => r#"<article class="prose">"#,
            Layout::Reference => r#"<article class="reference">"#,
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
    /// are every `h2` it has, or on a reference page the map lists them
    /// under it, folded, each `h2`'s `h3`s under it.
    #[test]
    fn every_page_is_in_the_map_and_lists_its_sections() {
        let scratch = built("docs-shell");
        let site = scratch.path();
        let h2 = regex::Regex::new(r"<h2[ >]").unwrap();
        let toc_link = regex::Regex::new(r##"<li><a href="#([^"]+)">"##).unwrap();
        let toc_section = regex::Regex::new(r##"<a href="#([^"]+)""##).unwrap();
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
            // On this page (a reference page's map): every h2 of the
            // article, by its id, in order.
            let article = html.split(article_tag(page)).nth(1).unwrap();
            let article = article.split(r#"<nav class="pager""#).next().unwrap();
            let sections = shell::sections(article);
            assert_eq!(
                h2.find_iter(article).count(),
                sections.len(),
                "{}: an h2 without an id",
                page.dir
            );
            let toc = match page.layout {
                Layout::Guide => html.split(r#"<aside class="toc""#).nth(1),
                Layout::Reference => html.split(r#"<div class="wide-foot">"#).nth(1),
            }
            .unwrap_or_default();
            let listed: Vec<&str> = match page.layout {
                Layout::Guide => toc_link
                    .captures_iter(toc)
                    .map(|c| c.get(1).unwrap().as_str())
                    .collect(),
                Layout::Reference => {
                    // The page folds its sections under it, the only
                    // links in the map to a part of a page.
                    let map = html.split(r#"<nav class="map""#).nth(1).unwrap();
                    let map = map.split(r#"<div class="map-foot">"#).next().unwrap();
                    let here =
                        format!(r#"<summary><a href="../{}/" aria-current="page""#, page.dir);
                    let under = map.split(&here).nth(1).unwrap();
                    toc_section
                        .captures_iter(under)
                        .map(|c| c.get(1).unwrap().as_str())
                        .filter(|id| sections.iter().any(|s| s.id == *id))
                        .collect()
                }
            };
            let ids: Vec<&str> = sections.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(listed, ids, "{}", page.dir);
            let edit = format!("/edit/main/{}", page.source.file());
            assert!(toc.contains(&edit), "{}", page.dir);
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
            assert!(root.join(page.source.file()).is_file(), "{edit}");
        }
    }

    /// The Webpages page is a reference page: its sections in the
    /// explorer's two columns, code and tables beside the words, its
    /// parameters as fields, and its sections folded under it in the map
    /// (createOrder and mount under JS reference) with no "On this page".
    /// Other pages' maps fold nothing.
    #[test]
    fn the_webpages_page_lays_out_its_reference_as_the_api_explorer_does() {
        let scratch = built("docs-reference");
        let site = scratch.path();
        let html = fs::read_to_string(site.join("webpages/index.html")).unwrap();
        let article = html.split(r#"<article class="reference">"#).nth(1).unwrap();
        let article = article.split("</article>").next().unwrap();

        // Its sections: the title across the width, then each two columns
        // where it has panels; JS reference heads its functions.
        let section = regex::Regex::new(r#"<section class="([^"]+)">"#).unwrap();
        let classes: Vec<&str> = section
            .captures_iter(article)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(
            classes,
            [
                "api-sec api-wide api-intro",
                "api-sec",
                "api-sec",
                "api-sec",
                "api-sec",
                "api-sec api-wide api-resource",
                "api-sec api-op",
                "api-sec api-op",
            ]
        );
        assert!(article.contains(
            r#"<p class="crumbs"><span>Docs</span><span>Integrations</span></p><h1>Webpages</h1>"#
        ));
        // A code block is a panel named by its info string, beside the
        // words; the statuses table too.
        assert!(article.contains(r#"<span class="api-panel-title">Signature</span>"#));
        assert!(article
            .contains(r#"<code class="language-js">const order = await Monokulo.createOrder({"#));
        let statuses = article.split(r#"id="order-statuses""#).nth(1).unwrap();
        let statuses = statuses.split("<section").next().unwrap();
        let side = statuses.split(r#"<div class="api-side">"#).nth(1).unwrap();
        assert!(side.contains("<td><code>cancelled</code></td>"), "{side}");
        // Parameters are fields: required, optional or their default, a
        // note under the description.
        let create = article.split(r#"id="createorder""#).nth(1).unwrap();
        let create = create.split("<section").next().unwrap();
        let main = create.split(r#"<div class="api-side">"#).next().unwrap();
        assert!(!main.contains("<table"), "{main}");
        assert!(main.contains(r#"<code>publicKey</code><span class="api-field-type">string</span><span class="api-field-required">required</span>"#), "{main}");
        assert!(main.contains(r#"<span class="api-field-type">optional</span>"#));
        assert!(main.contains(r#"<span class="api-field-type">default <code>true</code></span>"#));
        assert!(main.contains(
            r#"<p class="api-field-note">Public: anyone with the payment link can read it.</p>"#
        ));

        // The map folds its sections under it, and JS reference's under
        // that; nothing says "On this page".
        let map = html.split(r#"<nav class="map""#).nth(1).unwrap();
        let map = map.split("</nav>").next().unwrap();
        assert!(map.contains(r##"<li><details open><summary><a href="../webpages/" aria-current="page">Webpages</a></summary><ul><li><a href="#quick-start" data-nested>Quick start</a></li>"##), "{map}");
        assert!(map.contains(r##"<li><details open><summary><a href="#js-reference" data-nested>JS reference</a></summary><ul><li><a href="#createorder" data-nested>createOrder</a></li><li><a href="#mount" data-nested>mount</a></li></ul></details></li>"##), "{map}");
        assert!(!html.contains(r#"<aside class="toc""#));

        let webhooks = fs::read_to_string(site.join("webhooks/index.html")).unwrap();
        let map = webhooks.split(r#"<nav class="map""#).nth(1).unwrap();
        assert!(!map.split("</nav>").next().unwrap().contains("<details"));
    }

    /// Search finds every page and every section of each, at its address.
    #[test]
    fn the_search_index_lists_every_page_and_section() {
        let scratch = built("docs-search");
        let site = scratch.path();
        let index: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(site.join("search/docs.json")).unwrap())
                .unwrap();
        let has =
            |kind: &str, href: &str| index.iter().any(|e| e["kind"] == kind && e["href"] == href);
        for page in &PAGES {
            assert!(has("Guide", &format!("{}/", page.dir)), "{}", page.dir);
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            let article = html.split(article_tag(page)).nth(1).unwrap();
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

    /// The router's page installs from the newest release's repository,
    /// offers every repository in its picker (the newest first), writes
    /// each version's steps and package files, and links files the site
    /// holds; a fork's address is the one routers are told to fetch from,
    /// and its repository the one releases and edits go to.
    #[test]
    fn the_router_page_installs_from_the_newest_release_and_lists_every_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new("docs-router");
        let site = scratch.path();
        install::tests::repo(
            site,
            "25.12",
            "aarch64_cortex-a53",
            "monokulo-0.1.0-r42.apk",
        );
        install::tests::repo(site, "26.06", "x86_64", "monokulo-0.2.0-r50.apk");
        install::tests::repo(
            site,
            "26.06",
            "aarch64_cortex-a53",
            "monokulo-0.2.0-r50.apk",
        );
        write(
            root,
            site,
            "https://fork.example/",
            "https://github.com/Fork/monokulo/",
        )
        .unwrap();
        let page = fs::read_to_string(site.join("install-router/index.html")).unwrap();

        // The picker: every repository, the newest release first, so the
        // newest is what a script shows first.
        let option = regex::Regex::new(r#"<option value="([^"]+)""#).unwrap();
        let options: Vec<&str> = option
            .captures_iter(&page)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(
            options,
            [
                "26.06-aarch64_cortex-a53",
                "26.06-x86_64",
                "25.12-aarch64_cortex-a53"
            ]
        );
        // Each version's blocks: the repository line in LuCI's steps, the
        // terminal's, and its package files, each the fork's address.
        let target = regex::Regex::new(r#"data-target="([^"]+)""#).unwrap();
        for id in &options {
            let blocks = target.captures_iter(&page).filter(|c| &c[1] == *id).count();
            assert_eq!(blocks, 3, "{id}");
        }
        let line = regex::Regex::new(r"https://fork\.example/(openwrt/[^/]+/[^/]+)/packages\.adb")
            .unwrap();
        let first = line.captures(&page).unwrap();
        assert_eq!(&first[1], "openwrt/26.06/aarch64_cortex-a53");
        assert!(!page.contains("https://docs.monokulo.com/openwrt"));
        // The needs name every release and CPU.
        let needs = page.split(r#"<div class="req">"#).nth(1).unwrap();
        let needs = needs.split("</div>").next().unwrap();
        assert!(needs.contains("OpenWrt 26.06 / 25.12 with LuCI"), "{needs}");
        assert!(needs.contains("aarch64_cortex-a53, x86_64"), "{needs}");
        // The key to paste, whole, with a Copy button named for it, and the
        // same key published where the page says.
        let key = fs::read_to_string(root.join("openwrt/keys/monokulo.pem")).unwrap();
        assert!(page.contains(&format!("<code>{}</code></pre>", key.trim_end())));
        assert!(page.contains("&quot;copy_label&quot;:&quot;Copy the signing key&quot;}"));
        assert_eq!(
            fs::read_to_string(site.join("openwrt/monokulo.pem")).unwrap(),
            key
        );
        // Every package file it links is in the site.
        let href = regex::Regex::new(r#"href="\.\./(openwrt/[^"]+)""#).unwrap();
        let files: Vec<&str> = href
            .captures_iter(&page)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(files.len(), 3 * 2 + 1, "{files:?}");
        for file in files {
            assert!(site.join(file).is_file(), "{file}");
        }
        // The fork's own releases to download, and its sources to edit.
        let computer = fs::read_to_string(site.join("install-computer/index.html")).unwrap();
        let github = regex::Regex::new(r#"href="(https://github\.com/[^"]+)""#).unwrap();
        let releases: Vec<&str> = github
            .captures_iter(&computer)
            .map(|c| c.get(1).unwrap().as_str())
            .filter(|href| href.contains("/releases/"))
            .collect();
        assert_eq!(
            releases,
            [
                "https://github.com/Fork/monokulo/releases/latest",
                "https://github.com/Fork/monokulo/releases/tag/latest-main"
            ]
        );
        let edit = github
            .captures_iter(&page)
            .map(|c| c.get(1).unwrap().as_str().to_owned())
            .find(|href| href.contains("/edit/"))
            .unwrap();
        assert_eq!(
            edit,
            "https://github.com/Fork/monokulo/edit/main/xtask/src/pages/install/router.rs"
        );
    }

    /// A site without an apk repository has nothing for the router's page
    /// to install from: the build stops.
    #[test]
    fn the_docs_need_an_apk_repository_in_the_site() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new("docs-no-repo");
        fs::create_dir_all(scratch.join("openwrt")).unwrap();
        let error = write(root, scratch.path(), SITE_URL, GITHUB_URL).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!scratch.join("install-router").exists());
    }

    /// Every address the docs were published at under /docs/ before they
    /// moved to their own domain, that still redirect: a page keeps its old
    /// address's redirect for as long as it is in `PAGES` under the same
    /// folder (the JS library's, now Webpages, was dropped with it).
    const PUBLISHED_UNDER_DOCS: [&str; 8] = [
        "docs/index.html",
        "docs/walkthrough-geomart/index.html",
        "docs/paid/index.html",
        "docs/webhooks/index.html",
        "docs/plugins/index.html",
        "docs/apps/index.html",
        "docs/geomart/index.html",
        "docs/geomart/done.html",
    ];

    #[test]
    fn every_old_docs_address_sends_the_reader_to_its_page() {
        let scratch = built("docs-redirects");
        let site = scratch.path();
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
        // No page is published where the API's build writes.
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
