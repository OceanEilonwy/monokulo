//! The HTTP side of abuse protection (`crate::abuse`): who each request is
//! from, what that client may do, and the challenge it gets past its soft
//! limit (steps 9a, 9c and 9d).
//!
//! Two middlewares share [`guard`]:
//!
//! - [`pay_middleware`] on the public routes, the machine API (`/api/v1`)
//!   and the payer's pages (`/checkout/{order_id}`), inside their CORS
//!   layer, so a `429` is still readable cross-origin. It also finds the
//!   store the request is about ([`super::store_key::RequestStore`]): on
//!   the API the key's (`super::store_key`), on a checkout the order's. The
//!   store's secret key or an app's makes the client that store (its own
//!   limit, never challenged); its public key leaves the client anonymous;
//!   an unknown key is `401` after spending the anonymous client's budget,
//!   so keys can't be guessed quickly. The API's refusals are
//!   `application/problem+json` (`crate::api::runtime::problem`).
//! - [`site_middleware`] on everything else except static files: the landing,
//!   login, sign-up, invite-request and status pages can be challenged; a
//!   signed-in merchant (the dashboard, the POS) is counted against their own
//!   limit and never challenged.
//!
//! What a request may do depends on its [`RouteClass`] and its client's
//! [`Tier`]:
//!
//! | | Allowed | Challenge (past soft, no pass) | Blocked (past hard) |
//! |---|---|---|---|
//! | Page (GET) | continue | interstitial (`views::challenge`) | `429` page + `Retry-After` |
//! | Machine API | continue | `429` problem + `challenge` + `Monokulo-Challenge` | `429` problem + `retry_after` + `Retry-After` |
//! | Stream (SSE) | continue | `429` (can't solve anything) | `429` + `Retry-After` |
//! | Other (form posts, plugin calls) | continue | continue | `429` + `Retry-After` |
//! | Logs (browser, POS and plugin reports) | continue | - | `429` + `Retry-After`, report dropped |
//!
//! Log reports have a budget of their own (`abuse.client_logs_per_min`,
//! [`crate::abuse::AbuseProtection::check_logs`]) and don't count against
//! the client's main one, so a page that reports a lot of problems never
//! gets its visitor challenged or blocked, and a client spamming reports
//! only loses its reports.
//!
//! Signed-in merchants and store keys never reach "Challenge" (their soft
//! and hard limits are the same). Under-attack mode treats every anonymous
//! page and JSON request as past soft; a pass (10 minutes after a solved
//! challenge) still lets it through. Requests driven through the router
//! without a connection (only tests do that) have no anonymous identity and
//! aren't counted.
//!
//! A solved challenge comes back as `?monokulo_proof=<challenge>.<nonce>` on
//! a page (from `static/challenge.js`), `?monokulo_wait=<token>` (the
//! no-JavaScript wait), or the `Monokulo-Proof` header on the machine API. It
//! grants the client a pass; a page is then redirected to itself without the
//! parameter.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::abuse::proxy_protocol::OnionPeer;
use crate::abuse::stats::Event;
use crate::abuse::{identity, ClientIdentity, Tier, TrustedProxies};
use crate::views;

use super::store_key::{self, KeyCheck, RequestStore};
use super::AppState;
use crate::api::runtime::problem::Problem;

/// The request header carrying a solved challenge.
pub const PROOF_HEADER: &str = "monokulo-proof";
/// The response header carrying a new challenge.
pub const CHALLENGE_HEADER: &str = "monokulo-challenge";
const PROOF_PARAM: &str = "monokulo_proof";
const WAIT_PARAM: &str = "monokulo_wait";

/// How a route may be treated - see the module doc comment's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteClass {
    Page,
    Api,
    Stream,
    Other,
    /// Log reports: their own budget, never challenged.
    Logs,
    /// Not counted at all (the status indicator's cached poll).
    Exempt,
}

/// The anonymous client a request comes from: the Tor circuit on the onion
/// listener, otherwise the clearnet address behind any trusted proxies.
pub fn anonymous_identity(
    extensions: &Extensions,
    headers: &HeaderMap,
    trusted: &TrustedProxies,
) -> Option<ClientIdentity> {
    if let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<OnionPeer>>() {
        return Some(peer.identity());
    }
    let ConnectInfo(peer) = extensions.get::<ConnectInfo<SocketAddr>>()?;
    // Every `X-Forwarded-For` line, in order, as one list: a proxy that
    // adds a line of its own instead of appending to the client's leaves
    // the client's own line first, and reading only that would let the
    // client name any address it likes. A line that isn't text ends the
    // walk at the peer rather than being skipped.
    let lines: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|value| value.to_str().unwrap_or("?"))
        .collect();
    let joined = lines.join(",");
    let forwarded_for = (!joined.is_empty()).then_some(joined.as_str());
    let address = identity::client_address(peer.ip(), forwarded_for, trusted);
    // The request span recorded the connecting peer; behind a trusted proxy
    // this is the real client (truncated to its network when written out).
    tracing::Span::current().record("client.address", address.to_string());
    Some(ClientIdentity::from_address(address))
}

