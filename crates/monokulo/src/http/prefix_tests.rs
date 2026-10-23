//! Monokulo behind a reverse proxy that serves it under a path
//! (`public_uri = "https://shop.example/monokulo"`, the proxy stripping
//! `/monokulo` before passing a request on): every URL its pages give a
//! browser must carry that path (`crate::base`), or the browser leaves
//! Monokulo. Over Tor, which reaches Monokulo directly, none may.
//!
//! The guard walks the site as a signed-in admin with a store, an order
//! and a wallet would: it starts at the pages a visitor lands on, follows
//! every link, redirect, frame, script, stylesheet and event stream it
//! finds, and checks every URL in every page, fragment and `Location`. A
//! page added later is found by following the links to it; a root-absolute
//! URL in it fails here.

use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use crate::api::runtime::recording::{fixture, Fixture, Recorder};
use crate::http::test_support::{api_request, body_json};

const PREFIX: &str = "/monokulo";

/// Attributes whose value is text or data, never a URL the browser follows:
/// a hidden `next` input keeps the path inside Monokulo, unprefixed.
const NOT_URLS: &[&str] = &[
    "value",
    "placeholder",
    "title",
    "alt",
    "aria-label",
    "pattern",
    "data-copy",
    "data-value",
    "data-full",
];

