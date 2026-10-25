//! What pages are made of between the controls: panels, messages, hints,
//! facts, badges, disclosures, checklists and timelines.

use crate::{component, Attrs, Slot, UiEnum, UiRecord};

/// A panel's look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum PanelLook {
    /// Raised off the page, with space above and below.
    Raised,
    /// Flat, no outer space: one of a grid of them.
    Tile,
}

component! {
    /// A panel holding one part of a page: a form, a set of facts; or, as
    /// a tile, one of a grid of them.
    pub struct Panel {
        children: Slot,
        #[default(PanelLook::Raised)]
        look: PanelLook,
        rest: Attrs,
    }
    html {
        div class="box" data-look="tile" if look == PanelLook::Tile ..rest { (children) }
    }
}

pub(super) fn panel_examples() -> Vec<(&'static str, Panel)> {
    let tile = |title: &str, text: &str| {
        Panel::new(Slot::many([
            Slot::from(Heading::new(title).level(HeadingLevel::H3)),
            Slot::from(Hint::new(text)),
        ]))
        .look(PanelLook::Tile)
    };
    vec![
        (
            "A tile",
            tile(
                "Watch-only",
                "Each store gives Monokulo its view key, never its spend key.",
            ),
        ),
        (
            "A part of a page",
            Panel::new(Slot::many([
                Slot::from(Heading::new("Webhook").level(HeadingLevel::H3)),
                Slot::from(Hint::new("Monokulo posts each order's changes here.")),
            ])),
        ),
    ]
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
    /// (`role="alert"`), the rest are statuses, unless `quiet`. A message
    /// that asks for something takes a `title` and `actions` (Dismiss, a
    /// link to fix it).
    pub struct Alert {
        children: Slot,
        tone: Tone,
        /// Before everything, at the left: an icon (`assets::icons::INFO`).
        icon: Option<Slot>,
        /// Bold, before the words.
        title: Option<String>,
        /// Under the words: buttons or links.
        actions: Option<Slot>,
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
            @if block { div class=(tone) ..rest { @if let Some(icon) = icon { span class="alert-icon" aria-hidden="true" { (icon) } } @if let Some(title) = title { strong class="alert-title" { (title) } " " } (children) @if let Some(actions) = actions { span class="alert-actions" { (actions) } } } } @else { p class=(tone) ..rest { @if let Some(icon) = icon { span class="alert-icon" aria-hidden="true" { (icon) } } @if let Some(title) = title { strong class="alert-title" { (title) } " " } (children) @if let Some(actions) = actions { span class="alert-actions" { (actions) } } } }
        } @else if tone == Tone::Error {
            @if block { div class=(tone) role="alert" ..rest { @if let Some(icon) = icon { span class="alert-icon" aria-hidden="true" { (icon) } } @if let Some(title) = title { strong class="alert-title" { (title) } " " } (children) @if let Some(actions) = actions { span class="alert-actions" { (actions) } } } } @else { p class=(tone) role="alert" ..rest { @if let Some(icon) = icon { span class="alert-icon" aria-hidden="true" { (icon) } } @if let Some(title) = title { strong class="alert-title" { (title) } " " } (children) @if let Some(actions) = actions { span class="alert-actions" { (actions) } } } }
        } @else {
            @if block { div class=(tone) role="status" ..rest { @if let Some(icon) = icon { span class="alert-icon" aria-hidden="true" { (icon) } } @if let Some(title) = title { strong class="alert-title" { (title) } " " } (children) @if let Some(actions) = actions { span class="alert-actions" { (actions) } } } } @else { p class=(tone) role="status" ..rest { @if let Some(icon) = icon { span class="alert-icon" aria-hidden="true" { (icon) } } @if let Some(title) = title { strong class="alert-title" { (title) } " " } (children) @if let Some(actions) = actions { span class="alert-actions" { (actions) } } } }
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
        (
            "Asking for something",
            Alert::new(
                "If a scam site embeds it, that site's victims would pay you. Verify your domains in Settings and allow only them.",
                Tone::Warning,
            )
            .title("Any website can show this store's checkout")
            .actions(crate::Button::new("Dismiss").size(crate::ControlSize::Compact))
            .block(true)
            .quiet(true),
        ),
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
        (
            "Under a field",
            Hint::new("Shown to customers on the payment page."),
        ),
        ("In a line", Hint::new("(optional)").inline(true)),
    ]
}

