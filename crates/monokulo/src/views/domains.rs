//! A store's verified domains (`crate::embed_domains`): the settings card
//! (`store_settings::verified_domains_card`) with its restriction banner and
//! one card per domain, and the three pages adding a domain walks through:
//! the domain, the DNS record to publish, and the check.
//!
//! Every action is a form, so all of it works without JavaScript. With it,
//! a domain card's Check again and the check page's result swap only
//! themselves (fixi), and a domain still waiting is checked again every 30
//! seconds while its page is open (`static/domains.js`).

use maud::{html, Markup, PreEscaped};
use ui::assets::icons;

use super::PageChrome;

/// How a domain stands, for its tag and what its card says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainStatus {
    Verified,
    /// Never verified: its record hasn't been found yet.
    Waiting,
    /// Was verified and its record has gone, but it still counts.
    Failing,
    /// Its record has been gone too long: it no longer counts.
    Lapsed,
}

/// One of a store's domains, as its card shows it.
pub struct DomainView {
    pub id: String,
    pub domain: String,
    pub status: DomainStatus,
    /// Muted, under its name: when it was checked, or how long its record
    /// has been missing.
    pub meta: String,
    /// The TXT record's host, relative to the domain: `_monokulo`.
    pub record_host: &'static str,
    pub record_value: String,
    /// Why it isn't verified (the last check's answer); `None` once it is.
    pub reason: Option<String>,
    /// A check asked for too soon after the last: the seconds left.
    pub wait: Option<i64>,
}

/// The store a domain page belongs to, for its crumbs and links.
pub struct StoreRef<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

impl StoreRef<'_> {
    fn settings(&self) -> String {
        format!("/dashboard/stores/{}/settings", self.id)
    }

    /// The settings tab the Verified domains card is on.
    fn connections(&self) -> String {
        format!("{}?tab=connections", self.settings())
    }

    fn domain(&self, domain_id: &str) -> String {
        format!("{}/domains/{domain_id}", self.settings())
    }

    fn crumbs(&self) -> Vec<ui::Crumb> {
        super::crumbs(&[
            (&format!("/dashboard/stores/{}", self.id), self.name),
            (&self.connections(), "Settings"),
        ])
    }
}

fn icon(svg: &'static str) -> Markup {
    html! { (PreEscaped(svg)) }
}

fn with_icon(svg: &'static str, words: &str) -> Markup {
    html! { (icon(svg)) (words) }
}

fn then_icon(words: &str, svg: &'static str) -> Markup {
    html! { (words) (icon(svg)) }
}

fn tag(status: DomainStatus) -> ui::Tag {
    match status {
        DomainStatus::Verified => ui::Tag::new("Verified")
            .tone(ui::TagTone::Ok)
            .icon(icon(icons::CHECK)),
        DomainStatus::Waiting => ui::Tag::new("Not verified").tone(ui::TagTone::Unknown),
        DomainStatus::Failing => ui::Tag::new("Failing").tone(ui::TagTone::Stalled),
        DomainStatus::Lapsed => ui::Tag::new("No longer verified").tone(ui::TagTone::Error),
    }
}

fn record(domain: &DomainView) -> Markup {
    html! {
        (ui::DnsRecord::new(
            "TXT",
            ui::CopyValue::new(domain.record_host).copy_label(format!("Copy the host for {}", domain.domain)),
            ui::CopyValue::new(domain.record_value.as_str()).copy_label(format!("Copy the value for {}", domain.domain)),
        ))
    }
}

/// "Checked moments ago": a check asked for before the last one is
/// [`crate::embed_domains::MIN_CHECK_GAP_SECS`] old.
fn wait_note(wait: Option<i64>) -> Markup {
    html! {
        @if let Some(wait) = wait {
            (ui::FieldNote::new(format!(
                "Checked moments ago: you can check again in {wait} {}.",
                if wait == 1 { "second" } else { "seconds" }
            ))
                .attr("data-check-wait", ""))
        }
    }
}

