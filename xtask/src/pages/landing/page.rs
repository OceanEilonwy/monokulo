//! The landing page (GitHub Pages: /): about the product. The hero, with an
//! order's life beside it as the POS draws it; what Monokulo is good at;
//! the ways to take payments, each linking into the docs; the two calls a
//! checkout takes; and the way to the install guides, which are docs pages
//! (`install`). Rendered with maud, beside the site's stylesheet
//! (`xtask/src/pages/assets/styles.css`).
//!
//! The install guides were on this page, under `#install`, `#desktop` and
//! `#openwrt`. Links to those still land here: `#install` is the band that
//! links both guides, and its two buttons are `#desktop` and `#openwrt`, so
//! a reader without scripts lands on the link to follow. With a script,
//! `#desktop` and `#openwrt` go straight on to their guide, on arriving or
//! when the open page's address changes to one ([`OLD_ANCHORS`]).

use crate::pages::site_assets;
use crate::pages::site_header::{self, Section, GITHUB_MARK};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use ui::assets::icons;
use ui::{OrderState, ProgressStep, StatusIcon, StepProgress};

/// The install guides, from the site's root.
pub(super) const INSTALL_COMPUTER: &str = "install-computer/";
pub(super) const INSTALL_ROUTER: &str = "install-router/";

/// The anchors the install guides had on this page that now each have a
/// page of their own: (anchor, the guide's address).
pub(super) const OLD_ANCHORS: [(&str, &str); 2] =
    [("desktop", INSTALL_COMPUTER), ("openwrt", INSTALL_ROUTER)];

/// What Monokulo is good at: (heading, words).
const FEATURES: [(&str, &str); 6] = [
    (
        "Watch-only",
        "Each store gives Monokulo its view key, never its spend key. Payments go straight to the store’s own wallet; Monokulo just sees them arrive.",
    ),
    (
        "Checkout that works anywhere",
        "A hosted checkout page and embed for static sites, a point-of-sale screen, and webhooks when an order is paid. The checkout works without JavaScript.",
    ),
    (
        "Fast detection",
        "Payments show up within about a second of reaching the network, and are confirmed as blocks arrive.",
    ),
    (
        "Easy on the router",
        "One static program and SQLite. The engine runs inside Monokulo on threads of its own, kept to two of the Flint 2’s four cores at low priority, so routing comes first. Checking each block’s proof of work (on by default for mainnet) takes about 256 MB of memory; turn it off on the admin page if you run your own node.",
    ),
    (
        "Settings in your browser",
        "Manage stores, nodes and payment settings on Monokulo’s admin page. On OpenWrt, LuCI also controls start, stop, port, listen address and the engine’s CPUs under Services → Monokulo.",
    ),
    (
        "Survives upgrades",
        "Its settings, keys and databases are kept across OpenWrt firmware upgrades.",
    ),
];

/// The ways to take payments: (heading, words, the docs page, its name).
const WAYS: [(&str, &str, &str, &str); 4] = [
    (
        "Checkout on your site",
        "A script tag and two calls put the checkout in your page, even on a static site.",
        "js-library/",
        "JS library",
    ),
    (
        "Shop plugins",
        "Connect WooCommerce or another shop with a code from the dashboard.",
        "plugins/",
        "Shop plugins",
    ),
    (
        "Point of sale",
        "A till or app connects with its own key and takes payments in person.",
        "apps/",
        "Connected apps",
    ),
    (
        "Your own server",
        "Make and follow orders over HTTP, with every route documented beside a recorded example.",
        "api/",
        "API reference",
    ),
];

/// The two calls, as the JS library's page has them.
const SAMPLE: &str = r##"// on your checkout page, after monokulo-client.js
const order = await Monokulo.createOrder({
  publicKey: "pk_…",
  amount: "24.50",
  currency: "USD",
  merchantOrderId: "1042",
});

Monokulo.mount("#pay", order, {
  onPaid: () => showThanks(),
});"##;

/// An order's life, start to finish, each step in its state and drawn as
/// the POS draws it, with what happens there.
fn order_life() -> ui::ProgressSteps {
    let step = |label: &str, state, wedge: Option<u32>, detail: Markup| {
        ProgressStep::new(label, StepProgress::Done)
            .state(state)
            .icon(StatusIcon::new(state).maybe_progress(wedge))
            .detail(detail)
    };
    ui::ProgressSteps::new(
        "An order, start to finish",
        vec![
            step(
                "Awaiting payment",
                OrderState::Pending,
                None,
                html! { "Your page asks Monokulo for an order, and the customer scans its QR code in the checkout." },
            ),
            step(
                "Unconfirmed",
                OrderState::Unconfirmed,
                None,
                html! { "Seen in about a second, as the payment reaches the network." },
            ),
            step(
                "Confirming",
                OrderState::Confirming,
                Some(60),
                html! { "Confirmed as blocks arrive, straight to your own wallet." },
            ),
            step(
                "Paid",
                OrderState::Paid,
                None,
                html! { "Your server hears " code { "order.paid" } ", signed, so you ship on Monokulo’s word, not the browser’s." },
            ),
        ],
    )
    .vertical(true)
}