/// A heading's level. A page's own title, the `h1`, is its `PageHeader`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum HeadingLevel {
    H2,
    H3,
    H4,
}

component! {
    /// A section's or a part's heading, under the page's `PageHeader`.
    pub struct Heading {
        children: Slot,
        #[default(HeadingLevel::H2)]
        level: HeadingLevel,
        id: Option<String>,
    }
    html {
        @if level == HeadingLevel::H2 { h2 id=[id] { (children) } }
        @else if level == HeadingLevel::H3 { h3 id=[id] { (children) } }
        @else { h4 id=[id] { (children) } }
    }
}

pub(super) fn heading_examples() -> Vec<(&'static str, Heading)> {
    vec![
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
    /// Information beside a label, not a state: muted, light.
    Muted,
    /// Muted, with a dashed edge: what nothing has chosen (a default).
    Dashed,
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
        /// Before the words: an inline SVG or a status symbol.
        icon: Option<Slot>,
        rest: Attrs,
    }
    html {
        span class=join["tag", { "tag-" (tone) } if tone, { "state-" (state) } if state] ..rest {
            @if let Some(icon) = icon { span class="tag-icon" aria-hidden="true" { (icon) } }
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
        (
            "Not checked",
            Tag::new("Not checked").tone(TagTone::Unknown),
        ),
        ("Syncing", Tag::new("Syncing").tone(TagTone::Syncing)),
        ("Slow", Tag::new("Slow").tone(TagTone::Slow)),
        (
            "Muted, with an icon",
            Tag::new("Options file")
                .tone(TagTone::Muted)
                .icon(Slot::from("⚙")),
        ),
        (
            "Dashed: a default",
            Tag::new("Default").tone(TagTone::Dashed),
        ),
        (
            "An order waiting, shorter on a phone",
            Tag::new("Waiting for payment")
                .state(OrderState::Pending)
                .short("Waiting"),
        ),
        (
            "An order seen",
            Tag::new("Payment seen, unconfirmed")
                .state(OrderState::Unconfirmed)
                .short("Seen"),
        ),
        (
            "An order confirming",
            Tag::new("Confirming").state(OrderState::Confirming),
        ),
        ("An order paid", Tag::new("Paid").state(OrderState::Paid)),
        (
            "An order expired",
            Tag::new("Expired").state(OrderState::Expired),
        ),
        (
            "An order cancelled",
            Tag::new("Cancelled").state(OrderState::Cancelled),
        ),
    ]
}

component! {
    /// An order's state as the POS draws it, in a square box the caller's
    /// CSS sizes: a spinner while it waits, an empty disc once a payment is
    /// seen, the disc filling in a wedge as it confirms, a coin when paid
    /// (part of one, or two), and a symbol for an order that stopped. The
    /// coins and symbols are `<use>`s of `assets::STATUS_SYMBOLS`, which a
    /// page includes once; `assets::STATUS_CSS` draws them in the state's
    /// ink. It carries its state's colours itself (`state-*`), so it needs
    /// nothing around it. A picture of what the words near it say, so
    /// screen readers skip it.
    pub struct StatusIcon {
        state: OrderState,
        /// How far a confirming order is, in percent: the wedge.
        progress: Option<u32>,
    }
    html {
        span class={ "pos-icon pos-icon-" (state) " state-" (state) } aria-hidden="true" {
            @if state == OrderState::Pending { span class="pos-spinner" {} }
            @else if state == OrderState::Unconfirmed { span class="pos-disc" {} }
            @else if state == OrderState::Confirming {
                @if let Some(progress) = progress { span class="pos-disc" style={ "--progress: " (progress) "%;" } {} }
                @else { span class="pos-disc" {} }
            }
            @else if state == OrderState::Partial { svg viewBox="0 0 32 32" { use href="#pos-coin-partial" {} } }
            @else if state == OrderState::Paid { svg viewBox="0 0 32 32" { use href="#pos-coin" {} } }
            @else if state == OrderState::Overpaid { svg viewBox="0 0 44 32" { use href="#pos-coins-overpaid" {} } }
            @else if state == OrderState::Expired { svg viewBox="0 0 24 24" { use href="#pos-expired" {} } }
            @else if state == OrderState::DoubleSpend { svg viewBox="0 0 24 24" { use href="#pos-double-spend" {} } }
            @else if state == OrderState::Offline { svg viewBox="0 0 24 24" { use href="#pos-offline" {} } }
            @else if state == OrderState::Cancelled { svg viewBox="0 0 24 24" { use href="#pos-cancelled" {} } }
        }
    }
}

pub(super) fn status_icon_examples() -> Vec<(&'static str, StatusIcon)> {
    vec![
        ("Awaiting payment", StatusIcon::new(OrderState::Pending)),
        ("Unconfirmed", StatusIcon::new(OrderState::Unconfirmed)),
        (
            "Confirming, 3 of 5",
            StatusIcon::new(OrderState::Confirming).progress(60u32),
        ),
        ("Partially paid", StatusIcon::new(OrderState::Partial)),
        ("Paid", StatusIcon::new(OrderState::Paid)),
        ("Overpaid", StatusIcon::new(OrderState::Overpaid)),
        ("Expired", StatusIcon::new(OrderState::Expired)),
        ("Double spend", StatusIcon::new(OrderState::DoubleSpend)),
        ("Connection lost", StatusIcon::new(OrderState::Offline)),
        ("Cancelled", StatusIcon::new(OrderState::Cancelled)),
    ]
}

/// An HTTP request's method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum HttpMethod {
    Get,
    Post,
    Delete,
}

