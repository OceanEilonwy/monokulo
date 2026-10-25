//! The layout the docs and the API reference share. Under the site's bar: a
//! map of the section down the left (its pages in groups, each under a
//! heading with an icon), the page at a reading width, and beside it "On
//! this page", the page's sections (its `h2`s), with a link to edit the
//! page. Previous and Next end it. On a narrow screen the map is a menu
//! behind the bar's Menu button, the site's own links at its top: a
//! checkbox opens it, so it works without a script.
//!
//! Every page reads fully without a script. The site's script
//! (`assets/site/site.js`) marks the section being read in "On this page",
//! closes the map's menu on Escape, and adds search, from the index each
//! part's build writes beside its pages ([`write_index`]).

use super::site_assets;
use super::site_header::{self, Section, GITHUB_URL};
use crate::support::at;
use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde::Serialize;
use std::{fs, io, path::Path, sync::OnceLock};

/// The checkbox that opens the map on a narrow screen; the bar's Menu
/// button is its label.
const MAP_TOGGLE: &str = "map-toggle";

/// The icons beside the map's group headings: drawn on a 20 by 20 grid, in
/// the line of the `ui` package's.
pub(super) mod icon {
    /// A flag: Get started.
    pub(in crate::pages) const FLAG: &str = r#"<path d="M5 3v14M5 4h9l-2 3.5 2 3.5H5" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"/>"#;
    /// A coin with a plus: Take payments.
    pub(in crate::pages) const COIN: &str = r#"<circle cx="10" cy="10" r="6.5" fill="none" stroke="currentColor" stroke-width="1.7"/><path d="M7.5 10h5M10 7.5v5" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>"#;
    /// A plug: Integrations.
    pub(in crate::pages) const PLUG: &str = r#"<path d="M8 3v4M12 3v4M6 7h8v3a4 4 0 01-8 0zM10 14v3" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>"#;
    /// A book: Reference, and the API's Overview.
    pub(in crate::pages) const BOOK: &str = r#"<path d="M5 3.5h8.5a1.5 1.5 0 011.5 1.5v11.5H6.5A1.5 1.5 0 015 15zM5 15a1.5 1.5 0 011.5-1.5H15" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"/>"#;
    /// Braces: the API's resources.
    pub(in crate::pages) const BRACES: &str = r#"<path d="M7.5 4C5.5 4 5.5 5 5.5 7S4 9.5 4 10s1.5 1 1.5 3 0 3 2 3M12.5 4c2 0 2 1 2 3S16 9.5 16 10s-1.5 1-1.5 3 0 3-2 3" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>"#;
    /// A warning triangle: the API's errors.
    pub(in crate::pages) const ALERT: &str = r#"<path d="M10 3.5l7 12.5H3z" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"/><path d="M10 8.5v3.5" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/><circle cx="10" cy="14" r="1" fill="currentColor"/>"#;
}

/// A group of the map: its heading, its icon (one of [`icon`]) and its
/// links.
pub(super) struct Group {
    pub(super) title: String,
    pub(super) icon: &'static str,
    pub(super) links: Vec<Link>,
}

/// A link in the map.
pub(super) struct Link {
    pub(super) label: Markup,
    pub(super) href: String,
    /// Whether it's the page it's on, or where that page is.
    pub(super) current: Option<Mark>,
    /// A file rather than a page of the site (the OpenAPI document).
    pub(super) file: bool,
}

/// What a link in the map is to the page it's on, as `aria-current` says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mark {
    /// The page itself.
    Page,
    /// Where the page is: a problem's page is under Problems, which links
    /// the table of them.
    Location,
}

impl Mark {
    /// `current` as the map marks it: the page, or none.
    pub(super) fn page_if(current: bool) -> Option<Mark> {
        current.then_some(Mark::Page)
    }

    fn attribute(self) -> &'static str {
        match self {
            Mark::Page => "page",
            Mark::Location => "location",
        }
    }
}

/// A page of the docs or the API reference, `up` from the site's root.
pub(super) struct Page<'a> {
    pub(super) up: &'a str,
    /// The part of the site the header marks.
    pub(super) section: Section,
    /// The `<title>`, in full ("Webhooks · Monokulo docs").
    pub(super) title: String,
    pub(super) description: &'a str,
    /// What the map is of ("Docs", "API reference"), its accessible name.
    pub(super) map_label: &'a str,
    pub(super) map: Vec<Group>,
    /// The page: its `h1` and everything under it. Each `h2` with an `id`
    /// is a section in "On this page".
    pub(super) article: Markup,
    /// The pages before and after it, as (label, link).
    pub(super) previous: Option<(String, String)>,
    pub(super) next: Option<(String, String)>,
    /// Where its source is edited on GitHub.
    pub(super) edit: Option<String>,
}

/// A section of a page: an `h2`'s id and its words.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Heading {
    pub(super) id: String,
    pub(super) text: String,
}

