//! Composable, compile-time-checked HTML rendering (Maud).
//!
//! [`Page`] and [`nav`] own the page shell: every page in this module
//! builds its header and content and hands
//! them to [`Page`], which owns `<!doctype html>` through `</html>`, the shared
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

/// The project's site (docs.monokulo.com, GitHub Pages): the docs, built
/// and published with it (`cargo xtask pages docs`), each page a folder at
/// its top level.
pub const SITE_URL: &str = "https://docs.monokulo.com/";

/// The docs' front page: the guide for adding Monokulo to a site.
pub const DOCS_URL: &str = "https://docs.monokulo.com/guide/";

/// A docs page, by its folder under [`SITE_URL`] (`js-library/`).
pub fn docs_url(page: &str) -> String {
    format!("{SITE_URL}{page}")
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
        // With the chunks it imports, asked for at once rather than once
        // it has run.
        Load::Module => html! {
            script type="module" src=(src) {}
            @for chunk in assets::preloads(name) {
                link rel="modulepreload" href=(chunk);
            }
        },
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
    static_assets::font_faces(assets::url)
}

pub mod account;
pub mod admin;
pub mod auth;
pub mod challenge;
pub mod checkout;
pub mod connect;
pub mod connected_apps;
pub mod controls;
pub mod create_order;
pub mod domains;
pub mod engine;
pub mod home;

pub mod key_entry;
mod logo_art;
pub mod logs;
pub mod orders;
pub mod pos;
pub mod query;
pub mod scaling;
pub mod settings;
pub mod setup;
mod shorten;
pub mod status;
pub mod store_detail;
pub mod store_settings;
pub mod store_site;
pub mod time;
pub mod wallets;
pub mod webhooks;

pub use shorten::{short_address, short_address_text, short_id, short_id_text};

/// This site's own components and pages (`site.css`), painted with the
/// `ui` package's colour roles (`theme.css`, the only place a colour is
/// defined: `theme_tests`). The shared components' styles are the `ui`
/// package's (`ui.css`), linked after it. Baked with its compressed copies
/// (`crate::assets`).
pub(crate) const SITE: static_assets::Baked = static_assets::bake!("src/views/site.css");

/// Where a page sits (`ui::PageHeader`'s crumbs): links to the pages
/// above it, nearest last; one link is a back link.
pub fn crumbs(links: &[(&str, &str)]) -> Vec<ui::Crumb> {
    links
        .iter()
        .map(|(href, label)| ui::Crumb::new(crate::base::url(href), *label))
        .collect()
}