component! {
    /// A request's method, beside a route's path in the API reference: GET
    /// in a neutral tint, POST in the brand's, DELETE in the error's, so a
    /// route that changes or removes something stands out from one that
    /// reads.
    pub struct HttpMethodTag {
        method: HttpMethod,
        rest: Attrs,
    }
    html {
        span class="mk-http-method" data-method=(method) ..rest {
            @if method == HttpMethod::Get { "GET" }
            @else if method == HttpMethod::Post { "POST" }
            @else { "DELETE" }
        }
    }
}

pub(super) fn http_method_tag_examples() -> Vec<(&'static str, HttpMethodTag)> {
    vec![
        ("Reads", HttpMethodTag::new(HttpMethod::Get)),
        ("Makes or changes", HttpMethodTag::new(HttpMethod::Post)),
        ("Removes", HttpMethodTag::new(HttpMethod::Delete)),
    ]
}

/// What kind of answer an HTTP status code is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum HttpOutcome {
    /// 2xx and 3xx: it worked, or here's where.
    Ok,
    /// 4xx: the request was wrong.
    ClientError,
    /// 5xx: the server failed.
    ServerError,
    /// Anything else (1xx, or an API description's "default").
    Other,
}

impl HttpOutcome {
    /// The kind of `code` ("404"), by its first digit.
    pub fn of(code: &str) -> HttpOutcome {
        match code.as_bytes().first() {
            Some(b'2' | b'3') => HttpOutcome::Ok,
            Some(b'4') => HttpOutcome::ClientError,
            Some(b'5') => HttpOutcome::ServerError,
            _ => HttpOutcome::Other,
        }
    }
}

component! {
    /// A response's status code, in the API reference: success in the
    /// success tint, a client's error in the warning's, a server's in the
    /// error's. [`HttpStatusTag::of`] works the kind out from the code.
    pub struct HttpStatusTag {
        /// The code as the API names it ("200", "4xx").
        code: String,
        outcome: HttpOutcome,
        rest: Attrs,
    }
    html {
        span class="mk-http-status" data-outcome=(outcome) ..rest { (code) }
    }
}

impl HttpStatusTag {
    /// The tag for `code`, its kind by its first digit.
    pub fn of(code: impl Into<String>) -> HttpStatusTag {
        let code = code.into();
        let outcome = HttpOutcome::of(&code);
        HttpStatusTag::new(code, outcome)
    }
}

