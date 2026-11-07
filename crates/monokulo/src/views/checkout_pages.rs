//! Writing a custom checkout page: the wizard's pages, the preview, and the
//! made-up order a preview shows, which nobody can pay.

use checkout_template::{format_size, Place, Problem, BUDGET};
use engine_protocol::order_status::OrderStatus;
use maud::{html, Markup};

use super::checkout::CheckoutViewModel;
use super::checkout_layouts::Layout;
use super::PageChrome;

/// A state a preview can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewState {
    /// Its name in a link: `?state=paid`.
    pub slug: &'static str,
    pub name: &'static str,
    status: OrderStatus,
    reversed: bool,
}

/// Every state worth seeing, the ones a customer is most likely to first.
pub const PREVIEW_STATES: [PreviewState; 9] = [
    PreviewState {
        slug: "pending",
        name: "Pending",
        status: OrderStatus::Pending,
        reversed: false,
    },
    PreviewState {
        slug: "partial",
        name: "Partly paid",
        status: OrderStatus::Partial,
        reversed: false,
    },
    PreviewState {
        slug: "unconfirmed",
        name: "Seen",
        status: OrderStatus::Unconfirmed,
        reversed: false,
    },
    PreviewState {
        slug: "confirming",
        name: "Confirming",
        status: OrderStatus::Confirming,
        reversed: false,
    },
    PreviewState {
        slug: "paid",
        name: "Paid",
        status: OrderStatus::Paid,
        reversed: false,
    },
    PreviewState {
        slug: "overpaid",
        name: "Overpaid",
        status: OrderStatus::Overpaid,
        reversed: false,
    },
    PreviewState {
        slug: "expired",
        name: "Expired",
        status: OrderStatus::Expired,
        reversed: false,
    },
    PreviewState {
        slug: "cancelled",
        name: "Cancelled",
        status: OrderStatus::Cancelled,
        reversed: false,
    },
    PreviewState {
        slug: "reversed",
        name: "Payment reversed",
        status: OrderStatus::Unconfirmed,
        reversed: true,
    },
];

/// What a preview's code holds: says what it is to a wallet that reads it.
const SAMPLE_CODE: &str = "SAMPLE: NOT A REAL PAYMENT";
/// The length and look of an address, which no wallet will accept.
const SAMPLE_ADDRESS: &str = "SAMPLEADDRESSxNOTxAxREALxPAYMENTxDOxNOTxSENDxMONEYxHERExTHISxISxONLYxAxPREVIEWxOFxAxPAGExSAMPLE";

/// A made-up order in `state`, for a preview.
pub fn sample_order(
    state: &PreviewState,
    store_name: &str,
    style_hash: Option<String>,
    now: i64,
) -> Result<CheckoutViewModel, qrcode::types::QrError> {
    let awaiting = matches!(state.status, OrderStatus::Pending | OrderStatus::Partial);
    let received = match state.status {
        OrderStatus::Pending | OrderStatus::Expired | OrderStatus::Cancelled => "0.000000000000",
        OrderStatus::Partial => "0.282100000000",
        OrderStatus::Overpaid => "0.582100000000",
        _ => "0.482100000000",
    };
    let confirmations = match state.status {
        OrderStatus::Confirming => 2,
        OrderStatus::Paid | OrderStatus::Overpaid => 10,
        _ => 0,
    };
    Ok(CheckoutViewModel {
        order_id: crate::db::OrderId::new("order_sample".to_string()),
        store_name: store_name.to_string(),
        description: Some("Order #1042".to_string()),
        merchant_reference: None,
        merchant_site: None,
        merchant_icon: false,
        status_label: super::order_state(state.status).0.to_string(),
        status: state.status,
        address: SAMPLE_ADDRESS.to_string(),
        qr_code_svg: crate::qr::encode(SAMPLE_CODE)?,
        payment_uri: "#".to_string(),
        xmr_amount: "0.482100000000".to_string(),
        amount_due_xmr: if state.status == OrderStatus::Partial {
            "0.200000000000".to_string()
        } else {
            "0.482100000000".to_string()
        },
        previous_due_xmr: (state.status == OrderStatus::Partial)
            .then(|| "0.482100000000".to_string()),
        amount_received_xmr: received.to_string(),
        amount: "84.00".to_string(),
        currency: "USD".to_string(),
        confirmations,
        confirmations_required: 10,
        progress_percent: (confirmations * 10) as u8,
        // As a real one, so a preview shows the words a customer first sees.
        is_terminal: matches!(state.status, OrderStatus::Paid | OrderStatus::Overpaid),
        double_spend_detected_at: state.reversed.then_some(now - 300),
        clock: super::time::Clock::utc(now),
        local_times: false,
        expires_in_display: if awaiting { "14m 32s" } else { "" }.to_string(),
        created_at: now - 300,
        expires_at: now + 872,
        refund_address: None,
        refund_address_locked: false,
        refund_address_error: None,
        layout: Layout::Detailed,
        style_hash,
        page_token: None,
        refund_enabled: true,
        query_suffix: String::new(),
        auto_refresh: false,
        toggled_refresh_suffix: String::new(),
        payment_error: None,
        payments: if received == "0.000000000000" {
            Vec::new()
        } else {
            vec![super::checkout::CheckoutPaymentViewModel {
                txid: "0".repeat(64),
                amount_xmr: received.to_string(),
                confirmations,
                is_zero_conf: confirmations == 0,
            }]
        },
    })
}

