//! Composable, compile-time-checked HTML rendering (Maud), replacing the
//! old `handlebars`-based `TemplateEngine`/`.hbs` files.
//!
//! The thing that made adding a per-user theme awkward under the old setup
//! was structural, not cosmetic: every one of the ~20 old templates carried
//! its own literal `<html>`/`<head>`/`{{> nav}}` - there was no single place
//! that owned the page shell. [`layout`] and [`nav`] below are that place
//! now: every page in this module builds its own body content and hands it
//! to [`layout`], which owns `<!doctype html>` through `</html>`, the shared
//! `<head>` (the colours in `theme.css`, the components in `site.css`), and
//! the nav bar
//! (including the theme-toggle form) - so a per-user `data-theme` attribute,
//! or anything else every page needs, is a one-place change from here on.
//!
//! One submodule per page/page-group, mirroring `http/`'s own per-feature
//! split rather than one flat file.

use maud::{html, Markup, PreEscaped, DOCTYPE};
pub use shared::order_status::OrderStatus;

use crate::assets;
use crate::db::{Theme, UserRow};

/// Where the docs are: the guides for adding Monokulo to a site, built and
/// published with the GitHub Pages site (`cargo xtask pages docs`).
pub const DOCS_URL: &str = "https://oceaneilonwy.github.io/monokulo/docs/";

/// A docs page, by its file under [`DOCS_URL`] (`js-library/`).
pub fn docs_url(page: &str) -> String {
    format!("{DOCS_URL}{page}")
}

/// The small "opens elsewhere" arrow after a link to another site.
pub fn external_link_icon() -> Markup {
    html! {
        svg class="external-icon" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
            path d="M14 4h6v6M20 4l-9 9M18 14v6H4V6h6" {}
        }
    }
}

/// How a page loads a script: as the parser reaches it, once the page is
/// parsed (`defer`), or as a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    Now,
    Defer,
    Module,
}

/// A `<script>` for a script baked into the binary, by its name
/// (`crate::assets`), at the URL its content gives it.
pub fn script(name: &'static str, load: Load) -> Markup {
    let src = assets::url(name);
    match load {
        Load::Now => html! { script src=(src) {} },
        Load::Defer => html! { script src=(src) defer {} },
        Load::Module => html! { script type="module" src=(src) {} },
    }
}

/// A `<link rel="stylesheet">` for a stylesheet baked into the binary, by
/// its name (`crate::assets`), at the URL its content gives it.
pub fn stylesheet(name: &'static str) -> Markup {
    html! { link rel="stylesheet" href=(assets::url(name)); }
}

/// The `@font-face` rules for Manrope, the UI typeface, at the font files'
/// versioned URLs (`crate::assets`): the first thing in every page's style.
/// Self-hosted, not a Google Fonts `<link>`, deliberately: that CDN sees
/// every visitor's IP on every page load, checkout included, which is the
/// wrong tradeoff for a privacy-focused payment tool. Latin-only, matching
/// this UI's own text.
pub fn font_faces() -> String {
    [500, 700, 800]
        .map(|weight| {
            format!(
                "@font-face{{font-family:\"Manrope\";font-style:normal;font-weight:{weight};font-display:swap;src:url(\"{}\") format(\"woff2\")}}\n",
                assets::url(&format!("manrope-{weight}.woff2"))
            )
        })
        .concat()
}

pub mod account;
pub mod admin;
pub mod auth;
pub mod challenge;
pub mod checkout;
pub mod connect;
pub mod controls;
pub mod create_order;
pub mod dashboard;
pub mod engine;

pub mod key_entry;
mod logo_art;
pub mod logs;
pub mod orders;
pub mod pos;
pub mod scaling;
pub mod settings;
pub mod setup;
pub mod status;
pub mod store_detail;
pub mod store_settings;
pub mod time;
pub mod wallets;

/// Every colour, and the spacing, radius and type tokens: the only place a
/// colour is defined (`theme_tests`).
pub(crate) const THEME_CSS: &str = include_str!("theme.css");
/// The fonts and every shared component, painted with `THEME_CSS`'s roles.
pub(crate) const SITE_CSS: &str = include_str!("site.css");

/// Shared back links for store pages and pages nested under Orders.
pub fn store_breadcrumb(connection_id: &str, display_name: &str, include_orders: bool) -> Markup {
    html! {
        nav class="context-nav" aria-label="Breadcrumb" {
            a href=(format!("/dashboard/stores/{connection_id}")) title=(display_name) { (display_name) }
            @if include_orders {
                span class="breadcrumb-sep" aria-hidden="true" { "›" }
                a href=(format!("/dashboard/stores/{connection_id}/orders")) { "Orders" }
            }
        }
    }
}