pub(super) fn http_status_tag_examples() -> Vec<(&'static str, HttpStatusTag)> {
    vec![
        ("Success", HttpStatusTag::of("200")),
        ("A redirect", HttpStatusTag::of("303")),
        ("The client's error", HttpStatusTag::of("404")),
        ("Client errors", HttpStatusTag::of("4xx")),
        ("The server's error", HttpStatusTag::of("503")),
        ("Anything else", HttpStatusTag::of("default")),
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
        /// Only the icon and the edge, its name for screen readers and on
        /// hover: in a table's cell, beside a wallet's name.
        #[default(false)]
        icon_only: bool,
    }
    html {
        span class=join["tag-network", "is-main" if main, "is-test" if !main] data-size="icon" if icon_only
            title=(name) if icon_only {
            svg viewBox="0 0 24 24" aria-hidden="true" {
                @if main {
                    circle cx="12" cy="12" r="9" {}
                    path d="M7 15V9l5 5 5-5v6" {}
                } @else {
                    path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 1.7 3h10.6a2 2 0 0 0 1.7-3l-5-9V3" {}
                    path d="M7.5 15h9" {}
                }
            }
            span class="tag-network-name" { (name) }
        }
    }
}

pub(super) fn network_badge_examples() -> Vec<(&'static str, NetworkBadge)> {
    vec![
        ("Mainnet", NetworkBadge::new("Mainnet").main(true)),
        ("A test network", NetworkBadge::new("Stagenet")),
        (
            "Mainnet, icon only",
            NetworkBadge::new("Mainnet").main(true).icon_only(true),
        ),
        (
            "A test network, icon only",
            NetworkBadge::new("Stagenet").icon_only(true),
        ),
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
    /// A small panel: a one-line summary (a label and a figure) with a
    /// chevron, its body a few facts. For dense dashboards.
    Compact,
    /// A row of a list: a one-line summary, its body a form or details.
    /// What a `SortableList` holds.
    Row,
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
        /// Something in it needs attention: an error's edge (open it too).
        #[default(false)]
        alert: bool,
        rest: Attrs,
    }
    html {
        details data-alert="" if alert class=join["disclosure" if look == DisclosureLook::Plain, "keys-help" if look == DisclosureLook::Boxed,
                "more-options" if look == DisclosureLook::MoreOptions, "disclosure-compact" if look == DisclosureLook::Compact,
                "disclosure-row" if look == DisclosureLook::Row]
            open[open] ..rest {
            summary { (summary) }
            @if look == DisclosureLook::Boxed {
                div class="keys-help-body" { (children) }
            } @else if look == DisclosureLook::Compact {
                div class="disclosure-body" { (children) }
            } @else if look == DisclosureLook::Row {
                div class="disclosure-body" { (children) }
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
            Disclosure::new(
                "How it's checked",
                "Every block is checked against two nodes.",
            ),
        ),
        (
            "Boxed help",
            Disclosure::new("Where do I find these keys?", "Settings, then Keys.")
                .look(DisclosureLook::Boxed),
        ),
        (
            "More options",
            Disclosure::new(
                "More options",
                "Leave on mainnet unless this is a test wallet.",
            )
            .look(DisclosureLook::MoreOptions),
        ),
        (
            "Compact",
            Disclosure::new(
                Slot::many([
                    Slot::from(Tag::new("Scanning").tone(TagTone::Syncing)),
                    Slot::from(" 1 block behind"),
                ]),
                Facts::new(vec![
                    Fact {
                        term: "Progress".into(),
                        value: "1 block behind".into(),
                    },
                    Fact {
                        term: "Requests".into(),
                        value: "None yet".into(),
                    },
                ]),
            )
            .look(DisclosureLook::Compact)
            .open(true),
        ),
        (
            "A row",
            Disclosure::new(
                "Primary · node.example.com:18081",
                "The node's address, and whether it uses TLS.",
            )
            .look(DisclosureLook::Row),
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
            check(
                CheckMark::No,
                "No order from shop.example.com can still be paid",
                Some("2 can be paid until about 14:20"),
            ),
            check(
                CheckMark::Then,
                "Checkouts embedded on shop.example.com stop loading",
                None,
            ),
        ]),
    )]
}

/// One event of a [`Timeline`].
#[derive(Clone, Debug, UiRecord)]
pub struct Event {
    /// When ("12 Oct, 14:02").
    pub when: String,
    pub what: Slot,
    /// Under `what`, muted: more about it ("10 of 10 confirmations").
    pub detail: Option<Slot>,
    /// How it went, as a mark before it: `Notice` for something that
    /// happened, `Success`, `Warning` or `Error` for how it ended; none for
    /// a plain step. The words say it; the mark only backs them up.
    pub tone: Option<Tone>,
}

