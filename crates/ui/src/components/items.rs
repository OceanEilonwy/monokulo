//! Things listed one to a card, and the state a list is in: a store's
//! verified domains under the banner saying whether only they can show its
//! checkout.

use crate::{component, Attrs, Slot};

component! {
    /// A state the page is in, at the top of what it governs: an icon,
    /// the state in bold, a "?" (`HelpTip`) with what it means, and the
    /// one action that changes it. The tone's tint backs the words up; its
    /// edge stays thin.
    pub struct StatusBanner {
        /// The state, in a sentence ("Any website can show this checkout.").
        title: String,
        tone: crate::Tone,
        /// Before the title: an inline SVG (`assets::icons::LOCK`).
        icon: Option<Slot>,
        /// After the title: a `HelpTip`, anchored to the banner
        /// (`TipAnchor::Container`) so its help stays on a phone's screen.
        tip: Option<Slot>,
        /// At the right: the button that changes the state.
        action: Option<Slot>,
        rest: Attrs,
    }
    html {
        div class="mk-status-banner" data-tone=(tone) ..rest {
            @if let Some(icon) = icon { span class="mk-status-banner-icon" aria-hidden="true" { (icon) } }
            div class="mk-status-banner-title" {
                strong { (title) }
                @if let Some(tip) = tip { (tip) }
            }
            @if let Some(action) = action { div class="mk-status-banner-action" { (action) } }
        }
    }
}

pub(super) fn status_banner_examples() -> Vec<(&'static str, StatusBanner)> {
    let tip = |text: &str| {
        crate::HelpTip::new("What this means", text).anchor(crate::TipAnchor::Container)
    };
    vec![
        (
            "On",
            StatusBanner::new("Only your verified domains can show this checkout.", crate::Tone::Success)
                .icon("🔒")
                .tip(tip("Browsers won't show it on any other website, and orders started there are refused."))
                .action(
                    crate::Button::new("Allow any website")
                        .variant(crate::ButtonVariant::Caution)
                        .size(crate::ControlSize::Compact),
                ),
        ),
        (
            "Off",
            StatusBanner::new("Any website can show this checkout.", crate::Tone::Notice)
                .icon("🔓")
                .tip(tip("Allow only your verified domains to stop other websites showing it."))
                .action(
                    crate::Button::new("Only allow my domains")
                        .variant(crate::ButtonVariant::Primary)
                        .size(crate::ControlSize::Compact),
                ),
        ),
    ]
}

component! {
    /// One thing in a list, as a card: its icon, name and state on the
    /// first line with what to do at the right (the same place whatever
    /// its state), a line about it under them, and what it needs next
    /// below (a record to publish). Cards follow each other with a small
    /// gap.
    pub struct ItemCard {
        /// Its name: a heading (`h4`).
        title: Slot,
        /// Before the name: an inline SVG.
        icon: Option<Slot>,
        /// Right after the name: a `Tag`.
        status: Option<Slot>,
        /// At the head's right: its buttons.
        actions: Option<Slot>,
        /// Muted, under the head: when it was checked, what it's missing.
        meta: Option<Slot>,
        /// Under a line: what it needs next.
        body: Option<Slot>,
        rest: Attrs,
    }
    html {
        article class="mk-item-card" ..rest {
            div class="mk-item-card-head" {
                div class="mk-item-card-name" {
                    h4 class="mk-item-card-title" {
                        @if let Some(icon) = icon { span class="mk-item-card-icon" aria-hidden="true" { (icon) } }
                        span { (title) }
                    }
                    @if let Some(status) = status { (status) }
                }
                @if let Some(actions) = actions { div class="mk-item-card-actions" { (actions) } }
            }
            @if let Some(meta) = meta { p class="mk-item-card-meta" { (meta) } }
            @if let Some(body) = body { div class="mk-item-card-body" { (body) } }
        }
    }
}

pub(super) fn item_card_examples() -> Vec<(&'static str, ItemCard)> {
    let remove = || {
        Slot::from(
            crate::Button::new("Remove")
                .variant(crate::ButtonVariant::Danger)
                .size(crate::ControlSize::Compact),
        )
    };
    vec![
        (
            "Verified",
            ItemCard::new("rachelshandicrafts.com")
                .icon("🌐")
                .status(crate::Tag::new("Verified").tone(crate::TagTone::Ok))
                .actions(remove())
                .meta("Checked 3 h ago · re-checked every 6 h"),
        ),
        (
            "Waiting, with what it needs",
            ItemCard::new("shop.rachelshandicrafts.com")
                .icon("🌐")
                .status(crate::Tag::new("Not verified").tone(crate::TagTone::Unknown))
                .actions(Slot::many([
                    crate::Button::new("Check again")
                        .size(crate::ControlSize::Compact)
                        .into(),
                    remove(),
                ]))
                .meta("Added 10 min ago · the record isn't there yet")
                .body(DnsRecord::new(
                    "TXT",
                    crate::CopyValue::new("_monokulo").copy_label("Copy the host"),
                    crate::CopyValue::new("monokulo-verify=6c168c735d314966ac858a7a71de5eaf")
                        .copy_label("Copy the value"),
                )),
        ),
    ]
}

component! {
    /// A DNS record to publish, as a DNS provider's form asks for it: its
    /// type, host and value, the host and value each to copy
    /// (`CopyValue`). On a phone each is a line of its own, named.
    pub struct DnsRecord {
        /// `TXT`.
        record_type: String,
        /// The host, relative to the domain (`_monokulo`).
        host: Slot,
        value: Slot,
        rest: Attrs,
    }
    html {
        table class="table-stack mk-dns-record" ..rest {
            thead { tr { th scope="col" { "Type" } th scope="col" { "Host" } th scope="col" { "Value" } } }
            tbody {
                tr {
                    td data-label="Type" { code { (record_type) } }
                    td data-label="Host" { (host) }
                    td data-label="Value" { (value) }
                }
            }
        }
    }
}

pub(super) fn dns_record_examples() -> Vec<(&'static str, DnsRecord)> {
    vec![(
        "A TXT record",
        DnsRecord::new(
            "TXT",
            crate::CopyValue::new("_monokulo").copy_label("Copy the host"),
            crate::CopyValue::new("monokulo-verify=6c168c735d314966ac858a7a71de5eaf")
                .copy_label("Copy the value"),
        ),
    )]
}
