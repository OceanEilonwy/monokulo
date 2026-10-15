use crate::{component, Attrs, Slot, UiEnum};

/// How wide a page's column is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum PageWidth {
    /// A reading width: the app's pages.
    Normal,
    /// Room for side-by-side cards: the project's site.
    Wide,
}

component! {
    /// The site's bar: the brand on the left, then the links, the status
    /// dot and the account menu. On a phone the links fold into a menu
    /// behind a hamburger, a checkbox and its label: no JavaScript.
    pub struct SiteNav {
        /// The brand: the logo and the name.
        brand: Slot,
        /// The links, and the account menu once signed in.
        links: Slot,
        /// Where the brand goes.
        #[default(String::from("/"))]
        home: String,
        /// Beside the hamburger, in sight on a phone: the status dot.
        status: Option<Slot>,
        /// The width of the page under it, which its row lines up with.
        #[default(PageWidth::Normal)]
        width: PageWidth,
    }
    html {
        nav class="site-nav" data-width="wide" if width == PageWidth::Wide {
            div class="wrap site-nav-row" {
                a href=(home) class="site-nav-brand" { (brand) }
                @if let Some(status) = status { (status) }
                input type="checkbox" id="nav-toggle" class="nav-toggle-checkbox";
                label for="nav-toggle" class="nav-toggle-label" aria-label="Menu" { "☰" }
                div class="site-nav-links" { (links) }
            }
        }
    }
}

component! {
    /// A menu behind a button: a `details`, so it opens and closes without
    /// JavaScript. Once its island attaches, a click outside it or Escape
    /// closes it too, Escape putting focus back on its button.
    pub struct Menu {
        /// The button's accessible name ("Account menu").
        label: String,
        /// What the button shows (an icon).
        button: Slot,
        /// The menu's links and controls.
        children: Slot,
        /// The button's tooltip (the signed-in email).
        title: Option<String>,
    }
    behaviour "menu.behaviour.ts" {
        uses(),
        ref root,
    }
    html {
        details class="acct" ref=root {
            summary class="acct-button" aria-label=(label) title=[title] { (button) }
            div class="acct-menu" { (children) }
        }
    }
}

component! {
    /// One row of a `Menu`: a link, a form's button (Log out, a POST), or
    /// a row holding a control (the theme switch). A `note` sits at its
    /// right: a count, "change".
    pub struct MenuItem {
        children: Slot,
        href: Option<String>,
        /// Posts here when chosen.
        post_to: Option<String>,
        note: Option<Slot>,
        title: Option<String>,
        /// On the row's element (the link, the form or the row).
        rest: Attrs,
    }
    html {
        @if let Some(href) = href {
            a class="menu-item" href=(href) title=[title] ..rest {
                span { (children) }
                @if let Some(note) = note { span class="menu-note" { (note) } }
            }
        } @else {
            @if let Some(post_to) = post_to {
                form method="post" action=(post_to) class="menu-form" ..rest {
                    button type="submit" class="menu-item" title=[title] { span { (children) } }
                }
            } @else {
                div class="menu-item" title=[title] ..rest {
                    span { (children) }
                    @if let Some(note) = note { (note) }
                }
            }
        }
    }
}

pub(super) fn menu_item_examples() -> Vec<(&'static str, MenuItem)> {
    vec![
        ("A link with a note", MenuItem::new("Wallets").href("#component-menu-item").note("1")),
        ("A form's button", MenuItem::new("Log out").post_to("#component-menu-item")),
    ]
}

/// A colour theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum ThemeChoice {
    /// The browser's: light or dark as the system says.
    System,
    Light,
    Dark,
}