/// The crumbs of a store's pages, and of pages nested under its Orders.
pub fn store_crumbs(
    connection_id: &str,
    display_name: &str,
    include_orders: bool,
) -> Vec<ui::Crumb> {
    let store = format!("/stores/{connection_id}");
    let orders = format!("{store}/orders");
    if include_orders {
        crumbs(&[(&store, display_name), (&orders, "Orders")])
    } else {
        crumbs(&[(&store, display_name)])
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
    /// default. Checked into a `SafePath` (`crate::http::auth`) before
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
    /// The nav links the Query page: an admin's, while it is switched on
    /// (`crate::http::page_chrome`).
    pub query_enabled: bool,
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
                query_enabled: false,
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
                query_enabled: false,
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

const DEFAULT_VIEWPORT: &str = "width=device-width, initial-scale=1";

/// A page under the site's bar: its `<title>`, its header and what follows
/// it, inside `main.wrap`. The header is required, so every page has a
/// title (`ui::PageHeader`).
pub struct Page {
    title: String,
    header: ui::PageHeader,
    class: Option<String>,
    head: Option<Markup>,
    wide: bool,
}

impl Page {
    /// `title` is the full `<title>` text (not auto-suffixed: each caller
    /// states its own title exactly).
    pub fn new(title: impl Into<String>, header: ui::PageHeader) -> Self {
        Page {
            title: title.into(),
            header,
            class: None,
            head: None,
            wide: false,
        }
    }

    /// Classes beside `wrap` on the page's `main` (`settings-page`).
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = Some(class.into());
        self
    }

    /// A page of data (the Logs and Query pages): the page and the site
    /// bar's row 1200px wide, not the reading width.
    pub fn wide(mut self) -> Self {
        self.wide = true;
        self
    }

    /// Extra `<head>` content (a page's own stylesheet or script), after
    /// the shared head.
    pub fn head(mut self, head: Markup) -> Self {
        self.head = Some(head);
        self
    }

    pub fn render(self, chrome: &PageChrome, content: Markup) -> Markup {
        let mut class = match &self.class {
            Some(class) => format!("wrap {class}"),
            None => "wrap".to_string(),
        };
        if self.wide {
            class.push_str(" wrap-wide");
        }
        let body = html! {
            main class=(class) {
                (self.header)
                (content)
            }
        };
        page_shell(
            chrome,
            &self.title,
            DEFAULT_VIEWPORT,
            self.head,
            Some(nav(
                chrome,
                if self.wide {
                    ui::PageWidth::Full
                } else {
                    ui::PageWidth::Normal
                },
            )),
            body,
        )
    }
}

/// An order without the shop's reference: a dash in a table, and on a
/// phone's card, where a lone dash says nothing, "No reference".
pub fn no_reference() -> Markup {
    html! {
        span class="muted" {
            span class="no-value-table" { "—" }
            em class="no-value-card" { "No reference" }
        }
    }
}

/// What a dialog holds: shown in its dialog, under an `h2` with a ✕, or
/// without JavaScript as a page of its own, whose header carries the title.
pub struct DialogContent {
    pub title_id: String,
    pub title: Markup,
    /// Beside the title: a status.
    pub aside: Option<Markup>,
    pub body: Markup,
}

impl DialogContent {
    /// In its dialog.
    pub fn in_dialog(self) -> Markup {
        html! {
            (ui::DialogTitle::new(self.title_id, self.title).maybe_aside(self.aside.map(ui::Slot::from)))
            (self.body)
        }
    }

    /// As a page: the page's header, and its content.
    pub fn page(self, crumbs: Vec<ui::Crumb>, size: ui::DialogSize) -> (ui::PageHeader, Markup) {
        let header = ui::PageHeader::new(self.title)
            .title_id(self.title_id)
            .maybe_aside(self.aside.map(ui::Slot::from))
            .crumbs(crumbs);
        (header, ui::DialogPage::new(self.body).size(size).render())
    }
}

/// Same page shell, but with no nav bar - for a screen deliberately built
/// to have no site chrome at all (the POS terminal - see `views::pos`'s own
/// doc comment). Still carries the shared `<head>` and the per-user
/// `data-theme`, since a merchant who chose dark mode should get it here
/// too, even without a nav to toggle it from.
pub fn layout_bare(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(chrome, title, DEFAULT_VIEWPORT, None, None, body)
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

/// The chunks the islands in `parts` need, each once: their components'
/// behaviours and what those import (`assets::preloads`).
fn island_preloads<'a>(parts: impl Iterator<Item = &'a Markup>) -> Vec<String> {
    let mut chunks = Vec::new();
    for part in parts {
        for rest in part.0.split("data-island=\"").skip(1) {
            let Some((name, _)) = rest.split_once('"') else {
                continue;
            };
            for chunk in assets::preloads(name) {
                if !chunks.contains(&chunk) {
                    chunks.push(chunk);
                }
            }
        }
    }
    chunks
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
                // Where this Monokulo's paths start (`crate::base`), for
                // the few URLs a script makes itself: `/monokulo` behind a
                // proxy serving it there, empty at the root and over Tor.
                meta name="monokulo-base" content=(crate::base::path());
                link rel="icon" type="image/svg+xml" href=(assets::url("favicon.svg"));
                // The typeface, then the colours, then the components:
                // the stylesheets at their versioned URLs (crate::assets),
                // kept by the browser for a year once seen.
                style { (PreEscaped(font_faces())) }
                (stylesheet("theme.css"))
                (stylesheet("site.css"))
                (stylesheet("ui.css"))
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
                    // The shared components' islands: every Select, settings
                    // form, copy value and tabs (`ui`).
                    (script("ui-islands.js", Load::Module))
                    // And the behaviours of the islands on this page, so
                    // each attaches as soon as the script runs.
                    @for chunk in island_preloads(nav.iter().chain([&body])) {
                        link rel="modulepreload" href=(chunk);
                    }
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
                                (ui::Alert::new(alert.as_str(), ui::Tone::Error))
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

/// An order status's words and its `.state-*` colour class, and whether the
/// order can still change. The checkout, the POS and the dashboard show a
/// status with the same words and colours.
pub fn order_state(status: OrderStatus) -> (&'static str, ui::OrderState, bool) {
    use OrderStatus::*;
    match status {
        Pending => ("Waiting for payment", ui::OrderState::Pending, false),
        Unconfirmed => (
            "Payment seen, unconfirmed",
            ui::OrderState::Unconfirmed,
            false,
        ),
        Confirming => ("Confirming", ui::OrderState::Confirming, false),
        Partial => ("Partial payment received", ui::OrderState::Partial, false),
        Paid => ("Paid", ui::OrderState::Paid, true),
        Overpaid => ("Overpaid", ui::OrderState::Overpaid, true),
        Expired => ("Expired", ui::OrderState::Expired, true),
        OrderStatus::Cancelled => ("Cancelled", ui::OrderState::Cancelled, true),
    }
}

/// A status's words short enough for a phone's table column ("Waiting"
/// for "Waiting for payment"). The long words stay for screen readers.
pub fn order_state_short(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "Waiting",
        OrderStatus::Unconfirmed => "Seen",
        OrderStatus::Partial => "Part paid",
        other => order_state(other).0,
    }
}

/// An order's status as a badge ([`order_state`]): the full words, and the
/// short ones that replace them on a narrow screen (`.label-short`).
pub fn state_badge(status: OrderStatus) -> Markup {
    let (label, state, _) = order_state(status);
    let short = order_state_short(status);
    let tag = ui::Tag::new(label).state(state);
    html! { (if short == label { tag } else { tag.short(short) }) }
}

/// A Monero network as a badge (`.tag-network`), the same wherever a
/// network is shown: the word always, with an icon and an edge to back it
/// up. Mainnet (real money) has the brand's orange tint and a solid edge;
/// a test network a neutral tint and a dashed edge.
pub fn network_badge(network: &str) -> Markup {
    html! { (network_tag(network)) }
}

/// [`network_badge`] as its icon only, its name for screen readers and on
/// hover: in a table, beside a wallet's name.
pub fn network_icon(network: &str) -> Markup {
    html! { (network_tag(network).icon_only(true)) }
}

/// The badge for the test networks together: the wallets list's fold.
pub fn test_networks_badge() -> Markup {
    html! { (ui::NetworkBadge::new("Test networks")) }
}

fn network_tag(network: &str) -> ui::NetworkBadge {
    if network == "mainnet" {
        ui::NetworkBadge::new("Mainnet").main(true)
    } else {
        ui::NetworkBadge::new(scaling::network_name(network))
    }
}

/// An amount for people to read: XMR without its trailing zeros
/// (`0.420000000000` is `0.42`), anything else with the currency's usual
/// decimal places ([`fiat_amount`]).
pub fn display_amount(amount: &str, currency: &str) -> String {
    if currency.is_empty() {
        amount.to_string()
    } else if currency == "XMR" {
        format!("{} {currency}", trim_xmr(amount))
    } else {
        format!("{} {currency}", fiat_amount(amount, currency))
    }
}

/// A fiat amount as the order was priced, written with the currency's
/// usual minor units (`crate::currencies::minor_units`): trailing zeros
/// past them dropped, and missing ones added, so `12.50000000` USD and
/// `12.5` USD both read `12.50` and `1200.00` JPY reads `1200`. A
/// significant digit is never dropped (`12.505` USD stays). Anything that
/// isn't a plain decimal is shown as given.
pub fn fiat_amount(amount: &str, currency: &str) -> String {
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) {
        return amount.to_string();
    }
    let minor = usize::from(crate::currencies::minor_units(currency));
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() && minor == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:0<minor$}")
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
        "/admin/logs?q={}&range=all",
        url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
    );
    html! {
        @if chrome.is_admin {
            a class="logs-link" href=(crate::base::url(href)) { (text) }
        }
    }
}