/// What a store's wizard pages need of it.
pub struct WizardStore<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

impl WizardStore<'_> {
    fn page(&self) -> String {
        crate::base::url(format!("/stores/{}/settings/checkout-page", self.id))
    }

    fn crumbs(&self) -> Vec<ui::Crumb> {
        super::crumbs(&[
            (&format!("/stores/{}", self.id), self.name),
            (
                &format!("/stores/{}/settings?tab=checkout", self.id),
                "Checkout",
            ),
        ])
    }
}

fn steps(at: usize) -> Markup {
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
        (ui::WizardSteps::new("Making a custom page", vec![
            step(0, "Template", "and CSS"),
            step(1, "Preview", "every state"),
            step(2, "Approve", "on your domain"),
        ]))
    }
}

fn wizard_page(chrome: &PageChrome, store: &WizardStore, at: usize, body: Markup) -> Markup {
    let header = ui::PageHeader::new("Custom page")
        .crumbs(store.crumbs())
        .progress(steps(at));
    super::Page::new(format!("Custom page - {} - Monokulo", store.name), header)
        .class("settings-page")
        .render(chrome, body)
}

/// Where a problem is, as a merchant reads it.
fn place(problem: &Problem) -> String {
    match problem.place {
        Place::Template(line) => format!("Template, line {line}"),
        Place::Css(line) => format!("CSS, line {line}"),
        Place::EveryState => "Every state".to_string(),
    }
}

/// Step 1: the template and CSS, with what's wrong with them.
pub fn edit_page(
    chrome: &PageChrome,
    store: &WizardStore,
    template: &str,
    css: &str,
    problems: &[Problem],
    size: Option<u32>,
) -> Markup {
    let body = html! {
        @if !problems.is_empty() {
            (super::settings::plain_card(
                "template-problems",
                &match problems.len() {
                    1 => "1 thing to fix before previewing".to_string(),
                    n => format!("{n} things to fix before previewing"),
                },
                html! {
                    ul class="template-problems" role="alert" {
                        @for problem in problems {
                            li {
                                strong { (place(problem)) } ": " (problem.message)
                                @if let Some(hint) = &problem.hint { " " (ui::Hint::new(hint.as_str()).inline(true)) }
                            }
                        }
                    }
                },
            ))
        }
        form method="get" action=(store.page()) class="template-start" {
            (super::settings::plain_card("template-start", "Start from", html! {
                (ui::Hint::new("Replaces what's below. Our layouts are templates too."))
                (ui::Select::new(
                    "start",
                    Layout::ALL.into_iter()
                        .map(|layout| ui::Choice::new(layout.name(), layout.label()))
                        .collect::<Vec<_>>(),
                ).id("template-start-layout").attr("aria-label", "A layout to start from"))
                (ui::FormActions::new(ui::Button::new("Use this layout").kind(ui::ButtonType::Submit)))
            }))
        }
        form method="post" action=(store.page()) class="template-editor" {
            (super::settings::plain_card("template", "Template and CSS", html! {
                a href=(super::docs_url("custom-pages")) { "Custom page guide, with examples" }
                (super::settings::Field::new("Template", "template").render(html! {
                    (ui::TextArea::new("template")
                        .id("template")
                        .value(template)
                        .rows(18usize)
                        .attr("spellcheck", "false")
                        .invalid(problems.iter().any(|p| !matches!(p.place, Place::Css(_)))))
                }))
                (super::settings::Field::new("CSS (optional)", "css").render(html! {
                    (ui::TextArea::new("css")
                        .id("css")
                        .value(css)
                        .rows(10usize)
                        .attr("spellcheck", "false")
                        .invalid(problems.iter().any(|p| matches!(p.place, Place::Css(_)))))
                }))
                @if let Some(size) = size {
                    p class="template-size" {
                        label for="template-size" { "Template size " }
                        progress id="template-size" max=(BUDGET) value=(size.min(BUDGET)) { (size) }
                        " " (format_size(size)) " of " (format_size(BUDGET))
                    }
                }
            }))
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(ui::Button::new("Check and preview").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
                ui::Slot::from(ui::LinkButton::new("Cancel", crate::base::url(format!("/stores/{}/settings?tab=checkout", store.id)))),
            ])))
            (ui::Hint::new("Saved as a draft. Customers see nothing yet."))
        }
    };
    wizard_page(chrome, store, 0, body)
}

