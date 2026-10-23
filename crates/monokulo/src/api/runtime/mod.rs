//! What the API's resources are written with: the catalog the macros fill,
//! who may call what, HAL-FORMS, problems, scalars, extractors, the router,
//! the OpenAPI document and the generated reference. See `crate::api`.

pub mod auth;
pub mod catalog;
pub mod examples;
pub mod extract;
pub mod hal;
pub mod openapi;
pub mod problem;
#[cfg(any(test, feature = "test-support"))]
pub mod recording;
pub mod reference;
pub mod scalars;
#[cfg(test)]
mod tests;

use crate::http::AppState;

/// Where the API's docs, and each relation's and problem's page, live.
pub const DOCS: &str = "https://docs.monokulo.com";

/// The largest request body the API reads: a plugin's 20 log entries fit.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

/// Where this Monokulo's paths start for a request, on the network it came
/// on: the path of its address there, without a trailing `/`. `/monokulo`
/// when `public_uri` is `https://host/monokulo` (a reverse proxy serves it
/// under that prefix and strips it before passing a request on); empty at
/// the root of its host, on the onion address (tor reaches monokulo
/// directly), and while no address is set. Every link the API gives
/// (`_links`, a form's `target`, `Location`) and the handshakes' `api`
/// start with it, so a client resolving them against the host it called
/// reaches the same Monokulo.
pub fn path_prefix(state: &AppState, extensions: &axum::http::Extensions) -> String {
    let network = state.network_of(crate::network::NetworkKind::of(extensions));
    network
        .public_uri()
        .and_then(|uri| url::Url::parse(&uri).ok())
        .map(|uri| uri.path().trim_end_matches('/').to_owned())
        .unwrap_or_default()
}

/// Every handler's route, from the catalog.
pub fn router() -> axum::Router<AppState> {
    catalog::handlers()
        .into_iter()
        .fold(axum::Router::new(), |router, handler| {
            (handler.route)(router)
        })
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
}