impl Event {
    /// An event with nothing more to say about it.
    pub fn new(when: impl Into<String>, what: impl Into<Slot>) -> Self {
        Self {
            when: when.into(),
            what: what.into(),
            detail: None,
            tone: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<Slot>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }
}

component! {
    /// What happened to something, in order, each with when, what, and
    /// optionally more about it and a mark for how it went.
    pub struct Timeline {
        events: Vec<Event>,
    }
    html {
        ol class="timeline" {
            @for event in events {
                li data-tone=[event.tone] {
                    time { (event.when) }
                    span {
                        (event.what)
                        @if let Some(detail) = event.detail { small class="timeline-detail" { (detail) } }
                    }
                }
            }
        }
    }
}

pub(super) fn timeline_examples() -> Vec<(&'static str, Timeline)> {
    vec![
        (
            "A wallet's history",
            Timeline::new(vec![
                Event::new("12 Oct, 14:02", "Added"),
                Event::new("12 Oct, 14:09", "Backup checked"),
                Event::new("13 Oct, 09:41", "First payment"),
            ]),
        ),
        (
            "An order's payments, newest first",
            Timeline::new(vec![
                Event::new("14:31", "Paid")
                    .detail("10 of 10 confirmations, at block 3,284,117")
                    .tone(Tone::Success),
                Event::new("14:09", "Payment seen: 0.214 XMR")
                    .detail("In the mempool")
                    .tone(Tone::Notice),
                Event::new("14:02", "Order created").detail("From the dashboard"),
            ]),
        ),
    ]
}

component! {
    /// Where a list or a record would be, when there's nothing in it yet:
    /// an icon, what's missing in bold, what happens next, and what to do
    /// about it. "No payments have been made yet", "No orders yet".
    pub struct EmptyState {
        /// What's missing, in a few words.
        title: Slot,
        /// Before the words: an inline SVG (`assets::icons::CLOCK`).
        icon: Option<Slot>,
        /// Under the title, muted: what happens next, or why it's empty.
        text: Option<Slot>,
        /// Under that: links or buttons, the one that helps most first.
        actions: Option<Slot>,
        rest: Attrs,
    }
    html {
        div class="mk-empty" ..rest {
            @if let Some(icon) = icon { span class="mk-empty-icon" aria-hidden="true" { (icon) } }
            div class="mk-empty-body" {
                p class="mk-empty-title" { (title) }
                @if let Some(text) = text { p class="mk-empty-text" { (text) } }
                @if let Some(actions) = actions { div class="mk-empty-actions" { (actions) } }
            }
        }
    }
}

pub(super) fn empty_state_examples() -> Vec<(&'static str, EmptyState)> {
    vec![
        (
            "Waiting, with what to do",
            EmptyState::new("No payments have been made yet")
                .icon("⏱")
                .text("Payments show here as soon as they reach the network. This order expires at 14:32.")
                .actions(Slot::many([
                    crate::LinkButton::new("Share payment link", "#component-empty-state")
                        .variant(crate::ButtonVariant::Primary)
                        .size(crate::ControlSize::Compact)
                        .into(),
                    crate::LinkButton::new("Open checkout", "#component-empty-state")
                        .size(crate::ControlSize::Compact)
                        .into(),
                ])),
        ),
        ("Just the words", EmptyState::new("No orders yet")),
    ]
}

component! {
    /// A small square that stands for someone or something beside its
    /// name: a letter, or a picture of theirs (a site's icon) once it has
    /// loaded. Build it with [`Avatar::of`], which takes the letter from
    /// the name. Only decoration: the name beside it says who it is. The
    /// picture needs JavaScript; without it, or when the picture won't
    /// load, the letter stays.
    pub struct Avatar {
        /// One letter of the name, shown until the picture loads.
        initial: String,
        /// A picture to show instead (`https://example.com/favicon.ico`).
        src: Option<String>,
        rest: Attrs,
    }
    behaviour "avatar.behaviour.ts" {
        uses(),
        /// The picture has loaded: it shows in place of the letter.
        loaded: bool,
        fn load,
    }
    html {
        span class="mk-avatar" data-loaded="" if b.loaded aria-hidden="true" ..rest {
            span class="mk-avatar-initial" { (initial) }
            @if enhanced {
                @if let Some(src) = src {
                    img class="mk-avatar-image" src=(src) alt="" referrerpolicy="no-referrer" decoding="async" on:load=load;
                }
            }
        }
    }
}

impl Avatar {
    /// The first letter or digit of the name's first word ("Kafe Kolo"
    /// is K), or of the name if the first word has none.
    pub fn of(name: &str) -> Self {
        let first = |text: &str| text.chars().find(|c| c.is_alphanumeric());
        let initial = name
            .split_whitespace()
            .next()
            .and_then(first)
            .or_else(|| first(name))
            .or_else(|| name.trim().chars().next())
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_default();
        Self::new(initial)
    }
}

/// A globe, as a site's icon for the examples.
const SITE_ICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2'%3E%3Ccircle cx='12' cy='12' r='9'/%3E%3Cpath d='M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18'/%3E%3C/svg%3E";

pub(super) fn avatar_examples() -> Vec<(&'static str, Avatar)> {
    vec![
        ("A store's letter", Avatar::of("Kafe Kolo")),
        (
            "A site's icon, once it loads",
            Avatar::of("Kafe Kolo").src(SITE_ICON),
        ),
        (
            "A picture that won't load: the letter stays",
            Avatar::of("corner shop").src("data:image/png;base64,bm90IGFuIGltYWdl"),
        ),
    ]
}

#[cfg(test)]
mod avatar_tests {
    use super::Avatar;

