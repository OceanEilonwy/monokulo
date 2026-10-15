//! Every file the GitHub Pages site's pages link, in one table, baked into
//! xtask and named by its content the way monokulo names its own
//! (`static_assets`): `styles.<digest>.css`, which a browser may keep, as
//! a new build's pages link new names. Each part of the site (the landing
//! page, the docs, the testing report) writes the table into its own
//! `assets/` folder and links its files with [`url`].

use crate::support::at;
use maud::{html, Markup, PreEscaped};
use static_assets::{asset, Asset, CSS, JS, SVG, WOFF2};
use std::{fs, io, path::Path};

pub(super) static ASSETS: &[Asset] = &[
    // The colours, monokulo's own (`packages/ui/src/tokens.css`),
    // then the site's components (`assets/styles.css`), linked in that order.
    asset!(from "theme.css", "../packages/ui/src/tokens.css", CSS),
    asset!(from "styles.css", "src/pages/assets/styles.css", CSS),
    // The landing page's tabs and copy buttons, and the testing report's
    // filters and screenshot viewer.
    asset!(from "landing.js", "src/pages/assets/landing/landing.js", JS),
    asset!(from "coverage.js", "src/pages/assets/coverage/coverage.js", JS),
    // The typeface and the icon, monokulo's own files.
    asset!(from "manrope-500.woff2", "../packages/ui/src/fonts/manrope-500.woff2", WOFF2),
    asset!(from "manrope-700.woff2", "../packages/ui/src/fonts/manrope-700.woff2", WOFF2),
    asset!(from "manrope-800.woff2", "../packages/ui/src/fonts/manrope-800.woff2", WOFF2),
    asset!(from "favicon.svg", "../crates/monokulo/static/favicon.svg", SVG),
];

/// Writes every file into `dir` (a part's `assets/`) at its published name.
pub(super) fn write(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
    for asset in ASSETS {
        let path = dir.join(asset.file_name());
        fs::write(&path, asset.bytes).map_err(|e| at(&path, e))?;
    }
    Ok(())
}

/// The link to the file called `name` from a page `up` (`""`, `"../"`)
/// below its part's root, where `assets/` is.
pub(super) fn url(up: &str, name: &str) -> String {
    format!(
        "{up}assets/{}",
        static_assets::named(ASSETS, name).file_name()
    )
}

/// What every page's `<head>` loads: the icon, the typeface, the colours
/// and the components.
pub(super) fn head(up: &str) -> Markup {
    html! {
        link rel="icon" href=(url(up, "favicon.svg")) type="image/svg+xml";
        style { (PreEscaped(static_assets::font_faces(|name| url(up, name)))) }
        link rel="stylesheet" href=(url(up, "theme.css"));
        link rel="stylesheet" href=(url(up, "styles.css"));
    }
}

/// Every `assets/` link in `html` (a `href`, `src` or CSS `url()`) names a
/// file at its published name, and `dir` (that page's `assets/`) holds it;
/// returns the names linked.
#[cfg(test)]
pub(super) fn assert_links_published(html: &str, dir: &Path) -> Vec<&'static str> {
    let links = regex::Regex::new(r#"(?:href="|src="|url\(")([^"]*assets/[^"]+)""#).unwrap();
    let mut linked = Vec::new();
    for link in links.captures_iter(html) {
        let link = &link[1];
        let file = link.rsplit_once("assets/").unwrap().1;
        let asset = static_assets::published(ASSETS, file)
            .unwrap_or_else(|| panic!("{link} is not a published asset's name"));
        assert!(
            dir.join(file).is_file(),
            "{link} is not in {}",
            dir.display()
        );
        linked.push(asset.name);
    }
    linked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    #[test]
    fn every_file_is_written_at_the_name_the_pages_link() {
        let scratch = Scratch::new("site-assets");
        let dir = scratch.join("assets");
        write(&dir).unwrap();
        for asset in ASSETS {
            let linked = url("../", asset.name);
            let file = linked.strip_prefix("../assets/").unwrap();
            assert_eq!(file.len(), asset.name.len() + 17, "{linked}");
            assert_eq!(fs::read(dir.join(file)).unwrap(), asset.bytes);
        }
        let linked = assert_links_published(&head("").into_string(), &dir);
        assert_eq!(
            linked,
            [
                "favicon.svg",
                "manrope-500.woff2",
                "manrope-700.woff2",
                "manrope-800.woff2",
                "theme.css",
                "styles.css",
            ]
        );
    }

    #[test]
    #[should_panic(expected = "assets/styles.css is not a published asset's name")]
    fn a_bare_name_is_caught() {
        let scratch = Scratch::new("site-assets-bare");
        assert_links_published(
            r#"<link rel="stylesheet" href="assets/styles.css">"#,
            &scratch.join("assets"),
        );
    }
}
