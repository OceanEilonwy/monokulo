//! Files a page links by URL, baked into the binary that writes or serves
//! the page and named by their content.
//!
//! A crate lists its files once, one `asset!` line each, in a `&[Asset]`
//! table: the file's bytes and a digest of them, both computed when that
//! crate is built. A page links a file by its name and gets a file name
//! carrying the digest, `logo.3f9a1c2b4d5e6f70.svg` ([`Asset::file_name`]),
//! which a browser may keep for a year: new content is a new name. The only
//! files linked by their bare name are [`Policy::Public`] ones, whose name
//! others link and keep, and [`Policy::Hashed`] ones, a bundler's chunks
//! whose names carry a hash of their own.
//!
//! Two crates use it. monokulo serves its table from `GET /static/{*file}`
//! (`monokulo::assets`); `cargo xtask pages` writes its table into the
//! GitHub Pages site's `assets/` folder, which every part of the site shares.

/// Whether a file's name carries its digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Linked by its own pages at a name carrying its digest: kept for a year.
    Fingerprinted,
    /// Linked by others at its bare name, so that name is theirs to keep:
    /// checked on every use.
    Public,
    /// A bundler's chunk, its name already carrying its hash
    /// (`solid-CoMz0tzw.js`), linked by other chunks at that name: kept for a
    /// year like a fingerprinted file.
    Hashed,
}

#[derive(Clone, Copy, Debug)]
pub struct Asset {
    /// The file's name, with its extension: what pages ask for.
    pub name: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
    /// The same, brotli- and gzip-compressed when the binary was built
    /// ([`bake!`]): `None` where that wouldn't make it smaller, or the file
    /// is compressed already.
    pub brotli: Option<&'static [u8]>,
    pub gzip: Option<&'static [u8]>,
    /// Sixteen hex characters naming this version of the file: FNV-1a of
    /// its bytes, computed when the binary was built. A version label, not
    /// an integrity check.
    pub digest: &'static str,
    pub policy: Policy,
}

/// A file's bytes and its compressed copies, as [`bake!`] reads and makes
/// them when the crate listing the file is compiled.
#[derive(Clone, Copy, Debug)]
pub struct Baked {
    pub bytes: &'static [u8],
    pub brotli: Option<&'static [u8]>,
    pub gzip: Option<&'static [u8]>,
}

impl Baked {
    /// Bytes with no compressed copies: a font, an image, or text a test
    /// makes up.
    pub const fn plain(bytes: &'static [u8]) -> Self {
        Baked {
            bytes,
            brotli: None,
            gzip: None,
        }
    }

    /// The bytes as text, for a stylesheet a page also inlines. A file that
    /// isn't UTF-8 is a mistake, caught when the crate compiles.
    pub const fn text(&self) -> &'static str {
        match core::str::from_utf8(self.bytes) {
            Ok(text) => text,
            Err(_) => panic!("a baked text file isn't UTF-8"),
        }
    }
}

/// A file's [`Baked`] bytes and copies, read when the calling crate
/// compiles: `bake!("static/", "app.js")` under its directory,
/// `bake!(out_dir "/web/app.js")` from its build script's output,
/// `bake!(join "\n" ["a.css", "b.css"])` several files one after another.
pub use static_assets_macros::bake;

// `bake!` names this crate by its path, from here too.
extern crate self as static_assets;

impl Asset {
    /// The name the file is linked and published at: `name.<digest>.ext`,
    /// or its bare name if it's [`Policy::Public`].
    pub fn file_name(&self) -> String {
        match self.policy {
            Policy::Fingerprinted => {
                let (stem, extension) = self
                    .name
                    .rsplit_once('.')
                    .expect("an asset's name has an extension");
                format!("{stem}.{}.{extension}", self.digest)
            }
            Policy::Public | Policy::Hashed => self.name.to_string(),
        }
    }
}

