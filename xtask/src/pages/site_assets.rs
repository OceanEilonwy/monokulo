//! Every file the GitHub Pages site's pages link, in one table, baked into
//! xtask and named by its content the way monokulo names its own
//! (`static_assets`): `styles.<digest>.css`, which a browser may keep, as
//! a new build's pages link new names. The whole site shares one `assets/`
//! folder, at its root: each part (the landing page, the docs, the testing
//! report) writes the table there and links its files with [`url`], so a
//! reader moving between parts finds the typeface and stylesheets already
//! in the browser's cache, at the same URLs, rather than fetching another
//! copy (and drawing the page in the fallback font meanwhile). The
//! stylebook's build links the typeface there too (`apps/stylebook`).
//!
//! The `ui` components' islands script is built here too, with vite, the
//! way monokulo's build script builds it: xtask is what writes `crates/ui`'s
//! generated Solid components, so it can't need them to compile.

use crate::support::{at, root};
use maud::{html, Markup, PreEscaped};
use static_assets::{asset, Asset, CSS, JS, SVG, WOFF2};
use std::{fs, io, path::Path, process::Command, sync::OnceLock};

/// Every file, the islands script built on first use.
pub(super) fn assets() -> &'static [Asset] {
    static ALL: OnceLock<Vec<Asset>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut all = BAKED.to_vec();
        all.push(islands());
        all
    })
}

