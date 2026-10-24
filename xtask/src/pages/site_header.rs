//! The header every page of the GitHub Pages site shares: the brand and the
//! site's links, the same on the landing page, the docs and the testing
//! report: `ui::SiteNav`, monokulo's own bar. The stylebook, built by Vite
//! into /stylebook/, takes it as a partial `cargo xtask ui` writes.

use crate::logo;
use maud::{html, Markup, PreEscaped};

/// The repository the site links to when no other is given.
pub(crate) const GITHUB_URL: &str = "https://github.com/OceanEilonwy/monokulo";

/// Where the site is published (GitHub Pages, custom domain), with no
/// trailing slash: the address the landing page tells routers to install
/// from, and the one the docs' redirects name as canonical. monokulo.com
/// itself is a hosted Monokulo, not this site.
pub(crate) const SITE_URL: &str = "https://docs.monokulo.com";

/// The part of the site a page is in; the header marks its link as current.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    /// The landing page, whose own sections the first two links open.
    Home,
    Docs,
    /// The HTTP API reference (/api/, /rels/, /problems/).
    Api,
    Testing,
    Stylebook,
}

/// The header for a page in `current`, `root` being the relative link prefix
/// from the page to the site's root (`""` on the landing page, `"../"` on the
/// testing report).
pub(crate) fn render(current: Section, root: &str, github_url: &str) -> Markup {
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
        (link(Section::Docs, format!("{root}guide/"), "Docs"))
        (link(Section::Api, format!("{root}api/"), "API"))
        (link(Section::Testing, format!("{root}testing/"), "Testing"))
        (link(Section::Stylebook, format!("{root}stylebook/"), "Stylebook"))
        a href=(github_url) { "Source code" }
    };
    ui::SiteNav::new(brand, links)
        .home(home)
        .width(ui::PageWidth::Wide)
        .render()
}
