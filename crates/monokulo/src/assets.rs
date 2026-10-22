//! Every file baked into the binary that a page or a script asks for by
//! URL, in one table, served from one route (`GET /static/{*file}`).
//!
//! Each entry is one line, `asset!("name.ext", TYPE)`: the file's bytes and
//! a digest of them, both computed when the binary is built. A page links a
//! file by its name (`views::script`, `views::stylesheet`, [`url`]) and
//! gets a URL carrying that digest, `/static/logo.3f9a1c2b4d5e6f70.svg`,
//! which a browser may keep for a year: a new release's pages link new
//! URLs. Only that URL is served: a fingerprinted file's bare name, or an
//! old digest, is a 404. The one file others link by a fixed name, the
//! embed library, is `public`: linked bare and checked on every use. The
//! digest and the naming are `static_assets`', shared with the GitHub Pages
//! site.
//!
//! Everything is served from this binary rather than a CDN, so a
//! self-hoster's pages have no third-party dependency in the payment path,
//! work offline and over Tor, and leak no visitor's address: a Google Fonts
//! link would send every visitor's IP to Google on every page, checkout
//! included.

use axum::extract::Path;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use static_assets::{asset, CSS, JS, PNG, SVG, WASM, WOFF2};
pub use static_assets::{Asset, Policy};

pub static ASSETS: &[Asset] = &[
    // -- The pages' own scripts and styles --------------------------------
    // Every page's style, linked in this order: the colours and other
    // tokens (the `ui` package's), this site's own components and pages
    // (`views/site.css`), and the shared components (the `ui` package's).
    asset!(baked "theme.css", ui::assets::TOKENS, CSS),
    asset!(baked "site.css", crate::views::SITE, CSS),
    asset!(baked "ui.css", ui::assets::COMPONENTS, CSS),
    // Partial page updates and server-sent events on the server-rendered
    // pages (`http::fx`). fixi and ssexi are vendored, pinned copies (see
    // their `.SOURCE` files); fx-glue is what they leave out on purpose.
    asset!("fx-glue.js", JS),
    asset!("fixi.js", JS),
    asset!("ssexi.js", JS),
    // The shared components' behaviours: each interactive one (a Select)
    // renders as an island this attaches to, loading the behaviour's chunk
    // ([`CHUNKS`]) the first time a page has one (`apps/engine`, built by
    // `build.rs`).
    asset!(built "ui-islands.js", "/web/ui-islands.js", JS),
    // Browser problem reports (`http::telemetry_client`).
    asset!("telemetry.js", JS),
    // The checkout embed (`views::checkout`), the QR decoder it and the
    // POS load for refund addresses, and the "Checking your connection"
    // page's challenge solver (`views::challenge`).
    asset!("checkout.js", JS),
    asset!("jsQR.js", JS),
    asset!("challenge.js", JS),
    // The engine page (`docs/engine_visualizer.md`; `apps/engine`, built
    // by `build.rs` with the islands, sharing their copy of Solid).
    asset!(built "engine-view.js", "/web/engine-view.js", JS),
    asset!(built "engine-view.css", "/web/engine-view.css", CSS),
    // A store's key entry, and making a wallet in the browser, each with
    // the WebAssembly module its script fetches (`build.rs`).
    asset!("key-custody.js", JS),
    asset!(built "key-custody.wasm", "/key_custody.wasm", WASM),
    asset!("wallet-setup.js", JS),
    asset!(built "wallet-setup.wasm", "/wallet_setup.wasm", WASM),
    // One page each: the admin settings page's node rows, the account
    // page's email question, the Logs page, the order form, the store page,
    // a store's domains (checked again while one waits).
    asset!("admin-settings.js", JS),
    asset!("account.js", JS),
    asset!("logs.js", JS),
    asset!("query.js", JS),
    asset!("create-order.js", JS),
    asset!("domains.js", JS),
    // The POS app (`apps/pos`, built by `build.rs`).
    asset!(built "pos-app.js", "/pos/pos-app.js", JS),
    asset!(built "pos-app.css", "/pos/pos-app.css", CSS),
    // -- Fonts and marks ---------------------------------------------------
    // The UI typeface (site.css's `@font-face`), Latin subset only: this
    // UI has no other script.
    asset!(bytes "manrope-500.woff2", ui::assets::MANROPE_500, WOFF2),
    asset!(bytes "manrope-700.woff2", ui::assets::MANROPE_700, WOFF2),
    asset!(bytes "manrope-800.woff2", ui::assets::MANROPE_800, WOFF2),
    // The full Monokulo mark (with the chain and the facet lines),
    // ink-on-paper, for other sites to link to; the pages draw it inline
    // (`views::logo_mark`). The square version is every page's icon. Both
    // written by `cargo xtask logo`.
    asset!("logo.svg", SVG),
    asset!("favicon.svg", SVG),
    // Wallet app icons (`wallets::WALLET_APPS`).
    asset!("wallet-logos/cake.png", PNG),
    asset!("wallet-logos/monerocom.png", PNG),
    asset!("wallet-logos/stack.png", PNG),
    asset!("wallet-logos/feather.png", PNG),
    asset!("wallet-logos/gui.png", PNG),
    // -- Linked by others --------------------------------------------------
    // The thin embed library a merchant's static site `<script src>`s
    // (`docs/fx_refactor.md` decision 3): its URL is theirs to keep.
    asset!(public "monokulo-client.js", JS),
];