/// `crates/ui/web/islands.ts` built into one module, as monokulo serves it.
/// The build is kept in `target/pages-islands` under a stamp of what it was
/// built from, so the site's parts (and tests, each its own process) build
/// it once between them.
fn islands() -> Asset {
    let ui = root().join("crates/ui");
    let out = root().join("target/pages-islands");
    let file = out.join("ui-islands.js");
    let stamp = out.join("stamp");
    let inputs = fingerprint(&ui);
    let fresh = || fs::read_to_string(&stamp).is_ok_and(|s| s == inputs) && file.is_file();
    if !fresh() {
        let lock = root().join("target/pages-islands.lock");
        fs::create_dir_all(root().join("target")).expect("target/");
        while fs::create_dir(&lock).is_err() {
            // One a build that died left behind.
            let stale = fs::metadata(&lock)
                .and_then(|m| m.modified())
                .is_ok_and(|at| at.elapsed().is_ok_and(|age| age.as_secs() > 300));
            if stale {
                let _ = fs::remove_dir(&lock);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if !fresh() {
            let vite = root().join("node_modules/vite/bin/vite.js");
            let status = Command::new("node")
                .arg(&vite)
                .args([
                    "build",
                    "--config",
                    "vite.islands.config.js",
                    "--mode",
                    "production",
                    "--logLevel",
                    "warn",
                    "--outDir",
                ])
                .arg(&out)
                .arg("--emptyOutDir")
                .current_dir(&ui)
                .status();
            let built = status.is_ok_and(|s| s.success());
            if built {
                fs::write(&stamp, &inputs).expect("the islands build's stamp");
            }
            let _ = fs::remove_dir(&lock);
            assert!(built, "the islands build failed (npm ci first?)");
        } else {
            let _ = fs::remove_dir(&lock);
        }
    }
    let bytes: &'static [u8] = fs::read(&file)
        .expect("the islands build wrote ui-islands.js")
        .leak();
    let hex = static_assets::hex16(static_assets::fnv1a64(bytes));
    Asset {
        name: "ui-islands.js",
        content_type: JS,
        bytes,
        // GitHub Pages compresses what it serves.
        brotli: None,
        gzip: None,
        digest: String::from_utf8(hex.to_vec()).unwrap().leak(),
        policy: static_assets::Policy::Fingerprinted,
    }
}

/// A digest of everything the islands build reads from `crates/ui`: its
/// scripts, the generated Solid components and the build's config.
fn fingerprint(ui: &Path) -> String {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut dirs = vec![ui.join("web"), ui.join("solid")];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.push(ui.join("vite.islands.config.js"));
    files.push(ui.join("package.json"));
    files.sort();
    let mut all = Vec::new();
    for file in files {
        all.extend(file.to_string_lossy().as_bytes());
        all.extend(fs::read(&file).unwrap_or_default());
    }
    String::from_utf8(static_assets::hex16(static_assets::fnv1a64(&all)).to_vec()).unwrap()
}

static BAKED: &[Asset] = &[
    // The colours and the components, the `ui` package's (monokulo's own),
    // then the site's own pieces (`assets/styles.css`), linked in that order.
    asset!(text "theme.css", ui::assets::TOKENS_CSS, CSS),
    asset!(text "ui.css", ui::assets::COMPONENTS_CSS, CSS),
    asset!(from "styles.css", "src/pages/assets/styles.css", CSS),
    // Every page's search and the docs' shell (`shell`), the landing page's
    // OpenWrt version picker, and the testing report's filters and
    // screenshot viewer.
    asset!(from "site.js", "src/pages/assets/site/site.js", JS),
    asset!(from "landing.js", "src/pages/assets/landing/landing.js", JS),
    asset!(from "coverage.js", "src/pages/assets/coverage/coverage.js", JS),
    // The typeface (the `ui` package's) and the icon (monokulo's).
    asset!(bytes "manrope-500.woff2", ui::assets::MANROPE_500, WOFF2),
    asset!(bytes "manrope-700.woff2", ui::assets::MANROPE_700, WOFF2),
    asset!(bytes "manrope-800.woff2", ui::assets::MANROPE_800, WOFF2),
    asset!(from "favicon.svg", "../crates/monokulo/static/favicon.svg", SVG),
];

/// Writes every file into the `assets/` folder of the site at `site` (its
/// root, above every part) at its published name. Every part writes the
/// same files, so whichever is built first or alone, its pages' links hold.
pub(super) fn write(site: &Path) -> io::Result<()> {
    let dir = &site.join("assets");
    fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
    for asset in assets() {
        let path = dir.join(asset.file_name());
        fs::write(&path, asset.bytes).map_err(|e| at(&path, e))?;
    }
    Ok(())
}

/// The link to the file called `name` from a page `up` (`""`, `"../"`,
/// `"../../"`) below the site's root, where `assets/` is.
pub(super) fn url(up: &str, name: &str) -> String {
    format!(
        "{up}assets/{}",
        static_assets::named(assets(), name).file_name()
    )
}

/// What every page's `<head>` loads: the icon, the typeface, the colours,
/// the components and the site's own pieces, and the site's script, told
/// which search indexes there are (`shell::INDEXES`). Every page sets text in every
/// weight (its body, its header, its heading), so each is preloaded: the
/// browser asks for them with the page rather than once the stylesheets
/// say they are needed, and a cached copy GitHub Pages wants checked (it
/// sends `max-age=600`) is checked as early as it can be.
pub(super) fn head(up: &str) -> Markup {
    html! {
        link rel="icon" href=(url(up, "favicon.svg")) type="image/svg+xml";
        @for weight in static_assets::FONT_WEIGHTS {
            link rel="preload" href=(url(up, &format!("manrope-{weight}.woff2"))) as="font" type="font/woff2" crossorigin;
        }
        style { (PreEscaped(static_assets::font_faces(|name| url(up, name)))) }
        link rel="stylesheet" href=(url(up, "theme.css"));
        link rel="stylesheet" href=(url(up, "ui.css"));
        link rel="stylesheet" href=(url(up, "styles.css"));
        script type="module" src=(url(up, "ui-islands.js")) {}
        script defer src=(url(up, "site.js")) data-search=(super::shell::INDEXES.join(" ")) {}
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
        let asset = static_assets::published(assets(), file)
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
        write(scratch.path()).unwrap();
        let dir = scratch.join("assets");
        for asset in assets() {
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
                "manrope-500.woff2",
                "manrope-700.woff2",
                "manrope-800.woff2",
                "theme.css",
                "ui.css",
                "styles.css",
                "ui-islands.js",
                "site.js",
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