/// The class of a public route, by its method and route template.
fn pay_route_class(method: &Method, route: &str) -> RouteClass {
    use super::paths::{CheckoutEventsPath, CheckoutSharePath};
    use crate::api::runtime::catalog::{self, Kind};
    use axum_extra::routing::TypedPath;
    if let Some(resource) = catalog::resources()
        .into_iter()
        .find(|r| r.template == route && r.kind != Kind::Page)
    {
        return match resource.kind {
            Kind::Stream => RouteClass::Stream,
            _ if route == crate::api::v1::logs::Path::TEMPLATE => RouteClass::Logs,
            _ => RouteClass::Api,
        };
    }
    match (method.as_str(), route) {
        ("GET", CheckoutSharePath::PATH) => RouteClass::Page,
        ("GET", route) if route == crate::api::checkout::Path::TEMPLATE => RouteClass::Page,
        ("GET", CheckoutEventsPath::PATH) => RouteClass::Stream,
        _ => RouteClass::Other,
    }
}

/// The route template a request matched, or its path when it matched none.
fn route_of(request: &Request) -> String {
    request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(
            || request.uri().path().to_owned(),
            |m| m.as_str().to_owned(),
        )
}

fn site_route_class(method: &Method, path: &str) -> RouteClass {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), path, segments.as_slice()) {
        (_, "/status/summary", _) => RouteClass::Exempt,
        ("GET", "/" | "/login" | "/signup" | "/request-invite" | "/status", _) => RouteClass::Page,
        ("POST", "/telemetry/client", _) | ("POST", _, ["stores", _, "pos", "logs"]) => {
            RouteClass::Logs
        }
        _ => RouteClass::Other,
    }
}

/// See the module doc comment.
pub async fn pay_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let class = pay_route_class(request.method(), &route_of(&request));
    let api = super::paths::is_api(request.uri().path());
    let config = state.abuse.config();
    let anonymous = anonymous_identity(
        request.extensions(),
        request.headers(),
        &config.trusted_proxies,
    );
    let client = if api {
        match store_key::check(&state, request.headers()).await {
            KeyCheck::Absent => anonymous,
            KeyCheck::Known(caller) => {
                record_store(&caller.store);
                request
                    .extensions_mut()
                    .insert(RequestStore(caller.store.clone()));
                // The public key proves nothing: its caller stays anonymous.
                let client = if caller.credential.is_store_key() {
                    Some(ClientIdentity::Store(
                        caller.store.tenant_public_key.clone(),
                    ))
                } else {
                    anonymous
                };
                request.extensions_mut().insert(caller);
                client
            }
            KeyCheck::Invalid => {
                if let Some(anonymous) = &anonymous {
                    if let Tier::Blocked { retry_after_secs } =
                        state.abuse.check(anonymous, false, crate::now_unix())
                    {
                        return blocked(&state, class, retry_after_secs);
                    }
                }
                return Problem::Unauthorized
                    .detail(
                        "This key was not accepted. Check it, or connect the store or app again."
                            .to_owned(),
                    )
                    .into_response();
            }
        }
    } else {
        // A checkout page: the store its order belongs to.
        if let Some(order_id) = super::paths::checkout_order(request.uri().path()) {
            if let Ok(Some(store)) = state
                .db
                .read(move |db| db.get_store_connection_by_order(&order_id))
                .await
            {
                record_store(&store);
                request
                    .extensions_mut()
                    .insert(RequestStore(std::sync::Arc::new(store)));
            }
        }
        signed_in_identity(&state, request.headers())
            .await
            .or(anonymous)
    };
    guard(&state, class, api, client, request, next).await
}

/// Puts the store a request concerns on its lines (`store.id`).
fn record_store(store: &crate::db::StoreConnectionRow) {
    tracing::Span::current().record("store.id", store.id.as_str());
}