    #[test]
    fn the_letter_is_the_first_word_s_first_letter_or_digit() {
        let initial = |name: &str| Avatar::of(name).render().into_string();
        assert!(initial("Kafe Kolo").contains(">K<"));
        assert!(initial("corner shop").contains(">C<"));
        assert!(initial("(The) Shop").contains(">T<"));
        assert!(initial("— 7 Seas").contains(">7<"));
        assert!(initial("ésprit").contains(">É<"));
    }
}

/// How a table changes on a phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum TableLook {
    /// A table on a phone too, scrolling sideways.
    Plain,
    /// Each row a card: cells take their places by `CellPart`.
    Cards,
    /// Each row a box of its cells, each named by its `label`: rows of
    /// settings with inputs and buttons.
    Stack,
    /// Facts, a header cell then a cell per row: each label over its value.
    Facts,
}

component! {
    /// A table, in a box of its own that scrolls sideways when it's wider
    /// than the page. On a phone a column whose cells are `optional` goes,
    /// and `look` reshapes the rest. A page styling its columns names the
    /// table with `data-table`.
    pub struct Table {
        /// The rows: `Row`s (or a page's `tr`s).
        children: Slot,
        /// The header row.
        head: Option<Slot>,
        #[default(TableLook::Plain)]
        look: TableLook,
        /// For screen readers: what the table is.
        caption: Option<String>,
        /// Without its scroll box: a table already narrow enough.
        #[default(true)]
        scroll: bool,
        rest: Attrs,
    }
    html {
        @if scroll {
            div class="table-scroll" {
                table class=join["table-cards" if look == TableLook::Cards, "table-stack" if look == TableLook::Stack, "kv-table" if look == TableLook::Facts] ..rest {
                    @if let Some(caption) = caption { caption class="sr-only" { (caption) } }
                    @if let Some(head) = head { thead { (head) } }
                    tbody { (children) }
                }
            }
        } @else {
            table class=join["table-cards" if look == TableLook::Cards, "table-stack" if look == TableLook::Stack, "kv-table" if look == TableLook::Facts] ..rest {
                @if let Some(caption) = caption { caption class="sr-only" { (caption) } }
                @if let Some(head) = head { thead { (head) } }
                tbody { (children) }
            }
        }
    }
}