/// A domain's Check again: posted, it looks the record up now. With
/// JavaScript it swaps `target` (the card, or the check page's result), and
/// a domain still waiting asks again every 30 seconds (`data-recheck`).
fn check_form(
    store: &StoreRef,
    domain: &DomainView,
    target: &str,
    label: Markup,
    primary: bool,
) -> Markup {
    let action = format!("{}/check", store.domain(&domain.id));
    let button = ui::Button::new(label).kind(ui::ButtonType::Submit);
    let button = if primary {
        button.variant(ui::ButtonVariant::Primary)
    } else {
        button.size(ui::ControlSize::Compact)
    };
    html! {
        form method="post" action=(action) fx-action=(action) fx-method="POST" fx-target=(target) fx-swap="outerHTML"
            data-recheck[domain.status != DomainStatus::Verified] {
            @if target == "#domain-check" { input type="hidden" name="from" value="check"; }
            (button)
        }
    }
}

/// One domain's card and the question its Remove asks (with JavaScript).
pub fn card(store: &StoreRef, domain: &DomainView) -> Markup {
    let remove_form = format!("domain-delete-{}", domain.id);
    let remove_dialog = format!("domain-remove-{}", domain.id);
    html! {
        (card_only(store, domain))
        (ui::Dialog::new(remove_dialog.as_str(), format!("{remove_dialog}-title"), html! {
            (ui::DialogTitle::new(format!("{remove_dialog}-title"), html! { "Remove " (domain.domain) "?" }))
            p { "You would need a new DNS record to verify it again." }
            (ui::DialogActions::new(html! {
                (ui::Button::new("Keep it").attr("data-closes-dialog", ""))
                (ui::Button::new(with_icon(icons::TRASH, "Remove domain")).variant(ui::ButtonVariant::Danger).kind(ui::ButtonType::Submit)
                    .attr("form", remove_form.as_str()))
            }))
        }))
    }
}

/// One domain's card: its name, state and actions on one line (Check
/// again unless it's verified, and Remove), a line about it, and, until
/// it's verified, the record to publish and why the last check didn't find
/// it. A check swaps just this (its Remove question stays on the page).
pub fn card_only(store: &StoreRef, domain: &DomainView) -> Markup {
    let card_id = format!("domain-{}", domain.id);
    let remove_form = format!("domain-delete-{}", domain.id);
    let remove_dialog = format!("domain-remove-{}", domain.id);
    let remove = html! {
        form method="post" action=(format!("{}/delete", store.domain(&domain.id))) id=(remove_form) {
            (ui::Button::new(with_icon(icons::TRASH, "Remove"))
                .variant(ui::ButtonVariant::Danger)
                .size(ui::ControlSize::Compact)
                .kind(ui::ButtonType::Submit)
                .attr("data-opens-dialog", remove_dialog.as_str()))
        }
    };
    let actions = html! {
        @if domain.status != DomainStatus::Verified {
            (check_form(store, domain, &format!("#{card_id}"), with_icon(icons::REFRESH, "Check again"), false))
        }
        (remove)
    };
    let card = ui::ItemCard::new(domain.domain.as_str())
        .icon(icon(icons::GLOBE))
        .status(tag(domain.status))
        .actions(actions)
        .meta(domain.meta.as_str())
        .attr("id", card_id.as_str())
        .attr("data-domain", domain.domain.as_str());
    let card = match &domain.reason {
        Some(reason) if domain.status != DomainStatus::Verified => card.body(html! {
            p class="record-intro" { "Publish this record, then check again:" }
            (record(domain))
            (ui::FieldNote::new(html! { (icon(icons::WARNING)) " " (reason) }).tone(ui::NoteTone::Pending))
            (wait_note(domain.wait))
        }),
        _ => card,
    };
    html! { (card) }
}