/// See the module doc comment.
pub async fn site_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let class = site_route_class(request.method(), request.uri().path());
    if class == RouteClass::Exempt {
        return next.run(request).await;
    }
    let client = match signed_in_session(&state, request.headers()).await {
        Some((user, hash)) => {
            let client = ClientIdentity::User(user.id.as_str().to_string());
            // The handler's `AuthedUser` takes it from here rather than
            // looking the session up again.
            request
                .extensions_mut()
                .insert(super::ResolvedSession(user, hash));
            Some(client)
        }
        None => anonymous_identity(
            request.extensions(),
            request.headers(),
            &state.abuse.config().trusted_proxies,
        ),
    };
    guard(&state, class, false, client, request, next).await
}

/// A signed-in merchant's identity, when the request carries a valid
/// session cookie.
async fn signed_in_identity(state: &AppState, headers: &HeaderMap) -> Option<ClientIdentity> {
    signed_in_session(state, headers)
        .await
        .map(|(user, _)| ClientIdentity::User(user.id.into_string()))
}

/// A signed-in merchant and their session, when the request carries a
/// valid session cookie.
async fn signed_in_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<(crate::db::UserRow, shared::auth::TokenHash)> {
    headers.get(header::COOKIE)?;
    super::resolve_authed_user(state, headers).await
}

async fn guard(
    state: &AppState,
    class: RouteClass,
    api: bool,
    client: Option<ClientIdentity>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(client) = client else {
        return next.run(request).await;
    };
    let now = crate::now_unix();
    let abuse = &state.abuse;

    if class == RouteClass::Logs {
        if let Err(retry_after_secs) = abuse.check_logs(&client, now) {
            // Keyed by the kind of report, not the path (paths carry store
            // ids and keys a flood could vary).
            let path = request.uri().path();
            let source = if api {
                "plugin"
            } else if path.ends_with("/pos/logs") {
                "pos"
            } else {
                "browser"
            };
            shared::throttled!(
                format!("client-logs-dropped:{source}"),
                warn,
                report.source = source,
                "log reports dropped: a client is past abuse.client_logs_per_min"
            );
            return blocked(state, class, retry_after_secs);
        }
        request.extensions_mut().insert(client);
        return next.run(request).await;
    }

    // A solved challenge coming back.
    let mut redeem_error = None;
    if !client.is_authenticated() {
        let redeemed = match class {
            RouteClass::Page => query_param(&request, PROOF_PARAM)
                .map(|proof| abuse.challenges.redeem_proof(&proof, &client, now))
                .or_else(|| {
                    query_param(&request, WAIT_PARAM)
                        .map(|token| abuse.challenges.redeem_wait(&token, &client, now))
                }),
            RouteClass::Api => request
                .headers()
                .get(PROOF_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(|proof| abuse.challenges.redeem_proof(proof, &client, now)),
            _ => None,
        };
        match redeemed {
            Some(Ok(())) => {
                abuse.stats.record(Event::Solved, now);
                abuse.limiter.grant_pass(&client, now);
                if class == RouteClass::Page {
                    // Counted, so redemption itself can't be hammered.
                    if let Tier::Blocked { retry_after_secs } = abuse.check(&client, false, now) {
                        return blocked(state, class, retry_after_secs);
                    }
                    return see_other(&url_without_challenge_params(&request));
                }
            }
            Some(Err(e)) => {
                abuse.stats.record(Event::Refused, now);
                redeem_error = Some(e.message().to_string());
            }
            None => {}
        }
    }

    let challengeable = matches!(class, RouteClass::Page | RouteClass::Api);
    match abuse.check(&client, challengeable, now) {
        Tier::Allowed => {}
        Tier::Blocked { retry_after_secs } => {
            abuse.stats.record(Event::Refused, now);
            return blocked(state, class, retry_after_secs);
        }
        Tier::Challenge => match class {
            RouteClass::Page if request.method() == Method::GET => {
                abuse.stats.record(Event::Issued, now);
                return challenge_page(state, &client, &request, redeem_error, now);
            }
            RouteClass::Api => {
                abuse.stats.record(Event::Issued, now);
                return challenge_json(state, &client, redeem_error, now);
            }
            // A stream can't carry a challenge: past the soft limit it's
            // refused for a moment, as its problem's page says.
            RouteClass::Stream => {
                const RETRY_AFTER_SECS: u64 = 5;
                return Problem::RateLimited
                    .detail("Too many requests; retry shortly.".to_owned())
                    .with("retry_after", json!(RETRY_AFTER_SECS))
                    .header(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECS))
                    .into_response();
            }
            _ => {}
        },
    }
    request.extensions_mut().insert(client);
    next.run(request).await
}

