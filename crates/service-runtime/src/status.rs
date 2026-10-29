//! A standalone service's status, for this machine alone: `GET /status` on
//! a listener of its own, answered with what the service says of itself
//! as JSON (`monokulo-push-relay`'s and `monokulo-bell`'s `status.listen`).
//!
//! It is a listener apart from the service's own, never a route on it: a
//! TLS proxy on the same host forwards the service's listener to the
//! world, and would forward `/status` with it. The address it is bound to
//! is a loopback one (`live_settings::LoopbackAddr` refuses any other as
//! it is parsed), and a request whose `Host` isn't this machine is refused
//! too, so a web page can't read it by pointing a name of its own at
//! `127.0.0.1` (DNS rebinding). So it needs no credential. What it says is
//! counts, times and readiness: never a token, an endpoint, a body or a
//! key.

use std::future::Future;
use std::net::IpAddr;

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::Serialize;

/// The commit the binary was built from, when its build said
/// (`MONOKULO_GIT_COMMIT`, which release.yml, build-openwrt.sh and the
/// Dockerfile set).
pub fn commit() -> Option<&'static str> {
    option_env!("MONOKULO_GIT_COMMIT").filter(|commit| !commit.is_empty())
}

/// The binary's `--version`: `version`, and the commit's first nine
/// digits when the build named one (`0.1.0 (a5845fce4)`).
pub fn version(version: &str) -> String {
    match commit() {
        Some(commit) => format!("{version} ({})", commit.get(..9).unwrap_or(commit)),
        None => version.to_owned(),
    }
}

/// Something that happened, and when (Unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Event<O> {
    pub at: i64,
    pub outcome: O,
}

/// The last of something a service does (a push handed on, a ding sent),
/// and how it went: what its status says of it.
#[derive(Debug)]
pub struct Last<O>(Mutex<Option<Event<O>>>);

impl<O> Default for Last<O> {
    fn default() -> Self {
        Last(Mutex::new(None))
    }
}

impl<O: Copy> Last<O> {
    /// It happened now, and went `outcome`.
    pub fn record(&self, outcome: O) {
        *self.0.lock() = Some(Event {
            at: crate::time::now_unix(),
            outcome,
        });
    }

    /// The last one recorded, if any has been since the process started.
    pub fn get(&self) -> Option<Event<O>> {
        *self.0.lock()
    }
}

/// `GET /status`, answered with what `report` says now, as JSON, never
/// cached; anything else is 404, and a request for another host 403.
pub fn router<F, Fut, R>(report: F) -> Router
where
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: Serialize + 'static,
{
    Router::new().route(
        "/status",
        get(move |headers: HeaderMap| {
            let report = report.clone();
            async move {
                if !for_this_machine(&headers) {
                    return StatusCode::FORBIDDEN.into_response();
                }
                ([(header::CACHE_CONTROL, "no-store")], Json(report().await)).into_response()
            }
        }),
    )
}

/// Serves [`router`] on `listener` (bound to a loopback address) until
/// `shutdown` completes.
pub async fn serve<F, Fut, R>(
    listener: tokio::net::TcpListener,
    report: F,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()>
where
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: Serialize + 'static,
{
    axum::serve(listener, router(report))
        .with_graceful_shutdown(shutdown)
        .await
}

/// Whether the request's `Host` names this machine: `localhost` or a
/// loopback address, with or without a port. A request without one (an
/// HTTP/1.0 client on this machine) is taken; a browser always sends one.
fn for_this_machine(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST) else {
        return true;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    // `[::1]:8446`, `[::1]`, `127.0.0.1:8446`, `localhost`.
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::Response;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum Outcome {
        TooLarge,
    }

    fn reporting() -> Router {
        router(|| async { serde_json::json!({ "service": "test", "count": 3 }) })
    }

    async fn get(router: Router, path: &str, host: Option<&str>) -> (StatusCode, Response) {
        let mut request = Request::get(path);
        if let Some(host) = host {
            request = request.header(header::HOST, host);
        }
        let response = router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        (response.status(), response)
    }

    /// The report, as JSON, never cached, asked for by this machine's name
    /// in any of its forms.
    #[tokio::test]
    async fn the_status_is_the_report_as_json() {
        for host in [
            None,
            Some("127.0.0.1:8446"),
            Some("localhost:8446"),
            Some("[::1]:8446"),
            Some("[::1]"),
            Some("LOCALHOST"),
        ] {
            let (status, response) = get(reporting(), "/status", host).await;
            assert_eq!(status, StatusCode::OK, "{host:?}");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let report: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(report, serde_json::json!({ "service": "test", "count": 3 }));
        }
        assert_eq!(get(reporting(), "/", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            get(reporting(), "/v1/info", None).await.0,
            StatusCode::NOT_FOUND
        );
    }

    /// A page on another name pointed at 127.0.0.1 (DNS rebinding) is
    /// refused: the browser sends that name as the host.
    #[tokio::test]
    async fn a_request_for_another_host_is_refused() {
        for host in [
            "evil.example:8446",
            "evil.example",
            "192.168.1.1:8446",
            "127.0.0.1.evil.example",
            "[2001:db8::1]:8446",
        ] {
            assert_eq!(
                get(reporting(), "/status", Some(host)).await.0,
                StatusCode::FORBIDDEN,
                "{host}"
            );
        }
    }

    #[test]
    fn the_last_event_is_the_one_recorded_last() {
        let last = Last::default();
        assert_eq!(last.get(), None);
        last.record(Outcome::TooLarge);
        let event = last.get().unwrap();
        assert!(event.at > 0);
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({ "at": event.at, "outcome": "too-large" })
        );
    }

    #[test]
    fn the_version_names_the_commit_when_the_build_did() {
        match commit() {
            Some(commit) => assert_eq!(
                version("0.1.0"),
                format!("0.1.0 ({})", &commit[..9.min(commit.len())])
            ),
            None => assert_eq!(version("0.1.0"), "0.1.0"),
        }
    }
}
