//! `POST /telemetry/client`: problem reports from `static/telemetry.js`
//! (structured_logging.md 2.5).
//!
//! Anyone can post here, so nothing in a report is trusted: the body is
//! capped (see the route in `http::mod`), every text is clipped, the level
//! is always `warn` and every line carries `source = "browser"`, whatever
//! the report says, and the route is counted by the abuse limits like any other. A
//! report joins the trace of the page that sent it, when its `traceparent`
//! parses.

use axum::body::Bytes;
use axum::http::StatusCode;
use serde::Deserialize;

/// Most bytes a report may have; the route's body limit.
pub const MAX_BODY_BYTES: usize = 8 * 1024;
const MAX_TEXT: usize = 1000;

#[derive(Deserialize)]
struct ClientReport {
    traceparent: Option<String>,
    kind: String,
    message: String,
    detail: Option<String>,
    page: Option<String>,
}

/// Up to `MAX_TEXT` characters, control characters replaced, so a report
/// can't forge extra lines in the readable output format.
fn clip(text: &str) -> String {
    text.chars().take(MAX_TEXT).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

pub async fn client_report(body: Bytes) -> StatusCode {
    // Parsed from the bytes, not with the `Json` extractor, so the content
    // type doesn't matter (a beacon's can vary).
    let Ok(report) = serde_json::from_slice::<ClientReport>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let span = tracing::info_span!(parent: None, "browser report", source = "browser");
    if let Some(traceparent) = &report.traceparent {
        telemetry::trace::set_remote_parent(&span, traceparent);
    }
    let _entered = span.enter();
    let (kind, page, detail) = (clip(&report.kind), report.page.as_deref().map(clip), report.detail.as_deref().map(clip));
    tracing::warn!(browser.kind = %kind, url.path = page, detail, "{}", clip(&report.message));
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_clipped_and_lose_their_control_characters() {
        assert_eq!(clip("a\nb\r\tc"), "a b  c");
        assert_eq!(clip(&"x".repeat(5000)).len(), MAX_TEXT);
    }

    #[tokio::test]
    async fn a_report_that_is_not_json_is_refused() {
        assert_eq!(client_report(Bytes::from_static(b"nope")).await, StatusCode::BAD_REQUEST);
        let ok = br#"{"kind":"error","message":"x is undefined","page":"/dashboard"}"#;
        assert_eq!(client_report(Bytes::from_static(ok)).await, StatusCode::NO_CONTENT);
    }
}

/// `POST /pay/{pk}/logs`: warnings and errors the WooCommerce plugin
/// forwards when its "Send errors to Monokulo" option is on
/// (structured_logging.md 2.4). Authenticated with the store's secret key,
/// so the store is known and the level can be trusted as far as `error`;
/// texts are still clipped. Each entry joins the plugin request's trace
/// when it names one.
pub mod plugin {
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::Json;
    use serde::Deserialize;

    use super::clip;
    use crate::http::store_key::{self, KeyCheck};
    use crate::http::AppState;

    /// Most entries one request may carry, and the route's body limit.
    pub const MAX_ENTRIES: usize = 20;
    pub const MAX_BODY_BYTES: usize = 32 * 1024;

    #[derive(Deserialize)]
    pub struct PluginLogs {
        entries: Vec<PluginEntry>,
    }

    #[derive(Deserialize)]
    struct PluginEntry {
        /// WooCommerce's level names (`WC_Log_Levels`).
        level: String,
        message: String,
        trace_id: Option<String>,
    }

    pub async fn forward(State(state): State<AppState>, Path(pk): Path<String>, headers: HeaderMap, Json(logs): Json<PluginLogs>) -> StatusCode {
        if store_key::check(&state, &pk, &headers) != KeyCheck::Valid {
            return StatusCode::UNAUTHORIZED;
        }
        let Ok(Some(store)) = state.db.lock().get_store_connection_by_public_key(&pk) else {
            return StatusCode::UNAUTHORIZED;
        };
        if logs.entries.len() > MAX_ENTRIES {
            return StatusCode::PAYLOAD_TOO_LARGE;
        }
        for entry in logs.entries {
            let span = tracing::info_span!(parent: None, "woocommerce report", source = "woocommerce", store.id = %store.id);
            if let Some(trace_id) = entry.trace_id.as_deref().filter(|t| telemetry::store::api::is_trace_id(t)) {
                // The plugin names only its trace, not a span in it.
                telemetry::trace::set_remote_parent(&span, &format!("00-{trace_id}-0000000000000001-01"));
            }
            let _entered = span.enter();
            let message = clip(&entry.message);
            match entry.level.as_str() {
                "emergency" | "alert" | "critical" | "error" => tracing::error!("{message}"),
                "warning" => tracing::warn!("{message}"),
                // Anything quieter isn't forwarded by the plugin; ignore it.
                _ => {}
            }
        }
        StatusCode::NO_CONTENT
    }
}