pub(super) fn table_examples() -> Vec<(&'static str, Table)> {
    let row = |order: &str, status: Tag, amount: &str| {
        Slot::from(Row::new(Slot::many([
            Cell::new(order).part(CellPart::Title).into(),
            Slot::from(Cell::new(status).part(CellPart::Status)),
            Slot::from(Cell::new(amount).part(CellPart::Amount)),
        ])))
    };
    let head = Row::new(Slot::many([
        Cell::new("Order").header(true).into(),
        Slot::from(Cell::new("Status").header(true)),
        Slot::from(Cell::new("Amount").header(true)),
    ]));
    vec![
        (
            "Orders, cards on a phone",
            Table::new(Slot::many([
                row(
                    "Coffee",
                    Tag::new("Paid").state(OrderState::Paid),
                    "0.02 XMR",
                ),
                row(
                    "Tea",
                    Tag::new("Waiting for payment")
                        .state(OrderState::Pending)
                        .short("Waiting"),
                    "0.01 XMR",
                ),
            ]))
            .head(head)
            .look(TableLook::Cards),
        ),
        (
            "Facts",
            Table::new(Slot::many([
                Row::new(Slot::many([
                    Cell::new("Network").header(true).into(),
                    Slot::from(Cell::new("Mainnet")),
                ]))
                .into(),
                Slot::from(Row::new(Slot::many([
                    Cell::new("Height").header(true).into(),
                    Slot::from(Cell::new("3,412,001")),
                ]))),
            ]))
            .look(TableLook::Facts),
        ),
    ]
}

component! {
    /// A table's row.
    pub struct Row {
        children: Slot,
        rest: Attrs,
    }
    html {
        tr ..rest { (children) }
    }
}

/// Where a cell goes when its row is a card on a phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum CellPart {
    /// Top left, bold.
    Title,
    /// Top right.
    Status,
    /// Under the title, bold.
    Amount,
    /// Under the status, muted.
    When,
    /// Across the bottom, muted.
    Meta,
    /// Under the title, at its size and weight: a detail that isn't a
    /// figure (an address, a reference).
    Detail,
}

component! {
    /// A table's cell; a header cell with `header`.
    pub struct Cell {
        children: Slot,
        #[default(false)]
        header: bool,
        part: Option<CellPart>,
        /// Its column's name, shown above it in a `Stack` table on a phone.
        label: Option<String>,
        /// Left out on a phone.
        #[default(false)]
        optional: bool,
        rest: Attrs,
    }
    html {
        @if header {
            th class=join[{ "card-" (part) } if part, "col-optional" if optional] data-label=[label] ..rest { (children) }
        } @else {
            td class=join[{ "card-" (part) } if part, "col-optional" if optional] data-label=[label] ..rest { (children) }
        }
    }
}

/// Where a `HelpTip`'s help opens from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum TipAnchor {
    /// Under the "?", its left edge at the "?"'s.
    Tip,
    /// Under the container the page positions (a title bar), from its left
    /// edge: a wide tip that isn't at the far left then stays on a phone's
    /// screen.
    Container,
}

component! {
    /// A "?" that opens help over the page: a `details`, so it opens
    /// without JavaScript. Once its island attaches, a click outside it or
    /// Escape closes it, Escape putting focus back on the "?".
    pub struct HelpTip {
        /// What the help is about ("What the page shows"): the "?"'s name.
        label: String,
        children: Slot,
        id: Option<String>,
        /// Room for two columns of help, as wide as the page allows.
        #[default(false)]
        wide: bool,
        /// Where the help opens from.
        #[default(TipAnchor::Tip)]
        anchor: TipAnchor,
        rest: Attrs,
    }
    behaviour "help-tip.behaviour.ts" {
        uses(),
        ref root,
    }
    html {
        details class="help-tip" id=[id] data-size="wide" if wide data-anchor="container" if anchor == TipAnchor::Container
            ..rest ref=root {
            summary aria-label=(label) title=(label) { "?" }
            div class="help-tip-body" { (children) }
        }
    }
}

pub(super) fn help_tip_examples() -> Vec<(&'static str, HelpTip)> {
    vec![(
        "Search syntax",
        HelpTip::new(
            "How to search",
            "Fields compare with =, !=, <, >; join them with and, or. Quote text: order.id = 'o_1'.",
        ),
    )]
}

