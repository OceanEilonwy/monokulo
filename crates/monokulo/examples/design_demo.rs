//! The new designs, built from the real ui components inside monokulo's
//! real page chrome, with made-up data: the last check before they replace
//! today's pages. Nothing here saves: a button that would change something
//! goes to the screen it would lead to.
//!
//! ```sh
//! cargo run -p monokulo --example design_demo
//! ```
//!
//! then open the address it prints. `?theme=light` or `?theme=dark` on any
//! screen picks the theme (the index links both).

use axum::{extract::Query, response::Html, routing::get, Router};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use monokulo::db::Theme;
use monokulo::http::{build_router, AppState};
use monokulo::views::{self, Page, PageChrome};
use serde::Deserialize;
use ui::assets::icons;
use ui::{
    Alert, Button, ButtonType, ButtonVariant, Choice, ControlSize, CopyBlock, CopyValue, DataCell,
    DataGrid, DataKind, Dialog, DialogActions, DialogPage, DialogTitle, Disclosure, DisclosureLook,
    DnsRecord, Fact, Facts, FieldNote, FormActions, HelpTip, Hint, IconButton, IconLink, Input,
    ItemCard, LinkButton, NoteTone, PageHeader, Row, Select, Setting, SettingLayout, Slot,
    StatusBanner, TableBrowser, Tag, TagTone, TextArea, TipAnchor, Tone, WizardState, WizardStep,
    WizardSteps,
};

const ADDRESS: &str = "127.0.0.1:4400";
const STORE: &str = "Rachel's Handicrafts";
const DOMAIN: &str = "rachelshandicrafts.com";
const TOKEN: &str = "monokulo-verify=6c168c735d314966ac858a7a71de5eaf";

#[derive(Deserialize, Default)]
struct Look {
    theme: Option<String>,
    /// A screen's variant: a wizard step's state, a field's error.
    state: Option<String>,
}

impl Look {
    fn theme(&self) -> Theme {
        match self.theme.as_deref() {
            Some("light") => Theme::Light,
            Some("dark") => Theme::Dark,
            _ => Theme::System,
        }
    }

    /// `path` with this look's theme kept.
    fn href(&self, path: &str) -> String {
        match &self.theme {
            Some(theme) => {
                let join = if path.contains('?') { '&' } else { '?' };
                format!("{path}{join}theme={theme}")
            }
            None => path.to_string(),
        }
    }

    /// The site's chrome, signed in as a merchant.
    fn chrome(&self, path: &str) -> PageChrome {
        let mut chrome = PageChrome::from_user(None, self.href(path));
        chrome.logged_in = true;
        chrome.email = "rachel@example.com".into();
        chrome.wallet_count = 2;
        chrome.theme = self.theme();
        chrome.browser_reports = false;
        chrome
    }
}

/// An icon from ui's set, before (or after) a button's words.
fn icon(svg: &'static str) -> Markup {
    html! { (PreEscaped(svg)) }
}

fn with_icon(svg: &'static str, words: &str) -> Markup {
    html! { (icon(svg)) (words) }
}

fn then_icon(words: &str, svg: &'static str) -> Markup {
    html! { (words) (icon(svg)) }
}

fn settings_crumbs(look: &Look) -> Vec<ui::Crumb> {
    vec![
        ui::Crumb::new(look.href("/"), STORE),
        ui::Crumb::new(look.href("/domains/on"), "Settings"),
    ]
}

// ---------------------------------------------------------------- index

async fn index(Query(look): Query<Look>) -> Html<String> {
    let screens: &[(&str, &[(&str, &str)])] = &[
        (
            "1. Setup done: the store summary",
            &[("/setup-done", "The summary as facts")],
        ),
        (
            "2. Store settings: Wallet",
            &[("/wallet", "The stacked wallet setting")],
        ),
        (
            "3a. Verified domains",
            &[
                ("/domains/on", "Restriction on, a domain in each state"),
                ("/domains/off", "Restriction off, a domain verified"),
                ("/domains/empty", "Nothing verified, no domains yet"),
                (
                    "/domains/allow-any",
                    "Allow any website? (the dialog's own page)",
                ),
            ],
        ),
        (
            "3b. Add a domain",
            &[
                ("/domains/add", "Step 1: the domain"),
                ("/domains/add?state=error", "Step 1: a domain refused"),
                ("/domains/record", "Step 2: the record"),
                ("/domains/check?state=checking", "Step 3: checking"),
                ("/domains/check?state=missing", "Step 3: not found yet"),
                ("/domains/check?state=verified", "Step 3: verified"),
            ],
        ),
        (
            "4. Admin Query",
            &[
                ("/query", "Results"),
                ("/query/wide", "Many columns"),
                ("/query/big", "Big values and blobs"),
                ("/query/expand", "A value expanded (the dialog's own page)"),
                ("/query/refused", "A refused query"),
                ("/query/empty", "Nothing run yet"),
            ],
        ),
    ];
    let body = html! {
        p { "The new designs, built from the real ui components in monokulo's page chrome. Nothing here saves." }
        p {
            "Theme: "
            a href="/" { "System" } " · " a href="/?theme=light" { "Light" } " · " a href="/?theme=dark" { "Dark" }
        }
        @for (section, links) in screens {
            h2 { (section) }
            ul {
                @for (path, label) in *links {
                    li { a href=(look.href(path)) { (label) } }
                }
            }
        }
    };
    page(
        &look,
        "/",
        "Design demo",
        PageHeader::new("Design demo"),
        body,
    )
}