/// Whether only the store's verified domains may show its checkout, at the
/// top of the card: on (green), off (yellow, a warning), or off with
/// nothing verified to allow (its button disabled). Allowing any website
/// asks first with JavaScript; without, it posts straight away.
fn restriction_banner(store: &StoreRef, restricted: bool, can_restrict: bool) -> Markup {
    banner(store, restricted, can_restrict, false)
}

/// The banner again, to replace the page's (`data-fx-oob`) after a check:
/// a domain just verified may let the restriction be turned on.
pub fn restriction_banner_oob(store: &StoreRef, restricted: bool, can_restrict: bool) -> Markup {
    banner(store, restricted, can_restrict, true)
}

fn banner(store: &StoreRef, restricted: bool, can_restrict: bool, oob: bool) -> Markup {
    let tip = |text: &str| {
        ui::HelpTip::new("What this means", text.to_string()).anchor(ui::TipAnchor::Container)
    };
    let banner = if restricted {
        ui::StatusBanner::new("Only your verified domains can show this checkout.", ui::Tone::Success)
            .icon(icon(icons::LOCK))
            .tip(tip("Browsers won't show this checkout on any other website, and orders started there are refused. Subdomains of a verified domain count. The till and payment links always work."))
            .action(
                ui::Button::new(with_icon(icons::LOCK_OPEN, "Allow any website"))
                    .variant(ui::ButtonVariant::Caution)
                    .size(ui::ControlSize::Compact)
                    .kind(ui::ButtonType::Submit)
                    .attr("data-opens-dialog", "allow-any"),
            )
    } else if can_restrict {
        ui::StatusBanner::new("Any website can show this checkout.", ui::Tone::Warning)
            .icon(icon(icons::LOCK_OPEN))
            .tip(tip("Allow only your verified domains to stop other websites showing this checkout and taking orders for your store."))
            .action(
                ui::Button::new(with_icon(icons::LOCK, "Only allow my domains"))
                    .variant(ui::ButtonVariant::Primary)
                    .size(ui::ControlSize::Compact)
                    .kind(ui::ButtonType::Submit),
            )
    } else {
        ui::StatusBanner::new("Any website can show this checkout.", ui::Tone::Warning)
            .icon(icon(icons::LOCK_OPEN))
            .tip(tip("A domain must be verified before you can allow only your own. Add one below and publish its DNS record."))
            .action(
                ui::Button::new(with_icon(icons::LOCK, "Only allow my domains"))
                    .size(ui::ControlSize::Compact)
                    .disabled(true),
            )
    };
    html! {
        form id="embed-restriction" method="post" action=(format!("{}/embed-restriction", store.settings())) data-fx-oob[oob] {
            input type="hidden" name="restricted" value=(if restricted { "off" } else { "on" });
            (banner)
        }
        @if restricted && !oob {
            (ui::Dialog::new("allow-any", "allow-any-title", html! {
                (ui::DialogTitle::new("allow-any-title", "Allow any website to show this checkout?"))
                (ui::Checks::new(vec![
                    ui::Check { mark: ui::CheckMark::Then, what: format!("Any website can show the checkout and start orders for {}", store.name).into(), fix: None },
                    ui::Check { mark: ui::CheckMark::Then, what: "Your verified domains stay listed; turn the restriction back on any time".into(), fix: None },
                ]))
                (ui::DialogActions::new(html! {
                    (ui::Button::new("Cancel").attr("data-closes-dialog", ""))
                    (ui::Button::new(with_icon(icons::LOCK_OPEN, "Allow any website"))
                        .variant(ui::ButtonVariant::Caution)
                        .kind(ui::ButtonType::Submit)
                        .attr("form", "embed-restriction"))
                }))
            }))
        }
    }
}

fn add_button(store: &StoreRef, primary: bool) -> Markup {
    let button = ui::LinkButton::new(
        with_icon(icons::PLUS, "Add a domain"),
        format!("{}/domains/add", store.settings()),
    );
    let button = if primary {
        button.variant(ui::ButtonVariant::Primary)
    } else {
        button
    };
    html! { (ui::FormActions::new(button)) }
}