impl Heading {
    /// Its words as text, unescaped.
    pub(super) fn plain(&self) -> String {
        self.text
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }
}

/// The sections of a page's HTML: each `h2` with an `id`, in order, its
/// words without their markup (still escaped, as they were in the page).
pub(super) fn sections(html: &str) -> Vec<Heading> {
    static H2: OnceLock<regex::Regex> = OnceLock::new();
    static TAG: OnceLock<regex::Regex> = OnceLock::new();
    let h2 =
        H2.get_or_init(|| regex::Regex::new(r#"(?s)<h2 id="([^"]+)"[^>]*>(.*?)</h2>"#).unwrap());
    let tag = TAG.get_or_init(|| regex::Regex::new(r"<[^>]*>").unwrap());
    h2.captures_iter(html)
        .map(|c| Heading {
            id: c[1].to_owned(),
            text: tag.replace_all(&c[2], "").trim().to_owned(),
        })
        .collect()
}

/// The page, in the shell.
pub(super) fn render(page: Page) -> Markup {
    let up = page.up;
    let article = page.article.into_string();
    let toc = sections(&article);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (page.title) }
                meta name="description" content=(page.description);
                (site_assets::head(up))
            }
            body.shell-page {
                (site_header::with_map(page.section, up, GITHUB_URL, MAP_TOGGLE))
                div.shell {
                    nav.map id="map" aria-label=(page.map_label) {
                        (site_links(page.section, up))
                        @for group in &page.map {
                            div.map-group {
                                p.map-heading {
                                    svg viewBox="0 0 20 20" aria-hidden="true" focusable="false" { (PreEscaped(group.icon)) }
                                    (group.title)
                                }
                                ul {
                                    @for link in &group.links {
                                        li {
                                            a href=(link.href) aria-current=[link.current.map(Mark::attribute)] data-file[link.file] { (link.label) }
                                        }
                                    }
                                }
                            }
                        }
                        div.map-foot {
                            a href=(GITHUB_URL) { (PreEscaped(site_header::GITHUB_MARK)) "Source on GitHub" }
                            a href=(format!("{GITHUB_URL}/releases")) { "Releases" }
                            a href=(format!("{GITHUB_URL}/issues")) { "Report an issue" }
                        }
                    }
                    label.map-scrim for=(MAP_TOGGLE) aria-hidden="true" {}
                    main.shell-main {
                        article.prose {
                            (PreEscaped(&article))
                            @if page.previous.is_some() || page.next.is_some() {
                                nav.pager aria-label="Pages" {
                                    @if let Some((label, href)) = &page.previous {
                                        a.previous href=(href) { small { "Previous" } (label) }
                                    }
                                    @if let Some((label, href)) = &page.next {
                                        a.next href=(href) { small { "Next" } (label) }
                                    }
                                }
                            }
                        }
                        @if !toc.is_empty() || page.edit.is_some() {
                            aside.toc aria-label="On this page" {
                                @if !toc.is_empty() {
                                    p.toc-heading { "On this page" }
                                    ul {
                                        @for section in &toc {
                                            li { a href=(format!("#{}", section.id)) { (PreEscaped(&section.text)) } }
                                        }
                                    }
                                }
                                @if let Some(edit) = &page.edit {
                                    a.edit href=(edit) { "Edit this page on GitHub" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The site's links at the map's top, which a narrow screen shows in place
/// of the bar's.
fn site_links(current: Section, up: &str) -> Markup {
    html! {
        div.map-site {
            @for (section, href, label) in site_header::links(up) {
                a href=(href) aria-current=[(section == current).then_some("page")] { (label) }
            }
            @for link in site_header::resources(up, GITHUB_URL) {
                a href=(link.href) { (link.title) }
            }
        }
    }
}

/// One thing search finds: a page, a section of one, a resource, an
/// operation or a problem.
#[derive(Serialize, Debug)]
pub(super) struct Entry {
    /// What it's called.
    pub(super) title: String,
    /// Where it is or what it is, in a line.
    pub(super) sub: String,
    /// What kind of thing it is: "Guide", "Section", "Resource",
    /// "Endpoint", "Problem".
    pub(super) kind: &'static str,
    /// Its link, from the site's root (`webhooks/#add-one`).
    pub(super) href: String,
    /// An operation's HTTP method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) method: Option<String>,
}

/// The parts of the site that write a search index, each from its own
/// build: `search/{part}.json`. The script reads every one there is.
pub(super) const INDEXES: [&str; 2] = ["docs", "api"];

/// Writes `part`'s search index into the site at `site`.
pub(super) fn write_index(site: &Path, part: &str, entries: &[Entry]) -> io::Result<()> {
    debug_assert!(INDEXES.contains(&part), "{part}");
    let dir = site.join("search");
    fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
    let path = dir.join(format!("{part}.json"));
    let json = serde_json::to_string(entries).map_err(io::Error::other)?;
    fs::write(&path, json).map_err(|e| at(&path, e))
}