/// The chunks `ui-islands.js` and `engine-view.js` import: Solid, each
/// island's behaviour and what they share. Their names carry Vite's hash,
/// and they're served at those names (`build.rs` lists them).
pub static CHUNKS: &[Asset] = include!(concat!(env!("OUT_DIR"), "/web/chunks.rs"));

/// The chunks an entry imports (by its name, `"engine-view.js"`) or an
/// island's behaviour needs (by its component's, `"Select"`), from the
/// build's manifest (`build.rs`).
static PRELOADS: &[(&str, &[&str])] = include!(concat!(env!("OUT_DIR"), "/web/preloads.rs"));

/// The URLs of the chunks `script` (an entry or an island's component)
/// needs, for a page to ask for them alongside it: none for anything else.
pub fn preloads(script: &str) -> impl Iterator<Item = String> {
    PRELOADS
        .iter()
        .find(|(name, _)| *name == script)
        .map_or(&[][..], |(_, files)| files)
        .iter()
        .map(|file| format!("/static/{file}"))
}

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";

/// The asset called `name`, if there is one.
pub fn find(name: &str) -> Option<&'static Asset> {
    static_assets::find(ASSETS, name)
}

/// The URL a page links the asset called `name` at: with its digest, if
/// it's fingerprinted. A name that isn't an asset's is a mistake in the
/// view, caught by any test that renders it.
pub fn url(name: &str) -> String {
    format!("/static/{}", static_assets::named(ASSETS, name).file_name())
}

/// `GET /static/{*file}`: the asset published as `file`, its URL's last
/// part. A fingerprinted file or a chunk is kept for a year; a public one
/// is checked on every use.
pub async fn serve(headers: HeaderMap, Path(file): Path<String>) -> Response {
    let Some(asset) =
        static_assets::published(ASSETS, &file).or_else(|| static_assets::published(CHUNKS, &file))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cache_control = match asset.policy {
        Policy::Fingerprinted | Policy::Hashed => IMMUTABLE,
        Policy::Public => REVALIDATE,
    };
    respond(&headers, asset, cache_control)
}

/// The asset called `name`, as a fixed route serves it (the embed library
/// has one, for its CORS headers).
pub fn respond_named(headers: &HeaderMap, name: &str) -> Response {
    respond(headers, static_assets::named(ASSETS, name), REVALIDATE)
}