/// The Reload button a point-in-time page shows instead of refreshing by
/// itself (structured_logging.md D4): without JavaScript these pages are a
/// snapshot, and a reader reloads when they want the latest. `class` is
/// `reload` so the page's own script can hide it once it streams updates.
pub fn reload_button(href: &str) -> Markup {
    let icon = html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
            path d="M20 7v5h-5M4 17v-5h5M6.1 7a7 7 0 0 1 11.6-2L20 8M4 16l2.3 3A7 7 0 0 0 17.9 17" {}
        }
    };
    html! { (ui::IconLink::new("Refresh page", crate::base::url(href), icon).attr("data-reload", "")) }
}

/// The engine's health as the status indicator shows it: green, yellow or
/// red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Everything answers and scans.
    Ok,
    /// Everything works, but one block has taken over two minutes to scan.
    Slow,
    /// The engine turned the status request away for now (its rate
    /// limit): it is there, just asked too often.
    Busy,
    /// A node or the scan is failing, or the engine can't be reached.
    Problem,
}

impl Health {
    /// The name the summary endpoint and the dot's class use.
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Slow => "slow",
            Health::Busy => "busy",
            Health::Problem => "error",
        }
    }
}

/// The status indicator: `ui::StatusDot`, a dot linking to `/status`,
/// rendered with the engine's last known health so it is right without
/// JavaScript (every page load renders it afresh); its island polls for
/// live updates. `place`: the nav bar or the POS top bar.
pub fn status_indicator(health: Option<Health>) -> Markup {
    let health = match health {
        Some(Health::Ok) => ui::HealthState::Ok,
        Some(Health::Slow) => ui::HealthState::Slow,
        Some(Health::Busy) => ui::HealthState::Busy,
        Some(Health::Problem) => ui::HealthState::Error,
        None => ui::HealthState::Unknown,
    };
    ui::StatusDot::new(health)
        .href(crate::base::url("/status"))
        .poll(crate::base::url("/status/summary"))
        .render()
}

