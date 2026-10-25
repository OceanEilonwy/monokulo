//! `cargo xtask pages landing`: the landing page (GitHub Pages: /) from
//! maud (`page`), about the product. The install guides it links are docs
//! pages (`install`, built by `pages docs`).

mod page;

use super::site_assets;
use super::site_header::GITHUB_URL;
use crate::support::{at, Exit};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// `cargo xtask pages landing --site DIR [--github-url URL]`.
pub(crate) fn build(_root: &Path, args: &[&str]) -> io::Result<Exit> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_string());
    let (mut site, mut github_url) = (None, GITHUB_URL.to_string());
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| bad(&format!("{flag} needs a value")))?;
        match *flag {
            "--site" => site = Some(PathBuf::from(value)),
            "--github-url" => github_url = value.trim_end_matches('/').to_string(),
            _ => return Err(bad(&format!("unknown option {flag}"))),
        }
    }
    let site = site.ok_or_else(|| bad("pages landing needs --site DIR"))?;
    write(&site, &github_url)?;
    eprintln!("landing page in {}", site.display());
    Ok(Exit::SUCCESS)
}

/// Writes `index.html`, its assets and `.nojekyll` into `site`.
fn write(site: &Path, github_url: &str) -> io::Result<()> {
    fs::create_dir_all(site).map_err(|e| at(site, e))?;
    let index_html = site.join("index.html");
    fs::write(&index_html, page::render(github_url).0).map_err(|e| at(&index_html, e))?;
    site_assets::write(site)?;
    // Pages would otherwise run the site through Jekyll.
    let nojekyll = site.join(".nojekyll");
    fs::write(&nojekyll, "").map_err(|e| at(&nojekyll, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::{docs, install};
    use crate::support::Scratch;

    /// The homepage beside the docs, as the pages workflow builds them.
    fn built(scratch: &Scratch) -> (PathBuf, String) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let site = scratch.join("site");
        install::tests::repo(
            &site,
            "25.12",
            "aarch64_cortex-a53",
            "monokulo-0.1.0-r1.apk",
        );
        write(&site, "https://github.com/Owner/monokulo").unwrap();
        docs::write(root, &site, "https://docs.monokulo.com", GITHUB_URL).unwrap();
        let page = fs::read_to_string(site.join("index.html")).unwrap();
        (site, page)
    }

    /// Every link on the homepage within the site reaches a published page
    /// or file: the docs (the install guides among them), the API
    /// reference's front page and the shared assets. The testing report
    /// and the stylebook are other builds', and the source is the given
    /// repository's.
    #[test]
    fn every_link_reaches_a_published_page() {
        let scratch = Scratch::new("landing-links");
        let (site, page) = built(&scratch);
        let href = regex::Regex::new(r#"(?:href|src)="([^"]+)""#).unwrap();
        let mut checked = Vec::new();
        for link in href.captures_iter(&page).map(|c| c[1].to_owned()) {
            if link.starts_with("https://github.com/Owner/monokulo") || link.starts_with('#') {
                continue;
            }
            let path = link.split('#').next().unwrap();
            if ["testing/", "stylebook/"].contains(&path) {
                continue;
            }
            assert!(!path.contains("://"), "{link} leaves the site");
            let file = if path.is_empty() || path == "./" {
                site.join("index.html")
            } else if path.ends_with('/') {
                site.join(path).join("index.html")
            } else {
                site.join(path)
            };
            assert!(file.is_file(), "{link}: {}", file.display());
            checked.push(path.to_owned());
        }
        for guide in [page::INSTALL_COMPUTER, page::INSTALL_ROUTER] {
            assert!(checked.iter().any(|c| c == guide), "{guide}: {checked:?}");
        }
        let linked = site_assets::assert_links_published(&page, &site.join("assets"));
        assert_eq!(
            linked
                .iter()
                .filter(|name| name.ends_with(".css"))
                .collect::<Vec<_>>(),
            [&"theme.css", &"ui.css", &"styles.css", &"status.css"],
            "the status symbols' styles after the components'"
        );
        assert!(site.join(".nojekyll").exists());
    }

    /// The anchors the install guides had here still lead to them: each is
    /// a link to its guide's page (what a reader without scripts lands
    /// on), the page's script sends a reader with one straight there, and
    /// `#install` is the band that links both.
    #[test]
    fn the_old_install_anchors_lead_to_their_guides() {
        let scratch = Scratch::new("landing-anchors");
        let (_, page) = built(&scratch);
        let id = |anchor: &str| {
            let at = page
                .find(&format!(r#"id="{anchor}""#))
                .unwrap_or_else(|| panic!("#{anchor}"));
            let start = page[..at].rfind('<').unwrap();
            &page[start..start + page[start..].find('>').unwrap()]
        };
        let href = regex::Regex::new(r#"href="([^"]+)""#).unwrap();
        for (anchor, guide) in page::OLD_ANCHORS {
            let element = id(anchor);
            assert!(element.starts_with("<a "), "#{anchor}: {element}");
            assert_eq!(&href.captures(element).unwrap()[1], guide, "#{anchor}");
        }
        let band = id("install");
        assert!(band.starts_with("<section "), "{band}");
        let band = page.split(r#"id="install""#).nth(1).unwrap();
        let band = band.split("</section>").next().unwrap();
        for (anchor, _) in page::OLD_ANCHORS {
            assert!(band.contains(&format!(r#"id="{anchor}""#)), "#{anchor}");
        }
        // The script's table is the same anchors and guides.
        let script = page.split("<script>").nth(1).unwrap();
        let script = script.split("</script>").next().unwrap();
        let table = regex::Regex::new(r##""#([a-z]+)":"([^"]+)""##).unwrap();
        let pairs: Vec<(String, String)> = table
            .captures_iter(script)
            .map(|c| (c[1].to_owned(), c[2].to_owned()))
            .collect();
        let expected: Vec<(String, String)> = page::OLD_ANCHORS
            .iter()
            .map(|(a, g)| ((*a).to_owned(), (*g).to_owned()))
            .collect();
        assert_eq!(pairs, expected);
    }

    /// Every status symbol the order's life draws is defined on the page,
    /// once, and each of the POS's four states before it's paid shows.
    #[test]
    fn the_order_life_draws_with_symbols_the_page_defines() {
        let scratch = Scratch::new("landing-symbols");
        let (_, page) = built(&scratch);
        let used = regex::Regex::new(r##"<use href="#([^"]+)""##).unwrap();
        let used: Vec<String> = used.captures_iter(&page).map(|c| c[1].to_owned()).collect();
        assert!(!used.is_empty());
        for symbol in &used {
            assert_eq!(
                page.matches(&format!(r#"id="{symbol}""#)).count(),
                1,
                "{symbol}"
            );
        }
        let states = regex::Regex::new(r#"<li class="step-done state-([a-z-]+)""#).unwrap();
        let states: Vec<&str> = states
            .captures_iter(&page)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(states, ["pending", "unconfirmed", "confirming", "paid"]);
    }
}
