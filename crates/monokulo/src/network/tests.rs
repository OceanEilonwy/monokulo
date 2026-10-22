//! One instance, two networks: what a visitor on the web address and one
//! on the onion address each get from the same router.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use axum::Router;
use tower::ServiceExt;

use super::*;
use crate::http::{build_router, AppState};
use crate::settings::TorConfig;

/// A real v3 address (the Tor Project's own).
const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
const WEB: &str = "https://pay.example.com";

fn external_tor() -> TorConfig {
    TorConfig {
        mode: TorMode::External,
        onion_uri: Some(format!("http://{ONION}")),
        ..TorConfig::defaults()
    }
}

fn state_with(tor: TorConfig) -> AppState {
    AppState {
        settings: AppState::test_settings(Some(WEB)).with_tor(tor),
        ..AppState::for_tests()
    }
}

/// A request as a visitor on Tor sends it: through either tor, it carries
/// the circuit it came on.
fn over_tor(mut request: Request<Body>) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo(crate::tor::circuit_peer(7)));
    request
}

fn page(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("host", "pay.example.com")
        .body(Body::empty())
        .unwrap()
}

async fn headers_of(router: &Router, request: Request<Body>) -> HeaderMap {
    router
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .headers()
        .clone()
}

#[tokio::test]
async fn a_web_page_tells_tor_browser_where_the_same_page_is_on_tor() {
    let router = build_router(state_with(external_tor()));
    let headers = headers_of(&router, page("/dashboard/login?next=%2Fsetup")).await;
    assert_eq!(
        headers["onion-location"],
        format!("http://{ONION}/dashboard/login?next=%2Fsetup")
    );
    assert_eq!(
        headers["referrer-policy"],
        "strict-origin-when-cross-origin"
    );
}

#[tokio::test]
async fn a_page_on_tor_points_nowhere_else_and_sends_no_referer() {
    let router = build_router(state_with(external_tor()));
    let headers = headers_of(&router, over_tor(page("/dashboard/login"))).await;
    assert!(headers.get("onion-location").is_none(), "{headers:?}");
    assert_eq!(headers["referrer-policy"], "no-referrer");
}

#[tokio::test]
async fn onion_location_is_left_out_when_switched_off_tor_is_off_or_not_up_yet() {
    let switched_off = TorConfig {
        onion_location: false,
        ..external_tor()
    };
    let not_up_yet = TorConfig {
        mode: TorMode::Inside,
        ..TorConfig::defaults()
    };
    for tor in [switched_off, TorConfig::defaults(), not_up_yet] {
        let router = build_router(state_with(tor.clone()));
        let headers = headers_of(&router, page("/dashboard/login")).await;
        assert!(
            headers.get("onion-location").is_none(),
            "{tor:?}: {headers:?}"
        );
    }
}

#[test]
fn the_address_is_used_while_tor_publishes_it() {
    let tor = Tor::default();
    tor.set_status(crate::tor::TorStatus::Publishing {
        address: ONION.to_string(),
    });
    let state = AppState {
        tor,
        ..state_with(TorConfig {
            mode: TorMode::Inside,
            ..TorConfig::defaults()
        })
    };
    assert_eq!(
        onion_uri(&state.settings, &state.tor),
        Some(format!("http://{ONION}"))
    );
}

#[tokio::test]
async fn tor_inside_monokulo_gives_its_own_address_once_it_has_one() {
    let state = AppState {
        tor: Tor::running_at(ONION),
        ..state_with(TorConfig {
            mode: TorMode::Inside,
            ..TorConfig::defaults()
        })
    };
    let router = build_router(state);
    let headers = headers_of(&router, page("/dashboard/login")).await;
    assert_eq!(
        headers["onion-location"],
        format!("http://{ONION}/dashboard/login")
    );
}

#[test]
fn only_pages_get_onion_location() {
    let state = state_with(external_tor());
    let mut json = HeaderMap::new();
    json.insert("content-type", "application/json".parse().unwrap());
    state
        .network_of(NetworkKind::Web)
        .response_headers("/pay/pk/orders/1/status", &mut json);
    assert!(json.get("onion-location").is_none());
}