/// Every value a page needs to render the parts every page shares (the
/// `<html data-theme>` attribute and the nav bar) - built once per request,
/// almost always via [`PageChrome::from_user`], and threaded through to
/// whichever `views::*::page(...)` function is actually rendering.
pub struct PageChrome {
    pub logged_in: bool,
    pub is_admin: bool,
    /// The signed-in user's email, for the account menu (empty when no one
    /// is signed in).
    pub email: String,
    /// How many wallets the signed-in user has, shown by the account
    /// menu's Wallets shortcut (`crate::http::page_chrome` counts them).
    pub wallet_count: usize,
    pub theme: Theme,
    /// The current request's own path (+ query string, where relevant) -
    /// carried as the theme-toggle form's hidden `next` field so toggling
    /// theme redirects back to the page it was toggled from, not a fixed
    /// default. Checked into a `SafePath` (`crate::http::dashboard`) before
    /// ever being used as a redirect target, same as the login
    /// flow's own `next` - never trusted at face value just because it
    /// came from this struct.
    pub current_path: String,
    /// The engine's last known health for the status indicator, `None`
    /// while not known yet. See `crate::http::status_page::known_health`.
    pub health: Option<Health>,
    /// Problems the signed-in merchant needs to know about on every page
    /// (task 3.7): their stores that can't be scanned right now. Shown under
    /// the nav on every page that has one, so never in the POS terminal.
    pub alerts: Vec<String>,
    /// The page loads `static/telemetry.js`. Always on pages that aren't
    /// about one store; on a store's pages, only when it has opted in to
    /// client logs (`crate::http::page_chrome`).
    pub browser_reports: bool,
    /// The zone this page shows times in (`views::time`): the signed-in
    /// user's, or UTC.
    pub clock: time::Clock,
}

impl PageChrome {
    /// The constructor almost every caller wants: `user` is whatever
    /// [`AuthedUser`](crate::http::AuthedUser)/[`AuthedAdmin`](crate::http::AuthedAdmin)
    /// already resolved, or `None` for an unauthenticated page (still real
    /// per-request state for `/status`, which shows the account menu to a
    /// visitor who happens to have a session).
    pub fn from_user(user: Option<&UserRow>, current_path: impl Into<String>) -> Self {
        match user {
            Some(u) => PageChrome {
                logged_in: true,
                is_admin: u.is_admin,
                email: u.email.clone(),
                wallet_count: 0,
                theme: u.theme,
                current_path: current_path.into(),
                health: None,
                alerts: Vec::new(),
                browser_reports: true,
                clock: time::Clock::for_user(u),
            },
            None => PageChrome {
                logged_in: false,
                is_admin: false,
                email: String::new(),
                wallet_count: 0,
                theme: Theme::System,
                current_path: current_path.into(),
                health: None,
                alerts: Vec::new(),
                browser_reports: true,
                clock: time::Clock::utc(crate::now_unix()),
            },
        }
    }

    pub fn with_health(mut self, health: Option<Health>) -> Self {
        self.health = health;
        self
    }

    pub fn with_alerts(mut self, alerts: Vec<String>) -> Self {
        self.alerts = alerts;
        self
    }
}

fn theme_attr(theme: Theme) -> Option<&'static str> {
    match theme {
        Theme::System => None,
        Theme::Light => Some("light"),
        Theme::Dark => Some("dark"),
    }
}

/// The page shell every page in this module renders through: everything
/// from `<!doctype html>` to `</html>`, nav bar included. `title` is the
/// full `<title>` text (not auto-suffixed: each caller states its own
/// title exactly).
const DEFAULT_VIEWPORT: &str = "width=device-width, initial-scale=1";

pub fn layout(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(
        chrome,
        title,
        DEFAULT_VIEWPORT,
        None,
        Some(nav(chrome)),
        body,
    )
}

/// Same page shell, but with no nav bar - for a screen deliberately built
/// to have no site chrome at all (the POS terminal - see `views::pos`'s own
/// doc comment). Still carries the shared `<head>` and the per-user
/// `data-theme`, since a merchant who chose dark mode should get it here
/// too, even without a nav to toggle it from.
pub fn layout_bare(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(chrome, title, DEFAULT_VIEWPORT, None, None, body)
}

/// Same as [`layout`], plus arbitrary extra `<head>` content (e.g. a
/// conditional `<meta http-equiv="refresh">`) rendered right after the
/// shared head partial - for the handful of pages that need one.
pub fn layout_with_head(
    chrome: &PageChrome,
    title: &str,
    extra_head: Markup,
    body: Markup,
) -> Markup {
    page_shell(
        chrome,
        title,
        DEFAULT_VIEWPORT,
        Some(extra_head),
        Some(nav(chrome)),
        body,
    )
}

/// [`layout_bare`] plus extra `<head>` content and a custom `viewport`
/// (the POS terminal wants `maximum-scale=1, viewport-fit=cover` - no
/// accidental pinch-zoom on a counter device, and safe-area insets around a
/// notch/home-indicator - see `views::pos`'s own doc comment).
pub fn layout_bare_with_head(
    chrome: &PageChrome,
    title: &str,
    viewport: &str,
    extra_head: Markup,
    body: Markup,
) -> Markup {
    page_shell(chrome, title, viewport, Some(extra_head), None, body)
}

