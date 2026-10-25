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

use crate::api::runtime::problem::{Problem, ProblemDetails};
use crate::db::Database;
use crate::db::Db;

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
pub(crate) fn clip(text: &str) -> String {
    text.chars()
        .take(MAX_TEXT)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Whether reports from `page` may be kept: `false` for a page about a
/// store that hasn't opted in to client logs.
pub fn page_may_report(db: &Db, page: &str) -> bool {
    if let Some(store) = super::store_of_path(page) {
        return db
            .client_logging(&crate::db::ConnectionId::new(store))
            .unwrap_or(false);
    }
    if let Some(order_id) = super::paths::checkout_order(page) {
        return db.client_logging_by_order(&order_id).unwrap_or(false);
    }
    true
}

pub async fn client_report(
    State(db): State<Database>,
    body: Bytes,
) -> Result<StatusCode, ProblemDetails> {
    // Parsed from the bytes, not with the `Json` extractor, so the content
    // type doesn't matter (a beacon's can vary).
    let report = serde_json::from_slice::<ClientReport>(&body)
        .map_err(|e| Problem::InvalidRequest.detail(format!("That isn't a client report: {e}.")))?;
    let page = report.page.clone().unwrap_or_default();
    let may_report = db
        .read(move |db| Ok::<_, crate::db::DbError>(page_may_report(db, &page)))
        .await
        .unwrap_or(false);
    if !may_report {
        // Accepted and dropped, as a store that opted out would expect.
        return Ok(StatusCode::NO_CONTENT);
    }
    let span = tracing::info_span!(parent: None, "browser report", source = "browser");
    if let Some(traceparent) = &report.traceparent {
        telemetry::trace::set_remote_parent(&span, traceparent);
    }
    let _entered = span.enter();
    let (kind, page, detail) = (
        clip(&report.kind),
        report.page.as_deref().map(clip),
        report.detail.as_deref().map(clip),
    );
    tracing::warn!(browser.kind = %kind, url.path = page, detail, "{}", clip(&report.message));
    Ok(StatusCode::NO_CONTENT)
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
        db.create_user(
            &engine_protocol::ids::UserId::new("u1"),
            "a@example.com",
            "hash",
            false,
            0,
        )
        .unwrap();
        crate::db::test_rows::store_on_wallet(
            &db,
            &engine_protocol::ids::ConnectionId::new("c1"),
            &engine_protocol::ids::UserId::new("u1"),
            "https://shop.example.com",
            "pk_1",
            "sk_1",
            0,
            "XMR",
        )
        .unwrap();
        // An order of the store, so its checkout finds it.
        db.create_order_currency_metadata(
            &engine_protocol::ids::ConnectionId::new("c1"),
            &engine_protocol::ids::OrderId::new("o1"),
            "XMR",
            "1",
            pricing::xmr_amount::Piconero(1),
            "xmr",
            0,
            "XMR",
            None,
            10,
            false,
            None,
            true,
        )
        .unwrap();
        db
    }

    #[test]
    fn pages_about_a_store_report_only_once_it_opted_in() {
        let db = db_with_store();
        assert!(
            page_may_report(&db, "/admin/logs"),
            "admin pages always report"
        );
        assert!(page_may_report(&db, "/"));
        assert!(page_may_report(&db, ""));
        assert!(!page_may_report(&db, "/stores/c1/settings"));
        assert!(!page_may_report(&db, "/stores/c1/pos"));
        assert!(!page_may_report(&db, "/checkout/o1"));
        assert!(
            !page_may_report(&db, "/stores/unknown/orders"),
            "no store, no reports"
        );
        db.set_client_logging(&engine_protocol::ids::ConnectionId::new("c1"), true)
            .unwrap();
        assert!(page_may_report(&db, "/stores/c1/settings"));
        assert!(page_may_report(&db, "/checkout/o1"));
    }

    #[test]
    fn a_report_that_is_not_json_is_refused() {
        assert!(serde_json::from_slice::<ClientReport>(b"nope").is_err());
        let ok = br#"{"kind":"error","message":"x is undefined","page":"/"}"#;
        assert!(serde_json::from_slice::<ClientReport>(ok).is_ok());
    }
}