fn page(look: &Look, path: &str, title: &str, header: PageHeader, body: Markup) -> Html<String> {
    Html(
        Page::new(format!("{title} - Design demo"), header)
            .class("settings-page")
            .head(html! { style { (PreEscaped(DEMO_STYLE)) } })
            .render(&look.chrome(path), body)
            .into_string(),
    )
}

/// Page layout the designs add (no colours: those are the ui's roles).
const DEMO_STYLE: &str = r#"
.query-band { display: grid; grid-template-columns: minmax(12rem, 16rem) minmax(0, 1fr); gap: var(--space-md); height: 17rem; }
.query-band > * { min-height: 0; }
.query-editor { display: flex; flex-direction: column; gap: var(--space-sm); margin: 0; min-width: 0; }
.query-editor textarea { flex: 1; min-height: 6rem; font-family: var(--font-mono); resize: vertical; }
.query-editor .form-actions { margin: 0; align-items: center; }
.query-label { margin: 0; font-size: var(--text-xs); font-weight: 800; letter-spacing: .06em; text-transform: uppercase; color: var(--muted); }
.query-empty { text-align: center; }
.query-empty .form-actions { justify-content: center; }
.record-table-intro { margin: 0 0 var(--space-xs); font-size: var(--text-sm); font-weight: 700; }
.check-panel { display: flex; align-items: center; gap: var(--space-md); }
.check-spinner { flex: none; width: 1.6rem; height: 1.6rem; border: 3px solid var(--line); border-top-color: var(--accent); border-radius: 50%; animation: demo-spin .9s linear infinite; }
@keyframes demo-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) { .check-spinner { animation: none; } }
@media (max-width: 640px) {
  .query-band { grid-template-columns: minmax(0, 1fr); height: auto; }
  .query-band > .mk-table-browser { order: 2; }
}
"#;

// ------------------------------------------------------ 1. setup done

async fn setup_done(Query(look): Query<Look>) -> Html<String> {
    let header = PageHeader::new(format!("{STORE} is set up"))
        .progress(views::setup::steps(views::setup::Step::Done, None));
    let fact = |term: &str, value: Markup| Fact {
        term: term.into(),
        value: value.into(),
    };
    let body = html! {
        (Facts::new(vec![
            fact("Status", html! { (Tag::new("Not taking payments yet").tone(TagTone::Unknown)) }),
            fact("Wallet", html! { a href=(look.href("/wallet")) { (STORE) } " " (views::network_badge("mainnet")) }),
            fact("Prices in", html! { "AUD" }),
        ]))
        (FormActions::new(
            LinkButton::new(then_icon("Change in the store's settings", icons::ARROW_RIGHT), look.href("/wallet"))
                .variant(ButtonVariant::Quiet)
                .size(ControlSize::Compact),
        ))
        ul class="todo" {
            li {
                p {
                    strong { "Add the checkout to your site" }
                    (Hint::new(format!("A script tag and a button on {DOMAIN}, or a call from your server. The guide shows both, and how to verify the domain.")).inline(true))
                }
                (LinkButton::new("Open the guide", "#").variant(ButtonVariant::Primary))
            }
            li {
                p {
                    strong { "Take payments in person" }
                    (Hint::new("The till runs in any browser: type an amount, show the QR code.").inline(true))
                }
                (LinkButton::new("Open the till", "#"))
            }
        }
    };
    page(&look, "/setup-done", "Setup done", header, body)
}

// ------------------------------------------------------- 2. the wallet

async fn wallet(Query(look): Query<Look>) -> Html<String> {
    let header = PageHeader::new("Settings").crumbs(vec![ui::Crumb::new(look.href("/"), STORE)]);
    let picker = Select::new(
        "wallet_id",
        vec![
            Choice::new("w_rh01", STORE)
                .detail("47UhAM…3E32nS…qbLgRU")
                .network("mainnet")
                .current(true)
                .note("this store only")
                .selected(true),
            Choice::new("w_ms02", "Market stall float")
                .detail("48abcD…7fGh01…9xYzQ2")
                .network("mainnet")
                .note("used by 1 other store"),
        ],
    )
    .id("store-wallet")
    .required(true)
    .attr("aria-describedby", "store-wallet-help");
    let body = views::settings::plain_card(
        "wallet",
        "Wallet",
        html! {
            form method="get" action=(look.href("/wallet")) fx-submit-on-change {
                (Setting::new("Wallet", "store-wallet", picker)
                    .help(html! {
                        "New payments go into this wallet, since " time datetime="2026-10-17T13:57" { "17 Oct, 13:57" }
                        ". Only Mainnet wallets can be picked, because the store takes Mainnet payments."
                    })
                    .help_id("store-wallet-help")
                    .layout(SettingLayout::Stacked))
                // With JavaScript, picking a wallet asks at once.
                (FormActions::new(Button::new("Change wallet").kind(ButtonType::Submit).attr("data-only", "no-script")))
            }
            (Disclosure::new(
                "Wallet history (1 wallet)",
                ui::Table::new(html! {
                    tr aria-current="true" { td { a href="#" { (STORE) } } td { "17 Oct, 13:57" } td { "now" } td class="num" { "0" } }
                })
                .head(html! { tr { th { "Wallet" } th { "From" } th { "Until" } th class="num" { "Orders" } } })
                .scroll(false),
            ))
        },
    );
    page(&look, "/wallet", "Wallet", header, body)
}