/// The Verified domains card's body: the banner, then a card per domain and
/// Add a domain (the page's primary while nothing is verified), or, with
/// none yet, a box saying so.
pub fn domains_body(
    store: &StoreRef,
    domains: &[DomainView],
    restricted: bool,
    can_restrict: bool,
) -> Markup {
    html! {
        (ui::Hint::new("Prove you own the websites that show this store's checkout. A verified domain covers all of its subdomains."))
        (restriction_banner(store, restricted, can_restrict))
        @if domains.is_empty() {
            div class="box domains-empty" {
                p { strong { "No domains yet" } }
                (ui::Hint::new("Add the website your checkout runs on, like shop.example. It takes one DNS record and a few minutes."))
                (add_button(store, true))
            }
        } @else {
            div class="domain-list" {
                @for domain in domains { (card(store, domain)) }
            }
            (add_button(store, !can_restrict))
        }
    }
}

/// The steps of adding a domain, `at` the current one (0 to 2).
fn steps(at: usize, domain: Option<&str>) -> Markup {
    let step = |i: usize, label: &str, detail: &str| ui::WizardStep {
        label: label.into(),
        detail: detail.into(),
        done: usize::from(i < at),
        tasks: 1,
        state: match i {
            i if i < at => ui::WizardState::Done,
            i if i == at => ui::WizardState::Current,
            _ => ui::WizardState::Next,
        },
    };
    html! {
        (ui::WizardSteps::new("Adding a domain", vec![
            step(0, "Domain", domain.unwrap_or("the website")),
            step(1, "Add the record", "at your DNS provider"),
            step(2, "Check", "we ask its name servers"),
        ]))
    }
}

fn domain_page(
    chrome: &PageChrome,
    store: &StoreRef,
    title: &str,
    at: usize,
    domain: Option<&str>,
    body: Markup,
) -> Markup {
    let header = ui::PageHeader::new(title)
        .crumbs(store.crumbs())
        .progress(steps(at, domain));
    super::Page::new(format!("{title} - {} - Monokulo", store.name), header)
        .class("settings-page")
        .render(chrome, body)
}

/// `GET .../domains/add`, and the answer to an add that was refused: the
/// domain, what was typed and why it was refused.
pub fn add_page(chrome: &PageChrome, store: &StoreRef, typed: &str, error: Option<&str>) -> Markup {
    let body = html! {
        form method="post" action=(format!("{}/domains", store.settings())) {
            (super::settings::plain_card("domain", "Your website", html! {
                (ui::Setting::new("Domain", "new-domain", html! {
                    (ui::Input::new("domain")
                        .id("new-domain")
                        .value(typed)
                        .placeholder("shop.example")
                        .required(true)
                        .autocomplete("off")
                        .attr("spellcheck", "false")
                        .invalid(error.is_some())
                        .described_by(if error.is_some() { "new-domain-help new-domain-problem" } else { "new-domain-help" }))
                    @if let Some(error) = error {
                        (ui::FieldNote::new(error).tone(ui::NoteTone::Problem).id("new-domain-problem"))
                    }
                })
                .help("The website that shows your checkout, like shop.example. Its subdomains are covered too. Onion addresses can't be verified: they have no DNS.")
                .help_id("new-domain-help"))
            }))
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(ui::LinkButton::new("Cancel", format!("{}#card-verified-domains", store.connections()))),
                ui::Slot::from(ui::Button::new(then_icon("Next", icons::ARROW_RIGHT)).variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
            ])))
        }
    };
    domain_page(chrome, store, "Add a domain", 0, None, body)
}

/// Where the DNS settings are at the providers most merchants use.
const PROVIDERS: [(&str, &str); 4] = [
    ("Cloudflare", "your domain › DNS › Records › Add record"),
    ("Porkbun", "Domain Management › the domain › DNS"),
    (
        "Namecheap",
        "Domain List › Manage › Advanced DNS › Add new record",
    ),
    ("GoDaddy", "My Products › the domain › DNS › Add New Record"),
];