fn page_shell(
    chrome: &PageChrome,
    title: &str,
    viewport: &str,
    extra_head: Option<Markup>,
    nav: Option<Markup>,
    body: Markup,
) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" data-theme=[theme_attr(chrome.theme)] {
            head {
                meta charset="utf-8";
                meta name="viewport" content=(viewport);
                title { (title) }
                link rel="icon" type="image/svg+xml" href=(assets::url("favicon.svg"));
                // The typeface, then the colours, then the components:
                // the stylesheets at their versioned URLs (crate::assets),
                // kept by the browser for a year once seen.
                style { (PreEscaped(font_faces())) }
                (stylesheet("theme.css"))
                (stylesheet("site.css"))
                // The trace of the request that rendered this page, so
                // browser reports join it (structured_logging.md 2.5).
                @if let Some(traceparent) = telemetry::trace::current_traceparent() {
                    meta name="traceparent" content=(traceparent);
                }
                // Browser problem reports on the site's own pages only: not
                // the challenge page or the POS app, which render without
                // nav, and on a store's pages only when it opted in. (The
                // checkout embed adds its own, also only when opted in.)
                @if nav.is_some() && chrome.browser_reports {
                    (script("telemetry.js", Load::Defer))
                }
                @if nav.is_some() {
                    // Partial updates and live streams (structured_logging.md
                    // part 4), in this order: the glue sets fixi's defaults.
                    (script("fx-glue.js", Load::Defer))
                    (script("fixi.js", Load::Defer))
                    (script("ssexi.js", Load::Defer))
                    // Every dropdown (`views::controls`).
                    (script("mk-select.js", Load::Defer))
                    // Every settings form (`views::settings`).
                    (script("settings-form.js", Load::Defer))
                }
                @if let Some(extra_head) = extra_head {
                    (extra_head)
                }
            }
            body {
                @if let Some(nav) = nav {
                    (nav)
                    @if !chrome.alerts.is_empty() {
                        div class="wrap site-alerts" {
                            @for alert in &chrome.alerts {
                                p class="error" role="alert" { (alert) }
                            }
                        }
                    }
                }
                (body)
            }
        }
    }
}

/// The Monokulo mark, inline: a monocle whose lens is cut like a stone, its
/// facets laid out as a curve tree (`cargo xtask logo` draws it and explains
/// the design). Its lines are `currentColor`, so it takes the text colour of
/// whatever it sits on in either theme; the facets are the `--logo-*` roles.
/// Below 48px the facet lines would blur into the fill, so the small drawing
/// leaves them out and thickens the chain. `static/logo.svg` is the full
/// drawing for other sites to link to.
pub fn logo_mark(size: u32, class: &str) -> Markup {
    let art = if size >= 48 {
        logo_art::FULL
    } else {
        logo_art::SMALL
    };
    html! {
        svg class=(format!("logo-mark {class}")) viewBox="0 0 64 64" width=(size) height=(size) aria-hidden="true" focusable="false" {
            (PreEscaped(art))
        }
    }
}

/// What a page shows as an order's status: the engine's, or `Cancelled`
/// for a POS order cancelled before anything was paid (monokulo's own
/// record; the engine has no such status). Serialized by name, as the POS
/// app and the pages' `data-status` attributes read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStatus {
    Order(OrderStatus),
    Cancelled,
}

impl From<OrderStatus> for DisplayStatus {
    fn from(status: OrderStatus) -> Self {
        DisplayStatus::Order(status)
    }
}

impl From<&OrderStatus> for DisplayStatus {
    fn from(status: &OrderStatus) -> Self {
        DisplayStatus::Order(*status)
    }
}

impl From<&DisplayStatus> for DisplayStatus {
    fn from(status: &DisplayStatus) -> Self {
        *status
    }
}

impl DisplayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DisplayStatus::Order(status) => status.as_str(),
            DisplayStatus::Cancelled => "cancelled",
        }
    }
}

impl DisplayStatus {
    /// A status by its name, `cancelled` included, for tests.
    #[cfg(test)]
    pub fn named(name: &str) -> Self {
        match name {
            "cancelled" => DisplayStatus::Cancelled,
            other => DisplayStatus::Order(other.parse().expect("a known status")),
        }
    }
}

impl std::fmt::Display for DisplayStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for DisplayStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// An order status's words and its `.state-*` colour class, and whether the
/// order can still change. The checkout, the POS and the dashboard show a
/// status with the same words and colours.
pub fn order_state(status: impl Into<DisplayStatus>) -> (&'static str, &'static str, bool) {
    use OrderStatus::*;
    match status.into() {
        DisplayStatus::Order(Pending) => ("Waiting for payment", "state-pending", false),
        DisplayStatus::Order(Unconfirmed) => {
            ("Payment seen, unconfirmed", "state-unconfirmed", false)
        }
        DisplayStatus::Order(Confirming) => ("Confirming", "state-confirming", false),
        DisplayStatus::Order(Partial) => ("Partial payment received", "state-partial", false),
        DisplayStatus::Order(Paid) => ("Paid", "state-paid", true),
        DisplayStatus::Order(Overpaid) => ("Overpaid", "state-overpaid", true),
        DisplayStatus::Order(Expired) => ("Expired", "state-expired", true),
        DisplayStatus::Cancelled => ("Cancelled", "state-cancelled", true),
    }
}

/// A status's words short enough for a phone's table column ("Waiting"
/// for "Waiting for payment"). The long words stay for screen readers.
pub fn order_state_short(status: impl Into<DisplayStatus>) -> &'static str {
    match status.into() {
        DisplayStatus::Order(OrderStatus::Pending) => "Waiting",
        DisplayStatus::Order(OrderStatus::Unconfirmed) => "Seen",
        DisplayStatus::Order(OrderStatus::Partial) => "Part paid",
        other => order_state(other).0,
    }
}

