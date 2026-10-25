//! The docs pages about installing and running Monokulo, drawn in Rust
//! rather than from Markdown: their steps carry the `ui` package's tabs
//! and copyable commands, and the router's follow the apk repositories the
//! site holds. `computer` installs the desktop builds, `router` the OpenWrt
//! package, and `run` says where the config and data live and how to back
//! them up, update and remove Monokulo.
//!
//! The OpenWrt workflow publishes one signed apk repository per OpenWrt
//! release and architecture, `openwrt/<release>/<arch>/` in the site, and
//! `pages fetch` puts them there before the docs are built ([`repos`]). The
//! router's steps name one per repository; the docs' build publishes the
//! key that signs them beside them, as `openwrt/monokulo.pem`.

pub(super) mod computer;
pub(super) mod router;
pub(super) mod run;

use crate::support::{at, files_under};
use maud::{html, Markup};
use std::{fs, io, path::Path};

/// The key that signs the apk repositories, in the checkout.
const PUBLIC_KEY: &str = "openwrt/keys/monokulo.pem";

/// What the pages are drawn from: the site's apk repositories, the key
/// that signs them, and where the site and the source are.
pub(in crate::pages) struct Site {
    /// The newest release first, then by architecture; never empty.
    pub(super) repos: Vec<Repo>,
    /// The signing key, as routers are told to trust it.
    pub(super) key: String,
    /// Where routers fetch the repositories from, with no trailing slash.
    pub(super) site_url: String,
    /// The repository releases are downloaded from.
    pub(super) github_url: String,
}

impl Site {
    /// The site at `site`'s repositories and the checkout at `root`'s key;
    /// publishes the key beside the repositories. A site without an apk
    /// repository is an error: the router's steps would have nothing to
    /// install from.
    pub(in crate::pages) fn load(
        root: &Path,
        site: &Path,
        site_url: &str,
        github_url: &str,
    ) -> io::Result<Site> {
        let repos = repos(site)?;
        let key_path = root.join(PUBLIC_KEY);
        let key = fs::read_to_string(&key_path).map_err(|e| at(&key_path, e))?;
        let published = site.join("openwrt/monokulo.pem");
        fs::write(&published, &key).map_err(|e| at(&published, e))?;
        Ok(Site {
            repos,
            key: key.trim_end().to_owned(),
            site_url: site_url.trim_end_matches('/').to_owned(),
            github_url: github_url.trim_end_matches('/').to_owned(),
        })
    }
}

/// One apk repository in the site: `openwrt/<release>/<arch>/`.
pub(in crate::pages) struct Repo {
    pub(super) release: String,
    pub(super) arch: String,
    /// Its address in the site, `openwrt/<release>/<arch>`.
    pub(super) path: String,
    pub(super) apk: String,
    pub(super) size: u64,
}

impl Repo {
    /// What the page's dropdown and blocks call this repository.
    pub(super) fn id(&self) -> String {
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
                "no apk repository (openwrt/<release>/<arch>/packages.adb) in {}: \
                 the router's install page names one",
                site.display()
            ),
        ));
    }
    found.sort_by(|a, b| b.release.cmp(&a.release).then(a.arch.cmp(&b.arch)));
    Ok(found)
}

/// Lines to run (or to put in a file), with a Copy button named for what
/// it copies; what follows a `#` on a line is a note, left out.
fn command(code: impl Into<String>, copy_label: &str) -> ui::CopyBlock {
    ui::CopyBlock::new(code).copy_label(copy_label).comment("#")
}

/// Where something is in a menu (LuCI's, the admin page's): each place, a
/// `›` between them.
fn path(places: &[&str]) -> Markup {
    html! {
        @for (i, place) in places.iter().enumerate() {
            @if i > 0 { " " b { "›" } " " }
            (place)
        }
    }
}

/// Numbered steps: each a heading and what to do.
fn steps(steps: Vec<(&str, Markup)>) -> Markup {
    html! {
        ol.steps {
            @for (i, (title, body)) in steps.into_iter().enumerate() {
                li {
                    span.num aria-hidden="true" { (i + 1) }
                    div {
                        h3 { (title) }
                        (body)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::support::Scratch;
    use std::path::PathBuf;

    pub(in crate::pages) fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// An apk repository in the site at `site`, holding `apk`.
    pub(in crate::pages) fn repo(site: &Path, release: &str, arch: &str, apk: &str) {
        let dir = site.join("openwrt").join(release).join(arch);
        put(&dir.join(apk), "apk");
        put(&dir.join("packages.adb"), "index");
    }

    fn checkout(scratch: &Scratch) -> PathBuf {
        let root = scratch.join("root");
        put(&root.join(PUBLIC_KEY), "KEY\n");
        root
    }

    #[test]
    fn the_repositories_come_newest_release_first_and_a_site_without_one_is_an_error() {
        let scratch = Scratch::new("install-site");
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

        let loaded = Site::load(
            &root,
            &site,
            "https://example.org/",
            "https://github.com/o/m/",
        )
        .unwrap();
        let ids: Vec<String> = loaded.repos.iter().map(Repo::id).collect();
        assert_eq!(
            ids,
            [
                "26.06-aarch64_cortex-a53",
                "26.06-x86_64",
                "25.12-aarch64_cortex-a53"
            ]
        );
        assert_eq!(loaded.site_url, "https://example.org");
        assert_eq!(loaded.key, "KEY");
        // The key routers are told to trust, beside the repositories.
        assert_eq!(
            fs::read_to_string(site.join("openwrt/monokulo.pem")).unwrap(),
            "KEY\n"
        );

        let empty = scratch.join("empty");
        fs::create_dir_all(empty.join("openwrt")).unwrap();
        let error = Site::load(
            &root,
            &empty,
            "https://example.org",
            "https://github.com/o/m",
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