/// The site nav (`ui::SiteNav`): the brand and, once signed in, Home
/// (both `/`, the stores' overview), the admin links, the status dot, then
/// the account menu once
/// signed in (the theme switch is in it: an anonymous visitor has no
/// account to keep a choice against, and gets `prefers-color-scheme`), or
/// Log in and Sign up. The row is one height either way.
///
/// The status dot sits in the bar itself, not in the link list, so on a
/// phone it stays in sight beside the hamburger; on a wider screen CSS
/// orders it between the links and the account menu.
fn nav(chrome: &PageChrome, width: ui::PageWidth) -> Markup {
    let links = html! {
        @if chrome.logged_in {
            a href=(crate::base::url("/")) aria-current=[(chrome.current_path == "/").then_some("page")] { "Home" }
        }
        @if chrome.is_admin {
            a href=(crate::base::url("/admin/settings")) { "Admin" }
            a href=(crate::base::url("/admin/invites")) { "Invites" }
            a href=(crate::base::url("/admin/logs")) { "Logs" }
            @if chrome.query_enabled {
                a href=(crate::base::url(query::PAGE)) { "Query" }
            }
        }
        @if !chrome.logged_in {
            a href=(crate::base::url("/login")) { "Log in" }
            a href=(crate::base::url("/signup")) { "Sign up" }
        }
        @if chrome.logged_in {
            (account_menu(chrome))
        }
    };
    let brand = html! { (logo_mark(24, "site-nav-logo")) "Monokulo" };
    ui::SiteNav::new(brand, links)
        .home(crate::base::url("/"))
        .status(status_indicator(chrome.health))
        .width(width)
        .render()
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

/// The account menu (`ui::Menu`): a person icon button (named "Account
/// menu", its title the user's email) opening a `<details>` (no JavaScript
/// needed) with who they are, Account, their wallets, the theme switch, the
/// zone times are shown in, and Log out. On a phone its items sit in the
/// hamburger list instead.
pub fn account_menu(chrome: &PageChrome) -> Markup {
    let role = if chrome.is_admin { "Admin" } else { "Merchant" };
    let zone = chrome.clock.name();
    let zone_title = if chrome.clock.is_automatic() {
        format!("Every time on the site is shown in {zone}, this browser's zone.")
    } else {
        format!("Every time on the site is shown in {zone}.")
    };
    let items = html! {
        div class="acct-who" { strong { (chrome.email) } small { (role) } }
        (ui::MenuItem::new("Account").href(crate::base::url("/account")))
        (ui::MenuItem::new("Wallets").href(crate::base::url("/account?tab=wallets")).note(chrome.wallet_count.to_string()))
        hr;
        (ui::MenuItem::new("Theme").note(theme_toggle(chrome)))
        (ui::MenuItem::new(html! { "Times in " (zone) }).href(crate::base::url("/account#card-time")).note("change").title(zone_title))
        hr;
        (ui::MenuItem::new("Log out").post_to(crate::base::url("/logout")))
    };
    ui::Menu::new("Account menu", account_icon(), items)
        .title(chrome.email.clone())
        .render()
}

/// The theme switch (`ui::ThemeToggle`): Light, System and Dark, each a
/// submit button, so it works without JavaScript.
pub fn theme_toggle(chrome: &PageChrome) -> Markup {
    let theme = match chrome.theme {
        Theme::System => ui::ThemeChoice::System,
        Theme::Light => ui::ThemeChoice::Light,
        Theme::Dark => ui::ThemeChoice::Dark,
    };
    ui::ThemeToggle::new(
        theme,
        crate::base::url("/theme"),
        chrome.current_path.clone(),
    )
    .render()
}

#[cfg(test)]
mod theme_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fiat_amount_reads_with_its_currencys_usual_decimals() {
        // What the OpenCart plugin sends.
        assert_eq!(display_amount("12.50000000", "USD"), "12.50 USD");
        assert_eq!(display_amount("12.5", "EUR"), "12.50 EUR");
        assert_eq!(display_amount("12", "EUR"), "12.00 EUR");
        assert_eq!(display_amount("1200.00", "JPY"), "1200 JPY");
        assert_eq!(display_amount("1.5", "KWD"), "1.500 KWD");
        // A significant digit past the usual places stays.
        assert_eq!(display_amount("12.505000", "USD"), "12.505 USD");
        assert_eq!(display_amount("0.00012300", "BTC"), "0.000123 BTC");
        assert_eq!(display_amount("0.420000000000", "XMR"), "0.42 XMR");
        assert_eq!(display_amount("—", ""), "—");
    }

    #[test]
    fn a_page_asks_for_its_islands_behaviours_with_the_script_each_once() {
        let nav = html! { div data-island="Menu" {} };
        let body = html! {
            div data-island="Select" {} div data-island="Select" {} p { "data-island" }
        };
        let chunks = island_preloads([&nav, &body].into_iter());
        let menu: Vec<String> = assets::preloads("Menu").collect();
        let select: Vec<String> = assets::preloads("Select").collect();
        assert!(chunks.starts_with(&menu), "{chunks:?}");
        for chunk in &select {
            assert_eq!(chunks.iter().filter(|c| *c == chunk).count(), 1, "{chunk}");
        }
        assert_eq!(
            island_preloads([&html! { p { "no islands" } }].into_iter()),
            Vec::<String>::new()
        );
        // A module's own imports come with its script.
        let engine = script("engine-view.js", Load::Module).into_string();
        for chunk in assets::preloads("engine-view.js") {
            assert!(
                engine.contains(&format!(r#"<link rel="modulepreload" href="{chunk}">"#)),
                "{engine}"
            );
        }
    }

    #[test]
    fn a_network_badge_always_says_the_network_and_marks_mainnet_apart() {
        let main = network_badge("mainnet").into_string();
        assert!(
            main.starts_with(
                r#"<span class="tag-network is-main"><svg viewBox="0 0 24 24" aria-hidden="true">"#
            ),
            "{main}"
        );
        assert!(
            main.ends_with(r#"</svg><span class="tag-network-name">Mainnet</span></span>"#),
            "{main}"
        );
        for (network, word) in [("stagenet", "Stagenet"), ("testnet", "Testnet")] {
            let test = network_badge(network).into_string();
            assert!(
                test.starts_with(r#"<span class="tag-network is-test">"#),
                "{test}"
            );
            assert!(
                test.ends_with(&format!(
                    r#"</svg><span class="tag-network-name">{word}</span></span>"#
                )),
                "{test}"
            );
        }
        assert!(test_networks_badge()
            .into_string()
            .ends_with(r#"</svg><span class="tag-network-name">Test networks</span></span>"#));
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
            query_enabled: is_admin,
        }
    }

    #[test]
    fn logs_links_are_for_admins_and_quote_the_value() {
        let admin = signed_in("admin@example.org", true, Theme::System);
        let html = logs_link(&admin, "order.id", "o'1", "Logs").into_string();
        assert_eq!(
            html,
            r#"<a class="logs-link" href="/admin/logs?q=order.id+%3D+%27o%5C%271%27&amp;range=all">Logs</a>"#
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

    /// A page refreshes itself only where it must without JavaScript: the
    /// checkout (its state moves on) and the connection check (waiting is
    /// the proof). No other view writes a meta refresh.
    #[test]
    fn only_the_checkout_and_the_connection_check_refresh_themselves() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/views");
        let mut dirs = vec![dir.clone()];
        let mut refreshing = Vec::new();
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                // The page itself, not the tests that quote it.
                let page = source.split("#[cfg(test)]").next().unwrap();
                if page.contains(r#"http-equiv="refresh""#) {
                    refreshing.push(path.file_name().unwrap().to_string_lossy().into_owned());
                }
            }
        }
        refreshing.sort();
        assert_eq!(refreshing, ["challenge.rs", "checkout.rs"]);
    }
}
