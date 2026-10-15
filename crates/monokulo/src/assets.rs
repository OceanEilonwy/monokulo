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
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use static_assets::{asset, CSS, JS, PNG, SVG, WASM, WOFF2};
pub use static_assets::{Asset, Policy};

pub static ASSETS: &[Asset] = &[
    // -- The pages' own scripts and styles --------------------------------
    // Every page's style: the colours (`views/theme.css`) and the
    // components (`views/site.css`), linked in that order.
    asset!(text "theme.css", crate::views::THEME_CSS, CSS),
    asset!(text "site.css", crate::views::SITE_CSS, CSS),
    // Partial page updates and server-sent events on the server-rendered
    // pages (`http::fx`). fixi and ssexi are vendored, pinned copies (see
    // their `.SOURCE` files); fx-glue is what they leave out on purpose.
    asset!("fx-glue.js", JS),
    asset!("fixi.js", JS),
    asset!("ssexi.js", JS),
    // Every dropdown (`views::controls`).
    asset!(from "mk-select.js", "../../packages/ui/src/components/select.js", JS),
    // Every settings form (`views::settings`).
    asset!(from "settings-form.js", "../../packages/ui/src/components/settings.js", JS),
    // Browser problem reports (`http::telemetry_client`).
    asset!("telemetry.js", JS),
    // The checkout embed (`views::checkout`), the QR decoder it and the
    // POS load for refund addresses, and the "Checking your connection"
    // page's challenge solver (`views::challenge`).
    asset!("checkout.js", JS),
    asset!("jsQR.js", JS),
    asset!("challenge.js", JS),
    // The engine page (`docs/engine_visualizer.md`).
    asset!("engine-view.js", JS),
    // A store's key entry, and making a wallet in the browser, each with
    // the WebAssembly module its script fetches (`build.rs`).
    asset!("key-custody.js", JS),
    asset!(built "key-custody.wasm", "/key_custody.wasm", WASM),
    asset!("wallet-setup.js", JS),
    asset!(built "wallet-setup.wasm", "/wallet_setup.wasm", WASM),
    // One page each: the admin settings page's node rows, the account
    // page's email question, the Logs page, the order form, the store page.
    asset!("admin-settings.js", JS),
    asset!("account.js", JS),
    asset!("logs.js", JS),
    asset!("create-order.js", JS),
    asset!("store-detail.js", JS),
    asset!("confirm-dialogs.js", JS),
    // The POS app (`pos-ui/`, built by `build.rs`).
    asset!(built "pos-app.js", "/pos-ui/pos-app.js", JS),
    asset!(built "pos-app.css", "/pos-ui/pos-app.css", CSS),
    // -- Fonts and marks ---------------------------------------------------
    // The UI typeface (site.css's `@font-face`), Latin subset only: this
    // UI has no other script.
    asset!(from "manrope-500.woff2", "../../packages/ui/src/fonts/manrope-500.woff2", WOFF2),
    asset!(from "manrope-700.woff2", "../../packages/ui/src/fonts/manrope-700.woff2", WOFF2),
    asset!(from "manrope-800.woff2", "../../packages/ui/src/fonts/manrope-800.woff2", WOFF2),
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
/// part. A fingerprinted file is kept for a year; a public one is checked
/// on every use.
pub async fn serve(headers: HeaderMap, Path(file): Path<String>) -> Response {
    let Some(asset) = static_assets::published(ASSETS, &file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cache_control = match asset.policy {
        Policy::Fingerprinted => IMMUTABLE,
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
/// that already has it is answered `304 Not Modified` with no body.
fn respond(headers: &HeaderMap, asset: &'static Asset, cache_control: &'static str) -> Response {
    let etag = format!("\"{}\"", asset.digest);
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|tags| {
            tags.split(',')
                .any(|tag| tag.trim() == etag || tag.trim() == "*")
        });
    let cache = [
        (header::CACHE_CONTROL, cache_control.to_string()),
        (header::ETAG, etag),
    ];
    if fresh {
        return (StatusCode::NOT_MODIFIED, cache).into_response();
    }
    (
        cache,
        [(header::CONTENT_TYPE, asset.content_type)],
        asset.bytes,
    )
        .into_response()
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
}