// ------------------------------------------------ 3a. verified domains

#[derive(Clone, Copy, PartialEq)]
enum Restriction {
    On,
    Off,
    NothingVerified,
}

fn restriction_banner(look: &Look, restriction: Restriction) -> Markup {
    let tip =
        |text: &str| HelpTip::new("What this means", text.to_string()).anchor(TipAnchor::Container);
    let banner = match restriction {
        Restriction::On => StatusBanner::new("Only your verified domains can show this checkout.", Tone::Success)
            .icon(icon(icons::LOCK))
            .tip(tip("Browsers won't show this checkout on any other website, and orders started there are refused. Subdomains of a verified domain count. The till and payment links always work."))
            .action(
                // With JavaScript it asks first; without, its page asks.
                LinkButton::new(with_icon(icons::LOCK_OPEN, "Allow any website"), look.href("/domains/allow-any"))
                    .variant(ButtonVariant::Caution)
                    .size(ControlSize::Compact)
                    .attr("data-opens-dialog", "allow-any"),
            ),
        Restriction::Off => StatusBanner::new("Any website can show this checkout.", Tone::Notice)
            .icon(icon(icons::LOCK_OPEN))
            .tip(tip("Allow only your verified domains to stop other websites showing this checkout and taking orders for your store."))
            .action(
                LinkButton::new(with_icon(icons::LOCK, "Only allow my domains"), look.href("/domains/on"))
                    .variant(ButtonVariant::Primary)
                    .size(ControlSize::Compact),
            ),
        Restriction::NothingVerified => StatusBanner::new("Any website can show this checkout.", Tone::Notice)
            .icon(icon(icons::LOCK_OPEN))
            .tip(tip("A domain must be verified before you can allow only your own. Add one below and publish its DNS record."))
            .action(
                Button::new(with_icon(icons::LOCK, "Only allow my domains"))
                    .variant(ButtonVariant::Primary)
                    .size(ControlSize::Compact)
                    .disabled(true),
            ),
    };
    html! { (banner) }
}

fn allow_any_question(look: &Look) -> Markup {
    html! {
        (ui::Checks::new(vec![
            ui::Check { mark: ui::CheckMark::Then, what: format!("Any website can show the checkout and start orders for {STORE}").into(), fix: None },
            ui::Check { mark: ui::CheckMark::Then, what: "Your verified domains stay listed; turn the restriction back on any time".into(), fix: None },
        ]))
        (DialogActions::new(Slot::many([
            Slot::from(Button::new("Cancel").attr("data-closes-dialog", "")),
            Slot::from(LinkButton::new(with_icon(icons::LOCK_OPEN, "Allow any website"), look.href("/domains/off")).variant(ButtonVariant::Caution)),
        ])))
    }
}

enum DomainState {
    Verified,
    Waiting,
    Failing,
    Lapsed,
}

fn domain_card(look: &Look, name: &str, state: DomainState) -> Markup {
    let remove = Slot::from(
        Button::new(with_icon(icons::TRASH, "Remove"))
            .variant(ButtonVariant::Danger)
            .size(ControlSize::Compact),
    );
    let check = Slot::from(
        LinkButton::new(
            with_icon(icons::REFRESH, "Check again"),
            look.href("/domains/check?state=missing"),
        )
        .size(ControlSize::Compact),
    );
    let (tag, meta, reason) = match state {
        DomainState::Verified => (
            Tag::new("Verified").tone(TagTone::Ok).icon(icon(icons::CHECK)),
            "Checked 3 h ago · re-checked every 6 h".to_string(),
            None,
        ),
        DomainState::Waiting => (
            Tag::new("Not verified").tone(TagTone::Unknown),
            "Added 10 min ago · checked 2 min ago, the record isn't there yet".to_string(),
            Some(format!("The name servers have no TXT record at _monokulo.{name} yet.")),
        ),
        DomainState::Failing => (
            Tag::new("Failing").tone(TagTone::Stalled),
            "Record missing for 2 h · still counts as verified for 22 h".to_string(),
            Some("The record was there, and went 2 h ago. Publish it again within 22 h to keep the domain.".to_string()),
        ),
        DomainState::Lapsed => (
            Tag::new("No longer verified").tone(TagTone::Error),
            "Record missing for 3 days · checkouts on it are refused".to_string(),
            Some("The record has been missing for 3 days, so the domain no longer counts.".to_string()),
        ),
    };
    let card = ItemCard::new(name.to_string())
        .icon(icon(icons::GLOBE))
        .status(tag)
        .meta(meta);
    let card = match reason {
        None => card.actions(remove),
        Some(reason) => card.actions(Slot::many([check, remove])).body(html! {
            p class="record-table-intro" { "Publish this record, then check again:" }
            (record(name))
            (FieldNote::new(html! { (icon(icons::WARNING)) " " (reason) }).tone(NoteTone::Pending))
        }),
    };
    html! { (card) }
}

