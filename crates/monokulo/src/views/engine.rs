//! The engine page's shell (`docs/engine_visualizer.md`): the header with
//! the network picker, and where the page's app goes. The app
//! (`apps/engine`) draws the whole live view, the help's legend included,
//! from what `http::engine_page` sends it; it needs JavaScript, and without
//! it the page says so.

use maud::{html, Markup};

use super::{reload_button, script, stylesheet, Load, PageChrome};
use crate::views::scaling::network_name;
use ui::{Alert, Button, ButtonType, Choice, Select, Tone};

pub struct EnginePage {
    /// The networks the engine scans, for the picker.
    pub networks: Vec<String>,
    pub network: String,
    /// Why the engine couldn't be read, if it couldn't.
    pub error: Option<String>,
}

/// Every Monero network, in the order the picker shows them.
const NETWORKS: [&str; 3] = ["mainnet", "stagenet", "testnet"];

pub fn page(chrome: &PageChrome, page: &EnginePage) -> Markup {
    let head = html! {
        (stylesheet("engine-view.css"))
        (script("engine-view.js", Load::Module))
    };
    let header = ui::PageHeader::new("Engine")
        .crumbs(super::crumbs(&[("/status", "Status")]))
        // The legend's help tip: the app puts it here.
        .aside(html! { span id="engine-help-slot" {} })
        .actions(html! {
            // With JavaScript, picking a network goes there; without, Go
            // does.
            form class="engine-network" method="get" action="/status/engine" {
                label for="engine-network" class="visually-hidden" { "Network" }
                (Select::new(
                    "network",
                    NETWORKS
                        .iter()
                        .map(|network| {
                            let configured = page.networks.iter().any(|n| n == network);
                            let choice = Choice::new(network, network_name(network))
                                .selected(*network == page.network)
                                .disabled(!configured);
                            if configured { choice } else { choice.note("no node configured") }
                        })
                        .collect::<Vec<_>>(),
                )
                .id("engine-network")
                .compact(true))
                (Button::new("Go").kind(ButtonType::Submit).attr("data-only", "no-script"))
            }
            // Shown by the app while its stream is broken off.
            (reload_button(&format!("/status/engine?network={}", page.network)))
        });
    let body = html! {
        @if let Some(error) = &page.error {
            (Alert::new(format!("The engine's activity could not be read: {error}"), Tone::Error))
        } @else {
            div class="engine-app" id="engine-app" data-network=(page.network) {
                noscript {
                    (Alert::new(
                        "The engine page needs JavaScript: it follows the engine live. The Status page shows each network without it.",
                        Tone::Notice,
                    ))
                }
            }
        }
    };
    super::Page::new("Engine - Monokulo", header)
        .class("engine-page")
        .head(head)
        .render(chrome, body)
}