/// `GET .../domains/{id}/record`: the TXT record to publish, and the check.
pub fn record_page(chrome: &PageChrome, store: &StoreRef, domain: &DomainView) -> Markup {
    let full_name = format!("{}.{}", domain.record_host, domain.domain);
    let body = html! {
        p { "Sign in where you manage " strong { (domain.domain) } "'s DNS, add a new record, and fill it in like this:" }
        (record(domain))
        (ui::Hint::new(html! { "Some providers want the full name in Host: " code { (full_name) } ". TTL can stay at its default." }))
        (ui::Disclosure::new("Where to find DNS settings", html! {
            ul { @for (provider, path) in PROVIDERS { li { strong { (provider) } ": " (path) } } }
        }))
        (ui::Hint::new("You can leave and come back: the domain waits in Settings as Not verified, showing this record and Check again."))
        form method="post" action=(format!("{}/check", store.domain(&domain.id))) {
            input type="hidden" name="from" value="check";
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(ui::LinkButton::new(with_icon(icons::ARROW_LEFT, "Back"), format!("{}/domains/add", store.settings()))),
                ui::Slot::from(ui::Button::new(then_icon("I've added it, check now", icons::ARROW_RIGHT)).variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
            ])))
        }
    };
    domain_page(
        chrome,
        store,
        "Add the record",
        1,
        Some(&domain.domain),
        body,
    )
}

/// The check page's result (`#domain-check`): verified, or why not yet with
/// Check again, or not checked yet. While a check runs with JavaScript the
/// result gives way to "Checking…" (`aria-busy`, site.css).
pub fn check_result(store: &StoreRef, domain: &DomainView, checked: bool) -> Markup {
    let back = ui::Slot::from(ui::LinkButton::new(
        with_icon(icons::ARROW_LEFT, "Back to the record"),
        format!("{}/record", store.domain(&domain.id)),
    ));
    let full_name = format!("{}.{}", domain.record_host, domain.domain);
    html! {
        div id="domain-check" class="domain-check" {
            div class="box check-busy" role="status" {
                span class="check-spinner" aria-hidden="true" {}
                div {
                    strong { "Checking " (domain.domain) }
                    (ui::Hint::new(format!("Asking its name servers for the record at {full_name}…")))
                }
            }
            div class="check-result" {
                @if domain.status == DomainStatus::Verified {
                    (ui::Alert::new(
                        "It covers its subdomains too. Keep the record: Monokulo checks it every day.",
                        ui::Tone::Success,
                    )
                    .title(format!("{} is verified.", domain.domain))
                    .icon(icon(icons::CHECK))
                    .block(true))
                    (ui::FormActions::new(
                        ui::LinkButton::new(with_icon(icons::CHECK, "Done"), format!("{}#domain-{}", store.connections(), domain.id))
                            .variant(ui::ButtonVariant::Primary),
                    ))
                } @else if checked {
                    (ui::Alert::new(html! {
                        @if let Some(reason) = &domain.reason {
                            (reason) @if !reason.ends_with(['.', '!', '?']) { "." } " "
                        }
                        "DNS changes can take up to an hour to show."
                    }, ui::Tone::Error)
                    .title("Not found yet.")
                    .icon(icon(icons::WARNING))
                    .block(true))
                    (wait_note(domain.wait))
                    (ui::Hint::new("Checking again by itself every 30 seconds while this page is open.").attr("data-only", "script"))
                    (ui::Disclosure::new("Still not found?", html! {
                        ul {
                            li { "Most providers add the domain to the Host for you: type just " code { (domain.record_host) } ". Typing the full name there makes " code { (full_name) "." (domain.domain) } "." }
                            li { "Paste the value exactly, without quotes." }
                            li { "Check you edited the DNS of the provider the domain points to." }
                        }
                    }))
                    (ui::FormActions::new(ui::Slot::many([
                        back,
                        ui::Slot::from(check_form(store, domain, "#domain-check", with_icon(icons::REFRESH, "Check again"), true)),
                    ])))
                } @else {
                    (ui::Hint::new(format!("{} hasn't been checked yet. Publish its record, then check.", domain.domain)))
                    (ui::FormActions::new(ui::Slot::many([
                        back,
                        ui::Slot::from(check_form(store, domain, "#domain-check", with_icon(icons::REFRESH, "Check now"), true)),
                    ])))
                }
            }
        }
    }
}

