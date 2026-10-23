//! What the API's resources are written with: the catalog the macros fill,
//! who may call what, HAL-FORMS, problems, scalars, extractors, the router,
//! the OpenAPI document and the generated reference. See `crate::api`.

pub mod auth;
pub mod catalog;
pub mod client;
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

/// Every handler's route, from the catalog.
pub fn router() -> axum::Router<AppState> {
    catalog::handlers()
        .into_iter()
        .fold(axum::Router::new(), |router, handler| {
            (handler.route)(router)
        })
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
}