component! {
    /// The theme switch: System, Light and Dark, each a submit button, so
    /// it works without JavaScript; the thumb slides to the one hovered.
    pub struct ThemeToggle {
        theme: ThemeChoice,
        /// Where the choice is posted.
        action: String,
        /// The page to come back to.
        next: String,
    }
    html {
        form method="post" action=(action) class="nav-theme-form" {
            input type="hidden" name="next" value=(next);
            div class=join["theme-toggle", { "theme-toggle-" (theme) }] role="group" aria-label="Theme" {
                span class="theme-toggle-track" {
                    button type="submit" name="theme" value="system" class="theme-toggle-option theme-toggle-option-system"
                        aria-label="System theme" aria-pressed=join["true" if theme == ThemeChoice::System, "false" if theme != ThemeChoice::System]
                        title="System theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" {
                            rect x="3" y="3" width="18" height="13" rx="2" {}
                            line x1="12" y1="16" x2="12" y2="21" {}
                            line x1="8" y1="21" x2="16" y2="21" {}
                        }
                    }
                    button type="submit" name="theme" value="light" class="theme-toggle-option theme-toggle-option-light"
                        aria-label="Light theme" aria-pressed=join["true" if theme == ThemeChoice::Light, "false" if theme != ThemeChoice::Light]
                        title="Light theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" {
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
                    button type="submit" name="theme" value="dark" class="theme-toggle-option theme-toggle-option-dark"
                        aria-label="Dark theme" aria-pressed=join["true" if theme == ThemeChoice::Dark, "false" if theme != ThemeChoice::Dark]
                        title="Dark theme" {
                        svg viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true" {
                            path d="M21 12.5A9 9 0 1 1 11.5 3 7 7 0 0 0 21 12.5Z" {}
                        }
                    }
                    span class="theme-toggle-thumb" {}
                }
            }
        }
    }
}

/// How the instance is, as the status dot shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum HealthState {
    /// Everything answers and scans.
    Ok,
    /// Everything works, but a block has taken over two minutes to scan.
    Slow,
    /// A node or the scan is failing, or the engine can't be reached.
    Error,
    /// Not checked yet, or couldn't be.
    Unknown,
}

/// Where a status dot sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum DotPlace {
    /// The site's bar.
    Nav,
    /// The POS's top bar.
    Pos,
}

component! {
    /// A glowing dot linking to the status page, as the instance was when
    /// the page was rendered, so it is right without JavaScript. Once its
    /// island attaches it polls `poll` every 30 seconds while the tab is in
    /// sight. Its title and accessible name carry the words.
    pub struct StatusDot {
        health: HealthState,
        place: DotPlace,
        #[default(String::from("/status"))]
        href: String,
        /// Answers `{"state": "ok" | "slow" | ...}`.
        #[default(String::from("/status/summary"))]
        poll: String,
        /// Its id: a page has one, `status-indicator`.
        #[default(String::from("status-indicator"))]
        id: String,
    }
    behaviour "status-dot.behaviour.ts" {
        uses(health, poll),
        /// How it is now.
        now: HealthState,
        /// What that means, in words.
        words: String,
    }
    html {
        a href=(href) class=join["nav-status-link" if place == DotPlace::Nav, "pos-status-link" if place == DotPlace::Pos]
            id=(id)
            title=join["all systems healthy" if health == HealthState::Ok,
                "scanning is slow - see the status page" if health == HealthState::Slow,
                "an issue was detected - see the status page" if health == HealthState::Error,
                "status not checked yet" if health == HealthState::Unknown]
            title=(b.words)
            aria-label=join["Status: all systems healthy" if health == HealthState::Ok,
                "Status: scanning is slow - see the status page" if health == HealthState::Slow,
                "Status: an issue was detected - see the status page" if health == HealthState::Error,
                "Status: status not checked yet" if health == HealthState::Unknown]
            aria-label={ "Status: " (b.words) } {
            span class=join["status-dot", { "status-dot-" (health) }] class=join["status-dot", { "status-dot-" (b.now) }] {}
        }
    }
}

pub(super) fn site_nav_examples() -> Vec<(&'static str, SiteNav)> {
    vec![(
        "Signed out",
        SiteNav::new(
            "Monokulo",
            Slot::many([
                Slot::from(
                    crate::LinkButton::new("Log in", "#component-site-nav")
                        .variant(crate::ButtonVariant::Quiet),
                ),
                Slot::from(
                    crate::LinkButton::new("Sign up", "#component-site-nav")
                        .variant(crate::ButtonVariant::Quiet),
                ),
            ]),
        )
        .home("#component-site-nav")
        .status(
            StatusDot::new(HealthState::Ok, DotPlace::Nav)
                .href("#component-status-dot")
                .id("book-nav-status"),
        ),
    )]
}