/// The asset in `table` called `name`, if there is one.
pub fn find(table: &'static [Asset], name: &str) -> Option<&'static Asset> {
    table.iter().find(|asset| asset.name == name)
}

/// The asset in `table` called `name`. A name that isn't an asset's is a
/// mistake in the page, caught by any test that renders it.
pub fn named(table: &'static [Asset], name: &str) -> &'static Asset {
    find(table, name).unwrap_or_else(|| panic!("no static asset is called {name}"))
}

/// The asset in `table` published as `file`: its current
/// [`Asset::file_name`], exactly. A bare name of a fingerprinted file, or
/// an old digest, is no asset's.
pub fn published(table: &'static [Asset], file: &str) -> Option<&'static Asset> {
    table.iter().find(|asset| asset.file_name() == file)
}

/// The `@font-face` rules for Manrope, the UI typeface, at the URLs `url`
/// gives its files (`manrope-500.woff2` and so on): the first thing in a
/// page's style. Self-hosted, not a Google Fonts `<link>`, deliberately:
/// that CDN sees every visitor's IP on every page load. Latin-only.
pub fn font_faces(url: impl Fn(&str) -> String) -> String {
    FONT_WEIGHTS
        .map(|weight| {
            format!(
                "@font-face{{font-family:\"Manrope\";font-style:normal;font-weight:{weight};font-display:swap;src:url(\"{}\") format(\"woff2\")}}\n",
                url(&format!("manrope-{weight}.woff2"))
            )
        })
        .concat()
}

/// Manrope's weights, each a file `manrope-{weight}.woff2` in the table.
pub const FONT_WEIGHTS: [u16; 3] = [500, 700, 800];

pub const JS: &str = "text/javascript; charset=utf-8";
pub const CSS: &str = "text/css; charset=utf-8";
pub const SVG: &str = "image/svg+xml";
pub const PNG: &str = "image/png";
pub const WOFF2: &str = "font/woff2";
pub const WASM: &str = "application/wasm";

/// FNV-1a over `bytes`, at compile time.
pub const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// `x` as sixteen lowercase hex characters, at compile time.
pub const fn hex16(x: u64) -> [u8; 16] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = DIGITS[((x >> (60 - 4 * i)) & 0xf) as usize];
        i += 1;
    }
    out
}

/// One file of a table: its bytes, their digest and, for a text file, its
/// compressed copies ([`bake!`]), all baked in. Paths are the calling
/// crate's: under its `static/` folder, from its manifest directory, or in
/// its build's `OUT_DIR`.
///
/// - `asset!("name.ext", TYPE)`: a file under `static/`, fingerprinted.
/// - `asset!(from "name.ext", "path/from/the/crate", TYPE)`: a file
///   elsewhere, fingerprinted.
/// - `asset!(built "name.ext", "/path/in/out-dir", TYPE)`: a file the build
///   script makes, fingerprinted.
/// - `asset!(named "name-hash.ext", "/path/in/out-dir", TYPE)`: a chunk the
///   build script's bundler makes, its hash already in its name.
/// - `asset!(baked "name.ext", BAKED, TYPE)`: a file another module bakes
///   ([`bake!`], a stylesheet it also uses), fingerprinted.
/// - `asset!(text "name.ext", TEXT, TYPE)` and `asset!(bytes …)`: a
///   string or bytes another module holds, fingerprinted, not compressed.
/// - `asset!(public "name.ext", TYPE)` and
///   `asset!(public from "name.ext", "path", TYPE)`: a file whose bare name
///   others link.
#[macro_export]
macro_rules! asset {
    ($name:literal, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::bake!("static/", $name), $content_type, $crate::Policy::Fingerprinted)
    };
    (from $name:literal, $path:literal, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::bake!($path), $content_type, $crate::Policy::Fingerprinted)
    };
    (bytes $name:literal, $bytes:expr, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::Baked::plain($bytes), $content_type, $crate::Policy::Fingerprinted)
    };
    (text $name:literal, $text:expr, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::Baked::plain($text.as_bytes()), $content_type, $crate::Policy::Fingerprinted)
    };
    (baked $name:literal, $baked:expr, $content_type:expr) => {
        $crate::asset!(@entry $name, $baked, $content_type, $crate::Policy::Fingerprinted)
    };
    (named $name:literal, $path:literal, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::bake!(out_dir $path), $content_type, $crate::Policy::Hashed)
    };
    (built $name:literal, $path:literal, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::bake!(out_dir $path), $content_type, $crate::Policy::Fingerprinted)
    };
    (public $name:literal, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::bake!("static/", $name), $content_type, $crate::Policy::Public)
    };
    (public from $name:literal, $path:literal, $content_type:expr) => {
        $crate::asset!(@entry $name, $crate::bake!($path), $content_type, $crate::Policy::Public)
    };
    (@entry $name:literal, $baked:expr, $content_type:expr, $policy:expr) => {{
        const BAKED: $crate::Baked = $baked;
        // A bundle or a WebAssembly module can run to a megabyte.
        #[allow(long_running_const_eval)]
        const HEX: [u8; 16] = $crate::hex16($crate::fnv1a64(BAKED.bytes));
        const DIGEST: &str = match ::core::str::from_utf8(&HEX) {
            Ok(digest) => digest,
            Err(_) => panic!("hex is ASCII"),
        };
        $crate::Asset {
            name: $name,
            content_type: $content_type,
            bytes: BAKED.bytes,
            brotli: BAKED.brotli,
            gzip: BAKED.gzip,
            digest: DIGEST,
            policy: $policy,
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    static TABLE: &[Asset] = &[
        asset!(text "page.css", "body{}", CSS),
        asset!(from "lib.rs", "src/lib.rs", JS),
        asset!(public from "client.js", "Cargo.toml", JS),
    ];

    #[test]
    fn the_digest_is_computed_at_build_time_from_the_bytes() {
        const HEX: [u8; 16] = hex16(fnv1a64(b"hello"));
        assert_eq!(std::str::from_utf8(&HEX).unwrap(), "a430d84680aabd0b");
        assert_eq!(hex16(0), *b"0000000000000000");
        for asset in TABLE {
            assert_eq!(
                asset.digest,
                std::str::from_utf8(&hex16(fnv1a64(asset.bytes))).unwrap()
            );
        }
        assert_eq!(named(TABLE, "lib.rs").bytes, include_bytes!("lib.rs"));
    }

    #[test]
    fn a_file_is_published_at_its_digest_unless_others_link_it() {
        let css = named(TABLE, "page.css");
        let file = css.file_name();
        assert_eq!(file, format!("page.{}.css", css.digest));
        assert!(std::ptr::eq(published(TABLE, &file).unwrap(), css));
        assert_eq!(named(TABLE, "client.js").file_name(), "client.js");
        assert!(published(TABLE, "client.js").is_some());
        // The bare name of a fingerprinted file, or an old digest, is no
        // file's: only the current name is published.
        assert!(published(TABLE, "page.css").is_none());
        assert!(published(TABLE, "page.0123456789abcdef.css").is_none());
        assert!(published(TABLE, "").is_none());
        assert!(find(TABLE, "nothing.js").is_none());
    }

    #[test]
    fn a_text_file_is_baked_with_smaller_copies_and_other_files_without() {
        const CSS_FILE: Baked = bake!("tests/sample.css");
        let css = include_bytes!("../tests/sample.css");
        assert_eq!(CSS_FILE.bytes, css);
        let (brotli, gzip) = (CSS_FILE.brotli.unwrap(), CSS_FILE.gzip.unwrap());
        assert!(brotli.len() < css.len() / 2, "{}", brotli.len());
        assert!(gzip.len() < css.len() / 2, "{}", gzip.len());
        // gzip's header: the copy is a gzip stream.
        assert_eq!(&gzip[..2], &[0x1f, 0x8b]);
        // A Rust file, like a font or an image, isn't one a page compresses.
        const RUST: Baked = bake!("src/", "lib.rs");
        assert_eq!(RUST.bytes, include_bytes!("lib.rs"));
        assert!(RUST.brotli.is_none() && RUST.gzip.is_none());
        let table = asset!(baked "sample.css", CSS_FILE, CSS);
        assert_eq!(table.brotli, CSS_FILE.brotli);
    }

    #[test]
    fn files_joined_are_one_after_another_with_the_separator() {
        const BOTH: Baked = bake!(join "\n/* next */\n" ["tests/sample.css", "Cargo.toml"]);
        let joined = [
            &include_bytes!("../tests/sample.css")[..],
            b"\n/* next */\n",
            include_bytes!("../Cargo.toml"),
        ]
        .concat();
        assert_eq!(BOTH.bytes, joined);
        assert_eq!(BOTH.text(), std::str::from_utf8(&joined).unwrap());
    }

    #[test]
    fn every_weight_of_the_typeface_is_linked_where_the_page_says() {
        let faces = font_faces(|name| format!("/at/{name}"));
        for weight in FONT_WEIGHTS {
            assert!(
                faces.contains(&format!(
                    "font-weight:{weight};font-display:swap;src:url(\"/at/manrope-{weight}.woff2\")"
                )),
                "{faces}"
            );
        }
    }
}