/// An order's status as a badge ([`order_state`]): the full words, and the
/// short ones that replace them on a narrow screen (`.label-short`).
pub fn state_badge(status: impl Into<DisplayStatus> + Copy) -> Markup {
    let (label, class, _) = order_state(status);
    let short = order_state_short(status);
    html! {
        span class=(format!("tag {class}")) {
            @if short == label { (label) } @else {
                span class="label-long" { (label) }
                span class="label-short" aria-hidden="true" { (short) }
            }
        }
    }
}

/// An order id as people read it: without the `order_` every id starts
/// with, cut in the middle when it doesn't fit (as the macOS Finder cuts a
/// long file name), so its start and its last characters both show. The
/// whole id stays in the page, for find-in-page, and in `title`.
pub fn order_id_short(order_id: &str) -> Markup {
    let id = order_id.strip_prefix("order_").unwrap_or(order_id);
    mid_ellipsis(id, 6, order_id)
}

/// A wallet address as [`order_id_short`] shows an id: its start, an
/// ellipsis in the middle when it doesn't fit, and its last seven
/// characters whole. The whole address stays in the page and in `title`.
pub fn address_short(address: &str) -> Markup {
    mid_ellipsis(address, 7, address)
}

/// `text` cut in the middle when it doesn't fit (`.mid-ellipsis`), its
/// last `tail` characters always shown.
fn mid_ellipsis(text: &str, tail: usize, title: &str) -> Markup {
    let split = text
        .char_indices()
        .rev()
        .nth(tail - 1)
        .map_or(0, |(i, _)| i);
    html! {
        span class="mid-ellipsis" title=(title) {
            span class="mid-head" { (&text[..split]) }
            span class="mid-tail" { (&text[split..]) }
        }
    }
}

/// A Monero network as a badge (`.tag-network`), the same wherever a
/// network is shown: the word always, with an icon and an edge to back it
/// up. Mainnet (real money) has the brand's orange tint and a solid edge;
/// a test network a neutral tint and a dashed edge.
pub fn network_badge(network: &str) -> Markup {
    if network == "mainnet" {
        network_tag(true, "Mainnet")
    } else {
        network_tag(false, &network_word(network))
    }
}

/// The badge for the test networks together: the wallets list's fold.
pub fn test_networks_badge() -> Markup {
    network_tag(false, "Test networks")
}

/// A network's name as a word: "Stagenet".
fn network_word(network: &str) -> String {
    let mut chars = network.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The icons of [`network_badge`]: a coin marked M for mainnet, a flask
/// for a test network. `static/mk-select.js` draws the same two.
fn network_tag(main: bool, word: &str) -> Markup {
    html! {
        span class=(if main { "tag-network is-main" } else { "tag-network is-test" }) {
            svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" {
                @if main {
                    circle cx="12" cy="12" r="9" {}
                    path d="M7 15V9l5 5 5-5v6" {}
                } @else {
                    path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 1.7 3h10.6a2 2 0 0 0 1.7-3l-5-9V3" {}
                    path d="M7.5 15h9" {}
                }
            }
            (word)
        }
    }
}

/// An amount for people to read: XMR without its trailing zeros
/// (`0.420000000000` is `0.42`), anything else as given (`12.50` stays).
pub fn display_amount(amount: &str, currency: &str) -> String {
    let amount = if currency == "XMR" {
        trim_xmr(amount)
    } else {
        amount
    };
    if currency.is_empty() {
        amount.to_string()
    } else {
        format!("{amount} {currency}")
    }
}

/// An exact XMR amount without trailing zeros (`0.001000000000` is `0.001`).
pub fn trim_xmr(amount: &str) -> &str {
    if amount.contains('.') {
        amount.trim_end_matches('0').trim_end_matches('.')
    } else {
        amount
    }
}

/// A link to the Logs page searching for `field = value` over everything
/// kept, for admins on the pages about that thing (an order, a store).
pub fn logs_link(chrome: &PageChrome, field: &str, value: &str, text: &str) -> Markup {
    let query = format!(
        "{field} = '{}'",
        value.replace('\\', "\\\\").replace('\'', "\\'")
    );
    let href = format!(
        "/dashboard/admin/logs?q={}&range=all",
        url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
    );
    html! {
        @if chrome.is_admin {
            a class="logs-link" href=(href) { (text) }
        }
    }
}

/// The Reload button a point-in-time page shows instead of refreshing by
/// itself (structured_logging.md D4): without JavaScript these pages are a
/// snapshot, and a reader reloads when they want the latest. `class` is
/// `reload` so the page's own script can hide it once it streams updates.
pub fn reload_button(href: &str) -> Markup {
    html! {
        a class="btn btn-secondary reload" href=(href) aria-label="Refresh page" title="Refresh page" {
            svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
                path d="M20 7v5h-5M4 17v-5h5M6.1 7a7 7 0 0 1 11.6-2L20 8M4 16l2.3 3A7 7 0 0 0 17.9 17" {}
            }
        }
    }
}

