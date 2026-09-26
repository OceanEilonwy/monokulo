//! The HTTP side of abuse protection (`crate::abuse`): who each request is
//! from, what that client may do, and the challenge it gets past its soft
//! limit (steps 9a, 9c and 9d).
//!
//! Two middlewares share [`guard`]:
//!
//! - [`pay_middleware`] on the public `/pay/{pk}/...` routes (inside their
//!   CORS layer, so a `429` is still readable cross-origin). A presented
//!   store secret key (`super::store_key`) makes the client that store (its
//!   own limit, never challenged); a wrong key is `401` after spending the
//!   anonymous client's budget, so keys can't be guessed quickly.
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
//! | JSON API | continue | `429` + `challenge` + `Monokulo-Challenge` | `429` JSON + `Retry-After` |
//! | Stream (SSE) | continue | `429` (can't solve anything) | `429` + `Retry-After` |
//! | Other (form posts, plugin calls) | continue | continue | `429` + `Retry-After` |
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
//! no-JavaScript wait), or the `Monokulo-Proof` header on the JSON API. It
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

use super::embed_domains::public_key_of_pay_path;
use super::store_key::{self, KeyCheck, StoreKeyAuthenticated};
use super::AppState;

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
    /// Not counted at all (the status indicator's cached poll).
    Exempt,
}

/// The anonymous client a request comes from: the Tor circuit on the onion
/// listener, otherwise the clearnet address behind any trusted proxies.
pub fn anonymous_identity(extensions: &Extensions, headers: &HeaderMap, trusted: &TrustedProxies) -> Option<ClientIdentity> {
    if let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<OnionPeer>>() {
        return Some(peer.identity());
    }
    let ConnectInfo(peer) = extensions.get::<ConnectInfo<SocketAddr>>()?;
    let forwarded_for = headers.get("x-forwarded-for").and_then(|value| value.to_str().ok());
    Some(ClientIdentity::from_address(identity::client_address(peer.ip(), forwarded_for, trusted)))
}

fn pay_route_class(method: &Method, path: &str) -> RouteClass {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", ["pay", _, "orders", _]) | ("GET", ["pay", _, "orders", _, "share"]) => RouteClass::Page,
        ("POST", ["pay", _, "orders"]) | ("GET", ["pay", _, "orders", _, "status"]) => RouteClass::Api,
        ("GET", ["pay", _, "orders", _, "events"]) => RouteClass::Stream,
        _ => RouteClass::Other,
    }
}

fn site_route_class(method: &Method, path: &str) -> RouteClass {
    match (method.as_str(), path) {
        (_, "/status/summary") => RouteClass::Exempt,
        ("GET", "/" | "/dashboard/login" | "/dashboard/signup" | "/request-invite" | "/status") => RouteClass::Page,
        _ => RouteClass::Other,
    }
}

/// See the module doc comment.
pub async fn pay_middleware(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let class = pay_route_class(request.method(), request.uri().path());
    let config = state.abuse.config();
    let anonymous = anonymous_identity(request.extensions(), request.headers(), &config.trusted_proxies);
    let pk = public_key_of_pay_path(request.uri().path()).map(str::to_string);
    let key = match &pk {
        Some(pk) => store_key::check(&state, pk, request.headers()),
        None => KeyCheck::Absent,
    };
    let client = match key {
        KeyCheck::Absent => signed_in_identity(&state, request.headers()).or(anonymous),
        KeyCheck::Valid => {
            request.extensions_mut().insert(StoreKeyAuthenticated);
            pk.map(ClientIdentity::Store)
        }
        KeyCheck::Invalid => {
            if let Some(anonymous) = &anonymous {
                if let Tier::Blocked { retry_after_secs } = state.abuse.check(anonymous, false, crate::now_unix()) {
                    return blocked(&state, class, retry_after_secs);
                }
            }
            let error = "This store's secret key was not accepted. Check the key, or reconnect the store.";
            return (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": error }))).into_response();
        }
    };
    guard(&state, class, client, request, next).await
}

/// See the module doc comment.
pub async fn site_middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let class = site_route_class(request.method(), request.uri().path());
    if class == RouteClass::Exempt {
        return next.run(request).await;
    }
    let client = signed_in_identity(&state, request.headers())
        .or_else(|| anonymous_identity(request.extensions(), request.headers(), &state.abuse.config().trusted_proxies));
    guard(&state, class, client, request, next).await
}