fn record(domain: &str) -> Markup {
    html! {
        (DnsRecord::new(
            "TXT",
            CopyValue::new("_monokulo").copy_label(format!("Copy the host for {domain}")),
            CopyValue::new(TOKEN).copy_label(format!("Copy the value for {domain}")),
        ))
    }
}

fn add_domain(primary: bool, look: &Look) -> Markup {
    let button = LinkButton::new(
        with_icon(icons::PLUS, "Add a domain"),
        look.href("/domains/add"),
    );
    let button = if primary {
        button.variant(ButtonVariant::Primary)
    } else {
        button
    };
    html! { (FormActions::new(button)) }
}

fn domains_card(look: &Look, restriction: Restriction, cards: Markup, empty: bool) -> Markup {
    views::settings::plain_card(
        "verified-domains",
        "Verified domains",
        html! {
            (Hint::new("Prove you own the websites that show this store's checkout. A verified domain covers all of its subdomains."))
            (restriction_banner(look, restriction))
            @if empty {
                div class="box query-empty" {
                    p { strong { "No domains yet" } }
                    (Hint::new(format!("Add the website your checkout runs on, like {DOMAIN}. It takes one DNS record and a few minutes.")))
                    (add_domain(true, look))
                }
            } @else {
                (cards)
                (add_domain(restriction == Restriction::NothingVerified, look))
            }
        },
    )
}

async fn domains_on(Query(look): Query<Look>) -> Html<String> {
    let cards = html! {
        (domain_card(&look, DOMAIN, DomainState::Verified))
        (domain_card(&look, "shop.rachelshandicrafts.com", DomainState::Waiting))
        (domain_card(&look, "old-shop.example", DomainState::Failing))
        (domain_card(&look, "handicrafts.example", DomainState::Lapsed))
    };
    let body = html! {
        (domains_card(&look, Restriction::On, cards, false))
        (Dialog::new("allow-any", "allow-any-title", html! {
            (DialogTitle::new("allow-any-title", "Allow any website to show this checkout?"))
            (allow_any_question(&look))
        }))
    };
    page(
        &look,
        "/domains/on",
        "Verified domains",
        settings_header(&look),
        body,
    )
}

async fn domains_off(Query(look): Query<Look>) -> Html<String> {
    let cards = html! {
        (domain_card(&look, DOMAIN, DomainState::Verified))
        (domain_card(&look, "shop.rachelshandicrafts.com", DomainState::Waiting))
    };
    let body = domains_card(&look, Restriction::Off, cards, false);
    page(
        &look,
        "/domains/off",
        "Verified domains",
        settings_header(&look),
        body,
    )
}

async fn domains_empty(Query(look): Query<Look>) -> Html<String> {
    let body = domains_card(&look, Restriction::NothingVerified, html! {}, true);
    page(
        &look,
        "/domains/empty",
        "Verified domains",
        settings_header(&look),
        body,
    )
}

async fn allow_any_page(Query(look): Query<Look>) -> Html<String> {
    let header =
        PageHeader::new("Allow any website to show this checkout?").crumbs(settings_crumbs(&look));
    let body = html! { (DialogPage::new(allow_any_question(&look))) };
    page(
        &look,
        "/domains/allow-any",
        "Allow any website?",
        header,
        body,
    )
}

fn settings_header(look: &Look) -> PageHeader {
    PageHeader::new("Settings").crumbs(vec![ui::Crumb::new(look.href("/"), STORE)])
}

// ------------------------------------------------- 3b. add a domain

fn add_steps(at: usize, domain: Option<&str>) -> Markup {
    let state = |step| match step {
        s if s < at => WizardState::Done,
        s if s == at => WizardState::Current,
        _ => WizardState::Next,
    };
    let step = |i: usize, label: &str, detail: &str| WizardStep {
        label: label.into(),
        detail: detail.into(),
        done: usize::from(i < at),
        tasks: 1,
        state: state(i),
    };
    html! {
        (WizardSteps::new("Adding a domain", vec![
            step(0, "Domain", domain.unwrap_or("the website")),
            step(1, "Add the record", "at your DNS provider"),
            step(2, "Check", "we ask its name servers"),
        ]))
    }
}

