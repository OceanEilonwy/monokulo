//! Cross-site request forgery protection for the site's own routes: the
//! dashboard's forms, login, sign-up and the admin pages.
//!
//! A browser tells the server where a state-changing request came from:
//! `Sec-Fetch-Site` on every request from a current browser, `Origin` on
//! every cross-site `POST` from any browser of the last decade. A request
//! that says it came from another site is refused before any handler
//! runs. That holds whether or not a session cookie is sent, so a login
//! form posted from an attacker's page (login CSRF, signing the victim
//! into the attacker's account) is refused too, which `SameSite=Lax` on
//! the cookie alone never covers.
//!
//! A request with neither header (a plugin's server, `curl`, an old
//! browser's same-site form) is let through: neither header is how a
//! cross-site browser request arrives. API clients authenticate with a
//! bearer token, which a cross-site page can't attach, so a request that
//! carries `Authorization` is let through whatever its origin.
//!
//! Not for the public payment routes (`/pay/…`): those are called from
//! merchants' sites by design, under their own CORS and embed policy.

use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Refuses a state-changing request that a browser says came from another
/// site. See the module doc comment.
pub async fn same_origin_middleware(request: Request, next: Next) -> Response {
    if !is_state_changing(request.method()) || request.headers().contains_key(header::AUTHORIZATION)
    {
        return next.run(request).await;
    }
    let headers = request.headers();
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if same_origin(host, site, origin) {
        next.run(request).await
    } else {
        tracing::warn!(
            origin,
            sec_fetch_site = site,
            "refused a cross-site request to a state-changing route"
        );
        (
            StatusCode::FORBIDDEN,
            "This request came from another site and was refused.",
        )
            .into_response()
    }
}

fn is_state_changing(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// Whether a request with these headers is the site's own: by
/// `Sec-Fetch-Site` when a browser sent it (`same-origin`, or `none` for a
/// request the user typed or bookmarked), else by `Origin` naming this
/// host, else (neither sent) taken as not from a cross-site browser page.
fn same_origin(host: Option<&str>, sec_fetch_site: Option<&str>, origin: Option<&str>) -> bool {
    match sec_fetch_site {
        Some("same-origin" | "none") => return true,
        Some(_) => return false,
        None => {}
    }
    match origin {
        // A page from an opaque origin (`null`): not this site's.
        None => true,
        Some(origin) => {
            let origin_host = origin
                .strip_prefix("https://")
                .or_else(|| origin.strip_prefix("http://"));
            match (origin_host, host) {
                (Some(origin_host), Some(host)) => origin_host.eq_ignore_ascii_case(host),
                _ => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browser_saying_cross_site_is_refused_and_the_sites_own_requests_pass() {
        let host = Some("pay.example");
        assert!(same_origin(host, Some("same-origin"), None));
        assert!(same_origin(host, Some("none"), None));
        assert!(!same_origin(host, Some("cross-site"), None));
        assert!(
            !same_origin(host, Some("same-site"), Some("https://pay.example")),
            "a sibling subdomain is not this site, whatever Origin says"
        );
        assert!(same_origin(host, None, Some("https://pay.example")));
        assert!(same_origin(host, None, Some("http://PAY.example")));
        assert!(!same_origin(host, None, Some("https://evil.example")));
        assert!(!same_origin(host, None, Some("null")));
        assert!(
            !same_origin(None, None, Some("https://pay.example")),
            "no Host to compare with"
        );
        assert!(
            same_origin(host, None, None),
            "a request without either header is not a cross-site browser request"
        );
    }

    #[test]
    fn only_state_changing_methods_are_checked() {
        assert!(is_state_changing(&Method::POST));
        assert!(is_state_changing(&Method::DELETE));
        assert!(!is_state_changing(&Method::GET));
        assert!(!is_state_changing(&Method::HEAD));
        assert!(!is_state_changing(&Method::OPTIONS));
    }
}