/// The layout whose default frame a custom page gets: `monokulo-client.js`
/// frames a page it isn't asked a `view` for at Detailed's size.
const CUSTOM_PAGE_LAYOUT: Layout = Layout::Detailed;

/// "420 by 880", for a hint.
fn format_embed_size(layout: Layout) -> String {
    let (width, height) = layout.embed_size();
    format!("{width} by {height}")
}

/// A frame showing one state of a draft: sandboxed, so nothing in it runs, and
/// with its own address, so it's never taken for this page.
fn preview_frame(preview_token: &str, state: &PreviewState) -> Markup {
    frame_at(
        state,
        &format!(
            "/checkout-preview/{preview_token}/frame?state={}",
            state.slug
        ),
        CUSTOM_PAGE_LAYOUT.embed_size(),
    )
}

/// A sandboxed frame at the size the embed library would give the page, so
/// the preview shows what a customer gets, scrolling and all.
fn frame_at(state: &PreviewState, path: &str, (width, height): (u32, u32)) -> Markup {
    html! {
        figure class="template-preview" {
            figcaption { (state.name) }
            iframe
                sandbox=""
                loading="lazy"
                width=(width)
                height=(height)
                title=(format!("Your page when: {}", state.name))
                src=(crate::base::url(path)) {}
        }
    }
}

/// `/stores/{id}/settings/checkout-layout`: every state of a stock layout, in
/// the store's saved colours.
pub fn layout_preview(chrome: &PageChrome, store: &WizardStore, layout: Layout) -> Markup {
    let page = format!("/stores/{}/settings/checkout-layout", store.id);
    let header = ui::PageHeader::new(format!("{} layout", layout.label())).crumbs(store.crumbs());
    let body = html! {
        nav class="template-preview-states" aria-label="Layouts" {
            @for other in Layout::ALL {
                a href=(crate::base::url(format!("{page}?layout={}", other.name())))
                    aria-current=[(other == layout).then_some("page")] { (other.label()) }
            }
        }
        (super::settings::plain_card("layout-preview", "Every state", html! {
            (ui::Hint::new(format!("In your saved colours, at the embed's default size for this layout ({}), on a made-up order that can't be paid.", format_embed_size(layout))))
            div class="template-previews" {
                @for state in &PREVIEW_STATES {
                    (frame_at(state, &format!("{page}/frame?layout={}&state={}", layout.name(), state.slug), layout.embed_size()))
                }
            }
        }))
    };
    super::Page::new(
        format!("{} layout - {} - Monokulo", layout.label(), store.name),
        header,
    )
    .class("settings-page")
    .render(chrome, body)
}

/// Step 2: every state of the draft, with a link to show someone.
pub fn preview_step(
    chrome: &PageChrome,
    store: &WizardStore,
    preview_token: &str,
    link: &str,
    size: u32,
    next: Markup,
) -> Markup {
    let body = html! {
        (super::settings::plain_card("template-preview", "Every state", html! {
            (ui::Hint::new(format!("Template size {} of {}. At the embed's default size ({}), on a made-up order that can't be paid.", format_size(size), format_size(BUDGET), format_embed_size(CUSTOM_PAGE_LAYOUT))))
            div class="template-previews" {
                @for state in &PREVIEW_STATES { (preview_frame(preview_token, state)) }
            }
        }))
        (super::settings::plain_card("template-link", "Show someone", html! {
            (ui::Hint::new("Anyone with this link can see the preview until you change the draft or it expires."))
            (ui::CopyValue::new(link).copy_label("Copy the preview link"))
        }))
        (ui::FormActions::new(ui::Slot::many([
            ui::Slot::from(next),
            ui::Slot::from(ui::LinkButton::new("Edit", store.page())),
        ])))
    };
    wizard_page(chrome, store, 1, body)
}

