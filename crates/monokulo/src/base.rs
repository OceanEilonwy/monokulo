//! Where this Monokulo's paths start: its base path, and the one way every
//! URL it gives a browser is made.
//!
//! A reverse proxy may serve Monokulo under a path (`public_uri` is
//! `https://shop.example/monokulo`), stripping that prefix before passing a
//! request on. Monokulo's routes stay at the root (`/login`, `/stores/{id}`),
//! but every URL it hands out must carry the prefix, or the browser leaves
//! Monokulo: links, form actions, `Location`, asset URLs, fetch and stream
//! URLs, a meta refresh, the session cookie's `Path`.
//!
//! The prefix depends on the network a request arrived on ([`for_request`]):
//! the web address's path, but nothing on the onion address, which tor
//! reaches directly. So it is a per-request value: [`layer`] works it out
//! once per request and keeps it for everything that request renders,
//! the handler and the response body alike (a server-sent event stream
//! renders its fragments long after the handler returned). [`url`] turns a
//! path on this site into the URL to give, [`redirect`] sends a browser
//! there; nothing builds a site URL any other way. Paths kept inside
//! Monokulo (a login's `next`, a page's `current_path`) stay unprefixed and
//! become URLs only through [`url`].
//!
//! Scripts read the same prefix from the `<meta name="monokulo-base">` every
//! page carries (`views::page_shell`), for the few URLs they make
//! themselves; every other URL a script uses comes from the page, already
//! made by [`url`].
//!
//! The machine API takes its prefix explicitly from [`for_request`] too
//! (`api::runtime::auth::Context::base`), since its links are made in
//! handlers, not views.
//!
//! Outside a request (a unit test rendering a view), the prefix is empty.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::http::AppState;

tokio::task_local! {
    static BASE: Arc<str>;
}

/// Where this Monokulo's paths start for a request, on the network it came
/// on: the path of its address there, without a trailing `/`. `/monokulo`
/// when `public_uri` is `https://host/monokulo` (a reverse proxy serves it
/// under that prefix and strips it before passing a request on); empty at
/// the root of its host, on the onion address (tor reaches monokulo
/// directly), and while no address is set.
pub fn for_request(state: &AppState, extensions: &axum::http::Extensions) -> String {
    let network = state.network_of(crate::network::NetworkKind::of(extensions));
    network
        .public_uri()
        .and_then(|uri| url::Url::parse(&uri).ok())
        .map(|uri| uri.path().trim_end_matches('/').to_owned())
        .unwrap_or_default()
}

/// The current request's base path ([`for_request`]): `/monokulo`, or empty.
pub fn path() -> String {
    BASE.try_with(|base| base.to_string()).unwrap_or_default()
}

/// `path`, a path on this site starting with `/` (with any query or
/// fragment), as the URL to give a browser: under the current request's
/// base path. An empty path stays empty: the page itself, wherever it is.
pub fn url(path: impl AsRef<str>) -> String {
    let path = path.as_ref();
    if path.is_empty() {
        return String::new();
    }
    debug_assert!(
        path.starts_with('/') && !path.starts_with("//"),
        "not a path on this site: {path}"
    );
    BASE.try_with(|base| format!("{base}{path}"))
        .unwrap_or_else(|_| path.to_owned())
}

/// A `302 Found` to `path`, a path on this site ([`url`]), for sending a
/// browser on after a `GET` or after logging in (axum's own
/// `Redirect::to` issues `303 See Other`).
pub fn redirect(path: impl AsRef<str>) -> Response {
    location(StatusCode::FOUND, &url(path))
}

/// A `303 See Other` to `path` ([`url`]): after a form post, "see this
/// page", fetched with a `GET` whatever the post was, so reloading it never
/// posts again.
pub fn see_other(path: impl AsRef<str>) -> Response {
    location(StatusCode::SEE_OTHER, &url(path))
}

fn location(status: StatusCode, location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) => (status, [(header::LOCATION, value)]).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Runs `f` with `base` as the base path, as a request's handler runs: for
/// tests rendering a view as a prefixed deployment would.
pub fn sync_scope<R>(base: &str, f: impl FnOnce() -> R) -> R {
    BASE.sync_scope(Arc::from(base), f)
}

/// Middleware keeping the request's base path ([`for_request`]) for its
/// handler and for its response body, so whatever renders for the request
/// ([`url`]) carries it.
pub async fn layer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let base: Arc<str> = Arc::from(for_request(&state, request.extensions()));
    let response = BASE.scope(base.clone(), next.run(request)).await;
    response.map(|body| Body::new(Scoped { base, body }))
}

/// A response body polled with its request's base path set: a stream's
/// later frames render after the handler has returned.
struct Scoped {
    base: Arc<str>,
    body: Body,
}

impl http_body::Body for Scoped {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        let body = &mut this.body;
        BASE.sync_scope(this.base.clone(), || Pin::new(body).poll_frame(cx))
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_given_under_the_base_and_unchanged_outside_a_request() {
        assert_eq!(url("/login?next=/stores"), "/login?next=/stores");
        sync_scope("/monokulo", || {
            assert_eq!(url("/login"), "/monokulo/login");
            assert_eq!(url("/"), "/monokulo/");
            assert_eq!(path(), "/monokulo");
        });
        sync_scope("", || assert_eq!(url("/login"), "/login"));
        sync_scope("/monokulo", || assert_eq!(url(""), ""));
    }

    #[test]
    fn a_redirect_carries_the_base() {
        let response = sync_scope("/monokulo", || redirect("/stores"));
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()[header::LOCATION], "/monokulo/stores");
        let response = sync_scope("/monokulo", || see_other("/account"));
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/monokulo/account");
    }
}