/// The asset, with `Cache-Control` and an `ETag` of its content: a browser
/// that already has it is answered `304 Not Modified` with no body. A text
/// file goes compressed to a browser that takes it, brotli else gzip, in
/// the copy baked in with it (`static_assets::bake!`).
fn respond(headers: &HeaderMap, asset: &'static Asset, cache_control: &'static str) -> Response {
    let encoding = chosen(headers, asset);
    // Each encoding is its own version of the file, with its own tag.
    let etag = match encoding {
        Some((token, _)) => format!("\"{}-{token}\"", asset.digest),
        None => format!("\"{}\"", asset.digest),
    };
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|tags| {
            tags.split(',')
                .any(|tag| tag.trim() == etag || tag.trim() == "*")
        });
    let mut response = if fresh {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let (body, content_encoding) = match encoding {
            Some((token, bytes)) => (bytes, Some(token)),
            None => (asset.bytes, None),
        };
        let mut response = ([(header::CONTENT_TYPE, asset.content_type)], body).into_response();
        if let Some(token) = content_encoding {
            response
                .headers_mut()
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static(token));
        }
        response
    };
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    response_headers.insert(header::ETAG, HeaderValue::from_str(&etag).expect("an ETag"));
    if asset.brotli.is_some() || asset.gzip.is_some() {
        response_headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    response
}

