//! What pages are made of between the controls: panels, messages, hints,
//! facts, badges, disclosures, checklists and timelines.

use crate::{component, Attrs, Slot, UiEnum, UiRecord};

component! {
    /// A raised panel holding one part of a page: a form, a set of facts.
    pub struct Panel {
        children: Slot,
        rest: Attrs,
    }
    html {
        div class="box" ..rest { (children) }
    }
}

pub(super) fn panel_examples() -> Vec<(&'static str, Panel)> {
    vec![(
        "A part of a page",
        Panel::new(Slot::many([
            Slot::from(Heading::new("Webhook").level(HeadingLevel::H3)),
            Slot::from(Hint::new("Monokulo posts each order's changes here.")),
        ])),
    )]
}

/// What a message says about how something went. The words say it; the
/// colour only backs them up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum Tone {
    /// Something was refused or is broken.
    Error,
    /// Something needs care, but works.
    Warning,
    /// Something went through.
    Success,
    /// Information, neither good nor bad.
    Notice,
}

component! {
    /// A message about the page or what just happened: a refused save, a
    /// node that's down, a save that went through. An error is announced
    /// (`role="alert"`), the rest are statuses, unless `quiet`.
    pub struct Alert {
        children: Slot,
        tone: Tone,
        /// Not announced: a message that is part of the page as it loads.
        #[default(false)]
        quiet: bool,
        /// A `div`, for a message holding lists or paragraphs; a `p` otherwise.
        #[default(false)]
        block: bool,
        rest: Attrs,
    }
    html {
        @if quiet {
            @if block { div class=(tone) ..rest { (children) } } @else { p class=(tone) ..rest { (children) } }
        } @else if tone == Tone::Error {
            @if block { div class=(tone) role="alert" ..rest { (children) } } @else { p class=(tone) role="alert" ..rest { (children) } }
        } @else {
            @if block { div class=(tone) role="status" ..rest { (children) } } @else { p class=(tone) role="status" ..rest { (children) } }
        }
    }
}

pub(super) fn alert_examples() -> Vec<(&'static str, Alert)> {
    vec![
        (
            "Error",
            Alert::new("Could not save. Check the highlighted fields and try again.", Tone::Error),
        ),
        (
            "Warning",
            Alert::new("Connection interrupted. Payment status may be delayed.", Tone::Warning),
        ),
        ("Success", Alert::new("Saved.", Tone::Success)),
        ("Notice", Alert::new("No wallets yet.", Tone::Notice).quiet(true)),
    ]
}

component! {
    /// Muted help beside or under what it explains. A `span` inside a line
    /// of text when `inline`; a paragraph otherwise.
    pub struct Hint {
        children: Slot,
        #[default(false)]
        inline: bool,
        rest: Attrs,
    }
    html {
        @if inline {
            span class="hint" ..rest { (children) }
        } @else {
            p class="hint" ..rest { (children) }
        }
    }
}

pub(super) fn hint_examples() -> Vec<(&'static str, Hint)> {
    vec![
        ("Under a field", Hint::new("Shown to customers on the payment page.")),
        ("In a line", Hint::new("(optional)").inline(true)),
    ]
}

/// A heading's level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
    H4,
}

component! {
    /// A page's or a section's heading. A page's (`H1`) can have actions at
    /// its right: a Reload, a link to settings.
    pub struct Heading {
        children: Slot,
        #[default(HeadingLevel::H2)]
        level: HeadingLevel,
        /// Beside a page heading, at the right.
        actions: Option<Slot>,
        id: Option<String>,
    }
    html {
        @if let Some(actions) = actions {
            div class="page-heading" {
                @if level == HeadingLevel::H1 { h1 id=[id] { (children) } }
                @else if level == HeadingLevel::H2 { h2 id=[id] { (children) } }
                @else if level == HeadingLevel::H3 { h3 id=[id] { (children) } }
                @else { h4 id=[id] { (children) } }
                (actions)
            }
        } @else {
            @if level == HeadingLevel::H1 { h1 id=[id] { (children) } }
            @else if level == HeadingLevel::H2 { h2 id=[id] { (children) } }
            @else if level == HeadingLevel::H3 { h3 id=[id] { (children) } }
            @else { h4 id=[id] { (children) } }
        }
    }
}

pub(super) fn heading_examples() -> Vec<(&'static str, Heading)> {
    vec![
        (
            "A page's, with an action",
            Heading::new("Orders")
                .level(HeadingLevel::H1)
                .actions(crate::LinkButton::new("Reload", "#component-heading").size(crate::ControlSize::Compact)),
        ),
        ("A section's", Heading::new("Recent orders")),
        ("A part's", Heading::new("Webhook").level(HeadingLevel::H3)),
    ]
}

/// One fact of [`Facts`].
#[derive(Clone, Debug, UiRecord)]
pub struct Fact {
    /// What it is ("Network").
    pub term: String,
    pub value: Slot,
}