#[test]
fn each_visitor_gets_links_on_the_address_they_used() {
    let state = state_with(external_tor());
    let headers = HeaderMap::new();
    let web = OnNetwork::new(
        &*state.network_of(NetworkKind::Web),
        &headers,
        &Extensions::new(),
    );
    let onion = OnNetwork::new(
        &*state.network_of(NetworkKind::Onion),
        &headers,
        &Extensions::new(),
    );
    assert_eq!(web.link("/pay/x"), format!("{WEB}/pay/x"));
    assert_eq!(onion.link("/pay/x"), format!("http://{ONION}/pay/x"));
    assert!(web.may_load_external);
    assert!(!onion.may_load_external);
}

#[test]
fn without_a_known_address_a_link_uses_the_host_the_visitor_asked_for() {
    let state = AppState {
        settings: AppState::test_settings(None).with_tor(TorConfig {
            mode: TorMode::Inside,
            ..TorConfig::defaults()
        }),
        ..AppState::for_tests()
    };
    let mut headers = HeaderMap::new();
    headers.insert("host", ONION.parse().unwrap());
    let onion = OnNetwork::new(
        &*state.network_of(NetworkKind::Onion),
        &headers,
        &Extensions::new(),
    );
    assert_eq!(onion.link("/x"), format!("http://{ONION}/x"));
    headers.insert("host", "127.0.0.1:8081".parse().unwrap());
    let web = OnNetwork::new(
        &*state.network_of(NetworkKind::Web),
        &headers,
        &Extensions::new(),
    );
    assert_eq!(web.link("/x"), "http://127.0.0.1:8081/x");
}

#[test]
fn https_is_believed_only_from_a_trusted_proxy_and_never_on_tor() {
    let state = AppState {
        abuse: std::sync::Arc::new(crate::abuse::AbuseProtection::new(
            crate::abuse::AbuseConfig {
                trusted_proxies: TrustedProxies::parse("10.0.0.1").unwrap(),
                ..Default::default()
            },
        )),
        ..state_with(external_tor())
    };
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", "https".parse().unwrap());
    let from = |peer: &str| {
        let mut extensions = Extensions::new();
        extensions.insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        extensions
    };
    let web = state.network_of(NetworkKind::Web);
    assert!(web.is_secure(&from("10.0.0.1:5000"), &headers));
    assert!(
        !web.is_secure(&from("203.0.113.9:5000"), &headers),
        "anyone can send X-Forwarded-Proto"
    );
    assert!(!state
        .network_of(NetworkKind::Onion)
        .is_secure(&from("10.0.0.1:5000"), &headers));
    // A browser on an https page says so itself.
    let mut browser = HeaderMap::new();
    browser.insert("origin", "https://pay.example.com".parse().unwrap());
    assert!(web.is_secure(&Extensions::new(), &browser));
}

#[tokio::test]
async fn signing_in_on_tor_gives_a_cookie_tor_browser_sends_back() {
    let router = build_router(state_with(external_tor()));
    let login = |onion: bool| {
        let body = format!(
            "email={}&password={}",
            url::form_urlencoded::byte_serialize(crate::db::TEST_ADMIN_EMAIL.as_bytes())
                .collect::<String>(),
            url::form_urlencoded::byte_serialize(crate::db::TEST_ADMIN_PASSWORD.as_bytes())
                .collect::<String>(),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/dashboard/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-proto", "https")
            .header("host", "pay.example.com")
            .header("origin", "https://pay.example.com")
            .body(Body::from(body))
            .unwrap();
        if onion {
            over_tor(request)
        } else {
            request
        }
    };
    let cookie = |headers: HeaderMap| headers["set-cookie"].to_str().unwrap().to_lowercase();
    let web = cookie(headers_of(&router, login(false)).await);
    assert!(web.contains("secure"), "{web}");
    let tor = cookie(headers_of(&router, login(true)).await);
    assert!(
        !tor.contains("secure"),
        "a Secure cookie over http would be dropped: {tor}"
    );
}