/// The engine's health as the status indicator shows it: green, yellow or
/// red (docs/engine_scaling.md section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Everything answers and scans.
    Ok,
    /// Everything works, but one block has taken over two minutes to scan.
    Slow,
    /// A node or the scan is failing, or the engine can't be reached.
    Problem,
}

impl Health {
    /// The name the summary endpoint and the dot's class use.
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Slow => "slow",
            Health::Problem => "error",
        }
    }
}

/// Polls `GET /status/summary` and updates the indicator in place. Starts
/// straight away only when the page was rendered without a known health;
/// skips polls while the tab is hidden.
const STATUS_INDICATOR_SCRIPT: &str = r#"(function () {
  var link = document.getElementById("status-indicator");
  if (!link) return;
  var dot = link.querySelector(".status-dot");
  var POLL_MS = 30000;
  function show(state, title) { dot.className = "status-dot status-dot-" + state; link.title = title; link.setAttribute("aria-label", "Status: " + title); }
  function poll() {
    if (document.hidden) { setTimeout(poll, POLL_MS); return; }
    fetch("/status/summary", { cache: "no-store" }).then(function (r) {
      if (!r.ok) throw new Error("status unavailable");
      return r.json();
    }).then(function (data) {
      if (data && data.state === "ok") show("ok", "all systems healthy");
      else if (data && data.state === "slow") show("slow", "scanning is slow - see the status page");
      else show("error", "an issue was detected - see the status page");
    }).catch(function () {
      show("unknown", "could not check status");
    }).finally(function () {
      setTimeout(poll, POLL_MS);
    });
  }
  setTimeout(poll, dot.classList.contains("status-dot-unknown") ? 0 : POLL_MS);
})();"#;

/// The status indicator: a dot linking to `/status`, rendered with the
/// engine's last known health so it is right without JavaScript (every
/// page load renders it afresh). Its script only adds live updates by
/// polling. It is only the glowing dot - the title and aria-label carry the
/// words. `class` is the link's own class, for where it sits (nav bar, POS
/// top bar).
pub fn status_indicator(health: Option<Health>, class: &str) -> Markup {
    let (state, title) = match health {
        Some(Health::Ok) => ("ok", "all systems healthy"),
        Some(Health::Slow) => ("slow", "scanning is slow - see the status page"),
        Some(Health::Problem) => ("error", "an issue was detected - see the status page"),
        None => ("unknown", "status not checked yet"),
    };
    html! {
        a href="/status" class=(class) id="status-indicator" title=(title) aria-label=(format!("Status: {title}")) {
            span class=(format!("status-dot status-dot-{state}")) {}
        }
        script { (PreEscaped(STATUS_INDICATOR_SCRIPT)) }
    }
}

/// The site nav: the brand (linking to `/`, the dashboard), the admin
/// links, the status dot, then the account menu once signed in (the theme
/// switch is in it: an anonymous visitor has no account to keep a choice
/// against, and gets `prefers-color-scheme`), or Log in and Sign up. The
/// row is one height either way (`site.css`, `.site-nav-row`).
///
/// The status dot sits in the bar itself, not in the link list, so on a
/// phone it stays in sight beside the hamburger; on a wider screen CSS
/// orders it between the links and the account menu.
fn nav(chrome: &PageChrome) -> Markup {
    html! {
        nav class="site-nav" {
            div class="wrap site-nav-row" {
                a href="/" class="site-nav-brand" {
                    (logo_mark(24, "site-nav-logo"))
                    "Monokulo"
                }
                (status_indicator(chrome.health, "nav-status-link"))
                input type="checkbox" id="nav-toggle" class="nav-toggle-checkbox";
                label for="nav-toggle" class="nav-toggle-label" aria-label="Menu" { "☰" }
                div class="site-nav-links" {
                    @if chrome.is_admin {
                        a href="/dashboard/admin/settings" { "Admin" }
                        a href="/dashboard/admin/invites" { "Invites" }
                        a href="/dashboard/admin/logs" { "Logs" }
                    }
                    @if !chrome.logged_in {
                        a href="/dashboard/login" { "Log in" }
                        a href="/dashboard/signup" { "Sign up" }
                    }
                    @if chrome.logged_in {
                        (account_menu(chrome))
                    }
                }
            }
        }
    }
}

/// A generic person, for the account button: its lines are `currentColor`.
fn account_icon() -> Markup {
    html! {
        svg class="acct-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
            circle cx="12" cy="8" r="4" {}
            path d="M4 21a8 8 0 0 1 16 0" {}
        }
    }
}

/// Closes an open account menu on a click outside it, or on Escape. Only
/// an enhancement: without it the menu closes from its own button, as any
/// `<details>` does.
const ACCOUNT_MENU_SCRIPT: &str = r#"(function () {
  function each(fn) { Array.prototype.forEach.call(document.querySelectorAll("details.acct[open]"), fn); }
  document.addEventListener("click", function (event) {
    each(function (menu) { if (!menu.contains(event.target)) menu.open = false; });
  });
  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") each(function (menu) { menu.open = false; menu.querySelector("summary").focus(); });
  });
})();"#;