/// A signed-in merchant's identity, when the request carries a valid
/// session (cookie or bearer token).
fn signed_in_identity(state: &AppState, headers: &HeaderMap) -> Option<ClientIdentity> {
    if headers.get(header::AUTHORIZATION).is_none() && headers.get(header::COOKIE).is_none() {
        return None;
    }
    super::resolve_authed_user(state, headers).map(|(user, _)| ClientIdentity::User(user.id))
}

async fn guard(state: &AppState, class: RouteClass, client: Option<ClientIdentity>, mut request: Request, next: Next) -> Response {
    let Some(client) = client else { return next.run(request).await };
    let now = crate::now_unix();
    let abuse = &state.abuse;

    // A solved challenge coming back.
    let mut redeem_error = None;
    if !client.is_authenticated() {
        let redeemed = match class {
            RouteClass::Page => query_param(&request, PROOF_PARAM)
                .map(|proof| abuse.challenges.redeem_proof(&proof, &client, now))
                .or_else(|| query_param(&request, WAIT_PARAM).map(|token| abuse.challenges.redeem_wait(&token, &client, now))),
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
            RouteClass::Stream => {
                return (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "too many requests; retry shortly" })))
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
    url::form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
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
    let query: String = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(kept).finish();
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
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn challenge_page(state: &AppState, client: &ClientIdentity, request: &Request, error: Option<String>, now: i64) -> Response {
    let config = state.abuse.config();
    let issued = state.abuse.challenges.issue(client, config.challenge_bits, now);
    let continue_url = url_without_challenge_params(request);
    let wait_url = with_param(&continue_url, WAIT_PARAM, &state.abuse.challenges.issue_wait(client, now));
    let view = views::challenge::ChallengePageView { challenge: issued.challenge, difficulty: issued.difficulty, continue_url, wait_url, error };
    let chrome = views::PageChrome::from_user(None, request.uri().path());
    // 429: this is not the page that was asked for (yet).
    no_store((StatusCode::TOO_MANY_REQUESTS, views::challenge::challenge_page(&chrome, &view)).into_response())
}

fn challenge_json(state: &AppState, client: &ClientIdentity, error: Option<String>, now: i64) -> Response {
    let issued = state.abuse.challenges.issue(client, state.abuse.config().challenge_bits, now);
    let header_value = format!("{}; difficulty={}", issued.challenge, issued.difficulty);
    let body = json!({
        "error": error.unwrap_or_else(|| "Too many requests from this connection. Solve the challenge and retry with the Monokulo-Proof header.".to_string()),
        "challenge": {
            "challenge": issued.challenge,
            "difficulty": issued.difficulty,
            "expires_in": issued.expires_in,
            "algorithm": "sha256-leading-zero-bits",
            "proof_header": "Monokulo-Proof",
        },
    });
    let mut response = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();
    if let Ok(value) = HeaderValue::from_str(&header_value) {
        response.headers_mut().insert(CHALLENGE_HEADER, value);
    }
    no_store(response)
}

fn blocked(state: &AppState, class: RouteClass, retry_after_secs: u64) -> Response {
    let _ = state;
    let mut response = if class == RouteClass::Page {
        let chrome = views::PageChrome::from_user(None, "/");
        (StatusCode::TOO_MANY_REQUESTS, views::challenge::too_many_requests_page(&chrome, retry_after_secs)).into_response()
    } else {
        (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "rate limit exceeded", "retry_after": retry_after_secs }))).into_response()
    };
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
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
    use crate::engine_client::EngineClient;

    use super::super::{build_router, AppState};

    fn state(config: AbuseConfig) -> AppState {
        AppState {
            db: Db::open_in_memory().unwrap().into_shared(),
            engine_client: EngineClient::new("http://127.0.0.1:1"),
            encryption_key: [7u8; 32],
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only()),
            abuse: Arc::new(AbuseProtection::new(config)),
            dns: Arc::new(crate::embed_domains::UnavailableDns("no DNS in tests".to_string())),
        }
    }

    async fn status_from(router: &axum::Router, peer: &str, forwarded_for: Option<&str>) -> StatusCode {
        let mut builder = Request::builder().uri("/pay/pk_unknown/orders/o1/status");
        if let Some(value) = forwarded_for {
            builder = builder.header("x-forwarded-for", value);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        router.clone().oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn clients_behind_a_trusted_proxy_get_their_own_budgets_and_untrusted_forwarding_is_ignored() {
        let config = AbuseConfig {
            soft_per_min: 1,
            trusted_proxies: TrustedProxies::parse("127.0.0.1").unwrap(),
            ..Default::default()
        };
        let router = build_router(state(config));

        // Two visitors behind the local proxy: separate budgets.
        assert_eq!(status_from(&router, "127.0.0.1:1000", Some("198.51.100.1")).await, StatusCode::NOT_FOUND);
        assert_eq!(status_from(&router, "127.0.0.1:1001", Some("198.51.100.2")).await, StatusCode::NOT_FOUND);
        assert_eq!(status_from(&router, "127.0.0.1:1002", Some("198.51.100.1")).await, StatusCode::TOO_MANY_REQUESTS);

        // A direct client can't dodge its limit by inventing X-Forwarded-For.
        assert_eq!(status_from(&router, "203.0.113.9:1", Some("1.1.1.1")).await, StatusCode::NOT_FOUND);
        assert_eq!(status_from(&router, "203.0.113.9:2", Some("2.2.2.2")).await, StatusCode::TOO_MANY_REQUESTS);

        // One IPv6 /64 is one client.
        assert_eq!(status_from(&router, "[2001:db8:1:2::1]:1", None).await, StatusCode::NOT_FOUND);
        assert_eq!(status_from(&router, "[2001:db8:1:2::ffff]:1", None).await, StatusCode::TOO_MANY_REQUESTS);
    }

    fn get(uri: &str, peer: &str) -> Request<Body> {
        let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        request
    }

    async fn text(response: axum::response::Response) -> String {
        use http_body_util::BodyExt;
        String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    fn attribute(html: &str, name: &str) -> String {
        let start = html.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
        html[start..start + html[start..].find('"').unwrap()].replace("&amp;", "&")
    }

    fn low_limits() -> AbuseConfig {
        AbuseConfig { soft_per_min: 1, hard_per_min: 10, challenge_bits: 8, ..Default::default() }
    }

    #[tokio::test]
    async fn past_the_soft_limit_a_page_gets_the_interstitial_and_a_solved_proof_continues() {
        let router = build_router(state(low_limits()));
        let page = "/pay/pk_unknown/orders/o1?view=compact";
        let peer = "198.51.100.1:1";
        assert_eq!(router.clone().oneshot(get(page, peer)).await.unwrap().status(), StatusCode::NOT_FOUND);

        let response = router.clone().oneshot(get(page, peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let html = text(response).await;
        assert!(html.contains("Checking your connection"), "{html}");
        let challenge = attribute(&html, "data-challenge");
        assert_eq!(attribute(&html, "data-continue"), page);
        assert!(attribute(&html, "data-wait").starts_with("/pay/pk_unknown/orders/o1?view=compact&monokulo_wait="));

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
        assert_eq!(router.clone().oneshot(get(page, peer)).await.unwrap().status(), StatusCode::NOT_FOUND);
        // The same proof can't be used again (the client still holds its
        // pass, so the page itself is served).
        let response = router.clone().oneshot(get(&proof, peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "a replayed proof is not redeemed again");
    }

    #[tokio::test]
    async fn the_json_api_answers_429_with_a_challenge_and_accepts_the_proof_header() {
        let router = build_router(state(low_limits()));
        let uri = "/pay/pk_unknown/orders/o1/status";
        let peer = "198.51.100.2:1";
        router.clone().oneshot(get(uri, peer)).await.unwrap();
        let response = router.clone().oneshot(get(uri, peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let header = response.headers()["monokulo-challenge"].to_str().unwrap().to_string();
        let body: serde_json::Value = serde_json::from_str(&text(response).await).unwrap();
        let challenge = body["challenge"]["challenge"].as_str().unwrap().to_string();
        assert_eq!(header, format!("{challenge}; difficulty=8"));
        assert_eq!(body["challenge"]["difficulty"], 8);

        let mut retry = get(uri, peer);
        retry.headers_mut().insert("monokulo-proof", format!("{challenge}.{}", crate::abuse::challenge::solve(&challenge, 8)).parse().unwrap());
        assert_eq!(router.clone().oneshot(retry).await.unwrap().status(), StatusCode::NOT_FOUND, "the proof is accepted");

        // A proof for someone else's connection is refused (with a new challenge).
        let mut stolen = get(uri, "198.51.100.3:1");
        router.clone().oneshot(get(uri, "198.51.100.3:1")).await.unwrap();
        stolen.headers_mut().insert("monokulo-proof", format!("{challenge}.{}", crate::abuse::challenge::solve(&challenge, 8)).parse().unwrap());
        let response = router.clone().oneshot(stolen).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(text(response).await.contains("different connection"));
    }

    #[tokio::test]
    async fn past_the_hard_limit_everything_gets_429_with_retry_after() {
        let router = build_router(state(AbuseConfig { soft_per_min: 1, hard_per_min: 2, ..Default::default() }));
        let peer = "198.51.100.4:1";
        for _ in 0..2 {
            router.clone().oneshot(get("/pay/pk_unknown/orders/o1/status", peer)).await.unwrap();
        }
        let response = router.clone().oneshot(get("/pay/pk_unknown/orders/o1/status", peer)).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(response.headers().contains_key("retry-after"));
        assert!(!response.headers().contains_key("monokulo-challenge"), "nothing to solve past the hard limit");
        let page = router.clone().oneshot(get("/pay/pk_unknown/orders/o1", peer)).await.unwrap();
        assert_eq!(page.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(page.headers().contains_key("retry-after"));
        assert!(text(page).await.contains("Too many requests"));
        let stream = router.clone().oneshot(get("/pay/pk_unknown/orders/o1/events", peer)).await.unwrap();
        assert_eq!(stream.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn signed_in_merchants_are_never_challenged() {
        let state = state(low_limits());
        {
            let db = state.db.lock().unwrap();
            db.create_user("u1", "merchant@example.com", "x", false, 0).unwrap();
            db.create_session(&shared::auth::hash_secret_token("session-token"), "u1", crate::now_unix()).unwrap();
        }
        let router = build_router(state);
        for _ in 0..6 {
            let mut request = get("/", "198.51.100.5:1");
            request.headers_mut().insert("cookie", "session=session-token".parse().unwrap());
            let response = router.clone().oneshot(request).await.unwrap();
            // The landing page sends a signed-in merchant on to the dashboard.
            assert_eq!(response.status(), StatusCode::FOUND, "a signed-in merchant has their own, higher limit");
        }
        // An anonymous visitor to the same page is challenged past soft.
        router.clone().oneshot(get("/", "198.51.100.6:1")).await.unwrap();
        let html = text(router.clone().oneshot(get("/", "198.51.100.6:1")).await.unwrap()).await;
        assert!(html.contains("Checking your connection"));
    }

    #[tokio::test]
    async fn under_attack_challenges_every_anonymous_page_and_api_request_but_not_streams_or_static_files() {
        let router = build_router(state(AbuseConfig { under_attack: true, ..Default::default() }));
        let peer = "198.51.100.7:1";
        let html = text(router.clone().oneshot(get("/pay/pk_unknown/orders/o1", peer)).await.unwrap()).await;
        assert!(html.contains("Checking your connection"));
        let api = router.clone().oneshot(get("/pay/pk_unknown/orders/o1/status", peer)).await.unwrap();
        assert!(api.headers().contains_key("monokulo-challenge"));
        let stream = router.clone().oneshot(get("/pay/pk_unknown/orders/o1/events", peer)).await.unwrap();
        assert_eq!(stream.status(), StatusCode::NOT_FOUND, "streams aren't challenged");
        let script = router.clone().oneshot(get("/static/challenge.js", peer)).await.unwrap();
        assert_eq!(script.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cors_lets_any_site_send_a_proof_and_read_the_challenge() {
        let router = build_router(state(low_limits()));
        let preflight = Request::builder()
            .method("OPTIONS")
            .uri("/pay/pk_unknown/orders")
            .header("origin", "https://shop.example")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type,monokulo-proof")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(preflight).await.unwrap();
        let allowed = response.headers()["access-control-allow-headers"].to_str().unwrap().to_ascii_lowercase();
        assert!(allowed.contains("monokulo-proof"), "{allowed}");

        let mut request = get("/pay/pk_unknown/orders/o1/status", "198.51.100.8:1");
        request.headers_mut().insert("origin", "https://shop.example".parse().unwrap());
        let response = router.clone().oneshot(request).await.unwrap();
        let exposed = response.headers()["access-control-expose-headers"].to_str().unwrap().to_ascii_lowercase();
        assert!(exposed.contains("monokulo-challenge") && exposed.contains("retry-after"), "{exposed}");
    }
}