async fn add(Query(look): Query<Look>) -> Html<String> {
    let error = look.state.as_deref() == Some("error");
    let header = PageHeader::new("Add a domain")
        .crumbs(settings_crumbs(&look))
        .progress(add_steps(0, None));
    let body = html! {
        form method="get" action="/domains/record" {
            @if let Some(theme) = &look.theme { input type="hidden" name="theme" value=(theme); }
            (views::settings::plain_card("domain", "Domain", html! {
                (Setting::new("Domain", "new-domain", html! {
                    (Input::new("domain").id("new-domain").value(if error { "rachelshandicrafts" } else { "" })
                        .placeholder(DOMAIN).invalid(error)
                        .described_by(if error { "new-domain-help new-domain-problem" } else { "new-domain-help" }))
                    @if error {
                        (FieldNote::new("\"rachelshandicrafts\" has no dot. Type the whole domain, like rachelshandicrafts.com.")
                            .tone(NoteTone::Problem).id("new-domain-problem"))
                    }
                })
                .help("The website that shows your checkout. Its subdomains are covered too.")
                .help_id("new-domain-help"))
            }))
            (FormActions::new(Slot::many([
                Slot::from(LinkButton::new("Cancel", look.href("/domains/on"))),
                Slot::from(Button::new(then_icon("Next", icons::ARROW_RIGHT)).variant(ButtonVariant::Primary).kind(ButtonType::Submit)),
            ])))
        }
    };
    page(&look, "/domains/add", "Add a domain", header, body)
}

async fn record_step(Query(look): Query<Look>) -> Html<String> {
    let header = PageHeader::new("Add the record")
        .crumbs(settings_crumbs(&look))
        .progress(add_steps(1, Some(DOMAIN)));
    let providers = [
        ("Cloudflare", "your domain › DNS › Records › Add record"),
        ("Porkbun", "Domain Management › the domain › DNS"),
        (
            "Namecheap",
            "Domain List › Manage › Advanced DNS › Add new record",
        ),
        ("GoDaddy", "My Products › the domain › DNS › Add New Record"),
    ];
    let body = html! {
        p { "Sign in where you manage " strong { (DOMAIN) } "'s DNS, add a new record, and fill it in like this:" }
        (record(DOMAIN))
        (Hint::new(html! { "Some providers want the full name in Host: " code { "_monokulo." (DOMAIN) } ". TTL can stay at its default." }))
        (Disclosure::new("Where to find DNS settings", html! {
            ul { @for (provider, path) in providers { li { strong { (provider) } ": " (path) } } }
        }))
        (Hint::new("You can leave and come back: the domain waits in Settings as Not verified, showing this record and Check again."))
        (FormActions::new(Slot::many([
            Slot::from(LinkButton::new(with_icon(icons::ARROW_LEFT, "Back"), look.href("/domains/add"))),
            Slot::from(LinkButton::new(then_icon("I've added it, check now", icons::ARROW_RIGHT), look.href("/domains/check?state=checking")).variant(ButtonVariant::Primary)),
        ])))
    };
    page(&look, "/domains/record", "Add the record", header, body)
}

async fn check_step(Query(look): Query<Look>) -> Html<String> {
    let header = PageHeader::new("Check the record")
        .crumbs(settings_crumbs(&look))
        .progress(add_steps(2, Some(DOMAIN)));
    let back = || {
        Slot::from(LinkButton::new(
            with_icon(icons::ARROW_LEFT, "Back to the record"),
            look.href("/domains/record"),
        ))
    };
    let body = match look.state.as_deref() {
        Some("verified") => html! {
            (Alert::new(format!("It covers its subdomains too, like shop.{DOMAIN}. Keep the record: Monokulo checks it every 6 hours."), Tone::Success)
                .title(format!("{DOMAIN} is verified."))
                .icon(icon(icons::CHECK))
                .block(true))
            (FormActions::new(LinkButton::new(with_icon(icons::CHECK, "Done"), look.href("/domains/on")).variant(ButtonVariant::Primary)))
        },
        Some("missing") => html! {
            (Alert::new(html! {
                (DOMAIN) "'s name servers have no TXT record at " code { "_monokulo." (DOMAIN) } " holding " code { "monokulo-verify=6c16…5eaf" }
                ". DNS changes can take up to an hour to show."
            }, Tone::Error).title("Not found yet.").icon(icon(icons::WARNING)).block(true))
            (Hint::new("Checking again by itself every 30 seconds while this page is open. Last checked at 14:02.").attr("data-only", "script"))
            (Disclosure::new("Still not found?", html! {
                ul {
                    li { "Most providers add the domain to the Host for you: type just " code { "_monokulo" } ". Typing the full name there makes " code { "_monokulo." (DOMAIN) "." (DOMAIN) } "." }
                    li { "Paste the value exactly, without quotes." }
                    li { "Check you edited the DNS of the provider the domain points to." }
                }
            }))
            (FormActions::new(Slot::many([
                back(),
                Slot::from(LinkButton::new(with_icon(icons::REFRESH, "Check again"), look.href("/domains/check?state=verified")).variant(ButtonVariant::Primary)),
            ])))
        },
        _ => html! {
            div class="box check-panel" role="status" {
                span class="check-spinner" aria-hidden="true" {}
                div {
                    strong { "Checking " (DOMAIN) }
                    (Hint::new(format!("Asking its name servers for the record at _monokulo.{DOMAIN}…")))
                }
            }
            (FormActions::new(Slot::many([
                back(),
                Slot::from(Button::new("Checking…").variant(ButtonVariant::Primary).disabled(true).busy(true)),
            ])))
        },
    };
    page(&look, "/domains/check", "Check the record", header, body)
}