/// The account menu: a person icon button (named "Account menu", its title
/// the user's email) opening a `<details>` (no JavaScript needed) with who
/// they are, Account, their wallets, the theme switch, the zone times are
/// shown in, and Log out. On a phone its items sit in the hamburger list
/// instead (`site.css`).
fn account_menu(chrome: &PageChrome) -> Markup {
    let role = if chrome.is_admin { "Admin" } else { "Merchant" };
    let zone = chrome.clock.name();
    let zone_title = if chrome.clock.is_automatic() {
        format!("Every time on the site is shown in {zone}, this browser's zone.")
    } else {
        format!("Every time on the site is shown in {zone}.")
    };
    html! {
        details class="acct" {
            summary class="acct-button" aria-label="Account menu" title=(chrome.email) {
                (account_icon())
            }
            div class="acct-menu" {
                div class="acct-who" { strong { (chrome.email) } small { (role) } }
                a href="/account" { "Account" }
                a href="/account?tab=wallets" { "Wallets" span class="menu-note" { (chrome.wallet_count) } }
                hr;
                div class="menu-item" { span { "Theme" } (theme_toggle(chrome)) }
                a href="/account#card-time" title=(zone_title) {
                    span { "Times in " (zone) } span class="menu-note" { "change" }
                }
                hr;
                form method="post" action="/dashboard/logout" class="nav-logout-form" {
                    button type="submit" class="nav-link-button" { "Log out" }
                }
            }
        }
        script { (PreEscaped(ACCOUNT_MENU_SCRIPT)) }
    }
}

/// Three submit buttons select Light, System, or Dark directly without JS.
/// CSS previews each option on hover/focus and animates the indicator across
/// the form navigation in browsers that support view transitions.
pub fn theme_toggle(chrome: &PageChrome) -> Markup {
    html! {
        form method="post" action="/dashboard/theme" class="nav-theme-form" {
            input type="hidden" name="next" value=(chrome.current_path);
            div class=(format!("theme-toggle theme-toggle-{}", chrome.theme.as_str())) role="group" aria-label="Theme" {
                span class="theme-toggle-track" {
                    button type="submit" name="theme" value="system" class="theme-toggle-option theme-toggle-option-system" aria-label="System theme" aria-pressed=(chrome.theme == Theme::System) title="System theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            rect x="3" y="3" width="18" height="13" rx="2" {}
                            line x1="12" y1="16" x2="12" y2="21" {}
                            line x1="8" y1="21" x2="16" y2="21" {}
                        }
                    }
                    button type="submit" name="theme" value="light" class="theme-toggle-option theme-toggle-option-light" aria-label="Light theme" aria-pressed=(chrome.theme == Theme::Light) title="Light theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            circle cx="12" cy="12" r="4" {}
                            line x1="12" y1="2" x2="12" y2="4" {}
                            line x1="12" y1="20" x2="12" y2="22" {}
                            line x1="4.2" y1="4.2" x2="5.6" y2="5.6" {}
                            line x1="18.4" y1="18.4" x2="19.8" y2="19.8" {}
                            line x1="2" y1="12" x2="4" y2="12" {}
                            line x1="20" y1="12" x2="22" y2="12" {}
                            line x1="4.2" y1="19.8" x2="5.6" y2="18.4" {}
                            line x1="18.4" y1="5.6" x2="19.8" y2="4.2" {}
                        }
                    }
                    button type="submit" name="theme" value="dark" class="theme-toggle-option theme-toggle-option-dark" aria-label="Dark theme" aria-pressed=(chrome.theme == Theme::Dark) title="Dark theme" {
                        svg viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true" focusable="false" {
                            path d="M21 12.5A9 9 0 1 1 11.5 3 7 7 0 0 0 21 12.5Z" {}
                        }
                    }
                    span class="theme-toggle-thumb" {}
                }
            }
        }
    }
}