/// Every URL-bearing value in `html`: each attribute's (but [`NOT_URLS`]), a
/// meta refresh's target, a stylesheet's `url(...)`, and each string in an
/// inline script or JSON that looks like a path.
fn urls_in(html: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let attribute = regex::Regex::new(r#"\s([a-zA-Z_:][-a-zA-Z0-9_:.]*)="([^"]*)""#).unwrap();
    let tag = regex::Regex::new(r"<[a-zA-Z][^>]*>").unwrap();
    for tag in tag.find_iter(html) {
        for captures in attribute.captures_iter(tag.as_str()) {
            let (name, value) = (&captures[1], decode(&captures[2]));
            if NOT_URLS.contains(&name) {
                continue;
            }
            if name == "content" {
                if let Some((_, target)) = value.split_once(";url=") {
                    found.push((name.to_owned(), target.to_owned()));
                }
                continue;
            }
            found.push((name.to_owned(), value));
        }
    }
    let css_url = regex::Regex::new(r#"url\(\s*['"]?([^'")\s]+)"#).unwrap();
    for captures in css_url.captures_iter(html) {
        found.push(("url()".to_owned(), decode(&captures[1])));
    }
    let script = regex::Regex::new(r"(?s)<script[^>]*>(.*?)</script>").unwrap();
    let quoted_path = regex::Regex::new(r#"["'`](/[a-z][^"'`\s]*)"#).unwrap();
    for captures in script.captures_iter(html) {
        for path in quoted_path.captures_iter(&captures[1]) {
            found.push(("script".to_owned(), path[1].to_owned()));
        }
    }
    found
}

fn decode(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// Whether `value`, found in a page, is a URL that stays on this site
/// without its base path: a root-absolute path not under `prefix`.
fn escapes_prefix(value: &str, prefix: &str) -> bool {
    let value = value.trim();
    if !value.starts_with('/') || value.starts_with("//") {
        return false;
    }
    if prefix.is_empty() {
        return false;
    }
    !(value == prefix
        || value.starts_with(&format!("{prefix}/"))
        || value.starts_with(&format!("{prefix}?")))
}

/// The site path a URL from a page names, with the prefix taken off as the
/// proxy does; `None` for anything off the site or not a page to visit.
fn site_path(value: &str, prefix: &str) -> Option<String> {
    let value = value.trim();
    let rest = value.strip_prefix(prefix)?;
    let rest = rest.split('#').next()?;
    if !rest.starts_with('/') {
        return None;
    }
    Some(rest.to_owned())
}

/// One visit per path and set of query names, so paging and searching
/// don't multiply the walk.
fn visit_key(path: &str) -> String {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let names: BTreeSet<&str> = query
        .split('&')
        .filter_map(|pair| pair.split('=').next())
        .filter(|name| !name.is_empty())
        .collect();
    format!("{path}?{}", names.into_iter().collect::<Vec<_>>().join("&"))
}

/// Whether the walk should visit `path`: a page, a stream, a frame or a
/// file a page links. Not the API, which answers keys rather than
/// sessions, and not the handshakes, which need their parameters.
fn worth_visiting(path: &str) -> bool {
    !path.starts_with("/api/") && !path.starts_with("/plugins/connect")
}

struct Walk {
    router: axum::Router,
    cookie: String,
    prefix: &'static str,
    over_tor: bool,
    seen: BTreeSet<String>,
    queue: VecDeque<String>,
    problems: Vec<String>,
    pages: usize,
}

impl Walk {
    fn new(router: axum::Router, cookie: String, prefix: &'static str, over_tor: bool) -> Self {
        Walk {
            router,
            cookie,
            prefix,
            over_tor,
            seen: BTreeSet::new(),
            queue: VecDeque::new(),
            problems: Vec::new(),
            pages: 0,
        }
    }

    fn push(&mut self, path: String) {
        if worth_visiting(&path) && self.seen.insert(visit_key(&path)) {
            self.queue.push_back(path);
        }
    }

    fn check(&mut self, at: &str, what: &str, value: &str) {
        if escapes_prefix(value, self.prefix) {
            self.problems.push(format!(
                "{at}: {what} = {value:?} is not under {}",
                self.prefix
            ));
        }
        if self.over_tor && value.starts_with("/monokulo") {
            self.problems.push(format!(
                "{at}: {what} = {value:?} carries the web prefix over Tor"
            ));
        }
    }

    async fn run(&mut self, limit: usize) {
        while let Some(path) = self.queue.pop_front() {
            if self.pages >= limit {
                break;
            }
            self.pages += 1;
            self.visit(&path).await;
        }
    }

    async fn visit(&mut self, path: &str) {
        let mut request = Request::builder()
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .header(header::ACCEPT, "text/html")
            .body(Body::empty())
            .unwrap();
        if self.over_tor {
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                crate::abuse::proxy_protocol::OnionPeer {
                    source: "fc00:dead:beef:4dad::1".parse().unwrap(),
                },
            ));
        }
        let response = match tokio::time::timeout(
            Duration::from_secs(20),
            self.router.clone().oneshot(request),
        )
        .await
        {
            Ok(response) => response.unwrap(),
            Err(_) => {
                self.problems.push(format!("{path}: no answer"));
                return;
            }
        };
        // A URL that, its prefix taken off, names no route: a link given
        // with the prefix twice, or not at all where one was due.
        if response.status() == StatusCode::NOT_FOUND {
            self.problems.push(format!("{path}: not found"));
        }
        if let Some(location) = response.headers().get(header::LOCATION) {
            let location = location.to_str().unwrap().to_owned();
            self.check(path, "Location", &location);
            if let Some(next) = site_path(&location, self.prefix) {
                self.push(next);
            }
        }
        for cookie in response.headers().get_all(header::SET_COOKIE) {
            let cookie = cookie.to_str().unwrap();
            if let Some(cookie_path) = cookie
                .split(';')
                .find_map(|part| part.trim().strip_prefix("Path="))
            {
                let wanted = if self.prefix.is_empty() {
                    "/"
                } else {
                    self.prefix
                };
                if cookie_path != wanted {
                    self.problems
                        .push(format!("{path}: cookie {cookie:?} not on Path={wanted}"));
                }
            }
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let text = if content_type.starts_with("text/event-stream") {
            read_stream(response.into_body()).await
        } else if content_type.starts_with("text/html") {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            return;
        };
        for (what, value) in urls_in(&text) {
            self.check(path, &what, &value);
            if let Some(next) = site_path(&value, self.prefix) {
                self.push(next);
            }
        }
    }
}

/// What a stream sends in its first moments: its first events, fragments
/// rendered after the handler returned.
async fn read_stream(mut body: Body) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
    while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, body.frame()).await {
        if let Ok(data) = frame.into_data() {
            text.push_str(&String::from_utf8_lossy(&data));
        }
    }
    // Server-sent events carry each fragment's lines after `data: `.
    text.lines()
        .map(|line| {
            line.strip_prefix("data: ")
                .or(line.strip_prefix("data:"))
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A merchant with a store, an order on it and a wallet, behind a proxy
/// serving Monokulo at `https://shop.example/monokulo`.
async fn served_under_a_path() -> (Fixture, String, engine_test_support::TestEngineHandle) {
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let fixture = fixture(
        engine.router(),
        "https://shop.example/monokulo",
        &Recorder::default(),
    )
    .await;
    let made = fixture
        .router
        .clone()
        .oneshot(api_request(
            "POST",
            "/api/v1/orders",
            Some(&fixture.sk),
            Some(json!({"amount": "1", "currency": "XMR"})),
        ))
        .await
        .unwrap();
    assert_eq!(made.status(), StatusCode::CREATED);
    let order = body_json(made).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned();
    (fixture, order, engine)
}

/// The pages a visitor lands on before any link: the site's front doors,
/// a store's pages, an order's and its checkout.
fn landings(store: &str, order: &str) -> Vec<String> {
    vec![
        "/".to_owned(),
        "/login".to_owned(),
        "/signup".to_owned(),
        "/admin/setup".to_owned(),
        "/status".to_owned(),
        "/account".to_owned(),
        "/setup".to_owned(),
        "/setup/wallet?name=Kafe&network=mainnet".to_owned(),
        "/request-invite".to_owned(),
        "/apps/connect".to_owned(),
        "/admin/settings".to_owned(),
        "/admin/logs".to_owned(),
        "/admin/invites".to_owned(),
        format!("/stores/{store}"),
        format!("/stores/{store}/pos"),
        format!("/stores/{store}/orders/{order}"),
        format!("/checkout/{order}"),
        format!("/checkout/{order}/share"),
        format!("/checkout/{order}/events?fragments=true"),
    ]
}

/// Every URL the site's pages, fragments, streams and redirects give a
/// signed-in admin carries the base path.
#[tokio::test]
async fn every_url_a_page_gives_carries_the_path_monokulo_is_served_under() {
    let (fixture, order, _engine) = served_under_a_path().await;
    let mut walk = Walk::new(
        fixture.router.clone(),
        fixture.cookie.clone(),
        PREFIX,
        false,
    );
    for path in landings(&fixture.store_id, &order) {
        walk.push(path);
    }
    walk.run(400).await;
    assert!(walk.problems.is_empty(), "{}", walk.problems.join("\n"));
    assert!(walk.pages > 40, "walked only {} pages", walk.pages);
    // Signed out: the sign-in and sign-up pages, and the redirect to them.
    let mut walk = Walk::new(fixture.router.clone(), String::new(), PREFIX, false);
    for path in landings(&fixture.store_id, &order) {
        walk.push(path);
    }
    walk.run(100).await;
    assert!(walk.problems.is_empty(), "{}", walk.problems.join("\n"));
}

/// Over Tor, which reaches Monokulo directly, the same pages give paths at
/// the root: the web address's prefix is the proxy's, not the onion's.
#[tokio::test]
async fn over_tor_no_url_carries_the_web_address_s_path() {
    let (fixture, order, _engine) = served_under_a_path().await;
    let mut walk = Walk::new(fixture.router.clone(), fixture.cookie.clone(), "", true);
    for path in landings(&fixture.store_id, &order) {
        walk.push(path);
    }
    walk.run(400).await;
    assert!(walk.problems.is_empty(), "{}", walk.problems.join("\n"));
    assert!(walk.pages > 40, "walked only {} pages", walk.pages);
}

/// Signing in sets the session cookie on the base path, so it's sent to
/// Monokulo and not to whatever else the host serves; over Tor at `/`.
#[tokio::test]
async fn the_session_cookie_is_scoped_to_the_base_path() {
    let (fixture, _, _engine) = served_under_a_path().await;
    for (onion, path, home) in [(false, "/monokulo", "/monokulo/"), (true, "/", "/")] {
        let mut request = crate::http::test_support::form_post(
            "/login",
            None,
            &[
                ("email", crate::db::TEST_ADMIN_EMAIL),
                ("password", crate::db::TEST_ADMIN_PASSWORD),
            ],
        );
        if onion {
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                crate::abuse::proxy_protocol::OnionPeer {
                    source: "fc00:dead:beef:4dad::1".parse().unwrap(),
                },
            ));
        }
        let response = fixture.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.headers()[header::LOCATION], home);
        let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(
            cookie
                .split(';')
                .any(|part| part.trim() == format!("Path={path}")),
            "{cookie}"
        );
    }
}

#[test]
fn a_root_absolute_url_is_caught_and_a_prefixed_one_passes() {
    assert!(escapes_prefix("/login", PREFIX));
    assert!(escapes_prefix("/monokulogin", PREFIX));
    assert!(!escapes_prefix("/monokulo/login", PREFIX));
    assert!(!escapes_prefix("/monokulo", PREFIX));
    assert!(!escapes_prefix("https://docs.monokulo.com/x", PREFIX));
    assert!(!escapes_prefix("#card", PREFIX));
    assert!(!escapes_prefix("//cdn.example/x", PREFIX));
    let found = urls_in(
        r#"<a href="/a">x</a><form action="/monokulo/b"><input name="next" value="/c"><meta http-equiv="refresh" content="60;url=/d"><div data-events-url="/e"></div><style>@font-face{src:url(/f.woff2)}</style><script>fetch("/g")</script>"#,
    );
    let values: Vec<&str> = found.iter().map(|(_, value)| value.as_str()).collect();
    for wanted in ["/a", "/monokulo/b", "/d", "/e", "/f.woff2", "/g"] {
        assert!(values.contains(&wanted), "{wanted} in {values:?}");
    }
    assert!(!values.contains(&"/c"), "{values:?}");
}

/// Every redirect to a page on this site goes through `crate::base`
/// (`redirect`, `see_other`), so its `Location` carries the base path. The
/// walk above follows the redirects a `GET` gives; this keeps the ones a
/// form post gives on the same road. Setting `Location` by hand is left to
/// `base` itself, the API (whose links carry the base explicitly), the
/// challenge's redirect (made with `base::url`) and a redirect off the site
/// (`auth::redirect_away`).
#[test]
fn every_redirect_goes_through_the_base_path() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let allowed = [
        "base.rs",
        "api/runtime/hal.rs",
        "http/abuse.rs",
        "http/auth.rs",
    ];
    let mut dirs = vec![src.clone()];
    let mut by_hand = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let name = path
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if !name.ends_with(".rs")
                || name.ends_with("tests.rs")
                || name.contains("/tests/")
                || name.ends_with("test_support.rs")
                || allowed.contains(&name.as_str())
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            // The code, not its unit tests.
            let code = source.split("#[cfg(test)]\nmod tests").next().unwrap();
            for (number, line) in code.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains("Redirect::") || line.contains("header::LOCATION,") {
                    by_hand.push(format!("{name}:{}: {}", number + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        by_hand.is_empty(),
        "redirects not through crate::base:\n{}",
        by_hand.join("\n")
    );
}

/// The scripts' own URLs: a script makes a URL on this site only onto the
/// base path the page gives it (`siteUrl` from `@monokulo/ui/site`, or the
/// `monokulo-base` meta's content `+ "/path"`), or onto an address it was
/// given (the embed library's). A bare `"/path"` would leave a Monokulo
/// served under a path. Route names a script only compares or rewrites
/// (`.replace`, `.startsWith`) aren't URLs.
#[test]
fn scripts_make_site_urls_only_on_the_base_path() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Others' scripts, kept as they come.
    let vendored = ["fixi.js", "ssexi.js", "jsQR.js"];
    let mut files = Vec::new();
    let mut dirs = vec![
        root.join("crates/monokulo/static"),
        root.join("apps/pos/src"),
        root.join("apps/engine/src"),
        root.join("crates/ui/web"),
    ];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path
                .extension()
                .is_some_and(|ext| ext == "js" || ext == "ts" || ext == "tsx")
                && !vendored.iter().any(|name| path.ends_with(name))
            {
                files.push(path);
            }
        }
    }
    assert!(files.len() > 20, "found only {files:?}");
    let literal = regex::Regex::new(r#"["'`]/[a-z{]"#).unwrap();
    let mut bare = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap();
        for (number, line) in source.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with('*') || code.starts_with("/*") {
                continue;
            }
            if line.contains(".replace(") || line.contains(".startsWith(") {
                continue;
            }
            for found in literal.find_iter(line) {
                let before = line[..found.start()].trim_end();
                if !(before.ends_with('+') || before.ends_with("siteUrl(")) {
                    bare.push(format!(
                        "{}:{}: {}",
                        file.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        bare.is_empty(),
        "URLs not on the base path:\n{}",
        bare.join("\n")
    );
}