pub(super) fn menu_examples() -> Vec<(&'static str, Menu)> {
    vec![(
        "The account menu",
        Menu::new(
            "Example account menu",
            "Account",
            Slot::many([
                Slot::from(
                    crate::LinkButton::new("Account settings", "#component-menu")
                        .variant(crate::ButtonVariant::Quiet),
                ),
                Slot::from(
                    crate::LinkButton::new("Wallets", "#component-menu")
                        .variant(crate::ButtonVariant::Quiet),
                ),
            ]),
        )
        .title("alex@example.test"),
    )]
}

pub(super) fn theme_toggle_examples() -> Vec<(&'static str, ThemeToggle)> {
    vec![(
        "Following the system",
        ThemeToggle::new(ThemeChoice::System, "#component-theme-toggle", "/"),
    )]
}

pub(super) fn status_dot_examples() -> Vec<(&'static str, StatusDot)> {
    vec![
        (
            "Healthy",
            StatusDot::new(HealthState::Ok, DotPlace::Nav)
                .href("#component-status-dot")
                .poll("")
                .id("book-status-ok"),
        ),
        (
            "Slow",
            StatusDot::new(HealthState::Slow, DotPlace::Nav)
                .href("#component-status-dot")
                .poll("")
                .id("book-status-slow"),
        ),
        (
            "A problem",
            StatusDot::new(HealthState::Error, DotPlace::Nav)
                .href("#component-status-dot")
                .poll("")
                .id("book-status-error"),
        ),
    ]
}

/// One link of a [`PageHeader`]'s breadcrumbs.
#[derive(Clone, Debug, crate::UiRecord)]
pub struct Crumb {
    pub href: String,
    pub label: String,
}

impl Crumb {
    pub fn new(href: impl Into<String>, label: impl Into<String>) -> Self {
        Crumb { href: href.into(), label: label.into() }
    }
}

component! {
    /// The top of every page under the site's bar, always in this order:
    /// where the page sits (links to the pages above it, the nearest last,
    /// "Home › Logs"; one link alone is a back link, with an arrow), its
    /// title, then how far through a flow it is. The title is required, so
    /// no page goes without one. Beside the title: a status or a badge
    /// (`aside`), and at its right the page's own actions.
    pub struct PageHeader {
        /// The page's `h1`.
        title: Slot,
        #[default(Vec::new())]
        crumbs: Vec<Crumb>,
        /// Beside the title: a badge, a status, a help tip.
        aside: Option<Slot>,
        /// At the title's right: Reload, a network picker.
        actions: Option<Slot>,
        /// Under the title: a flow's progress (`WizardSteps`).
        progress: Option<Slot>,
        title_id: Option<String>,
    }
    html {
        header class="mk-page-header" {
            @if crumbs {
                nav class="mk-breadcrumbs" aria-label="Breadcrumb" {
                    @for link in crumbs {
                        a href=(link.href) title=(link.label) { (link.label) }
                    }
                }
            }
            div class="mk-page-title" {
                h1 id=[title_id] { (title) }
                @if let Some(aside) = aside { (aside) }
                @if let Some(actions) = actions { div class="mk-page-actions" { (actions) } }
            }
            @if let Some(progress) = progress { (progress) }
        }
    }
}

pub(super) fn page_header_examples() -> Vec<(&'static str, PageHeader)> {
    let here = |label: &str| Crumb::new("#component-page-header", label);
    let progress = |state: crate::WizardState| {
        let step = |label: &str, detail: &str, done, state| crate::WizardStep {
            label: label.into(),
            detail: detail.into(),
            done,
            tasks: 1,
            state,
        };
        crate::WizardSteps::new(
            "Setup progress",
            vec![
                step("Store", "In progress", 0, state),
                step("Wallet", "Next", 0, crate::WizardState::Next),
                step("Done", "Next", 0, crate::WizardState::Next),
            ],
        )
    };
    vec![
        ("A title alone", PageHeader::new("Home")),
        ("Back to a page", PageHeader::new("Admin settings").crumbs(vec![here("Home")])),
        (
            "Two levels up, with an action",
            PageHeader::new("Order 4f2a")
                .crumbs(vec![here("Corner store with a long name to see it shortened"), here("Orders")])
                .aside(crate::Tag::new("Paid").tone(crate::TagTone::Ok))
                .actions(crate::LinkButton::new("Reload", "#component-page-header").size(crate::ControlSize::Compact)),
        ),
        ("In a flow", PageHeader::new("Set up a store").progress(progress(crate::WizardState::Current))),
    ]
}