component! {
    /// Facts about one thing, each named: a wallet's network, an order's
    /// amount.
    pub struct Facts {
        facts: Vec<Fact>,
        rest: Attrs,
    }
    html {
        dl class="facts" ..rest {
            @for fact in facts {
                dt { (fact.term) }
                dd { (fact.value) }
            }
        }
    }
}

pub(super) fn facts_examples() -> Vec<(&'static str, Facts)> {
    let fact = |term: &str, value: Slot| Fact {
        term: term.into(),
        value,
    };
    vec![(
        "A wallet",
        Facts::new(vec![
            fact("Network", NetworkBadge::new("Mainnet").main(true).into()),
            fact("Watching since", "block 3,412,001".into()),
            fact("Balance", "1.25 XMR".into()),
        ]),
    )]
}

/// How a tag reads, beside its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum TagTone {
    Ok,
    Error,
    Unknown,
    Syncing,
    Stalled,
    Slow,
}

/// An order's state: each its own colours, the same wherever an order's
/// state is shown (the checkout, the POS, the dashboard).
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum OrderState {
    Pending,
    Unconfirmed,
    Confirming,
    Partial,
    Paid,
    Overpaid,
    Expired,
    Cancelled,
    DoubleSpend,
    Offline,
    Unknown,
}

component! {
    /// A short word or two about something's state: a node's health, an
    /// order's status. `short` is what a phone shows instead, the long
    /// words kept for screen readers.
    pub struct Tag {
        children: Slot,
        tone: Option<TagTone>,
        state: Option<OrderState>,
        short: Option<String>,
        rest: Attrs,
    }
    html {
        span class=join["tag", { "tag-" (tone) } if tone, { "state-" (state) } if state] ..rest {
            @if let Some(short) = short {
                span class="label-long" { (children) }
                span class="label-short" aria-hidden="true" { (short) }
            } @else {
                (children)
            }
        }
    }
}

pub(super) fn tag_examples() -> Vec<(&'static str, Tag)> {
    vec![
        ("Plain", Tag::new("Default")),
        ("Healthy", Tag::new("Healthy").tone(TagTone::Ok)),
        ("Down", Tag::new("Down").tone(TagTone::Error)),
        ("Not checked", Tag::new("Not checked").tone(TagTone::Unknown)),
        ("Syncing", Tag::new("Syncing").tone(TagTone::Syncing)),
        ("Slow", Tag::new("Slow").tone(TagTone::Slow)),
        (
            "An order waiting, shorter on a phone",
            Tag::new("Waiting for payment")
                .state(OrderState::Pending)
                .short("Waiting"),
        ),
        ("An order seen", Tag::new("Payment seen, unconfirmed").state(OrderState::Unconfirmed).short("Seen")),
        ("An order confirming", Tag::new("Confirming").state(OrderState::Confirming)),
        ("An order paid", Tag::new("Paid").state(OrderState::Paid)),
        ("An order expired", Tag::new("Expired").state(OrderState::Expired)),
        ("An order cancelled", Tag::new("Cancelled").state(OrderState::Cancelled)),
    ]
}

component! {
    /// A Monero network, the same wherever a network is shown: the word
    /// always, with an icon and an edge to back it up. Mainnet (real money)
    /// has the brand's orange tint, a coin and a solid edge; a test network
    /// a neutral tint, a flask and a dashed edge.
    pub struct NetworkBadge {
        /// The network's name ("Mainnet", "Stagenet", "Test networks").
        name: String,
        #[default(false)]
        main: bool,
    }
    html {
        span class=join["tag-network", "is-main" if main, "is-test" if !main] {
            svg viewBox="0 0 24 24" aria-hidden="true" {
                @if main {
                    circle cx="12" cy="12" r="9" {}
                    path d="M7 15V9l5 5 5-5v6" {}
                } @else {
                    path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 1.7 3h10.6a2 2 0 0 0 1.7-3l-5-9V3" {}
                    path d="M7.5 15h9" {}
                }
            }
            (name)
        }
    }
}

pub(super) fn network_badge_examples() -> Vec<(&'static str, NetworkBadge)> {
    vec![
        ("Mainnet", NetworkBadge::new("Mainnet").main(true)),
        ("A test network", NetworkBadge::new("Stagenet")),
    ]
}

component! {
    /// A long value shortened on screen (an id, an address) whose whole
    /// value is still what a double-click selects, a copy takes, a screen
    /// reader reads and find-in-page matches: the short text shows, and the
    /// whole value lies over it, transparent. Short enough already, it is
    /// just the text.
    pub struct ShortValue {
        value: String,
        /// What shows.
        short: String,
    }
    html {
        @if value == short {
            (value)
        } @else {
            span class="short-value" title=(value) {
                span class="short-value-text" aria-hidden="true" { (short) }
                span class="short-value-full" { (value) }
            }
        }
    }
}

