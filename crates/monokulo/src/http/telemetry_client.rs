//! `POST /telemetry/client`: problem reports from `static/telemetry.js`
//! (structured_logging.md 2.5).
//!
//! Anyone can post here, so nothing in a report is trusted: the body is
//! capped (see the route in `http::mod`), every text is clipped, the level
//! is always `warn` and every line carries `source = "browser"`, whatever
//! the report says, and the route is counted by the abuse limits like any other. A
//! report joins the trace of the page that sent it, when its `traceparent`
//! parses.
//!
//! A report from one store's pages (its dashboard pages, POS or checkout)
//! is dropped unless that store opted in to client logs
//! (`db::Db::client_logging`); the pages of such a store don't load the
//! script in the first place. Reports from pages about no store (the admin
//! pages, the dashboard home) are always kept: they carry no merchant or
//! customer data.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

use crate::db::Db;
use crate::http::AppState;

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

/// Whether reports from `page` may be kept: `false` for a page about a
/// store that hasn't opted in to client logs.
pub fn page_may_report(db: &Db, page: &str) -> bool {
    if let Some(store) = super::store_of_path(page) {
        return db.client_logging(store).unwrap_or(false);
    }
    if let Some(pk) = super::embed_domains::public_key_of_pay_path(page) {
        return db.client_logging_by_public_key(pk).unwrap_or(false);
    }
    true
}

pub async fn client_report(State(state): State<AppState>, body: Bytes) -> StatusCode {
    // Parsed from the bytes, not with the `Json` extractor, so the content
    // type doesn't matter (a beacon's can vary).
    let Ok(report) = serde_json::from_slice::<ClientReport>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    if !page_may_report(&state.db.lock(), report.page.as_deref().unwrap_or("")) {
        // Accepted and dropped, as a store that opted out would expect.
        return StatusCode::NO_CONTENT;
    }
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

    fn db_with_store() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.create_user("u1", "a@example.com", "hash", false, 0).unwrap();
        db.create_store_connection("c1", "u1", "woocommerce", "https://shop.example.com", "pk_1", "sk_1", "http://127.0.0.1:1", 0, "XMR").unwrap();
        db
    }

    #[test]
    fn pages_about_a_store_report_only_once_it_opted_in() {
        let db = db_with_store();
        assert!(page_may_report(&db, "/dashboard/admin/logs"), "admin pages always report");
        assert!(page_may_report(&db, "/dashboard"));
        assert!(page_may_report(&db, ""));
        assert!(!page_may_report(&db, "/dashboard/stores/c1/settings"));
        assert!(!page_may_report(&db, "/dashboard/stores/c1/pos"));
        assert!(!page_may_report(&db, "/pay/pk_1/orders/o1"));
        assert!(!page_may_report(&db, "/dashboard/stores/unknown/orders"), "no store, no reports");
        db.set_client_logging("c1", true).unwrap();
        assert!(page_may_report(&db, "/dashboard/stores/c1/settings"));
        assert!(page_may_report(&db, "/pay/pk_1/orders/o1"));
    }

    #[test]
    fn a_report_that_is_not_json_is_refused() {
        assert!(serde_json::from_slice::<ClientReport>(b"nope").is_err());
        let ok = br#"{"kind":"error","message":"x is undefined","page":"/dashboard"}"#;
        assert!(serde_json::from_slice::<ClientReport>(ok).is_ok());
    }
}

/// `POST /pay/{pk}/logs`: warnings and errors the WooCommerce plugin
/// forwards when its "Send errors to Monokulo" option is on
/// (structured_logging.md 2.4), kept only when the store opted in to
/// client logs. Authenticated with the store's secret key,
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
        // The store's own setting wins over the plugin's option: refused
        // unless the store opted in to client logs. (403, not a silent 204:
        // the shop's server is authenticated, so it can be told why.)
        if !state.db.lock().client_logging(&store.id).unwrap_or(false) {
            return StatusCode::FORBIDDEN;
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