fn query_param(request: &Request, name: &str) -> Option<String> {
    let query = request.uri().query()?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// The request's path and query, minus any challenge parameters.
fn url_without_challenge_params(request: &Request) -> String {
    let path = request.uri().path();
    let kept: Vec<(String, String)> = request
        .uri()
        .query()
        .map(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .filter(|(key, _)| key != PROOF_PARAM && key != WAIT_PARAM)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    if kept.is_empty() {
        return path.to_string();
    }
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(kept)
        .finish();
    format!("{path}?{query}")
}

fn with_param(url: &str, name: &str, value: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    let value: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
    format!("{url}{separator}{name}={value}")
}

fn see_other(location: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    no_store(response)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn challenge_page(
    state: &AppState,
    client: &ClientIdentity,
    request: &Request,
    error: Option<String>,
    now: i64,
) -> Response {
    let config = state.abuse.config();
    let issued = state
        .abuse
        .challenges
        .issue(client, config.challenge_bits, now);
    let continue_url = url_without_challenge_params(request);
    let wait_url = with_param(
        &continue_url,
        WAIT_PARAM,
        &state.abuse.challenges.issue_wait(client, now),
    );
    let view = views::challenge::ChallengePageView {
        challenge: issued.challenge,
        difficulty: issued.difficulty,
        continue_url,
        wait_url,
        error,
    };
    let chrome = views::PageChrome::from_user(None, request.uri().path());
    // 429: this is not the page that was asked for (yet).
    no_store(
        (
            StatusCode::TOO_MANY_REQUESTS,
            views::challenge::challenge_page(&chrome, &view),
        )
            .into_response(),
    )
}

fn challenge_json(
    state: &AppState,
    client: &ClientIdentity,
    error: Option<String>,
    now: i64,
) -> Response {
    let issued = state
        .abuse
        .challenges
        .issue(client, state.abuse.config().challenge_bits, now);
    let header_value = format!("{}; difficulty={}", issued.challenge, issued.difficulty);
    let mut problem = Problem::ChallengeRequired
        .detail(error.unwrap_or_else(|| "Too many requests from this connection. Solve the challenge and retry with the Monokulo-Proof header.".to_string()))
        .with(
            "challenge",
            json!({
                "challenge": issued.challenge,
                "difficulty": issued.difficulty,
                "expires_in": issued.expires_in,
                "algorithm": "sha256-leading-zero-bits",
                "proof_header": "Monokulo-Proof",
            }),
        );
    if let Ok(value) = HeaderValue::from_str(&header_value) {
        problem = problem.header(header::HeaderName::from_static(CHALLENGE_HEADER), value);
    }
    no_store(problem.into_response())
}

fn blocked(state: &AppState, class: RouteClass, retry_after_secs: u64) -> Response {
    let _ = state;
    let mut response = if class == RouteClass::Page {
        let chrome = views::PageChrome::from_user(None, "/");
        (
            StatusCode::TOO_MANY_REQUESTS,
            views::challenge::too_many_requests_page(&chrome, retry_after_secs),
        )
            .into_response()
    } else {
        // Everything but a page is a machine's (the API, a plugin's or
        // app's handshake, a script's fetch, a stream): a problem.
        Problem::RateLimited
            .detail(format!(
                "Rate limit exceeded; retry in {retry_after_secs} s."
            ))
            .with("retry_after", json!(retry_after_secs))
            .into_response()
    };
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
    no_store(response)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::abuse::{AbuseConfig, AbuseProtection, TrustedProxies};
    use crate::db::Db;

    use super::super::{build_router, AppState};

    fn state(config: AbuseConfig) -> AppState {
        AppState {
            abuse: Arc::new(AbuseProtection::new(config)),
            ..AppState::for_tests_with_db(Db::open_in_memory().unwrap().into_shared())
        }
    }

    async fn status_from(
        router: &axum::Router,
        peer: &str,
        forwarded_for: Option<&str>,
    ) -> StatusCode {
        status_from_lines(router, peer, forwarded_for.into_iter().collect()).await
    }

    /// `status_from` with one `X-Forwarded-For` header line per entry.
    async fn status_from_lines(
        router: &axum::Router,
        peer: &str,
        forwarded_for: Vec<&str>,
    ) -> StatusCode {
        let mut builder = Request::builder().uri("/api/v1");
        for value in forwarded_for {
            builder = builder.header("x-forwarded-for", value);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            peer.parse::<std::net::SocketAddr>().unwrap(),
        ));
        router.clone().oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn clients_behind_a_trusted_proxy_get_their_own_budgets_and_untrusted_forwarding_is_ignored(
    ) {
        let config = AbuseConfig {
            soft_per_min: 1,
            trusted_proxies: TrustedProxies::parse("127.0.0.1").unwrap(),
            ..Default::default()
        };
        let router = build_router(state(config));

        // Two visitors behind the local proxy: separate budgets.
        assert_eq!(
            status_from(&router, "127.0.0.1:1000", Some("198.51.100.1")).await,
            StatusCode::OK
        );
        assert_eq!(
            status_from(&router, "127.0.0.1:1001", Some("198.51.100.2")).await,
            StatusCode::OK
        );
        assert_eq!(
            status_from(&router, "127.0.0.1:1002", Some("198.51.100.1")).await,
            StatusCode::TOO_MANY_REQUESTS
        );

        // A direct client can't dodge its limit by inventing X-Forwarded-For.
        assert_eq!(
            status_from(&router, "203.0.113.9:1", Some("1.1.1.1")).await,
            StatusCode::OK
        );
        assert_eq!(
            status_from(&router, "203.0.113.9:2", Some("2.2.2.2")).await,
            StatusCode::TOO_MANY_REQUESTS
        );

        // One IPv6 /64 is one client.
        assert_eq!(
            status_from(&router, "[2001:db8:1:2::1]:1", None).await,
            StatusCode::OK
        );
        assert_eq!(
            status_from(&router, "[2001:db8:1:2::ffff]:1", None).await,
            StatusCode::TOO_MANY_REQUESTS
        );

        // A proxy that adds its own header line (rather than appending to
        // the client's) still identifies the client by the line it added:
        // the client's own first line names whatever it likes, to no
        // effect. Two such requests from one client are one budget.
        assert_eq!(
            status_from_lines(&router, "127.0.0.1:2000", vec!["9.9.9.9", "198.51.100.7"]).await,
            StatusCode::OK
        );
        assert_eq!(
            status_from_lines(&router, "127.0.0.1:2001", vec!["8.8.8.8", "198.51.100.7"]).await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    fn get(uri: &str, peer: &str) -> Request<Body> {
        let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            peer.parse::<std::net::SocketAddr>().unwrap(),
        ));
        request
    }

    async fn text(response: axum::response::Response) -> String {
        use http_body_util::BodyExt;
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    fn attribute(html: &str, name: &str) -> String {
        let start = html.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
        html[start..start + html[start..].find('"').unwrap()].replace("&amp;", "&")
    }

    /// A plugin's or app's handshake, a browser's log report, a stream and
    /// the API past their limit all get the `rate-limited` problem with
    /// `Retry-After`, never `{"error": ...}`: only a page gets a page.
    #[tokio::test]
    async fn every_machine_route_past_its_limit_gets_the_rate_limited_problem() {
        let router = build_router(state(AbuseConfig {
            soft_per_min: 1,
            hard_per_min: 1,
            client_logs_per_min: 1,
            ..Default::default()
        }));
        let post = |uri: &str, peer: &str| {
            let mut request = Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                peer.parse::<std::net::SocketAddr>().unwrap(),
            ));
            request
        };
        let routes = [
            (
                post("/plugins/connect/acme-cart/finish", "203.0.113.1:1"),
                "203.0.113.1:1",
            ),
            (post("/apps/connect", "203.0.113.2:1"), "203.0.113.2:1"),
            (post("/telemetry/client", "203.0.113.3:1"), "203.0.113.3:1"),
            (get("/checkout/o1/events", "203.0.113.4:1"), "203.0.113.4:1"),
            (get("/api/v1/orders/o1", "203.0.113.5:1"), "203.0.113.5:1"),
        ];
        for (request, peer) in routes {
            let (method, uri) = (request.method().clone(), request.uri().clone());
            let again = || {
                let mut request = Request::builder()
                    .method(method.clone())
                    .uri(uri.clone())
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap();
                request.extensions_mut().insert(axum::extract::ConnectInfo(
                    peer.parse::<std::net::SocketAddr>().unwrap(),
                ));
                request
            };
            let _ = router.clone().oneshot(request).await.unwrap();
            let mut limited = None;
            for _ in 0..20 {
                let response = router.clone().oneshot(again()).await.unwrap();
                if response.status() == StatusCode::TOO_MANY_REQUESTS
                    && !response.headers().contains_key("monokulo-challenge")
                {
                    limited = Some(response);
                    break;
                }
            }
            let limited = limited.unwrap_or_else(|| panic!("{uri} was never limited"));
            assert!(limited.headers().contains_key("retry-after"), "{uri}");
            let body = crate::http::test_support::assert_problem(limited, "rate-limited").await;
            assert!(body["retry_after"].is_u64(), "{uri}: {body}");
        }
    }

    fn low_limits() -> AbuseConfig {
        AbuseConfig {
            soft_per_min: 1,
            hard_per_min: 10,
            challenge_bits: 8,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn past_the_soft_limit_a_page_gets_the_interstitial_and_a_solved_proof_continues() {
        let router = build_router(state(low_limits()));
        let page = "/checkout/o1?view=compact";
        let peer = "198.51.100.1:1";
        assert_eq!(
            router
                .clone()
                .oneshot(get(page, peer))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );

        let response = router.clone().oneshot(get(page, peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let html = text(response).await;
        assert!(html.contains("Checking your connection"), "{html}");
        let challenge = attribute(&html, "data-challenge");
        assert_eq!(attribute(&html, "data-continue"), page);
        assert!(
            attribute(&html, "data-wait").starts_with("/checkout/o1?view=compact&monokulo_wait=")
        );

        // A wait token used straight away is too early: the interstitial again, saying so.
        let wait = attribute(&html, "data-wait");
        let html = text(router.clone().oneshot(get(&wait, peer)).await.unwrap()).await;
        assert!(html.contains("Please wait a few more seconds."), "{html}");

        // The solved proof redeems, redirects to the page without the parameter...
        let nonce = crate::abuse::challenge::solve(&challenge, 8);
        let proof = format!("{page}&monokulo_proof={challenge}.{nonce}");
        let response = router.clone().oneshot(get(&proof, peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], page);
        // ...and the pass lets the next request through without a challenge.
        assert_eq!(
            router
                .clone()
                .oneshot(get(page, peer))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        // The same proof can't be used again (the client still holds its
        // pass, so the page itself is served).
        let response = router.clone().oneshot(get(&proof, peer)).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a replayed proof is not redeemed again"
        );
    }

    #[tokio::test]
    async fn the_json_api_answers_429_with_a_challenge_and_accepts_the_proof_header() {
        let router = build_router(state(low_limits()));
        let uri = "/api/v1";
        let peer = "198.51.100.2:1";
        router.clone().oneshot(get(uri, peer)).await.unwrap();
        let response = router.clone().oneshot(get(uri, peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let header = response.headers()["monokulo-challenge"]
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let body: serde_json::Value = serde_json::from_str(&text(response).await).unwrap();
        assert_eq!(body["status"], 429);
        let challenge = body["challenge"]["challenge"].as_str().unwrap().to_string();
        assert_eq!(header, format!("{challenge}; difficulty=8"));
        assert_eq!(body["challenge"]["difficulty"], 8);

        let mut retry = get(uri, peer);
        retry.headers_mut().insert(
            "monokulo-proof",
            format!(
                "{challenge}.{}",
                crate::abuse::challenge::solve(&challenge, 8)
            )
            .parse()
            .unwrap(),
        );
        assert_eq!(
            router.clone().oneshot(retry).await.unwrap().status(),
            StatusCode::OK,
            "the proof is accepted"
        );

        // A proof for someone else's connection is refused (with a new challenge).
        let mut stolen = get(uri, "198.51.100.3:1");
        router
            .clone()
            .oneshot(get(uri, "198.51.100.3:1"))
            .await
            .unwrap();
        stolen.headers_mut().insert(
            "monokulo-proof",
            format!(
                "{challenge}.{}",
                crate::abuse::challenge::solve(&challenge, 8)
            )
            .parse()
            .unwrap(),
        );
        let response = router.clone().oneshot(stolen).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(text(response).await.contains("different connection"));
    }

    #[tokio::test]
    async fn past_the_hard_limit_everything_gets_429_with_retry_after() {
        let router = build_router(state(AbuseConfig {
            soft_per_min: 1,
            hard_per_min: 2,
            ..Default::default()
        }));
        let peer = "198.51.100.4:1";
        for _ in 0..2 {
            router.clone().oneshot(get("/api/v1", peer)).await.unwrap();
        }
        let response = router.clone().oneshot(get("/api/v1", peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(response.headers().contains_key("retry-after"));
        assert!(
            !response.headers().contains_key("monokulo-challenge"),
            "nothing to solve past the hard limit"
        );
        let body: serde_json::Value = serde_json::from_str(&text(response).await).unwrap();
        assert_eq!(body["status"], 429);
        assert!(body["retry_after"].as_u64().is_some(), "{body}");
        let page = router
            .clone()
            .oneshot(get("/checkout/o1", peer))
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(page.headers().contains_key("retry-after"));
        assert!(text(page).await.contains("Too many requests"));
        let stream = router
            .clone()
            .oneshot(get("/checkout/o1/events", peer))
            .await
            .unwrap();
        assert_eq!(stream.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn signed_in_merchants_are_never_challenged() {
        let state = state(low_limits());
        {
            let db = state.db.lock();
            db.create_user(
                &shared::ids::UserId::new("u1"),
                "merchant@example.com",
                "x",
                false,
                0,
            )
            .unwrap();
            db.create_session(
                &shared::auth::RawToken::presented("session-token").hash(),
                &shared::ids::UserId::new("u1"),
                crate::now_unix(),
            )
            .unwrap();
        }
        let router = build_router(state);
        for _ in 0..6 {
            let mut request = get("/", "198.51.100.5:1");
            request
                .headers_mut()
                .insert("cookie", "session=session-token".parse().unwrap());
            let response = router.clone().oneshot(request).await.unwrap();
            // `/` is a signed-in merchant's dashboard.
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "a signed-in merchant has their own, higher limit"
            );
        }
        // An anonymous visitor to the same page is challenged past soft.
        router
            .clone()
            .oneshot(get("/", "198.51.100.6:1"))
            .await
            .unwrap();
        let html = text(
            router
                .clone()
                .oneshot(get("/", "198.51.100.6:1"))
                .await
                .unwrap(),
        )
        .await;
        assert!(html.contains("Checking your connection"));
    }

    fn report(peer: &str) -> Request<Body> {
        let body = r#"{"kind":"error","message":"x is undefined","page":"/admin/logs"}"#;
        let mut request = Request::builder()
            .method("POST")
            .uri("/telemetry/client")
            .body(Body::from(body))
            .unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            peer.parse::<std::net::SocketAddr>().unwrap(),
        ));
        request
    }

    #[tokio::test]
    async fn a_client_flooding_log_reports_loses_only_its_reports_and_is_never_challenged() {
        let router = build_router(state(AbuseConfig {
            client_logs_per_min: 3,
            under_attack: false,
            ..low_limits()
        }));
        let peer = "198.51.100.20:1";
        for _ in 0..3 {
            assert_eq!(
                router.clone().oneshot(report(peer)).await.unwrap().status(),
                StatusCode::NO_CONTENT
            );
        }
        let dropped = router.clone().oneshot(report(peer)).await.unwrap();
        assert_eq!(dropped.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(dropped.headers().contains_key("retry-after"));
        assert!(
            !dropped.headers().contains_key("monokulo-challenge"),
            "no challenge for log reports"
        );

        // The reports spent nothing of the main budget (soft limit 1): the
        // first page is served, not challenged.
        let page = router.clone().oneshot(get("/", peer)).await.unwrap();
        assert_ne!(
            page.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the page is served, not challenged"
        );
        // Another client's reports have their own budget.
        assert_eq!(
            router
                .clone()
                .oneshot(report("198.51.100.21:1"))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn under_attack_never_challenges_log_reports() {
        let router = build_router(state(AbuseConfig {
            under_attack: true,
            ..Default::default()
        }));
        let response = router
            .clone()
            .oneshot(report("198.51.100.22:1"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[test]
    fn log_routes_are_classed_as_logs() {
        use axum::http::Method;
        assert_eq!(
            super::site_route_class(&Method::POST, "/telemetry/client"),
            super::RouteClass::Logs
        );
        assert_eq!(
            super::site_route_class(&Method::POST, "/stores/c1/pos/logs"),
            super::RouteClass::Logs
        );
        assert_eq!(
            super::site_route_class(&Method::GET, "/stores/c1/pos/logs"),
            super::RouteClass::Other
        );
        assert_eq!(
            super::pay_route_class(&Method::POST, "/api/v1/logs"),
            super::RouteClass::Logs
        );
    }

    /// The public routes' classes come from the API's catalog: its
    /// resources are the machine API and its two event streams, and the
    /// payer's pages and their stream are classed by their templates.
    #[test]
    fn public_routes_are_classed_from_the_api_catalog_and_the_checkout_paths() {
        use super::RouteClass::{Api, Other, Page, Stream};
        use axum::http::Method;
        for (method, route, class) in [
            (Method::GET, "/api/v1", Api),
            (Method::POST, "/api/v1/orders", Api),
            (Method::GET, "/api/v1/orders/{order_id}", Api),
            (Method::GET, "/api/v1/store", Api),
            (Method::DELETE, "/api/v1/app", Api),
            (Method::GET, "/api/v1/events", Stream),
            (Method::GET, "/api/v1/orders/{order_id}/events", Stream),
            (Method::GET, "/checkout/{order_id}", Page),
            (Method::GET, "/checkout/{order_id}/share", Page),
            (Method::GET, "/checkout/{order_id}/events", Stream),
            (Method::POST, "/checkout/{order_id}/refund-address", Other),
        ] {
            assert_eq!(
                super::pay_route_class(&method, route),
                class,
                "{method} {route}"
            );
        }
    }

    #[tokio::test]
    async fn under_attack_challenges_every_anonymous_page_and_api_request_but_not_streams_or_static_files(
    ) {
        let router = build_router(state(AbuseConfig {
            under_attack: true,
            ..Default::default()
        }));
        let peer = "198.51.100.7:1";
        let html = text(
            router
                .clone()
                .oneshot(get("/checkout/o1", peer))
                .await
                .unwrap(),
        )
        .await;
        assert!(html.contains("Checking your connection"));
        let api = router.clone().oneshot(get("/api/v1", peer)).await.unwrap();
        assert!(api.headers().contains_key("monokulo-challenge"));
        let stream = router
            .clone()
            .oneshot(get("/checkout/o1/events", peer))
            .await
            .unwrap();
        assert_eq!(
            stream.status(),
            StatusCode::NOT_FOUND,
            "streams aren't challenged"
        );
        let script = router
            .clone()
            .oneshot(get(&crate::assets::url("challenge.js"), peer))
            .await
            .unwrap();
        assert_eq!(script.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cors_lets_any_site_send_a_proof_and_read_the_challenge() {
        let router = build_router(state(low_limits()));
        let preflight = Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/orders")
            .header("origin", "https://shop.example")
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "content-type,monokulo-proof",
            )
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(preflight).await.unwrap();
        let allowed = response.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(allowed.contains("monokulo-proof"), "{allowed}");

        let mut request = get("/api/v1", "198.51.100.8:1");
        request
            .headers_mut()
            .insert("origin", "https://shop.example".parse().unwrap());
        let response = router.clone().oneshot(request).await.unwrap();
        let exposed = response.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            exposed.contains("monokulo-challenge") && exposed.contains("retry-after"),
            "{exposed}"
        );
    }

    #[tokio::test]
    async fn only_operators_see_challenge_activity_on_the_status_page() {
        let state = state(low_limits());
        {
            let db = state.db.lock();
            db.create_user(
                &shared::ids::UserId::new("admin"),
                "admin@example.com",
                "x",
                true,
                0,
            )
            .unwrap();
            db.create_user(
                &shared::ids::UserId::new("merchant"),
                "m@example.com",
                "x",
                false,
                0,
            )
            .unwrap();
            db.create_session(
                &shared::auth::RawToken::presented("admin-token").hash(),
                &shared::ids::UserId::new("admin"),
                crate::now_unix(),
            )
            .unwrap();
            db.create_session(
                &shared::auth::RawToken::presented("merchant-token").hash(),
                &shared::ids::UserId::new("merchant"),
                crate::now_unix(),
            )
            .unwrap();
        }
        state
            .abuse
            .stats
            .record(crate::abuse::stats::Event::Issued, crate::now_unix());
        let router = build_router(state);
        let page = |cookie: Option<&'static str>| {
            let mut request = Request::builder()
                .uri("/status")
                .body(Body::empty())
                .unwrap();
            if let Some(cookie) = cookie {
                request
                    .headers_mut()
                    .insert("cookie", cookie.parse().unwrap());
            }
            router.clone().oneshot(request)
        };
        let html = text(page(Some("session=admin-token")).await.unwrap()).await;
        assert!(html.contains(r#"id="abuse-protection""#), "{html}");
        assert!(html.contains("<td>Issued</td><td>1</td>"), "{html}");
        for cookie in [None, Some("session=merchant-token")] {
            let html = text(page(cookie).await.unwrap()).await;
            assert!(!html.contains(r#"id="abuse-protection""#), "{cookie:?}");
        }
    }
}
