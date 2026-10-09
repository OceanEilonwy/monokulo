//! `cargo xtask pages docs`: the guides for adding Monokulo to a site
//! (GitHub Pages: /docs/), from the Markdown in `docs/site/`, beside the
//! landing page and the quality report. Each page is rendered with maud
//! around pulldown-cmark's HTML, with the same header and a tab per page;
//! a fenced `diff` block becomes the walkthrough's added and kept lines.
//! The sample shop the walkthrough builds, `examples/geomart/`, is copied
//! to `/docs/geomart/` so its pages run there.

use crate::logo;
use crate::support::{at, Exit};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// One page of the docs: its Markdown file in `docs/site/`, the folder it
/// is published as (empty for the front page), and its tab's label.
pub(super) struct Page {
    pub(super) source: &'static str,
    pub(super) dir: &'static str,
    pub(super) tab: &'static str,
}

/// The docs, in the order of their tabs.
pub(super) const PAGES: [Page; 4] = [
    Page {
        source: "index.md",
        dir: "",
        tab: "Key steps",
    },
    Page {
        source: "walkthrough-geomart.md",
        dir: "walkthrough-geomart",
        tab: "Walkthrough: Geomart",
    },
    Page {
        source: "js-library.md",
        dir: "js-library",
        tab: "JS library",
    },
    Page {
        source: "paid.md",
        dir: "paid",
        tab: "When is it paid?",
    },
];

/// The sample shop's files, from `examples/geomart/` to `geomart/`.
const GEOMART: [&str; 2] = ["index.html", "done.html"];
const FONT_WEIGHTS: [u16; 3] = [500, 700, 800];
const REPO_URL: &str = "https://github.com/OceanEilonwy/monokulo";

/// `cargo xtask pages docs --out DIR`.
pub(crate) fn build(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let out = match args {
        ["--out", dir] => PathBuf::from(dir),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pages docs needs --out DIR",
            ))
        }
    };
    write(root, &out)?;
    eprintln!("docs in {}: {} page(s)", out.display(), PAGES.len());
    Ok(Exit::SUCCESS)
}

/// Writes every page, the assets they use and the sample shop into `out`.
pub(super) fn write(root: &Path, out: &Path) -> io::Result<()> {
    for page in &PAGES {
        let source = root.join("docs/site").join(page.source);
        let markdown = fs::read_to_string(&source).map_err(|e| at(&source, e))?;
        let dir = out.join(page.dir);
        fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
        let path = dir.join("index.html");
        fs::write(&path, render(page, &markdown).0).map_err(|e| at(&path, e))?;
    }
    let assets = out.join("assets");
    fs::create_dir_all(&assets).map_err(|e| at(&assets, e))?;
    let mut copies = vec![
        (
            root.join("crates/monokulo/src/views/theme.css"),
            assets.join("theme.css"),
        ),
        (
            root.join("web/pages/docs/docs.css"),
            assets.join("docs.css"),
        ),
    ];
    copies.extend(FONT_WEIGHTS.iter().map(|weight| {
        let name = format!("manrope-{weight}.woff2");
        (
            root.join("crates/monokulo/static").join(&name),
            assets.join(name),
        )
    }));
    let geomart = out.join("geomart");
    fs::create_dir_all(&geomart).map_err(|e| at(&geomart, e))?;
    copies.extend(
        GEOMART
            .iter()
            .map(|name| (root.join("examples/geomart").join(name), geomart.join(name))),
    );
    for (from, to) in copies {
        fs::copy(&from, &to).map_err(|e| at(&from, e))?;
    }
    Ok(())
}

/// How far `page` is from the docs' root, as a relative link prefix.
fn up(page: &Page) -> &'static str {
    if page.dir.is_empty() {
        ""
    } else {
        "../"
    }
}

/// A page: the header, the tabs, and its Markdown.
pub(super) fn render(page: &Page, markdown: &str) -> Markup {
    let up = up(page);
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
                link rel="icon" href=(format!("{up}../assets/favicon.svg")) type="image/svg+xml";
                link rel="stylesheet" href=(format!("{up}assets/theme.css"));
                link rel="stylesheet" href=(format!("{up}assets/docs.css"));
            }
            body {
                header.chrome {
                    div.bar {
                        a.brand href=(format!("{up}../")) {
                            svg.logo-mark width="30" height="30" viewBox="0 0 64 64" aria-hidden="true" focusable="false" {
                                (PreEscaped(logo::small()))
                            }
                            span { "Monokulo" }
                        }
                        nav aria-label="Site" {
                            a href=(format!("{up}../#install")) { "Install" }
                            a href=(format!("{up}./")) aria-current="page" { "Docs" }
                            a href=(format!("{up}../quality/")) { "Quality" }
                            a href=(REPO_URL) { "Source code" }
                        }
                    }
                }
                main.docs-page {
                    nav.docs-tabs aria-label="Docs" {
                        @for other in &PAGES {
                            @let href = if other.dir.is_empty() { format!("{up}./") } else { format!("{up}{}/", other.dir) };
                            @if other.dir == page.dir {
                                a href=(href) aria-current="page" { (other.tab) }
                            } @else {
                                a href=(href) { (other.tab) }
                            }
                        }
                    }
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
                events.push(Event::Html("<div class=\"table-scroll\">".into()));
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
        assert!(html.contains(r#"<a href=".././">Key steps</a>"#), "{html}");
        assert!(html.contains(r#"href="../assets/docs.css""#));
        assert!(
            html.contains(r#"<div class="table-scroll"><table>"#),
            "{html}"
        );
    }

    #[test]
    fn every_page_and_the_sample_shop_are_written() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let out = std::env::temp_dir().join(format!("monokulo-docs-{}", std::process::id()));
        write(root, &out).unwrap();
        for page in &PAGES {
            let html = fs::read_to_string(out.join(page.dir).join("index.html")).unwrap();
            assert!(html.contains("<article>"), "{}", page.source);
        }
        let walkthrough = fs::read_to_string(out.join("walkthrough-geomart/index.html")).unwrap();
        assert!(
            walkthrough.contains(r#"id="step-6""#),
            "the Done page links here"
        );
        assert!(out.join("geomart/index.html").is_file());
        assert!(out.join("geomart/done.html").is_file());
        assert!(out.join("assets/theme.css").is_file());
        let _ = fs::remove_dir_all(&out);
    }
}
