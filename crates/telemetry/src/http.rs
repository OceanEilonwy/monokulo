//! One span per HTTP request, for axum servers (structured_logging.md 2.1).
//!
//! [`server`] is an axum middleware. It opens an `HTTP request` span with
//! OpenTelemetry's HTTP server attributes, joins the caller's trace when the
//! request carries a `traceparent` header, writes one line when the
//! response is ready, and returns the trace in a `traceresponse` header so a
//! caller (or a person reporting a problem) can find it.

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::{ConnectInfo, MatchedPath, Request};
use axum::http::{HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use tracing::field::Empty;
use tracing::Instrument;

use crate::trace;

/// The W3C response header naming the request's trace.
pub const TRACERESPONSE: &str = "traceresponse";

/// Wraps every request in a span. Add it as the router's outermost layer
/// (`router.layer(axum::middleware::from_fn(telemetry::http::server))`),
/// so every other middleware's lines carry the request's fields.
///
/// `client.address` is the connecting peer; a server behind a proxy can
/// record the real client over it (`Span::current().record(..)`) once it
/// has worked it out. Either way it is truncated to its network when
/// written out.
pub async fn server(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request.extensions().get::<MatchedPath>().map(|p| p.as_str().to_string());
    let path = request.uri().path().to_string();
    let name = match &route {
        Some(route) => format!("{method} {route}"),
        None => method.to_string(),
    };
    let span = tracing::info_span!(
        "HTTP request",
        otel.name = %name,
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = %method,
        http.route = route.as_deref(),
        url.path = %path,
        client.address = Empty,
        http.response.status_code = Empty,
    );
    if let Some(ConnectInfo(peer)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
        span.record("client.address", peer.ip().to_string());
    }
    if let Some(parent) = request.headers().get(trace::TRACEPARENT).and_then(|v| v.to_str().ok()) {
        trace::set_remote_parent(&span, parent);
    }

    let started = Instant::now();
    let mut response = next.run(request).instrument(span.clone()).await;
    let status = response.status();
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    span.record("http.response.status_code", status.as_u16());
    if status.is_server_error() {
        span.record("otel.status_code", "ERROR");
    }
    if let Some(value) = trace::traceparent(&span).and_then(|v| HeaderValue::from_str(&v).ok()) {
        response.headers_mut().insert(TRACERESPONSE, value);
    }

    let shown = route.as_deref().unwrap_or(&path);
    let _entered = span.enter();
    if status.is_server_error() {
        tracing::warn!(duration_ms, "{method} {shown} {}", status.as_u16());
    } else if is_quiet(&method, &path) {
        tracing::debug!(duration_ms, "{method} {shown} {}", status.as_u16());
    } else {
        tracing::info!(duration_ms, "{method} {shown} {}", status.as_u16());
    }
    response
}

/// Requests too frequent and too dull to log at `info`: static files.
fn is_quiet(method: &Method, path: &str) -> bool {
    method == Method::GET && path.starts_with("/static/")
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use crate::tests::{subscriber, Capture};
    use crate::Format;

    async fn call(router: Router, request: axum::http::Request<Body>) -> axum::response::Response {
        router.oneshot(request).await.unwrap()
    }

    fn router() -> Router {
        Router::new()
            .route("/stores/{id}", get(|| async { tracing::info!(store.id = "s_1", "looked up"); "ok" }))
            .route("/boom", get(|| async { (axum::http::StatusCode::BAD_GATEWAY, "no") }))
            .route("/static/x.js", get(|| async { "js" }))
            .layer(axum::middleware::from_fn(super::server))
    }

    fn lines(capture: &Capture) -> Vec<serde_json::Value> {
        capture.json_lines()
    }

    #[tokio::test]
    async fn a_request_gets_a_span_with_http_attributes_and_one_line_at_the_end() {
        let (_telemetry, capture, _guard) = subscriber(Format::Json, "info");
        let request = axum::http::Request::get("/stores/s_1?x=secret").body(Body::empty()).unwrap();
        let response = call(router(), request).await;
        let traceresponse = response.headers().get(super::TRACERESPONSE).unwrap().to_str().unwrap().to_string();

        let lines = lines(&capture);
        assert_eq!(lines.len(), 2, "{}", capture.text());
        let (handler, finished) = (&lines[0], &lines[1]);
        assert_eq!(handler["message"], "looked up");
        assert_eq!(handler["attributes"]["http.route"], "/stores/{id}");
        assert_eq!(handler["attributes"]["http.request.method"], "GET");
        assert_eq!(finished["message"], "GET /stores/{id} 200");
        assert_eq!(finished["attributes"]["http.response.status_code"], 200);
        assert!(finished["attributes"]["duration_ms"].is_u64());
        assert_eq!(finished["attributes"]["url.path"], "/stores/s_1", "never the query string");
        assert_eq!(handler["trace_id"], finished["trace_id"]);
        assert!(traceresponse.contains(finished["trace_id"].as_str().unwrap()), "{traceresponse}");
        assert!(!capture.text().contains("secret"));
    }

    #[tokio::test]
    async fn a_callers_traceparent_is_joined() {
        let (_telemetry, capture, _guard) = subscriber(Format::Json, "info");
        let request = axum::http::Request::get("/stores/s_1")
            .header("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
            .body(Body::empty())
            .unwrap();
        let response = call(router(), request).await;
        assert!(response.headers()[super::TRACERESPONSE].to_str().unwrap().starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-"));
        assert!(lines(&capture).iter().all(|l| l["trace_id"] == "4bf92f3577b34da6a3ce929d0e0e4736"));
    }

    #[tokio::test]
    async fn server_errors_are_warnings_and_static_files_are_debug() {
        let (_telemetry, capture, _guard) = subscriber(Format::Json, "info");
        call(router(), axum::http::Request::get("/boom").body(Body::empty()).unwrap()).await;
        call(router(), axum::http::Request::get("/static/x.js").body(Body::empty()).unwrap()).await;
        call(router(), axum::http::Request::get("/nowhere").body(Body::empty()).unwrap()).await;
        let lines = lines(&capture);
        assert_eq!(lines.len(), 2, "{}", capture.text());
        assert_eq!(lines[0]["level"], "WARN");
        assert_eq!(lines[0]["message"], "GET /boom 502");
        assert_eq!(lines[1]["message"], "GET /nowhere 404", "an unmatched path is shown as requested");
    }
}