#[cfg(test)]
mod theme_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_id_drops_its_prefix_and_keeps_its_last_six_characters_whole() {
        let html = order_id_short("order_a8723b2e45b0d44e").into_string();
        assert_eq!(
            html,
            r#"<span class="mid-ellipsis" title="order_a8723b2e45b0d44e"><span class="mid-head">a8723b2e45</span><span class="mid-tail">b0d44e</span></span>"#
        );
        assert!(order_id_short("abc")
            .into_string()
            .contains(r#"<span class="mid-head"></span><span class="mid-tail">abc</span>"#));
    }

    #[test]
    fn an_address_keeps_its_last_seven_characters_whole_and_all_of_it_in_the_page() {
        let address = "4AdUndXHHZ6cfufTMvppY6JwXNouMBzSkbLYfpAV5Usx3skxNgYeYTRj5UzqtReoS44qo9mtmXCqY45DJ852K5Jv2684Rge";
        let html = address_short(address).into_string();
        assert_eq!(
            html,
            format!(
                r#"<span class="mid-ellipsis" title="{address}"><span class="mid-head">{}</span><span class="mid-tail">2684Rge</span></span>"#,
                &address[..address.len() - 7]
            )
        );
    }

    #[test]
    fn a_network_badge_always_says_the_network_and_marks_mainnet_apart() {
        let main = network_badge("mainnet").into_string();
        assert!(
            main.starts_with(r#"<span class="tag-network is-main"><svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">"#),
            "{main}"
        );
        assert!(main.ends_with("</svg>Mainnet</span>"), "{main}");
        for (network, word) in [("stagenet", "Stagenet"), ("testnet", "Testnet")] {
            let test = network_badge(network).into_string();
            assert!(
                test.starts_with(r#"<span class="tag-network is-test">"#),
                "{test}"
            );
            assert!(test.ends_with(&format!("</svg>{word}</span>")), "{test}");
        }
        assert!(test_networks_badge()
            .into_string()
            .ends_with(r#"</svg>Test networks</span>"#));
    }

    #[test]
    fn status_indicator_is_rendered_with_the_known_health_and_polls_only_as_an_enhancement() {
        let healthy = status_indicator(Some(Health::Ok), "nav-status-link").into_string();
        assert!(healthy.contains(r#"<a href="/status" class="nav-status-link" id="status-indicator" title="all systems healthy" aria-label="Status: all systems healthy"><span class="status-dot status-dot-ok"></span></a>"#), "got: {healthy}");
        assert!(healthy.contains("/status/summary"));

        let problem = status_indicator(Some(Health::Problem), "pos-status-link").into_string();
        let slow = status_indicator(Some(Health::Slow), "nav-status-link").into_string();
        assert!(
            slow.contains(r#"class="status-dot status-dot-slow""#),
            "{slow}"
        );
        assert!(slow.contains("scanning is slow"), "{slow}");
        assert!(
            problem.contains(r#"class="status-dot status-dot-error""#),
            "got: {problem}"
        );

        let unknown = status_indicator(None, "nav-status-link").into_string();
        assert!(
            unknown.contains(r#"class="status-dot status-dot-unknown""#),
            "got: {unknown}"
        );

        let chrome = PageChrome::from_user(None, "/").with_health(Some(Health::Ok));
        assert!(nav(&chrome)
            .into_string()
            .contains("status-dot status-dot-ok"));
    }

    /// A signed-in user's chrome, as `page_chrome` makes it.
    fn signed_in(email: &str, is_admin: bool, theme: Theme) -> PageChrome {
        PageChrome {
            logged_in: true,
            is_admin,
            email: email.to_string(),
            wallet_count: 3,
            theme,
            current_path: "/".to_string(),
            health: None,
            alerts: Vec::new(),
            browser_reports: true,
            clock: time::Clock::new(Some("Australia/Perth"), None, 0),
        }
    }

    #[test]
    fn an_admin_sees_the_status_dot_in_the_bar_then_admin_links_then_their_account_menu() {
        let html = nav(&signed_in("rachel.dz@example.org", true, Theme::Dark)).into_string();
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {html}"))
        };
        // The dot is in the bar itself, before the hamburger and the link
        // list, so a phone shows it beside the hamburger; site.css orders
        // it after the links on a wider screen.
        let status = at(r#"<a href="/status" class="nav-status-link""#);
        let hamburger = at(r#"<label for="nav-toggle""#);
        let links = at(r#"<div class="site-nav-links">"#);
        let admin = at(r#"href="/dashboard/admin/settings">Admin<"#);
        let invites = at(r#"href="/dashboard/admin/invites">Invites<"#);
        let logs = at(r#"href="/dashboard/admin/logs">Logs<"#);
        let menu = at(r#"<details class="acct">"#);
        assert!(
            status < hamburger
                && hamburger < links
                && links < admin
                && admin < invites
                && invites < logs
                && logs < menu,
            "{html}"
        );
        // The brand is the way to the dashboard: no separate link, and no
        // top-level wallets link (the account menu has it).
        assert!(
            html.contains(r#"<a href="/" class="site-nav-brand">"#),
            "{html}"
        );
        assert!(!html.contains(r#"href="/dashboard""#), "{html}");
        assert!(!html.contains(">wallets<"), "{html}");
    }

    #[test]
    fn the_account_menu_holds_who_account_wallets_theme_times_and_log_out_in_that_order() {
        let html = nav(&signed_in("rachel.dz@example.org", true, Theme::Dark)).into_string();
        let menu = &html[html.find(r#"<div class="acct-menu">"#).expect(&html)..];
        // The button is a person icon only: named for what it opens, the
        // email in its title.
        let button = &html[html.find("<summary").expect(&html)..html.find("</summary>").unwrap()];
        assert!(
            button.starts_with(r#"<summary class="acct-button" aria-label="Account menu" title="rachel.dz@example.org"><svg class="acct-icon""#),
            "{button}"
        );
        assert!(!button.contains("rachel.dz<"), "no name shown: {button}");
        let at = |needle: &str| {
            menu.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {menu}"))
        };
        let who = at("<strong>rachel.dz@example.org</strong><small>Admin</small>");
        let account = at(r#"<a href="/account">Account</a>"#);
        let wallets =
            at(r#"<a href="/account?tab=wallets">Wallets<span class="menu-note">3</span></a>"#);
        let theme = at(r#"<form method="post" action="/dashboard/theme" class="nav-theme-form">"#);
        let times = at(r##"<a href="/account#card-time""##);
        let logout = at(r#"action="/dashboard/logout""#);
        assert!(
            who < account
                && account < wallets
                && wallets < theme
                && theme < times
                && times < logout,
            "{menu}"
        );
        assert!(
            menu.contains(
                r#"Times in Australia/Perth</span><span class="menu-note">change</span>"#
            ),
            "{menu}"
        );
        assert!(menu.contains(">Log out</button>"), "{menu}");

        let merchant = nav(&signed_in("ann@example.org", false, Theme::System)).into_string();
        assert!(merchant.contains("<small>Merchant</small>"), "{merchant}");
        assert!(!merchant.contains("/dashboard/admin"), "{merchant}");
    }

    #[test]
    fn signed_out_the_nav_offers_log_in_and_sign_up_and_no_account_controls() {
        let chrome = PageChrome::from_user(None, "/dashboard/login");
        let html = nav(&chrome).into_string();
        assert!(
            html.contains(r#"<a href="/dashboard/login">Log in</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/dashboard/signup">Sign up</a>"#),
            "{html}"
        );
        assert!(!html.contains("admin"));
        assert!(!html.contains(r#"action="/dashboard/logout""#));
        assert!(!html.contains("theme-toggle"));
        assert!(!html.contains("acct"));
    }

    /// #1: the bar is one height whatever it holds, so a signed-out page's
    /// is as tall as a signed-in one's (its tallest control, 2em, plus the
    /// row's padding). #9: the status dot's link is a 2em box whose
    /// `::before` makes a 44px target. The account button is 2em too.
    #[test]
    fn the_nav_row_keeps_one_height_and_the_status_dot_is_a_44px_target() {
        let css = include_str!("site.css");
        let rule = |selector: &str| {
            let at = &css[css.find(selector).unwrap_or_else(|| panic!("{selector}"))..];
            at[..at.find('}').unwrap()].to_string()
        };
        assert!(rule(".site-nav-row {").contains("min-height: calc(2em + 1.4rem);"));
        assert!(rule(".nav-status-link {").contains("width: 2em; height: 2em;"));
        assert!(rule(".acct > summary {").contains("width: 2em; height: 2em;"));
        assert!(css.contains(
            r#".nav-status-link::before { content: ""; position: absolute; inset: min(0px, calc((2em - 44px) / 2)); }"#
        ));
    }

    #[test]
    fn theme_toggle_renders_a_slider_with_a_thumb_positioned_for_the_current_theme() {
        for (theme, class) in [
            (Theme::Light, "theme-toggle-light"),
            (Theme::System, "theme-toggle-system"),
            (Theme::Dark, "theme-toggle-dark"),
        ] {
            let chrome = PageChrome {
                current_path: "/dashboard/stores".to_string(),
                ..signed_in("ann@example.org", false, theme)
            };
            let html = nav(&chrome).into_string();
            assert!(
                html.contains(&class.to_string()),
                "expected {class} on the toggle for {theme:?}, got: {html}"
            );
            assert!(
                html.contains("theme-toggle-option-light")
                    && html.contains("theme-toggle-option-dark"),
                "expected both sun and moon options, got: {html}"
            );
            assert!(
                html.contains("theme-toggle-thumb"),
                "expected a thumb element, got: {html}"
            );
            // Each option submits its theme through the same no-JS form.
            assert!(html.contains(r#"<form method="post" action="/dashboard/theme""#));
            assert!(html.contains(r#"<input type="hidden" name="next" value="/dashboard/stores">"#));
            for value in ["light", "system", "dark"] {
                assert!(html.contains(&format!(r#"name="theme" value="{value}""#)));
            }
            assert_eq!(
                html.matches("aria-pressed=\"true\"").count(),
                1,
                "only the current theme should be pressed"
            );
        }
    }

    #[test]
    fn theme_hover_moves_the_thumb_and_recolors_only_the_hovered_icon() {
        let css = include_str!("site.css");
        for option in ["light", "system", "dark"] {
            assert!(css.contains(&format!(
                ".theme-toggle-option-{option}:is(:hover, :focus-visible) ~ .theme-toggle-thumb"
            )));
        }
        assert!(css.contains(".theme-toggle:has(.theme-toggle-option:is(:hover, :focus-visible)) .theme-toggle-option { color: var(--muted); }"));
        assert!(css.contains(".theme-toggle:has(.theme-toggle-option:is(:hover, :focus-visible)) .theme-toggle-option:is(:hover, :focus-visible) { color: var(--accent-ink); }"));
    }

    #[test]
    fn logs_links_are_for_admins_and_quote_the_value() {
        let admin = signed_in("admin@example.org", true, Theme::System);
        let html = logs_link(&admin, "order.id", "o'1", "Logs").into_string();
        assert_eq!(
            html,
            r#"<a class="logs-link" href="/dashboard/admin/logs?q=order.id+%3D+%27o%5C%271%27&amp;range=all">Logs</a>"#
        );
        let query = "order.id = 'o\\'1'";
        assert_eq!(
            telemetry::query::parse(query).unwrap().unwrap().to_string(),
            query,
            "the link's query parses back"
        );
        let merchant = PageChrome {
            is_admin: false,
            ..admin
        };
        assert_eq!(
            logs_link(&merchant, "order.id", "o1", "Logs").into_string(),
            ""
        );
    }
}