/// `GET .../domains/{id}/check`: the last check's result, and a way to check
/// again.
pub fn check_page(
    chrome: &PageChrome,
    store: &StoreRef,
    domain: &DomainView,
    checked: bool,
) -> Markup {
    let body = html! {
        (check_result(store, domain, checked))
        (super::script("domains.js", super::Load::Defer))
    };
    domain_page(
        chrome,
        store,
        "Check the record",
        2,
        Some(&domain.domain),
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreRef<'static> {
        StoreRef {
            id: "conn_1",
            name: "Rachel's Handicrafts",
        }
    }

    fn domain(status: DomainStatus) -> DomainView {
        DomainView {
            id: "d1".to_string(),
            domain: "shop.example".to_string(),
            status,
            meta: "Added 10 min ago · not checked yet".to_string(),
            record_host: "_monokulo",
            record_value: "monokulo-verify=abc".to_string(),
            reason: (status != DomainStatus::Verified).then(|| "No TXT record yet.".to_string()),
            wait: None,
        }
    }

    /// The check's three answers: not checked yet (Check now), not found
    /// (why, tips, Check again, checking again by itself), verified (Done).
    #[test]
    fn the_check_says_not_checked_not_found_or_verified() {
        let html = check_result(&store(), &domain(DomainStatus::Waiting), false).into_string();
        assert!(
            html.contains("shop.example hasn't been checked yet."),
            "{html}"
        );
        assert!(html.contains("Check now"), "{html}");

        let html = check_result(&store(), &domain(DomainStatus::Waiting), true).into_string();
        assert!(html.contains("Not found yet."), "{html}");
        assert!(
            html.contains("No TXT record yet. DNS changes can take up to an hour to show."),
            "{html}"
        );
        assert!(html.contains("Still not found?"), "{html}");
        assert!(
            html.contains("<code>_monokulo.shop.example.shop.example</code>"),
            "{html}"
        );
        assert!(html.contains("data-recheck"), "{html}");
        assert!(
            html.contains(r#"<input type="hidden" name="from" value="check">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/dashboard/stores/conn_1/settings/domains/d1/record""#),
            "{html}"
        );
        // While a check runs, "Checking…" stands in for the answer.
        assert!(html.contains("Checking shop.example"), "{html}");

        let html = check_result(&store(), &domain(DomainStatus::Verified), true).into_string();
        assert!(html.contains("shop.example is verified."), "{html}");
        assert!(
            html.contains(
                r##"href="/dashboard/stores/conn_1/settings?tab=connections#domain-d1""##
            ),
            "{html}"
        );
        assert!(!html.contains("data-recheck"), "{html}");
    }

    #[test]
    fn a_refused_domain_keeps_what_was_typed_and_says_why_beside_it() {
        let html = add_page(
            &PageChrome::from_user(None, "/dashboard/stores/conn_1/settings"),
            &store(),
            "rachelshandicrafts",
            Some("Enter a full domain, like shop.example."),
        )
        .into_string();
        assert!(html.contains(r#"value="rachelshandicrafts""#), "{html}");
        assert!(html.contains(r#"aria-invalid="true""#), "{html}");
        assert!(html.contains(r#"id="new-domain-problem""#), "{html}");
        assert!(
            html.contains("Enter a full domain, like shop.example."),
            "{html}"
        );
        assert!(
            html.contains(r#"<li aria-current="step"><span>Domain"#),
            "{html}"
        );
    }
}
