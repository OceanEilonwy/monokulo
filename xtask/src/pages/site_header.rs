//! The header every page of the GitHub Pages site shares, the same on the
//! landing page, the docs, the API reference and the testing report:
//! `ui::SiteNav`, monokulo's own bar. The logo goes home; the links are the
//! site's parts (Home, Docs, API reference) and a Resources menu (releases,
//! the testing report, the stylebook, issues); at the right end are search,
//! GitHub and, on the homepage, Get started. The stylebook, built by Vite
//! into /stylebook/, takes it as a partial `cargo xtask ui` writes.
//!
//! Search is a script's (`assets/site/site.js`): its button is hidden until
//! the script shows it, so a page without scripts doesn't offer it.

use crate::logo;
use maud::{html, Markup, PreEscaped};
use ui::assets::icons;

/// The repository the site links to when no other is given.
pub(crate) const GITHUB_URL: &str = "https://github.com/OceanEilonwy/monokulo";

/// Where the site is published (GitHub Pages, custom domain), with no
/// trailing slash: the address the landing page tells routers to install
/// from, and the one the docs' redirects name as canonical. monokulo.com
/// itself is a hosted Monokulo, not this site.
pub(crate) const SITE_URL: &str = "https://docs.monokulo.com";

/// GitHub's mark (Octicons' `mark-github`), beside the bar's GitHub link and
/// the docs' link to the source.
pub(super) const GITHUB_MARK: &str = r#"<svg class="github-mark" width="20" height="20" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" focusable="false"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>"#;

/// The part of the site a page is in; the header marks its link as current.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    /// The landing page: about the product, with Get started in the bar.
    Home,
    Docs,
    /// The HTTP API reference (/api/, /rels/, /problems/).
    Api,
    /// The testing report and the stylebook, which the Resources menu
    /// links: no link in the bar is theirs.
    Testing,
    Stylebook,
}

/// The pages a reader goes to beside the docs: the Resources menu's, and on a
/// phone the docs' map's, which the bar's links are folded away under.
/// `root` is the relative link prefix from the page to the site's root.
pub(super) fn resources(root: &str, github_url: &str) -> [ui::NavMenuLink; 4] {
    let link = |href: String, icon: &str, title: &str, line: &str| ui::NavMenuLink {
        href,
        icon: ui::Slot::from(PreEscaped(icon.to_owned())),
        title: title.into(),
        line: line.into(),
    };
    [
        link(
            format!("{github_url}/releases"),
            icons::PACKAGE,
            "Releases",
            "Downloads and what changed in each",
        ),
        link(
            format!("{root}testing/"),
            icons::FLASK,
            "Test reports",
            "Tests, coverage, fuzzing and scale runs",
        ),
        link(
            format!("{root}stylebook/"),
            icons::PALETTE,
            "Design system",
            "The UI components, live",
        ),
        link(
            format!("{github_url}/issues"),
            icons::BUG,
            "Report an issue",
            "Bugs and feature requests on GitHub",
        ),
    ]
}

/// The bar's links, `root` being the relative link prefix from the page to
/// the site's root: each part's address and its label.
pub(super) fn links(root: &str) -> [(Section, String, &'static str); 3] {
    let home = if root.is_empty() { "./" } else { root };
    [
        (Section::Home, home.to_owned(), "Home"),
        (Section::Docs, docs_front(root), "Docs"),
        (Section::Api, format!("{root}api/"), "API reference"),
    ]
}

/// The docs' first page, where Docs and Get started go.
pub(super) fn docs_front(root: &str) -> String {
    format!("{root}{}/", super::docs::PAGES[0].dir)
}

/// The header for a page in `current`, `root` being the relative link prefix
/// from the page to the site's root (`""` on the landing page, `"../"` on the
/// testing report).
pub(crate) fn render(current: Section, root: &str, github_url: &str) -> Markup {
    bar(current, root, github_url, None)
}

/// The header for a page with a map of its section beside it (the docs, the
/// API reference): on a narrow screen the map is a menu, its button (`for`
/// the checkbox `map_toggle`, which the page's map opens with) leading the
/// bar.
pub(super) fn with_map(current: Section, root: &str, github_url: &str, map_toggle: &str) -> Markup {
    let lead = html! {
        input.map-toggle type="checkbox" id=(map_toggle) aria-label="Show the map of this section";
        label.map-button.mk-btn data-size="compact" for=(map_toggle) aria-hidden="true" {
            (PreEscaped(icons::MENU)) "Menu"
        }
    };
    bar(current, root, github_url, Some(lead))
}

fn bar(current: Section, root: &str, github_url: &str, lead: Option<Markup>) -> Markup {
    let home = if root.is_empty() { "./" } else { root };
    let brand = html! {
        svg.site-nav-logo.logo-mark width="30" height="30" viewBox="0 0 64 64" aria-hidden="true" focusable="false" {
            (PreEscaped(logo::small()))
        }
        "Monokulo"
    };
    let links = html! {
        @for (section, href, label) in links(root) {
            a href=(href) aria-current=[(section == current).then_some("page")] { (label) }
        }
        (ui::NavMenu::new("Resources", resources(root, github_url).to_vec()))
    };
    let actions = html! {
        button.site-nav-search type="button" aria-haspopup="dialog" aria-keyshortcuts="/ Control+K" hidden {
            (PreEscaped(icons::SEARCH))
            span { "Search docs and API" }
            kbd { "/" }
        }
        a.site-nav-out href=(github_url) aria-label="Monokulo on GitHub" {
            (PreEscaped(GITHUB_MARK)) span { "GitHub" }
        }
        @if current == Section::Home {
            (ui::LinkButton::new(
                html! { "Get started" (PreEscaped(icons::ARROW_RIGHT)) },
                docs_front(root),
            )
            .variant(ui::ButtonVariant::Primary)
            .size(ui::ControlSize::Compact))
        }
    };
    let nav = ui::SiteNav::new(brand, links)
        .home(home)
        .actions(actions)
        .width(ui::PageWidth::Window);
    match lead {
        Some(lead) => nav.lead(lead),
        None => nav,
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every part's bar links the site's parts and resources from where its
    /// pages are; only the homepage's offers Get started, and search waits
    /// for the script that runs it.
    #[test]
    fn the_bar_links_every_part_and_get_started_only_at_home() {
        for (section, root) in [
            (Section::Home, ""),
            (Section::Docs, "../"),
            (Section::Api, "../../"),
            (Section::Testing, "../"),
            (Section::Stylebook, "../"),
        ] {
            let html = render(section, root, GITHUB_URL).into_string();
            let home = if root.is_empty() { "./" } else { root };
            for href in [
                home.to_owned(),
                format!("{root}introduction/"),
                format!("{root}api/"),
                format!("{root}testing/"),
                format!("{root}stylebook/"),
                format!("{GITHUB_URL}/releases"),
                format!("{GITHUB_URL}/issues"),
                GITHUB_URL.to_owned(),
            ] {
                assert!(
                    html.contains(&format!(r#"href="{href}""#)),
                    "{section:?}: {href}"
                );
            }
            let get_started = html.contains("Get started");
            assert_eq!(get_started, section == Section::Home, "{section:?}");
            assert!(html.contains(r#"class="site-nav-search""#), "{section:?}");
            let search = html.split(r#"class="site-nav-search""#).nth(1).unwrap();
            assert!(
                search.split('>').next().unwrap().contains("hidden"),
                "{section:?}"
            );
            // At most one link is the page's part.
            assert!(
                html.matches(r#"aria-current="page""#).count() <= 1,
                "{section:?}"
            );
        }
    }
}
