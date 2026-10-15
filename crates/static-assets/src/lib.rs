//! Files a page links by URL, baked into the binary that writes or serves
//! the page and named by their content.
//!
//! A crate lists its files once, one `asset!` line each, in a `&[Asset]`
//! table: the file's bytes and a digest of them, both computed when that
//! crate is built. A page links a file by its name and gets a file name
//! carrying the digest, `logo.3f9a1c2b4d5e6f70.svg` ([`Asset::file_name`]),
//! which a browser may keep for a year: new content is a new name. The only
//! files linked by their bare name are [`Policy::Public`] ones, whose name
//! others link and keep.
//!
//! Two crates use it. monokulo serves its table from `GET /static/{*file}`
//! (`monokulo::assets`); `cargo xtask pages` writes its table into each part
//! of the GitHub Pages site's `assets/` folder.

/// Whether a file's name carries its digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Linked by its own pages at a name carrying its digest: kept for a year.
    Fingerprinted,
    /// Linked by others at its bare name, so that name is theirs to keep:
    /// checked on every use.
    Public,
}

#[derive(Debug)]
pub struct Asset {
    /// The file's name, with its extension: what pages ask for.
    pub name: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
    /// Sixteen hex characters naming this version of the file: FNV-1a of
    /// its bytes, computed when the binary was built. A version label, not
    /// an integrity check.
    pub digest: &'static str,
    pub policy: Policy,
}

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
            Policy::Public => self.name.to_string(),
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

/// One file of a table: its bytes and their digest, both baked in. Paths
/// are the calling crate's: under its `static/` folder, from its manifest
/// directory, or in its build's `OUT_DIR`.
///
/// - `asset!("name.ext", TYPE)`: a file under `static/`, fingerprinted.
/// - `asset!(from "name.ext", "path/from/the/crate", TYPE)`: a file
///   elsewhere, fingerprinted.
/// - `asset!(built "name.ext", "/path/in/out-dir", TYPE)`: a file the build
///   script makes, fingerprinted.
/// - `asset!(text "name.ext", TEXT, TYPE)`: a file another module holds
///   as a string, fingerprinted.
/// - `asset!(public "name.ext", TYPE)` and
///   `asset!(public from "name.ext", "path", TYPE)`: a file whose bare name
///   others link.
#[macro_export]
macro_rules! asset {
    ($name:literal, $content_type:expr) => {
        $crate::asset!(from $name, concat!("static/", $name), $content_type)
    };
    (from $name:literal, $path:expr, $content_type:expr) => {
        $crate::asset!(@entry $name, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $path)), $content_type, $crate::Policy::Fingerprinted)
    };
    (bytes $name:literal, $bytes:expr, $content_type:expr) => {
        $crate::asset!(@entry $name, $bytes, $content_type, $crate::Policy::Fingerprinted)
    };
    (text $name:literal, $text:expr, $content_type:expr) => {
        $crate::asset!(@entry $name, $text.as_bytes(), $content_type, $crate::Policy::Fingerprinted)
    };
    (built $name:literal, $path:literal, $content_type:expr) => {
        $crate::asset!(@entry $name, include_bytes!(concat!(env!("OUT_DIR"), $path)), $content_type, $crate::Policy::Fingerprinted)
    };
    (public $name:literal, $content_type:expr) => {
        $crate::asset!(public from $name, concat!("static/", $name), $content_type)
    };
    (public from $name:literal, $path:expr, $content_type:expr) => {
        $crate::asset!(@entry $name, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $path)), $content_type, $crate::Policy::Public)
    };
    (@entry $name:literal, $bytes:expr, $content_type:expr, $policy:expr) => {{
        const BYTES: &[u8] = $bytes;
        // A bundle or a WebAssembly module can run to a megabyte.
        #[allow(long_running_const_eval)]
        const HEX: [u8; 16] = $crate::hex16($crate::fnv1a64(BYTES));
        const DIGEST: &str = match ::core::str::from_utf8(&HEX) {
            Ok(digest) => digest,
            Err(_) => panic!("hex is ASCII"),
        };
        $crate::Asset {
            name: $name,
            content_type: $content_type,
            bytes: BYTES,
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