/// A preview link's page: says plainly what it is, around a sandboxed frame.
pub fn shared_preview(chrome: &PageChrome, preview_token: &str, state: &PreviewState) -> Markup {
    let body = html! {
        (ui::Alert::new("Preview only. This is not a real payment: don't send anything.", ui::Tone::Warning))
        nav class="template-preview-states" aria-label="Order states" {
            @for other in &PREVIEW_STATES {
                a href=(crate::base::url(format!("/checkout-preview/{preview_token}?state={}", other.slug)))
                    aria-current=[(other.slug == state.slug).then_some("page")] { (other.name) }
            }
        }
        (preview_frame(preview_token, state))
    };
    super::layout_bare(chrome, "Preview only", body)
}

/// Where a domain or file a page's proof goes, from its name.
fn proof_details(domain: &str, token: &str) -> Markup {
    use crate::checkout_pages::{proof_token, TEMPLATE};
    let value = TEMPLATE.value(&proof_token(token, domain));
    if domain_proof::is_onion(domain) {
        html! {
            dl class="template-proof" {
                dt { "Address" }
                dd { (ui::CopyValue::new(TEMPLATE.well_known_url(domain)).copy_label("Copy the file's address")) }
                dt { "Contents" }
                dd { (ui::CopyValue::new(value).copy_label("Copy the file's contents")) }
            }
        }
    } else {
        html! {
            dl class="template-proof" {
                dt { "Type" } dd { "TXT" }
                dt { "Host" }
                dd { (ui::CopyValue::new(TEMPLATE.label).copy_label("Copy the host")) }
                dt { "Value" }
                dd { (ui::CopyValue::new(value).copy_label("Copy the value")) }
            }
        }
    }
}

/// What the last check of a domain's proof found, when it was asked for.
pub enum ApproveResult {
    NotChecked,
    Found,
    NotFound(String),
    CantCheck(String),
}

/// Step 3: the record or file each domain approves this version with, and whether
/// it's there.
pub fn approve_step(
    chrome: &PageChrome,
    store: &WizardStore,
    token: &str,
    results: &[(String, ApproveResult)],
) -> Markup {
    let check = format!("{}/approve", store.page());
    let all_found = results
        .iter()
        .all(|(_, result)| matches!(result, ApproveResult::Found));
    let check_label = match results {
        [(domain, _)] if domain_proof::is_onion(domain) => "Check for the file",
        [_] => "Check for the record",
        _ => "Check both",
    };
    let body = html! {
        @if results.len() > 1 {
            p { "This store has a web domain and an onion address, so both must approve this version before you can use it. Each version has its own." }
        }
        @for (index, (domain, result)) in results.iter().enumerate() {
            @let onion = domain_proof::is_onion(domain);
            (super::settings::plain_card(&format!("template-approve-{index}"), if results.len() > 1 { domain.as_str() } else { "Approve this version" }, html! {
                @if onion {
                    p { "An onion address has no DNS, so your shop serves a small file. We fetch it over Tor, never over the web." @if results.len() == 1 { " Each version has its own." } }
                    (ui::Hint::new("This is weaker than DNS: whoever can change your shop's files can pass this check."))
                } @else {
                    p { "Add this record to " strong { (domain) } ". It proves this version is yours, so a stolen key can't put new words in front of your customers." @if results.len() == 1 { " Each version has its own." } }
                }
                (proof_details(domain, token))
                @match result {
                    ApproveResult::NotChecked => {}
                    ApproveResult::Found => {
                        (ui::Alert::new(if onion { "Found the file." } else { "Found the record." }, ui::Tone::Success))
                    }
                    ApproveResult::NotFound(reason) => {
                        (ui::Alert::new(format!("Not found. {reason}"), ui::Tone::Warning))
                    }
                    ApproveResult::CantCheck(reason) => {
                        (ui::Alert::new(format!("Can't check right now. {reason}"), ui::Tone::Warning))
                    }
                }
            }))
        }
        @if all_found {
            form method="post" action=(format!("{}/publish", store.page())) {
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(ui::Button::new("Use this template").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
                    ui::Slot::from(ui::LinkButton::new("Preview every state", format!("{}/preview", store.page()))),
                ])))
            }
        } @else {
            form method="post" action=(check) {
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(ui::Button::new(check_label).variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
                    ui::Slot::from(ui::LinkButton::new("Back to the preview", format!("{}/preview", store.page()))),
                ])))
            }
        }
    };
    wizard_page(chrome, store, 2, body)
}

