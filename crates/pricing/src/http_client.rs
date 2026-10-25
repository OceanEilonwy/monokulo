//! The HTTP clients every outbound call in this workspace is made with:
//! `EngineClient` in `monokulo` to the engine, and the exchange-rate
//! providers here to their APIs. Each has connect and read timeouts; calls
//! to our own services also carry the caller's trace.
//!
//! There is no response cache. No engine endpoint marks a response
//! cacheable, and the rate providers keep their own per-currency TTL caches,
//! which have to know when a price was really fetched to judge its age.

use http::Extensions;
use reqwest::{Request, Response};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware, Next};

/// Builds the client for a call to a third party, such as an exchange-rate
/// API. `user_agent` distinguishes callers in the server's logs.
pub fn build_client(user_agent: &str) -> ClientWithMiddleware {
    client_builder(user_agent, reqwest::header::HeaderMap::new()).build()
}

/// [`build_client`] for calls to our own services (monokulo to the engine),
/// which also sends the caller's trace as a `traceparent` header
/// (structured_logging.md 2.3). Not for third parties such as exchange-rate
/// APIs, which have no use for our trace ids.
pub fn build_traced_client(user_agent: &str) -> ClientWithMiddleware {
    build_traced_client_with_headers(user_agent, reqwest::header::HeaderMap::new())
}

/// [`build_traced_client`], sending `headers` on every request (a
/// credential the peer requires on every route, say).
pub fn build_traced_client_with_headers(
    user_agent: &str,
    headers: reqwest::header::HeaderMap,
) -> ClientWithMiddleware {
    client_builder(user_agent, headers)
        .with(PropagateTrace)
        .build()
}

/// How long a connection may take to open, and how long a response may
/// go without a byte arriving. The second bounds a peer that accepted the
/// connection and then stalled; an event stream is kept alive well within
/// it (the engine sends a comment every 15 s).
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn client_builder(user_agent: &str, headers: reqwest::header::HeaderMap) -> ClientBuilder {
    let inner = reqwest::Client::builder()
        .user_agent(user_agent.to_string())
        .default_headers(headers)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .expect("a validated user agent string can't fail to build a client");
    ClientBuilder::new(inner)
}

/// Adds a `traceparent` header naming the current span, when there is one.
pub struct PropagateTrace;

#[async_trait::async_trait]
impl Middleware for PropagateTrace {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        if let Some(value) = telemetry::trace::current_traceparent()
            .and_then(|v| reqwest::header::HeaderValue::from_str(&v).ok())
        {
            req.headers_mut()
                .insert(telemetry::trace::TRACEPARENT, value);
        }
        next.run(req, extensions).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;

    async fn spawn_echo_traceparent_server() -> String {
        let app = Router::new().route(
            "/echo",
            get(|headers: axum::http::HeaderMap| async move {
                headers
                    .get("traceparent")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_traced_client_sends_the_callers_trace_and_a_plain_one_does_not() {
        let base_url = spawn_echo_traceparent_server().await;
        let (_telemetry, subscriber) = telemetry::build(
            "test",
            telemetry::Format::Json,
            false,
            "info",
            std::io::sink,
        );
        let _guard = tracing::subscriber::set_default(subscriber);
        let span = tracing::info_span!("caller");
        let expected = telemetry::trace::traceparent(&span).unwrap();

        let traced = build_traced_client("test-agent");
        let sent = tracing::Instrument::instrument(
            async {
                traced
                    .get(format!("{base_url}/echo"))
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap()
            },
            span.clone(),
        )
        .await;
        assert_eq!(sent, expected);

        let plain = build_client("test-agent");
        let sent = tracing::Instrument::instrument(
            async {
                plain
                    .get(format!("{base_url}/echo"))
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap()
            },
            span,
        )
        .await;
        assert_eq!(sent, "", "third parties never get our trace ids");
    }
}