// ------------------------------------------------------- 4. the query

#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Results,
    Wide,
    Big,
    Refused,
    Empty,
}

fn query_header(look: &Look) -> PageHeader {
    PageHeader::new("Query")
        .crumbs(vec![ui::Crumb::new(look.href("/"), "Home")])
        .actions(html! {
            form class="engine-network" method="get" action="/query" {
                label for="query-db" class="visually-hidden" { "Database" }
                (Select::new("db", vec![Choice::new("monokulo", "Monokulo").selected(true), Choice::new("engine", "Engine")])
                    .id("query-db")
                    .compact(true))
                (Button::new("Go").kind(ButtonType::Submit).attr("data-only", "no-script"))
            }
            (LinkButton::new("Export CSV", "#").size(ControlSize::Compact))
            (IconLink::new("Run again", look.href("/query"), icon(icons::REFRESH)))
        })
}

fn table_browser() -> Markup {
    let column = |name: &str, value: Markup| Fact {
        term: name.into(),
        value: value.into(),
    };
    let text = |t: &str| html! { (t) };
    let tables: Vec<(&str, &str, Vec<Fact>)> = vec![
        (
            "orders",
            "1,204",
            vec![
                column("id", text("TEXT")),
                column("status", text("TEXT")),
                column("amount_xmr", text("INTEGER")),
            ],
        ),
        (
            "sessions",
            "4",
            vec![
                column("user_id", text("TEXT")),
                column(
                    "token",
                    html! { (Tag::new("hidden").tone(TagTone::Muted).icon(icon(icons::LOCK))) },
                ),
            ],
        ),
        (
            "store_domains",
            "4",
            vec![
                column("domain", text("TEXT")),
                column(
                    "token",
                    html! { (Tag::new("hidden").tone(TagTone::Muted).icon(icon(icons::LOCK))) },
                ),
            ],
        ),
        (
            "stores",
            "3",
            vec![column("id", text("TEXT")), column("name", text("TEXT"))],
        ),
        (
            "users",
            "12",
            vec![
                column("id", text("TEXT")),
                column("email", text("TEXT")),
                column(
                    "password_hash",
                    html! { (Tag::new("hidden").tone(TagTone::Muted).icon(icon(icons::LOCK))) },
                ),
            ],
        ),
        (
            "wallets",
            "5",
            vec![column("id", text("TEXT")), column("name", text("TEXT"))],
        ),
        (
            "webhook_deliveries",
            "38",
            vec![column("id", text("TEXT")), column("payload", text("TEXT"))],
        ),
    ];
    html! {
        (TableBrowser::new("Tables", Slot::many(tables.into_iter().map(|(name, rows, columns)| {
            Slot::from(Disclosure::new(
                html! { (name) (Tag::new(rows).tone(TagTone::Muted)) },
                Facts::new(columns),
            ).look(DisclosureLook::Compact))
        })))
        .filter(Input::new("table-filter").label("Filter tables").placeholder("Filter tables…").size(ControlSize::Compact)))
    }
}

fn editor(sql: &str) -> Markup {
    html! {
        form class="box query-editor" method="get" action="/query" {
            label class="query-label" for="query-sql" { "SQL" }
            (TextArea::new("sql").id("query-sql").value(sql).rows(5usize).described_by("query-sql-help"))
            (FormActions::new(Slot::many([
                Slot::from(Hint::new("One statement. Rows past 1,000 are cut.").inline(true).attr("id", "query-sql-help")),
                Slot::from(Disclosure::new("Examples", html! {
                    ul { li { a href="#" { "Latest orders" } } li { a href="#" { "Stores and their wallets" } } li { a href="#" { "Unpaid orders per store" } } }
                })),
                Slot::from(html! { span class="spacer" {} }),
                Slot::from(Button::new("Run").variant(ButtonVariant::Primary).kind(ButtonType::Submit)),
            ])))
        }
    }
}

fn head_row(names: &[&str]) -> Slot {
    Slot::from(Row::new(Slot::many(
        names
            .iter()
            .map(|n| Slot::from(DataCell::new(n.to_string()).header(true))),
    )))
}

fn cell(value: &str) -> Slot {
    match value {
        "NULL" => Slot::from(DataCell::new("NULL").kind(DataKind::Null)),
        "hidden" => Slot::from(DataCell::new("hidden").kind(DataKind::Hidden)),
        v if v.parse::<f64>().is_ok() => {
            Slot::from(DataCell::new(v.to_string()).kind(DataKind::Number))
        }
        v => Slot::from(DataCell::new(v.to_string())),
    }
}

fn rows(rows: &[&[&str]]) -> Slot {
    Slot::many(
        rows.iter()
            .map(|row| Slot::from(Row::new(Slot::many(row.iter().map(|v| cell(v)))))),
    )
}