/// What the Custom page card on the Checkout tab shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PageCard {
    /// A custom page needs a domain that's been verified.
    #[default]
    NeedsDomain,
    /// Nothing written yet.
    None,
    /// Waiting for its records or files.
    Draft {
        domains: Vec<String>,
        days_left: i64,
    },
    /// In use.
    Working {
        domains: Vec<String>,
        /// Domains that count and haven't approved it: they show the layout so far.
        waiting: Vec<String>,
        checked: String,
        /// Why the last look got no answer: it keeps showing, which is worth saying.
        trouble: Option<String>,
        draft: bool,
    },
    /// In use, with a proof missing: still showing.
    Failing {
        domains: Vec<String>,
        /// Which one, in the words of the last look.
        reason: Option<String>,
        token: String,
        days_left: i64,
        draft: bool,
    },
    /// Published, but customers don't see it.
    NotShowing { reason: Option<String>, draft: bool },
    /// This Monokulo's operator took it down.
    TurnedOff { reason: Option<String> },
}

/// The name of the card on the Checkout tab.
pub const CARD_NAME: &str = "custom-page";

impl PageCard {
    /// The card's state, in a word or two, beside its title.
    pub fn tag(&self) -> ui::Tag {
        let (words, tone) = match self {
            PageCard::NeedsDomain => ("Not available yet", ui::TagTone::Muted),
            PageCard::None => ("Not set up", ui::TagTone::Muted),
            PageCard::Draft { .. } => ("Waiting for approval", ui::TagTone::Warning),
            PageCard::Working { .. } => ("Working", ui::TagTone::Ok),
            PageCard::Failing { .. } => ("Failing", ui::TagTone::Warning),
            PageCard::NotShowing { .. } => ("Not showing", ui::TagTone::Error),
            PageCard::TurnedOff { .. } => ("Turned off by admin", ui::TagTone::Error),
        };
        ui::Tag::new(words).tone(tone)
    }
}

/// A button of the card. The card sits among the settings form's, and a form can't
/// hold another, so each button names its form, which is outside
/// ([`page_card_forms`]).
fn form_button(label: &str, action: &str) -> ui::Button {
    ui::Button::new(label)
        .kind(ui::ButtonType::Submit)
        .attr("form", format!("custom-page-{action}"))
}

/// The forms the card's buttons send: once on the page, outside the settings form.
pub fn page_card_forms(store_id: &str) -> Markup {
    let page = crate::base::url(format!("/stores/{store_id}/settings/checkout-page"));
    html! {
        @for action in ["recheck", "stop", "discard"] {
            form id=(format!("custom-page-{action}")) method="post" action=(format!("{page}/{action}")) hidden {}
        }
    }
}

