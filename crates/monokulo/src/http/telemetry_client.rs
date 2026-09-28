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
