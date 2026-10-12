//! `cargo xtask pages landing`: the landing page (GitHub Pages: /) from
//! maud (`page`), beside the signed apk repositories the OpenWrt workflow
//! made (`openwrt/<release>/<arch>/` in the site, one per OpenWrt release and
//! architecture); it publishes the public key as `openwrt/monokulo.pem`.
//!
//! The page lists every repository's package files. Its install steps name
//! one repository, the newest release's (then the first architecture), so
//! the page's text about the router has to match it.

mod page;

use super::site_header::GITHUB_URL;
use super::STYLESHEET;
use crate::support::{at, files_under, Exit};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const PUBLIC_KEY: &str = "openwrt/keys/monokulo.pem";
/// The page's stylesheet fonts, from monokulo's own static files.
const FONT_WEIGHTS: [u16; 3] = [500, 700, 800];
const LOGOS: [&str; 2] = ["logo.svg", "favicon.svg"];

/// `cargo xtask pages landing --site DIR [--github-url URL]`.
pub(crate) fn build(root: &Path, args: &[&str]) -> io::Result<Exit> {
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
    write(root, &site, &github_url)?;
    eprintln!("landing page in {}", site.display());
    Ok(Exit::SUCCESS)
}

/// The GitHub Pages address of a repository's URL: https://owner.github.io/name.
fn pages_url(github_url: &str) -> io::Result<String> {
    let path = github_url.strip_prefix("https://github.com/").unwrap_or("");
    match path.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() => {
            Ok(format!("https://{}.github.io/{name}", owner.to_lowercase()))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("--github-url {github_url} is not https://github.com/OWNER/NAME"),
        )),
    }
}

/// One apk repository in the site: `openwrt/<release>/<arch>/`.
struct Repo {
    release: String,
    arch: String,
    /// Its address in the site, `openwrt/<release>/<arch>`.
    path: String,
    apk: String,
    size: u64,
}

impl Repo {
    /// The package's version and package release, `0.1.0-r42`.
    fn version(&self) -> &str {
        self.apk
            .strip_prefix("monokulo-")
            .and_then(|v| v.strip_suffix(".apk"))
            .unwrap_or(&self.apk)
    }

    /// What the page's dropdown and blocks call this repository.
    fn id(&self) -> String {
        format!("{}-{}", self.release, self.arch)
    }

    fn size(&self) -> String {
        format!("{} MB", self.size / 1024 / 1024)
    }
}

/// The values in order of first appearance, joined: the releases or
/// architectures the repositories cover.
fn distinct(values: impl Iterator<Item = String>, separator: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    for value in values {
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    seen.join(separator)
}

/// Every apk repository (a folder with a `packages.adb`) under
/// `openwrt/<release>/<arch>/`, the newest release first, then by
/// architecture.
fn repos(site: &Path) -> io::Result<Vec<Repo>> {
    let openwrt = site.join("openwrt");
    let mut found = Vec::new();
    for index in files_under(&openwrt, |_| false)? {
        if index.file_name().is_none_or(|name| name != "packages.adb") {
            continue;
        }
        let dir = index.parent().unwrap_or(&openwrt);
        let parts: Vec<String> = dir
            .strip_prefix(&openwrt)
            .unwrap_or(dir)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let [release, arch] = parts.as_slice() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not openwrt/<release>/<arch>/", dir.display()),
            ));
        };
        let (apk, size) = fs::read_dir(dir)
            .map_err(|e| at(dir, e))?
            .filter_map(Result::ok)
            .find_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (name.starts_with("monokulo-") && name.ends_with(".apk"))
                    .then(|| (name, entry.metadata().map_or(0, |m| m.len())))
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no monokulo-*.apk in {}", dir.display()),
                )
            })?;
        found.push(Repo {
            release: release.clone(),
            arch: arch.clone(),
            path: format!("openwrt/{release}/{arch}"),
            apk,
            size,
        });
    }
    if found.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no apk repository (openwrt/<release>/<arch>/packages.adb) in {}",
                site.display()
            ),
        ));
    }
    found.sort_by(|a, b| b.release.cmp(&a.release).then(a.arch.cmp(&b.arch)));
    Ok(found)
}

