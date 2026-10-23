//! `cargo xtask pages docs`: the guides for adding Monokulo to a site, from
//! the Markdown in `xtask/src/pages/docs/`, beside the landing page and the
//! coverage report. Each page is rendered with maud around pulldown-cmark's
//! HTML, with the same header and a tab per page; a fenced `diff` block
//! becomes the walkthrough's added and kept lines.
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

use super::site_assets;
use super::site_header::{self, Section, GITHUB_URL, SITE_URL};
use crate::support::{at, Exit};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// One page of the docs: its Markdown file in `xtask/src/pages/docs/`, the
/// folder it is published as at the site's top level, its tab's label, the
/// part of the site the header marks, and the folder it was published as
/// before the docs moved to their own domain (`docs/…`), which keeps a
/// redirect to it; none for a page added since.
pub(super) struct Page {
    pub(super) source: &'static str,
    pub(super) dir: &'static str,
    pub(super) tab: &'static str,
    pub(super) section: Section,
    pub(super) old: Option<&'static str>,
}

/// The docs, in the order of their tabs.
pub(super) const PAGES: [Page; 8] = [
    Page {
        source: "index.md",
        dir: "guide",
        tab: "Key steps",
        section: Section::Docs,
        old: Some("docs"),
    },
    Page {
        source: "walkthrough-geomart.md",
        dir: "walkthrough-geomart",
        tab: "Walkthrough: Geomart",
        section: Section::Docs,
        old: Some("docs/walkthrough-geomart"),
    },
    Page {
        source: "js-library.md",
        dir: "js-library",
        tab: "JS library",
        section: Section::Docs,
        old: Some("docs/js-library"),
    },
    Page {
        source: "paid.md",
        dir: "paid",
        tab: "When is it paid?",
        section: Section::Docs,
        old: Some("docs/paid"),
    },
    Page {
        source: "webhooks.md",
        dir: "webhooks",
        tab: "Webhooks",
        section: Section::Docs,
        old: Some("docs/webhooks"),
    },
    Page {
        source: "plugins.md",
        dir: "plugins",
        tab: "Shop plugins",
        section: Section::Docs,
        old: Some("docs/plugins"),
    },
    Page {
        source: "apps.md",
        dir: "apps",
        tab: "Connected apps",
        section: Section::Docs,
        old: Some("docs/apps"),
    },
    Page {
        source: "api.md",
        dir: "api",
        tab: "HTTP API",
        section: Section::Api,
        old: None,
    },
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
    for page in &PAGES {
        let source = root.join("xtask/src/pages/docs").join(page.source);
        let markdown = fs::read_to_string(&source).map_err(|e| at(&source, e))?;
        put(
            site.join(page.dir).join("index.html"),
            render(page, &markdown).0,
        )?;
    }
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

/// A page: the header, the tabs, and its Markdown. Every page is a folder
/// at the site's top level, so the site's root is `../` from each.
pub(super) fn render(page: &Page, markdown: &str) -> Markup {
    let title = markdown
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .unwrap_or(page.tab);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Monokulo docs" }
                meta name="description" content="How to add Monokulo, self-hosted Monero payments, to your own website.";
                (site_assets::head("../"))
            }
            body {
                (site_header::render(page.section, "../", GITHUB_URL))
                main.docs-page {
                    (ui::TabLinks::new("Docs", PAGES.iter().map(|other| ui::TabLink {
                        href: format!("../{}/", other.dir),
                        label: other.tab.into(),
                        current: other.dir == page.dir,
                        count: None,
                    }).collect::<Vec<_>>()))
                    article { (markdown_html(markdown)) }
                }
            }
        }
    }
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
    fn headings_get_anchors_and_diffs_show_what_was_added() {
        let html = markdown_html(
            "### Step 6 · Verify, test, go live\n\n```diff\n# shop.html\n   kept\n+  added <b>\n```\n",
        )
        .into_string();
        assert!(
            html.contains(r#"<h3 id="step-6">Step 6 · Verify, test, go live</h3>"#),
            "{html}"
        );
        assert!(html.contains(r#"<pre class="diff"><span class="file">shop.html</span><span class="ctx">   kept</span><span class="add">+  added &lt;b&gt;</span></pre>"#), "{html}");
    }

    #[test]
    fn a_page_has_every_tab_with_its_own_current_and_links_up_from_its_folder() {
        let page = &PAGES[2];
        let html = render(
            page,
            "# JS library\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n",
        )
        .into_string();
        assert!(html.contains("<title>JS library · Monokulo docs</title>"));
        assert!(
            html.contains(r#"<a href="../js-library/" aria-current="page">JS library</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="../guide/">Key steps</a>"#),
            "{html}"
        );
        assert!(html.contains(r#"<a href="../api/">HTTP API</a>"#), "{html}");
        assert!(html.contains(&format!(
            r#"href="{}""#,
            site_assets::url("../", "styles.css")
        )));
        assert!(html.contains(r#"<div class="tbl"><table>"#), "{html}");
    }

    #[test]
    fn every_page_and_the_sample_shop_are_written() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let scratch = Scratch::new("docs");
        let site = scratch.path();
        write(root, site).unwrap();
        for page in &PAGES {
            let html = fs::read_to_string(site.join(page.dir).join("index.html")).unwrap();
            assert!(html.contains("<article>"), "{}", page.source);
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
            api.contains(r#"<a href="../api/" aria-current="page">API</a>"#),
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