/// The card on the Checkout tab.
pub fn page_card(store_id: &str, fallback: Layout, card: &PageCard) -> Markup {
    let page = crate::base::url(format!("/stores/{store_id}/settings/checkout-page"));
    let edit = || ui::LinkButton::new("Edit", page.clone());
    let recheck = || form_button("Check again", "recheck");
    let stop = || form_button("Stop using", "stop");
    let continue_draft = |draft: bool| {
        html! {
            @if draft { (ui::LinkButton::new("Continue your draft", format!("{page}/approve"))) }
        }
    };
    let body = match card {
        PageCard::NeedsDomain => html! {
            p { "A custom page needs a web domain or onion address you've proved is yours. Verify one, then come back here." }
            (ui::LinkButton::new("Verify a domain", crate::base::url(format!("/stores/{store_id}/settings?tab=domains"))))
        },
        PageCard::None => html! {
            p { "Write your own checkout with a template and optional CSS. Your domains approve every version you publish, so a leaked key can't put new words in front of customers." }
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(ui::LinkButton::new("Create a custom page", page.clone()).variant(ui::ButtonVariant::Primary)),
                ui::Slot::from(html! { a href=(super::docs_url("custom-pages")) { "Custom page guide" } }),
            ])))
        },
        PageCard::Draft { domains, days_left } => html! {
            p { "Customers don't see this draft until it's approved on " strong { (domains.join(" and ")) } ". It's kept for " (days_left) " more days." }
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(ui::LinkButton::new("Continue", format!("{page}/approve")).variant(ui::ButtonVariant::Primary)),
                ui::Slot::from(edit()),
                ui::Slot::from(form_button("Discard draft", "discard")),
            ])))
        },
        PageCard::Working {
            domains,
            waiting,
            checked,
            trouble,
            draft,
        } => html! {
            p { "Showing to customers where your site frames it. Checked " (checked) " on " strong { (domains.join(" and ")) } "." }
            @if let Some(trouble) = trouble {
                (ui::Hint::new(trouble.as_str()))
            }
            @if !waiting.is_empty() {
                (ui::Hint::new(format!("{} shows {} until it has this version's proof. We'll look soon.", waiting.join(" and "), fallback.label())))
            }
            (ui::Hint::new(format!("Direct visits to a payment link show {}.", fallback.label())))
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(continue_draft(*draft)),
                ui::Slot::from(edit()),
                ui::Slot::from(stop()),
            ])))
        },
        PageCard::Failing {
            domains,
            reason,
            token,
            days_left,
            draft,
        } => html! {
            p {
                "We can't find every proof of this version. Customers still see your page for "
                (days_left) " more days, then " (fallback.label()) " on all your domains. Add what's missing, then check again."
            }
            @if let Some(reason) = reason {
                (ui::Hint::new(reason.as_str()))
            }
            @for domain in domains {
                p { strong { (domain) } }
                (proof_details(domain, token))
            }
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(recheck()),
                ui::Slot::from(continue_draft(*draft)),
                ui::Slot::from(edit()),
                ui::Slot::from(stop()),
            ])))
        },
        PageCard::NotShowing { reason, draft } => html! {
            p { "Customers now see " strong { (fallback.label()) } " instead of your page. Open orders keep working. Your page comes back by itself once a check finds every record and file." }
            @if let Some(reason) = reason {
                (ui::Hint::new(reason.as_str()))
            }
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(recheck()),
                ui::Slot::from(continue_draft(*draft)),
                ui::Slot::from(edit()),
                ui::Slot::from(stop()),
            ])))
        },
        PageCard::TurnedOff { reason } => html! {
            p { "The operator of this Monokulo turned off your custom page. Customers see your layout, " strong { (fallback.label()) } "." }
            @if let Some(reason) = reason { p { strong { "Reason: " } (reason) } }
            (ui::Hint::new("You can edit a draft, but can't publish it while this is off."))
            (edit())
        },
    };
    super::settings::plain_card_with_tag(CARD_NAME, "Custom page", card.tag(), body)
}

/// One row of the operator's list.
pub struct AdminPageRow {
    pub store_id: String,
    pub store_name: String,
    pub domain: String,
    pub status: &'static str,
    pub published: String,
    pub off: bool,
}