pub(super) fn short_value_examples() -> Vec<(&'static str, ShortValue)> {
    vec![
        (
            "An order id",
            ShortValue::new("order_a8723b2e45b0d44ea8723b2e45b0d44e", "a8723b…b0d44e"),
        ),
        ("Short enough", ShortValue::new("a8723b", "a8723b")),
    ]
}

/// A disclosure's look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum DisclosureLook {
    /// An underlined summary with an arrow, in the text.
    Plain,
    /// A bordered box, its summary a bar: help to open ("Where do I find
    /// these keys?").
    Boxed,
    /// A boxed "More options" at the end of a form: more space round it.
    MoreOptions,
}

component! {
    /// More to see behind a summary: a `details`, so it opens without
    /// JavaScript.
    pub struct Disclosure {
        summary: Slot,
        children: Slot,
        #[default(DisclosureLook::Plain)]
        look: DisclosureLook,
        #[default(false)]
        open: bool,
        rest: Attrs,
    }
    html {
        details class=join["disclosure" if look == DisclosureLook::Plain, "keys-help" if look == DisclosureLook::Boxed, "more-options" if look == DisclosureLook::MoreOptions]
            open[open] ..rest {
            summary { (summary) }
            @if look == DisclosureLook::Boxed {
                div class="keys-help-body" { (children) }
            } @else {
                (children)
            }
        }
    }
}

pub(super) fn disclosure_examples() -> Vec<(&'static str, Disclosure)> {
    vec![
        (
            "Plain",
            Disclosure::new("How it's checked", "Every block is checked against two nodes."),
        ),
        (
            "Boxed help",
            Disclosure::new("Where do I find these keys?", "Settings, then Keys.")
                .look(DisclosureLook::Boxed),
        ),
        (
            "More options",
            Disclosure::new("More options", "Leave on mainnet unless this is a test wallet.")
                .look(DisclosureLook::MoreOptions),
        ),
    ]
}

/// Where a check stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum CheckMark {
    /// Done: a tick.
    Ok,
    /// Not yet: a cross.
    No,
    /// What will happen: an arrow.
    Then,
}

/// One row of [`Checks`].
#[derive(Clone, Debug, UiRecord)]
pub struct Check {
    pub mark: CheckMark,
    pub what: Slot,
    /// How to fix it, or more about it, under it.
    pub fix: Option<Slot>,
}

component! {
    /// What's done and what isn't, or what will happen: a list of rows
    /// each with a mark, which screen readers hear as "Done:" or
    /// "Not yet:".
    pub struct Checks {
        checks: Vec<Check>,
        rest: Attrs,
    }
    html {
        ul class="checks" ..rest {
            @for check in checks {
                li class=(check.mark) {
                    @if check.mark == CheckMark::Ok {
                        span class="mark" aria-hidden="true" { "✓" }
                    } @else if check.mark == CheckMark::No {
                        span class="mark" aria-hidden="true" { "✕" }
                    } @else {
                        span class="mark" aria-hidden="true" { "→" }
                    }
                    span {
                        @if check.mark == CheckMark::Ok {
                            span class="visually-hidden" { "Done: " }
                        } @else if check.mark == CheckMark::No {
                            span class="visually-hidden" { "Not yet: " }
                        }
                        (check.what)
                        @if let Some(fix) = check.fix { span class="fix" { (fix) } }
                    }
                }
            }
        }
    }
}

pub(super) fn checks_examples() -> Vec<(&'static str, Checks)> {
    let check = |mark, what: &str, fix: Option<&str>| Check {
        mark,
        what: what.into(),
        fix: fix.map(Slot::from),
    };
    vec![(
        "Before removing a website",
        Checks::new(vec![
            check(CheckMark::Ok, "No plugin is connected to it", None),
            check(CheckMark::No, "No order from shop.example.com can still be paid", Some("2 can be paid until about 14:20")),
            check(CheckMark::Then, "Checkouts embedded on shop.example.com stop loading", None),
        ]),
    )]
}

/// One event of a [`Timeline`].
#[derive(Clone, Debug, UiRecord)]
pub struct Event {
    /// When ("12 Oct, 14:02").
    pub when: String,
    pub what: Slot,
}

component! {
    /// What happened to something, in order, each with when.
    pub struct Timeline {
        events: Vec<Event>,
    }
    html {
        ol class="timeline" {
            @for event in events {
                li { time { (event.when) } span { (event.what) } }
            }
        }
    }
}

pub(super) fn timeline_examples() -> Vec<(&'static str, Timeline)> {
    let event = |when: &str, what: &str| Event {
        when: when.into(),
        what: what.into(),
    };
    vec![(
        "A wallet's history",
        Timeline::new(vec![
            event("12 Oct, 14:02", "Added"),
            event("12 Oct, 14:09", "Backup checked"),
            event("13 Oct, 09:41", "First payment"),
        ]),
    )]
}