/// The copy to send, by its `Content-Encoding`: brotli if the browser takes
/// it, else gzip, else none (the file as it is). One it lists with `q=0`
/// it refuses.
fn chosen(headers: &HeaderMap, asset: &'static Asset) -> Option<(&'static str, &'static [u8])> {
    let value = headers.get(header::ACCEPT_ENCODING)?.to_str().ok()?;
    let takes = |token: &str| {
        value.split(',').any(|offer| {
            let mut parts = offer.split(';').map(str::trim);
            let name = parts.next().unwrap_or("");
            let refused = parts.any(|param| {
                param
                    .strip_prefix("q=")
                    .and_then(|q| q.parse::<f32>().ok())
                    .is_some_and(|q| q == 0.0)
            });
            name.eq_ignore_ascii_case(token) && !refused
        })
    };
    [("br", asset.brotli), ("gzip", asset.gzip)]
        .into_iter()
        .find_map(|(token, copy)| copy.filter(|_| takes(token)).map(|copy| (token, copy)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_computed_at_build_time_from_the_bytes() {
        let asset = find("fx-glue.js").unwrap();
        assert_eq!(asset.bytes, include_bytes!("../static/fx-glue.js"));
        assert_eq!(
            asset.digest,
            std::str::from_utf8(&static_assets::hex16(static_assets::fnv1a64(asset.bytes)))
                .unwrap()
        );
    }

    #[test]
    fn every_name_links_to_a_url_the_route_serves() {
        for asset in ASSETS {
            let linked = url(asset.name);
            let file = linked.strip_prefix("/static/").unwrap();
            let found =
                static_assets::published(ASSETS, file).unwrap_or_else(|| panic!("{linked}"));
            assert!(std::ptr::eq(found, asset), "{linked}");
        }
        assert_eq!(url("monokulo-client.js"), "/static/monokulo-client.js");
        let logo = url("logo.svg");
        assert!(
            logo.starts_with("/static/logo.")
                && logo.ends_with(".svg")
                && logo.len() == "/static/logo..svg".len() + 16,
            "{logo}"
        );
    }

    #[tokio::test]
    async fn served_with_the_cache_policy_its_name_asks_for_and_only_at_that_name() {
        let cache = |file: &str| {
            let file = file.to_string();
            async move {
                let response = serve(HeaderMap::new(), Path(file)).await;
                (
                    response.status(),
                    response
                        .headers()
                        .get(header::CACHE_CONTROL)
                        .map(|v| v.to_str().unwrap().to_string()),
                )
            }
        };
        let current = url("admin-settings.js");
        assert_eq!(
            cache(current.strip_prefix("/static/").unwrap()).await,
            (StatusCode::OK, Some(IMMUTABLE.to_string()))
        );
        assert_eq!(
            cache("monokulo-client.js").await,
            (StatusCode::OK, Some(REVALIDATE.to_string()))
        );
        for not_published in [
            "admin-settings.0123456789abcdef.js",
            "admin-settings.js",
            "nothing.js",
            "",
        ] {
            assert_eq!(
                cache(not_published).await,
                (StatusCode::NOT_FOUND, None),
                "{not_published}"
            );
        }
    }

    fn fixi() -> String {
        static_assets::named(ASSETS, "fixi.js").file_name()
    }

    #[tokio::test]
    async fn a_browser_that_has_the_file_is_told_so_without_a_body() {
        let first = serve(HeaderMap::new(), Path(fixi())).await;
        let etag = first.headers()[header::ETAG].clone();
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let again = serve(headers, Path(fixi())).await;
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn every_chunk_the_pages_scripts_import_is_served_at_that_name_for_a_year() {
        assert!(CHUNKS.iter().any(|chunk| chunk.name.starts_with("solid-")));
        for chunk in CHUNKS {
            assert_eq!(chunk.policy, Policy::Hashed);
            assert!(
                ASSETS.iter().all(|asset| asset.file_name() != chunk.name),
                "{}",
                chunk.name
            );
            let response = serve(HeaderMap::new(), Path(chunk.name.to_string())).await;
            assert_eq!(response.status(), StatusCode::OK, "{}", chunk.name);
            assert_eq!(response.headers()[header::CACHE_CONTROL], IMMUTABLE);
        }
        // What an entry or a chunk imports is next to it: one of the chunks.
        let imported = |bytes: &[u8]| -> Vec<String> {
            let text = std::str::from_utf8(bytes).unwrap();
            text.split("\"./")
                .skip(1)
                .filter_map(|rest| rest.split_once('"').map(|(name, _)| name.to_string()))
                .filter(|name| name.ends_with(".js"))
                .collect()
        };
        let scripts = ["ui-islands.js", "engine-view.js"]
            .map(|name| find(name).unwrap())
            .into_iter()
            .chain(CHUNKS);
        let mut seen = 0;
        for script in scripts {
            for name in imported(script.bytes) {
                seen += 1;
                assert!(
                    static_assets::published(CHUNKS, &name).is_some(),
                    "{} imports {name}, which isn't served",
                    script.name
                );
            }
        }
        assert!(seen > 0, "the entries import their chunks");
    }

    async fn fetch(file: &str, accept_encoding: Option<&str>) -> (axum::http::HeaderMap, Vec<u8>) {
        let mut headers = HeaderMap::new();
        if let Some(value) = accept_encoding {
            headers.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_str(value).unwrap(),
            );
        }
        let response = serve(headers, Path(file.to_string())).await;
        assert_eq!(response.status(), StatusCode::OK, "{file}");
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (headers, body.to_vec())
    }

    #[tokio::test]
    async fn a_script_goes_compressed_to_a_browser_that_takes_it_and_unpacks_to_the_file() {
        use std::io::Read;
        let asset = find("jsQR.js").unwrap();
        let file = asset.file_name();

        let (headers, body) = fetch(&file, Some("gzip, deflate, br, zstd")).await;
        assert_eq!(headers[header::CONTENT_ENCODING], "br");
        assert_eq!(headers[header::VARY], "Accept-Encoding");
        assert_eq!(
            headers[header::ETAG].to_str().unwrap(),
            format!("\"{}-br\"", asset.digest)
        );
        assert!(body.len() < asset.bytes.len() / 4, "{} bytes", body.len());
        let mut unpacked = Vec::new();
        brotli::Decompressor::new(&body[..], 4096)
            .read_to_end(&mut unpacked)
            .unwrap();
        assert_eq!(unpacked, asset.bytes);

        // Brotli refused, gzip it is.
        let (headers, body) = fetch(&file, Some("br;q=0, gzip")).await;
        assert_eq!(headers[header::CONTENT_ENCODING], "gzip");
        let mut unpacked = Vec::new();
        flate2::read::GzDecoder::new(&body[..])
            .read_to_end(&mut unpacked)
            .unwrap();
        assert_eq!(unpacked, asset.bytes);

        // Neither: the file as it is, still saying it varies.
        let (headers, body) = fetch(&file, None).await;
        assert!(headers.get(header::CONTENT_ENCODING).is_none());
        assert_eq!(headers[header::VARY], "Accept-Encoding");
        assert_eq!(
            headers[header::ETAG].to_str().unwrap(),
            format!("\"{}\"", asset.digest)
        );
        assert_eq!(body, asset.bytes);
    }

    #[tokio::test]
    async fn the_compressed_copy_has_its_own_tag_for_a_browser_that_has_it() {
        let file = fixi();
        let (headers, _) = fetch(&file, Some("br")).await;
        assert_eq!(headers[header::CONTENT_ENCODING], "br");
        let mut again = HeaderMap::new();
        again.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("br"));
        again.insert(header::IF_NONE_MATCH, headers[header::ETAG].clone());
        let response = serve(again, Path(file)).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
    }

    #[tokio::test]
    async fn a_font_is_sent_as_it_is() {
        let font = find("manrope-500.woff2").unwrap();
        let (headers, body) = fetch(&font.file_name(), Some("br, gzip")).await;
        assert!(headers.get(header::CONTENT_ENCODING).is_none());
        assert!(headers.get(header::VARY).is_none());
        assert_eq!(body, font.bytes);
    }

    #[test]
    fn a_behaviour_and_an_entry_name_the_chunks_they_need_and_each_is_served() {
        let select: Vec<String> = preloads("Select").collect();
        assert!(select[0].starts_with("/static/Select-"), "{select:?}");
        assert!(
            select.iter().any(|url| url.starts_with("/static/solid-")),
            "{select:?}"
        );
        let engine: Vec<String> = preloads("engine-view.js").collect();
        assert!(
            engine.iter().any(|url| url.starts_with("/static/solid-")),
            "{engine:?}"
        );
        assert_eq!(preloads("fixi.js").count(), 0);
        for (_, files) in PRELOADS {
            for file in *files {
                assert!(static_assets::published(CHUNKS, file).is_some(), "{file}");
            }
        }
    }

    #[test]
    fn every_script_style_and_module_is_baked_with_its_compressed_copies() {
        use std::io::Read;
        for asset in ASSETS.iter().chain(CHUNKS) {
            let text = [JS, CSS, SVG, WASM].contains(&asset.content_type);
            if !text {
                // Fonts and images are compressed already.
                assert!(
                    asset.brotli.is_none() && asset.gzip.is_none(),
                    "{}",
                    asset.name
                );
                continue;
            }
            // Every text file here comes out smaller, bar the tiniest.
            if asset.bytes.len() < 512 {
                continue;
            }
            let brotli = asset
                .brotli
                .unwrap_or_else(|| panic!("{} has no brotli copy", asset.name));
            let gzip = asset
                .gzip
                .unwrap_or_else(|| panic!("{} has no gzip copy", asset.name));
            assert!(
                brotli.len() <= gzip.len() && gzip.len() < asset.bytes.len(),
                "{}",
                asset.name
            );
            let mut unpacked = Vec::new();
            brotli::Decompressor::new(brotli, 4096)
                .read_to_end(&mut unpacked)
                .unwrap();
            assert_eq!(unpacked, asset.bytes, "{}", asset.name);
        }
        // The stylesheets made of several files, and the one from this crate.
        for name in ["ui.css", "theme.css", "site.css"] {
            assert!(find(name).unwrap().brotli.is_some(), "{name}");
        }
    }
}