fn results(shown: Shown) -> Markup {
    let hidden = || {
        Slot::from(
            Tag::new("1 column hidden")
                .tone(TagTone::Muted)
                .icon(icon(icons::LOCK)),
        )
    };
    match shown {
        Shown::Results => html! {
            (DataGrid::new(
                head_row(&["id", "email", "created_at", "password_hash"]),
                rows(&[
                    &["us_1", "rachel@example.com", "1760700000", "hidden"],
                    &["us_2", "ann@example.org", "NULL", "hidden"],
                    &["us_3", "sam@example.net", "1760610000", "hidden"],
                    &["us_4", "kim@example.com", "1760500000", "hidden"],
                ]),
                "Query results",
            )
            .stats(Slot::many([Slot::from("4 rows · 3 ms · monokulo"), hidden()])))
        },
        Shown::Wide => {
            let names = [
                "id",
                "store_id",
                "status",
                "amount_xmr",
                "amount_fiat",
                "currency",
                "rate",
                "address",
                "subaddress",
                "payment_id",
                "tx_hash",
                "confirmations",
                "required",
                "network",
                "wallet_id",
                "source",
                "external_ref",
                "created_at",
                "paid_at",
                "expires_at",
                "description",
                "customer_note",
                "refund_address",
            ];
            let row = |id: &'static str,
                       store: &'static str,
                       status: &'static str,
                       xmr: &'static str,
                       fiat: &'static str|
             -> Vec<&'static str> {
                vec![
                    id,
                    store,
                    status,
                    xmr,
                    fiat,
                    "AUD",
                    "290.97",
                    "86Lp…Hq3w",
                    "14",
                    "NULL",
                    "a91f…07c2",
                    "12",
                    "10",
                    "mainnet",
                    "w_rh01",
                    "woocommerce",
                    "#1042",
                    "2026-10-17 05:58",
                    "2026-10-17 06:01",
                    "2026-10-17 06:58",
                    "Hand-thrown mug",
                    "NULL",
                    "NULL",
                ]
            };
            let data = [
                row("order_2282ae", "st_8f3a", "confirmed", "0.0842", "24.50"),
                row("order_77c0d1", "st_8f3a", "paid", "0.0310", "9.00"),
                row("order_09ab44", "st_2c91", "pending", "0.1200", "35.00"),
                row("order_5d6e2f", "st_77d0", "expired", "0.0100", "2.90"),
            ];
            let data: Vec<&[&str]> = data.iter().map(|r| r.as_slice()).collect();
            html! {
                (DataGrid::new(head_row(&names), rows(&data), "Query results")
                    .stats("4 rows · 23 columns · 9 ms · engine")
                    .scroll_hint(html! { (icon(icons::ARROW_LEFT)) " Scroll sideways for more columns " (icon(icons::ARROW_RIGHT)) }))
            }
        }
        Shown::Big => {
            let long = |id: &str, json: &str, size: &str, cut: Option<&str>| {
                let cell = DataCell::new(json.to_string())
                    .size(size.to_string())
                    .actions(Slot::many([
                        Slot::from(
                            IconLink::new(
                                format!("Expand payload of {id}"),
                                "/query/expand",
                                icon(icons::EXPAND),
                            )
                            .attr("data-opens-dialog", "expand-value"),
                        ),
                        Slot::from(IconButton::new(
                            format!("Copy payload of {id}"),
                            icon(icons::COPY),
                        )),
                    ]));
                Slot::from(match cut {
                    Some(cut) => cell.cut(cut.to_string()),
                    None => cell,
                })
            };
            let blob = |hex: &str| {
                Slot::from(
                    DataCell::new(format!("‹blob 64 B› {hex}…"))
                        .kind(DataKind::Blob)
                        .actions(IconButton::new("Copy the bytes as hex", icon(icons::COPY))),
                )
            };
            let row = |id: &str, event: &str, payload: Slot, signature: Slot, status: &str| {
                Slot::from(Row::new(Slot::many([
                    cell(id),
                    cell(event),
                    payload,
                    signature,
                    cell(status),
                ])))
            };
            html! {
                (DataGrid::new(
                    head_row(&["id", "event", "payload", "signature", "status"]),
                    Slot::many([
                        row("wd_1a2b", "order.paid", long("wd_1a2b", r#"{"event":"order.paid","order":{"id":"order_2282ae00c55f42fb8e295d4b45c8bfc3","store":"Rachel's Handicrafts"}}"#, "1.8 KB", None), blob("9f3a1c07e2d4b8"), "delivered"),
                        row("wd_3c4d", "order.confirmed", long("wd_3c4d", r#"{"event":"order.confirmed","order":{"id":"order_77c0d1","store":"Rachel's Handicrafts"}}"#, "1.9 KB", None), blob("41be0d77a01c5e"), "delivered"),
                        row("wd_5e6f", "store.export", long("wd_5e6f", r#"{"event":"store.export","orders":[{"id":"order_09ab44"},{"id":"order_5d6e2f"}]}"#, "2.4 MB", Some("cut at 64 KiB of 2.4 MB")), cell("NULL"), "failed"),
                    ]),
                    "Query results",
                )
                .stats(Slot::many([Slot::from("3 rows · 6 ms · monokulo"), hidden()])))
                (Dialog::new("expand-value", "expand-value-title", expanded()).size(ui::DialogSize::Wide))
            }
        }
        Shown::Refused => html! {
            (Alert::new(html! { code { "UPDATE" } " changes data, so it is refused. Use a " code { "SELECT" } "." }, Tone::Error)
                .title("Not run: this page only reads.")
                .icon(icon(icons::WARNING)))
        },
        Shown::Empty => html! {
            div class="box query-empty" {
                p { strong { "Pick a table on the left, or write a SELECT." } }
                (Hint::new("Nothing here can change data. Results stop at 1,000 rows and 5 seconds. Every query is written to the Logs page."))
                (FormActions::new(Slot::many(["Latest orders", "Stores and their wallets", "Unpaid orders per store"].map(|label| {
                    Slot::from(LinkButton::new(label, "/query").size(ControlSize::Compact))
                }))))
            }
        },
    }
}

const EXPANDED_JSON: &str = r##"{
  "event": "order.paid",
  "order": {
    "id": "order_2282ae00c55f42fb8e295d4b45c8bfc3",
    "store": "Rachel's Handicrafts",
    "status": "paid",
    "amount": { "xmr": "0.084200000000", "fiat": "24.50", "currency": "AUD", "rate": "290.97" },
    "address": "86LpA1yW…Hq3wZ",
    "confirmations": 0,
    "required_confirmations": 10,
    "external_ref": "#1042",
    "created_at": "2026-10-17T05:58:12Z",
    "paid_at": "2026-10-17T06:01:40Z"
  },
  "delivered_at": "2026-10-17T06:01:41Z"
}"##;

fn expanded() -> Markup {
    html! {
        (DialogTitle::new("expand-value-title", "payload").aside(Hint::new("wd_1a2b · 1.8 KB · JSON").inline(true)))
        (CopyBlock::new(EXPANDED_JSON).copy_label("Copy the payload"))
        (DialogActions::new(Button::new("Close").attr("data-closes-dialog", "")))
    }
}

async fn query_page(look: Look, path: &str, shown: Shown) -> Html<String> {
    let sql = match shown {
        Shown::Results => {
            "SELECT id, email, created_at, password_hash\nFROM users\nORDER BY created_at DESC"
        }
        Shown::Wide => "SELECT * FROM orders",
        Shown::Big => "SELECT * FROM webhook_deliveries",
        Shown::Refused => "UPDATE stores SET name = 'Rachel''s' WHERE id = 'st_8f3a'",
        Shown::Empty => "",
    };
    let body = html! {
        div class="query-band" {
            (table_browser())
            (editor(sql))
        }
        (results(shown))
    };
    page(&look, path, "Query", query_header(&look), body)
}

async fn query(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query", Shown::Results).await
}
async fn query_wide(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/wide", Shown::Wide).await
}
async fn query_big(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/big", Shown::Big).await
}
async fn query_refused(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/refused", Shown::Refused).await
}
async fn query_empty(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/empty", Shown::Empty).await
}

async fn query_expand(Query(look): Query<Look>) -> Html<String> {
    let header = PageHeader::new("payload")
        .crumbs(vec![ui::Crumb::new(look.href("/query/big"), "Query")])
        .aside(Hint::new("wd_1a2b · 1.8 KB · JSON").inline(true));
    let body = html! { (DialogPage::new(CopyBlock::new(EXPANDED_JSON).copy_label("Copy the payload")).size(ui::DialogSize::Wide)) };
    page(&look, "/query/expand", "payload", header, body)
}

#[tokio::main]
async fn main() {
    let demo = Router::new()
        .route("/", get(index))
        .route("/setup-done", get(setup_done))
        .route("/wallet", get(wallet))
        .route("/domains/on", get(domains_on))
        .route("/domains/off", get(domains_off))
        .route("/domains/empty", get(domains_empty))
        .route("/domains/allow-any", get(allow_any_page))
        .route("/domains/add", get(add))
        .route("/domains/record", get(record_step))
        .route("/domains/check", get(check_step))
        .route("/query", get(query))
        .route("/query/wide", get(query_wide))
        .route("/query/big", get(query_big))
        .route("/query/expand", get(query_expand))
        .route("/query/refused", get(query_refused))
        .route("/query/empty", get(query_empty));
    // The real router serves the pages' scripts, styles and fonts; the
    // demo's routes stand in for its pages.
    let assets = build_router(AppState::for_tests()).layer(axum::middleware::from_fn(only_static));
    let app = demo.fallback_service(assets);
    let address = std::env::var("DESIGN_DEMO_ADDRESS").unwrap_or_else(|_| ADDRESS.to_string());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .expect("bind the demo's address");
    println!("Design demo: http://{}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("serve the demo");
}

/// Only the real router's assets and the status dot's poll: none of its
/// pages answer, so a link the demo doesn't have says so.
async fn only_static(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path();
    if path.starts_with("/static/") || path == "/status/summary" {
        next.run(request).await
    } else {
        (
            axum::http::StatusCode::NOT_FOUND,
            Html(html! { (DOCTYPE) p { "Not part of the demo." } }.into_string()),
        )
            .into_response()
    }
}

use axum::response::IntoResponse;
