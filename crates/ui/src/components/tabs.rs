use crate::{component, Slot, UiRecord};

/// One tab of [`Tabs`]: its label and the panel it shows.
#[derive(Clone, Debug, UiRecord)]
pub struct Tab {
    /// The panel's id: the tab links to it.
    pub id: String,
    /// What the tab says: words, or a logo and words.
    pub label: Slot,
    pub panel: Slot,
}

component! {
    /// Panels of one page, one shown at a time. Without JavaScript every
    /// panel shows, and the tabs are links to them. Once its island
    /// attaches, the tabs are a tablist: one panel at a time, picked with
    /// a click or the arrow keys, Home and End. Each pick sends `mk-tab`
    /// (`{id}`, the panel shown) from the tabs, for a page script to follow.
    pub struct Tabs {
        /// What the tabs are of.
        label: String,
        tabs: Vec<Tab>,
    }
    behaviour "tabs.behaviour.ts" {
        uses(tabs),
        /// The shown panel's id.
        selected: String,
        fn pick(String),
        fn key,
        ref root,
    }
    html {
        div class="mk-tabset" ref=root {
            div class="mk-tabs" aria-label=(label) role="tablist" if enhanced {
                @for tab in tabs {
                    a href={ "#" (tab.id) } id={ (tab.id) "-tab" } role="tab" if enhanced
                        aria-controls=(tab.id) if enhanced aria-selected="true" if tab.id == b.selected
                        tabindex="-1" if tab.id != b.selected on:click=pick(tab.id) on:keydown=key {
                        (tab.label)
                    }
                }
            }
            div class="mk-tab-panels" {
                @for tab in tabs {
                    section id=(tab.id) role="tabpanel" if enhanced aria-labelledby={ (tab.id) "-tab" } if enhanced
                        tabindex="0" if enhanced hidden[tab.id != b.selected] {
                        (tab.panel)
                    }
                }
            }
        }
    }
}

/// A link of [`TabLinks`].
#[derive(Clone, Debug, UiRecord)]
pub struct TabLink {
    pub href: String,
    pub label: String,
    /// The page this is.
    pub current: bool,
    /// How many things the page holds, beside its label.
    pub count: Option<String>,
}

component! {
    /// Tabs that are separate pages: plain links, the current one marked.
    /// No behaviour: each is a page of its own.
    pub struct TabLinks {
        /// What the pages are of.
        label: String,
        links: Vec<TabLink>,
    }
    html {
        nav class="mk-tabs" aria-label=(label) {
            @for link in links {
                a href=(link.href) aria-current="page" if link.current {
                    (link.label)
                    @if let Some(count) = link.count { span class="mk-tab-count" { (count) } }
                }
            }
        }
    }
}

pub(super) fn tabs_examples() -> Vec<(&'static str, Tabs)> {
    let tab = |id: &str, label: &str, panel: &str| Tab {
        id: id.into(),
        label: label.into(),
        panel: panel.into(),
    };
    vec![(
        "A POS's views",
        Tabs::new(
            "Example POS views",
            vec![
                tab(
                    "example-charge",
                    "Charge",
                    "Enter an amount to start a payment.",
                ),
                tab("example-history", "History", "No payments yet."),
                tab(
                    "example-terminal",
                    "Terminal",
                    "This terminal is ready to receive payments.",
                ),
            ],
        ),
    )]
}

pub(super) fn tab_links_examples() -> Vec<(&'static str, TabLinks)> {
    let link = |href: &str, label: &str, current| TabLink {
        href: href.into(),
        label: label.into(),
        current,
        count: None,
    };
    vec![(
        "Settings sections",
        TabLinks::new(
            "Example settings navigation",
            vec![
                link("#component-tab-links", "General", true),
                link("#component-select", "Networks", false),
                TabLink {
                    count: Some("3".into()),
                    ..link("#component-alert", "Notifications", false)
                },
            ],
        ),
    )]
}