/// The page, its links to the source at `github_url`.
pub(super) fn render(github_url: &str) -> Markup {
    let old_anchors = OLD_ANCHORS
        .iter()
        .map(|(anchor, to)| format!("\"#{anchor}\":\"{to}\""))
        .collect::<Vec<_>>()
        .join(",");
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Monokulo: self-hosted Monero payments" }
                meta name="description" content="Monokulo is a self-hosted Monero payment gateway for Linux, macOS, Windows and OpenWrt routers like the GL.iNet Flint 2. Watch-only: it never holds a spend key.";
                // An old link to an install guide that has its own page now
                // goes on to it, as does one followed on the open page.
                script { (PreEscaped(format!("(()=>{{const to={{{old_anchors}}};const go=()=>{{const page=to[location.hash];if(page)location.replace(page);}};go();addEventListener(\"hashchange\",go);}})();"))) }
                // Colours come only from monokulo's own theme (crates/ui/web/tokens.css,
                // copied here at build time): light or dark follows the OS.
                (site_assets::head(""))
                // The POS's status symbols, for the order's life.
                link rel="stylesheet" href=(site_assets::url("", "status.css"));
            }
            body {
                (PreEscaped(ui::assets::STATUS_SYMBOLS))
                (site_header::render(Section::Home, "", github_url))
                main id="top" class="home" {
                    section.hero {
                        div {
                            p.eyebrow { "Self-hosted Monero payments" }
                            h1 { "Accept Monero, on your own computer or router" }
                            p.lead {
                                "Monokulo is a self-hosted Monero payment gateway. It runs on Linux, macOS, Windows or your OpenWrt router, watches the chain with each store’s view key, and tells the store when an order is paid. It never holds a spend key, so it can never move your coins."
                            }
                            div.actions {
                                (ui::LinkButton::new(html! { "Get started" (PreEscaped(icons::ARROW_RIGHT)) }, site_header::docs_front("")).variant(ui::ButtonVariant::Primary))
                                (ui::LinkButton::new(html! { (PreEscaped(GITHUB_MARK)) "View on GitHub" }, github_url))
                            }
                            ul.chips aria-label="Runs on" {
                                @for chip in ["Linux", "macOS", "Windows", "OpenWrt", "Open source"] {
                                    li { (chip) }
                                }
                            }
                        }
                        div.flow { (order_life()) }
                    }
                    section.home-section aria-labelledby="why" {
                        h2 id="why" { "Why Monokulo" }
                        p { "One program with the payment engine inside it, a dashboard in your browser, and a Monero node it can reach." }
                        div.features {
                            @for (heading, words) in FEATURES {
                                (ui::Panel::new(html! { h3 { (heading) } p { (words) } }).look(ui::PanelLook::Tile))
                            }
                        }
                    }
                    section.home-section aria-labelledby="ways" {
                        h2 id="ways" { "Ways to take payments" }
                        p { "Start with the one that fits your shop. They all end in the same webhook." }
                        div.ways {
                            @for (heading, words, href, name) in WAYS {
                                a href=(href) {
                                    strong { (heading) }
                                    span { (words) }
                                    span.way-link { (name) " →" }
                                }
                            }
                        }
                    }
                    section.dev aria-labelledby="two-calls" {
                        div.home-section {
                            h2 id="two-calls" { "Two calls to a checkout" }
                            p { "Make an order with your store’s public key and show the checkout where you want it. Your server ships when the signed webhook says it’s paid." }
                            div.actions {
                                (ui::LinkButton::new("Accept your first payment", "guide/"))
                                (ui::LinkButton::new("Read the API reference", "api/"))
                            }
                        }
                        pre { code { (SAMPLE) } }
                    }
                    section.install-band id="install" aria-labelledby="install-heading" {
                        div {
                            h2 id="install-heading" { "Install it in about ten minutes" }
                            p { "On Linux, macOS or Windows, or on an OpenWrt router." }
                        }
                        div.actions {
                            (ui::LinkButton::new("Install on a computer", INSTALL_COMPUTER).attr("id", "desktop"))
                            (ui::LinkButton::new("Install on a router", INSTALL_ROUTER).attr("id", "openwrt"))
                        }
                    }
                    footer.home-foot {
                        span { "Monokulo is open source." }
                        nav aria-label="Footer" {
                            a href=(site_header::docs_front("")) { "Docs" }
                            a href="api/" { "API reference" }
                            a href=(format!("{github_url}/releases")) { "Releases" }
                            a href="testing/" { "Test reports" }
                            a href="stylebook/" { "Design system" }
                            a href=(github_url) { "GitHub" }
                        }
                    }
                }
            }
        }
    }
}
