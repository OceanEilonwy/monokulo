//! The header every page of the GitHub Pages site shares: the brand and the
//! site's links, the same on the landing page, the docs and the testing
//! report: `ui::SiteNav`, monokulo's own bar. The stylebook, built by Vite
//! into /stylebook/, has its own page and no header.

use crate::logo;
use maud::{html, Markup, PreEscaped};

/// The repository the site links to when no other is given.
pub(super) const GITHUB_URL: &str = "https://github.com/OceanEilonwy/monokulo";

/// The part of the site a page is in; the header marks its link as current.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Section {
    /// The landing page, whose own sections the first two links open.
    Home,
    Docs,
    Testing,
}

/// The header for a page in `current`, `root` being the relative link prefix
/// from the page to the site's root (`""` on the landing page, `"../"` on the
/// testing report).
pub(super) fn render(current: Section, root: &str, github_url: &str) -> Markup {
    let link = |section: Section, href: String, label: &str| {
        html! {
            a href=(href) aria-current=[(section == current).then_some("page")] { (label) }
        }
    };
    let home = if root.is_empty() { "./" } else { root };
    let brand = html! {
        svg.site-nav-logo.logo-mark width="30" height="30" viewBox="0 0 64 64" aria-hidden="true" focusable="false" {
            (PreEscaped(logo::small()))
        }
        "Monokulo"
    };
    let links = html! {
        a href=(format!("{root}#how")) { "How it works" }
        a href=(format!("{root}#install")) { "Install" }
        (link(Section::Docs, format!("{root}docs/"), "Docs"))
        (link(Section::Testing, format!("{root}testing/"), "Testing"))
        a href=(format!("{root}stylebook/")) { "Stylebook" }
        a href=(github_url) { "Source code" }
    };
    ui::SiteNav::new(brand, links)
        .home(home)
        .width(ui::PageWidth::Wide)
        .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every section gets the same links, in the same order, only the
    /// current one marked, each reaching the same place from where it is.
    #[test]
    fn every_section_links_the_same_places_and_marks_only_its_own() {
        let pages = [
            (Section::Home, "", None),
            (Section::Docs, "../", Some("Docs")),
            (Section::Docs, "../../", Some("Docs")),
            (Section::Testing, "../", Some("Testing")),
        ];
        for (section, root, current) in pages {
            let html = render(section, root, GITHUB_URL).0;
            let labels: Vec<&str> = html
                .split("</a>")
                .filter_map(|a| a.rsplit('>').next())
                .filter(|label| !label.is_empty())
                .collect();
            assert_eq!(
                labels,
                [
                    "Monokulo",
                    "How it works",
                    "Install",
                    "Docs",
                    "Testing",
                    "Stylebook",
                    "Source code"
                ],
                "{section:?} at {root:?}"
            );
            for href in ["#how", "#install", "docs/", "testing/", "stylebook/"] {
                assert!(
                    html.contains(&format!(r#"href="{root}{href}""#)),
                    "{href} {html}"
                );
            }
            assert!(html.contains(&format!(r#"href="{GITHUB_URL}""#)));
            let marked: Vec<&str> = html
                .split(r#" aria-current="page">"#)
                .skip(1)
                .map(|rest| rest.split('<').next().unwrap())
                .collect();
            assert_eq!(
                marked,
                current.into_iter().collect::<Vec<_>>(),
                "{section:?}"
            );
        }
        assert!(render(Section::Home, "", GITHUB_URL)
            .0
            .contains(r#"<a href="./" class="site-nav-brand">"#));
    }
}