component! {
    /// Links to the pages before and after this one of a long list, and
    /// where this one is. Real links: with `fixi_target`, fixi swaps the
    /// next page into that element and keeps the address in step.
    pub struct Pagination {
        /// What is paged ("Orders pages").
        label: String,
        previous: Option<String>,
        next: Option<String>,
        #[default(String::from("Previous"))]
        previous_label: String,
        #[default(String::from("Next"))]
        next_label: String,
        /// Where this page is ("Page 2 of 5").
        status: Option<String>,
        fixi_target: Option<String>,
        rest: Attrs,
    }
    html {
        nav class="mk-pagination" aria-label=(label) ..rest {
            @if let Some(previous) = previous {
                a href=(previous) rel="prev" fx-action=(previous) if fixi_target fx-target=[fixi_target]
                    fx-push-url="" if fixi_target { "← " (previous_label) }
            }
            @if let Some(status) = status { span class="mk-pagination-status" { (status) } }
            @if let Some(next) = next {
                a href=(next) rel="next" fx-action=(next) if fixi_target fx-target=[fixi_target]
                    fx-push-url="" if fixi_target { (next_label) " →" }
            }
        }
    }
}

pub(super) fn pagination_examples() -> Vec<(&'static str, Pagination)> {
    vec![
        (
            "Both ways, with where",
            Pagination::new("Invite pages")
                .previous("#component-pagination")
                .next("#component-pagination")
                .status("Page 2 of 5"),
        ),
        (
            "Newest first, at the start",
            Pagination::new("Orders pages")
                .next("#component-pagination")
                .next_label("Older"),
        ),
    ]
}

component! {
    /// One thing in a list of them, as a card: a title and a line under it,
    /// its status at the right, a figure (an amount) or a few facts, and a
    /// foot with a detail and what to do with it. An order on the POS's
    /// list; a store on the dashboard's.
    pub struct Card {
        title: Slot,
        subtitle: Option<Slot>,
        /// At the head's right: a `Tag`.
        status: Option<Slot>,
        /// The figure: an amount.
        amount: Option<Slot>,
        /// Under the head (and the figure): a few facts about it (`Facts`).
        body: Option<Slot>,
        /// Muted, at the foot's left.
        detail: Option<Slot>,
        /// At the foot's right: a link or a button.
        action: Option<Slot>,
        rest: Attrs,
    }
    html {
        article class="mk-card" ..rest {
            div class="mk-card-head" {
                div {
                    h3 class="mk-card-title" { (title) }
                    @if let Some(subtitle) = subtitle { p class="mk-card-sub" { (subtitle) } }
                }
                @if let Some(status) = status { (status) }
            }
            @if let Some(amount) = amount { p class="mk-card-amount" { (amount) } }
            @if let Some(body) = body { div class="mk-card-body" { (body) } }
            div class="mk-card-foot" {
                @if let Some(detail) = detail { small { (detail) } }
                @if let Some(action) = action { (action) }
            }
        }
    }
}

pub(super) fn card_examples() -> Vec<(&'static str, Card)> {
    let fact = |term: &str, value: &str| Fact {
        term: term.into(),
        value: value.into(),
    };
    vec![
        (
            "A store, with facts and actions",
            Card::new("Kafe Kolo")
                .subtitle("kafekolo.example")
                .status(Tag::new("Ready").tone(TagTone::Ok))
                .body(Facts::new(vec![
                    fact("Received", "3.412 XMR"),
                    fact("Orders in 24 h", "7"),
                    fact("Last order", "12 min ago"),
                ]))
                .action(crate::FormActions::new(Slot::many([
                    crate::LinkButton::new("Open POS", "#component-card")
                        .size(crate::ControlSize::Compact)
                        .into(),
                    crate::LinkButton::new("Settings", "#component-card")
                        .size(crate::ControlSize::Compact)
                        .into(),
                ]))),
        ),
        (
            "An order",
            Card::new("Table 4")
                .subtitle("Created 14:02")
                .status(
                    Tag::new("Waiting for payment")
                        .state(OrderState::Pending)
                        .short("Waiting"),
                )
                .amount("0.42 XMR")
                .detail("Expires in 52 min")
                .action(
                    crate::LinkButton::new("Open", "#component-card")
                        .variant(crate::ButtonVariant::Quiet)
                        .size(crate::ControlSize::Compact),
                ),
        ),
    ]
}