/// `/admin/custom-pages`: every custom page on this instance, for its operator.
pub fn admin_list(chrome: &PageChrome, rows: &[AdminPageRow], query: &str) -> Markup {
    let header = ui::PageHeader::new("Custom pages");
    let body = html! {
        (super::settings::plain_card("admin-custom-pages", "Custom pages", html! {
            form method="get" class="admin-custom-find" {
                (ui::Input::new("q")
                    .id("custom-pages-find")
                    .value(query)
                    .placeholder("Find by domain or order ID")
                    .attr("aria-label", "Find by domain or order ID"))
                (ui::Button::new("Find").kind(ui::ButtonType::Submit))
            }
            @if rows.is_empty() {
                (ui::Hint::new("No custom pages here."))
            } @else {
                table class="admin-custom-pages" {
                    thead { tr { th { "Store" } th { "Domain" } th { "Status" } th { "Published" } th { "" } } }
                    tbody {
                        @for row in rows {
                            tr {
                                td { (row.store_name) }
                                td { (row.domain) }
                                td { (row.status) }
                                td { (row.published) }
                                td class="inline-actions" {
                                    (ui::LinkButton::new("View", crate::base::url(format!("/admin/custom-pages/{}", row.store_id))))
                                    @if row.off {
                                        form method="post" action=(crate::base::url(format!("/admin/custom-pages/{}/on", row.store_id))) class="inline-form" {
                                            (ui::Button::new("Turn on").kind(ui::ButtonType::Submit))
                                        }
                                    } @else {
                                        (ui::LinkButton::new("Turn off", crate::base::url(format!("/admin/custom-pages/{}/off", row.store_id))))
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }))
    };
    super::Page::new("Custom pages - Monokulo", header)
        .class("settings-page")
        .render(chrome, body)
}

/// Asks before turning a store's page off.
pub fn admin_turn_off(
    chrome: &PageChrome,
    store_id: &str,
    store_name: &str,
    fallback: Layout,
) -> Markup {
    let header = ui::PageHeader::new(format!("Turn off {store_name}'s custom page?"));
    let body = html! {
        form method="post" action=(crate::base::url(format!("/admin/custom-pages/{store_id}/off"))) {
            (super::settings::plain_card("admin-turn-off", "Turn it off", html! {
                p {
                    "Customers see " strong { (fallback.label()) } ", the store's layout, from their next page load. Open pages reload. Orders keep working. "
                    "A new DNS record won't bring the page back, and the merchant can't publish until you turn it on again."
                }
                (ui::Setting::new("Reason (optional, the merchant sees this)", "turn-off-reason", html! {
                    (ui::Input::new("reason").id("turn-off-reason").attr("maxlength", "280"))
                }))
            }))
            (ui::FormActions::new(ui::Slot::many([
                ui::Slot::from(ui::Button::new("Turn off").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
                ui::Slot::from(ui::LinkButton::new("Cancel", crate::base::url("/admin/custom-pages"))),
            ])))
        }
    };
    super::Page::new("Turn off a custom page - Monokulo", header)
        .class("settings-page")
        .render(chrome, body)
}

/// What the operator sees of a store's page: its frame, in a sandbox.
pub fn admin_view(
    chrome: &PageChrome,
    store_id: &str,
    store_name: &str,
    state: &PreviewState,
) -> Markup {
    let header = ui::PageHeader::new(format!("{store_name}'s custom page"));
    let body = html! {
        nav class="template-preview-states" aria-label="Order states" {
            @for other in &PREVIEW_STATES {
                a href=(crate::base::url(format!("/admin/custom-pages/{store_id}?state={}", other.slug)))
                    aria-current=[(other.slug == state.slug).then_some("page")] { (other.name) }
            }
        }
        figure class="template-preview" {
            figcaption { (state.name) }
            iframe sandbox="" title=(format!("The page when: {}", state.name))
                src=(crate::base::url(format!("/admin/custom-pages/{store_id}/frame?state={}", state.slug))) {}
        }
    };
    super::Page::new("A custom page - Monokulo", header)
        .class("settings-page")
        .render(chrome, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_order_cant_be_paid() {
        for state in &PREVIEW_STATES {
            let order = sample_order(state, "Shop", None, 1_000).unwrap();
            assert!(monero_address_is_rejected(&order.address));
            assert_eq!(order.payment_uri, "#");
        }
    }

    fn monero_address_is_rejected(address: &str) -> bool {
        <monero::Address as std::str::FromStr>::from_str(address).is_err()
    }

    #[test]
    fn every_state_a_preview_can_show_has_its_own_link() {
        let mut slugs: Vec<&str> = PREVIEW_STATES.iter().map(|state| state.slug).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), PREVIEW_STATES.len());
        assert!(OrderStatusCovered::all());
    }

    /// A status nobody thought to preview fails to compile here.
    struct OrderStatusCovered;
    impl OrderStatusCovered {
        fn all() -> bool {
            [
                OrderStatus::Pending,
                OrderStatus::Partial,
                OrderStatus::Unconfirmed,
                OrderStatus::Confirming,
                OrderStatus::Paid,
                OrderStatus::Overpaid,
                OrderStatus::Expired,
                OrderStatus::Cancelled,
            ]
            .into_iter()
            .all(|status| {
                let shown = match status {
                    OrderStatus::Pending
                    | OrderStatus::Partial
                    | OrderStatus::Unconfirmed
                    | OrderStatus::Confirming
                    | OrderStatus::Paid
                    | OrderStatus::Overpaid
                    | OrderStatus::Expired
                    | OrderStatus::Cancelled => status,
                };
                PREVIEW_STATES.iter().any(|state| state.status == shown)
            })
        }
    }
}