/// Writes `index.html`, its assets and `.nojekyll` into `site`.
fn write(root: &Path, site: &Path, github_url: &str) -> io::Result<()> {
    let pages_url = pages_url(github_url)?;
    let repos = repos(site)?;

    let key_path = root.join(PUBLIC_KEY);
    let key = fs::read_to_string(&key_path).map_err(|e| at(&key_path, e))?;
    let page = page::render(github_url, &pages_url, key.trim_end(), &repos).0;
    // The key the page tells routers to trust, beside the repositories.
    let published_key = site.join("openwrt/monokulo.pem");
    fs::write(&published_key, &key).map_err(|e| at(&published_key, e))?;
    let index_html = site.join("index.html");
    fs::write(&index_html, page).map_err(|e| at(&index_html, e))?;

    let assets = site.join("assets");
    fs::create_dir_all(&assets).map_err(|e| at(&assets, e))?;
    let statics = root.join("crates/monokulo/static");
    let mut copies = vec![
        (
            root.join("crates/monokulo/src/views/theme.css"),
            assets.join("theme.css"),
        ),
        (root.join(STYLESHEET), assets.join("styles.css")),
        (
            root.join("xtask/src/pages/assets/landing/landing.js"),
            assets.join("landing.js"),
        ),
    ];
    copies.extend(
        LOGOS
            .iter()
            .map(|name| (statics.join(name), assets.join(name))),
    );
    copies.extend(FONT_WEIGHTS.iter().map(|weight| {
        let name = format!("manrope-{weight}.woff2");
        (statics.join(&name), assets.join(name))
    }));
    for (from, to) in copies {
        fs::copy(&from, &to).map_err(|e| at(&from, e))?;
    }
    // Pages would otherwise run the site through Jekyll.
    let nojekyll = site.join(".nojekyll");
    fs::write(&nojekyll, "").map_err(|e| at(&nojekyll, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn checkout(scratch: &Scratch) -> PathBuf {
        let root = scratch.join("root");
        put(&root.join("xtask/src/pages/assets/landing/landing.js"), "");
        put(&root.join(STYLESHEET), "");
        put(&root.join(PUBLIC_KEY), "KEY\n");
        put(&root.join("crates/monokulo/src/views/theme.css"), "");
        for name in LOGOS {
            put(&root.join("crates/monokulo/static").join(name), "");
        }
        for weight in FONT_WEIGHTS {
            put(
                &root.join(format!("crates/monokulo/static/manrope-{weight}.woff2")),
                "",
            );
        }
        root
    }

    fn repo(site: &Path, release: &str, arch: &str, apk: &str) {
        let dir = site.join("openwrt").join(release).join(arch);
        put(&dir.join(apk), "apk");
        put(&dir.join("packages.adb"), "index");
    }

    #[test]
    fn the_page_installs_from_the_newest_release_and_lists_every_repository() {
        let scratch = Scratch::new("landing");
        let root = checkout(&scratch);
        let site = scratch.join("site");
        repo(
            &site,
            "25.12",
            "aarch64_cortex-a53",
            "monokulo-0.1.0-r42.apk",
        );
        repo(&site, "26.06", "x86_64", "monokulo-0.2.0-r50.apk");
        repo(
            &site,
            "26.06",
            "aarch64_cortex-a53",
            "monokulo-0.2.0-r50.apk",
        );

        write(&root, &site, "https://github.com/Owner/monokulo").unwrap();

        let page = fs::read_to_string(site.join("index.html")).unwrap();
        // The install steps name the newest release's repository, and the
        // footer its version.
        assert!(page.contains(
            "https://owner.github.io/monokulo/openwrt/26.06/aarch64_cortex-a53/packages.adb"
        ));
        assert!(page.contains("Monokulo 0.2.0-r50 · "));
        // A dropdown option, and the install steps and package files, for
        // each version, the newest first.
        let newest = page
            .find("<option value=\"26.06-aarch64_cortex-a53\">")
            .unwrap();
        let oldest = page
            .find("<option value=\"25.12-aarch64_cortex-a53\">")
            .unwrap();
        assert!(newest < oldest);
        for id in ["25.12-aarch64_cortex-a53", "26.06-x86_64"] {
            assert!(page.contains(&format!("data-target=\"{id}\"")), "{id}");
        }
        assert!(page.contains("OpenWrt 26.06 / 25.12 with LuCI"));
        assert!(page.contains("<pre>KEY</pre>"));
        for path in [
            "openwrt/25.12/aarch64_cortex-a53/monokulo-0.1.0-r42.apk",
            "openwrt/26.06/aarch64_cortex-a53/packages.adb",
            "openwrt/26.06/x86_64/monokulo-0.2.0-r50.apk",
        ] {
            assert!(page.contains(&format!("href=\"{path}\"")), "{path}");
        }
        assert_eq!(
            fs::read_to_string(site.join("openwrt/monokulo.pem")).unwrap(),
            "KEY\n"
        );
        assert!(site.join(".nojekyll").exists());
        for asset in ["theme.css", "styles.css", "landing.js"] {
            assert!(site.join("assets").join(asset).exists(), "{asset}");
        }
    }

    #[test]
    fn a_site_without_the_apk_repository_is_an_error() {
        let scratch = Scratch::new("landing-empty");
        let site = scratch.join("site");
        fs::create_dir_all(site.join("openwrt")).unwrap();
        let error = write(scratch.path(), &site, GITHUB_URL).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
