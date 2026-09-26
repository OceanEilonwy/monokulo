//! The HTTP side of abuse protection (`crate::abuse`): working out who each
//! request is from and spending that client's budget.
//!
//! [`abuse_middleware`] runs on the public `/pay/{pk}/...` routes (inside
//! their CORS layer, so a `429` is still readable cross-origin). It:
//!
//! 1. identifies the client ([`anonymous_identity`]): the Tor circuit on the
//!    onion listener, otherwise the clearnet address behind any trusted
//!    proxies. Requests driven through the router without a connection (only
//!    tests do that) have no identity and aren't counted;
//! 2. checks a presented store secret key (`super::store_key`): the right
//!    key makes the client that store (its own budget, and the request is
//!    marked `StoreKeyAuthenticated`); a wrong one gets `401` after spending
//!    the anonymous client's budget, so keys can't be guessed quickly;
//! 3. spends one request of the client's budget, answering `429` when it is
//!    used up, and leaves the [`ClientIdentity`] in the request's extensions
//!    for handlers (the live-update stream cap uses it).

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::abuse::proxy_protocol::OnionPeer;
use crate::abuse::{identity, ClientIdentity, TrustedProxies};

use super::embed_domains::public_key_of_pay_path;
use super::store_key::{self, KeyCheck, StoreKeyAuthenticated};
use super::AppState;

/// The anonymous client a request comes from - see the module doc comment.
pub fn anonymous_identity(extensions: &Extensions, headers: &HeaderMap, trusted: &TrustedProxies) -> Option<ClientIdentity> {
    if let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<OnionPeer>>() {
        return Some(peer.identity());
    }
    let ConnectInfo(peer) = extensions.get::<ConnectInfo<SocketAddr>>()?;
    let forwarded_for = headers.get("x-forwarded-for").and_then(|value| value.to_str().ok());
    Some(ClientIdentity::from_address(identity::client_address(peer.ip(), forwarded_for, trusted)))
}

fn too_many_requests() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "rate limit exceeded" }))).into_response()
}

/// See the module doc comment.
pub async fn abuse_middleware(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let config = state.abuse.config();
    let anonymous = anonymous_identity(request.extensions(), request.headers(), &config.trusted_proxies);
    let now = crate::now_unix();

    let pk = public_key_of_pay_path(request.uri().path()).map(str::to_string);
    let key = match &pk {
        Some(pk) => store_key::check(&state, pk, request.headers()),
        None => KeyCheck::Absent,
    };
    let client = match key {
        KeyCheck::Absent => anonymous,
        KeyCheck::Valid => {
            request.extensions_mut().insert(StoreKeyAuthenticated);
            pk.map(ClientIdentity::Store)
        }
        KeyCheck::Invalid => {
            if let Some(anonymous) = &anonymous {
                if !state.abuse.check(anonymous, now) {
                    return too_many_requests();
                }
            }
            let error = "This store's secret key was not accepted. Check the key, or reconnect the store.";
            return (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": error }))).into_response();
        }
    };

    if let Some(client) = client {
        if !state.abuse.check(&client, now) {
            return too_many_requests();
        }
        request.extensions_mut().insert(client);
    }
    next.run(request).await
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
            per_client_per_min: 1,
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
}
