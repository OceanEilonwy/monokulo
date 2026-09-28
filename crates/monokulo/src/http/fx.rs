//! Serving fixi's partial requests from the same handlers as full pages
//! (structured_logging.md 4.2).
//!
//! A form enhanced with fixi posts as usual but with an `FX-Request`
//! header, and swaps whatever comes back into one section of the page.
//! `fetch` follows redirects by itself, so answering such a request with
//! the usual post-then-redirect would swap a whole page into that section:
//! a handler answers it with the section's fragment instead ([`respond`]).
//! Without JavaScript nothing changes: post, redirect, reload.

use std::convert::Infallible;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use maud::Markup;

/// The request header fixi sends.
pub const FX_REQUEST: &str = "fx-request";

/// Whether the request came from fixi (true) or is an ordinary navigation
/// or form post (false).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FxRequest(pub bool);

impl<S: Send + Sync> FromRequestParts<S> for FxRequest {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(FxRequest(parts.headers.get(FX_REQUEST).is_some_and(|v| v.as_bytes() == b"true")))
    }
}

/// The browser's time zone, as fixi's glue script sends it: the
/// `X-Timezone` header on its own requests, and a `tz` cookie it sets for
/// full page loads. Only plausible zone names are believed; whether the
/// zone exists is up to whoever formats with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timezone(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for Timezone {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Timezone(browser_zone(&parts.headers)))
    }
}

/// [`Timezone`]'s zone, from a request's headers.
pub fn browser_zone(headers: &axum::http::HeaderMap) -> Option<String> {
    let plausible = |z: &str| !z.is_empty() && z.len() <= 64 && z.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c));
    let header = headers.get("x-timezone").and_then(|v| v.to_str().ok()).map(str::to_string);
    let cookie = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix("tz="))
        .map(|z| z.replace("%2F", "/").replace("%2f", "/"))
        .next();
    header.into_iter().chain(cookie).find(|z| plausible(z))
}

/// Answers a form post: for fixi, the section's fragment (with its saved
/// or error state rendered in); otherwise the usual redirect. `fragment`
/// is only rendered when it's needed.
pub fn respond(fx: FxRequest, redirect_to: &str, fragment: impl FnOnce() -> Markup) -> Response {
    if fx.0 {
        Html(fragment().into_string()).into_response()
    } else {
        crate::http::dashboard::redirect_302(redirect_to)
    }
}

/// A fragment answering a fixi request that failed validation: the
/// section with its errors, as `422` (fixi swaps it like a success; the
/// glue script only stops a 5xx from being swapped).
pub fn invalid(fragment: Markup) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Html(fragment.into_string())).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn handler(fx: FxRequest) -> Response {
        respond(fx, "/done", || maud::html! { section id="s" { "saved" } })
    }

    #[tokio::test]
    async fn a_fixi_post_gets_the_fragment_and_any_other_post_the_redirect() {
        let router = Router::new().route("/save", post(handler));
        let plain = router.clone().oneshot(Request::post("/save").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(plain.status(), StatusCode::FOUND);
        assert_eq!(plain.headers()["location"], "/done");

        let fx = router.oneshot(Request::post("/save").header("FX-Request", "true").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(fx.status(), StatusCode::OK);
        let body = fx.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], br#"<section id="s">saved</section>"#);
    }

    #[tokio::test]
    async fn only_a_plausible_time_zone_is_believed() {
        let zone = |value: &str| {
            let (mut parts, _) = Request::get("/").header("x-timezone", value).body(()).unwrap().into_parts();
            futures_util::FutureExt::now_or_never(Timezone::from_request_parts(&mut parts, &())).unwrap().unwrap().0
        };
        assert_eq!(zone("Europe/London"), Some("Europe/London".into()));
        assert_eq!(zone("America/Argentina/Buenos_Aires"), Some("America/Argentina/Buenos_Aires".into()));
        assert_eq!(zone("<script>"), None);
        let (mut parts, _) = Request::get("/").header("cookie", "session=x; tz=Asia%2FTokyo").body(()).unwrap().into_parts();
        let from_cookie = futures_util::FutureExt::now_or_never(Timezone::from_request_parts(&mut parts, &())).unwrap().unwrap().0;
        assert_eq!(from_cookie.as_deref(), Some("Asia/Tokyo"));
    }
}
