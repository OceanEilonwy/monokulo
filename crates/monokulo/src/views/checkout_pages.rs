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

/// A frame showing one state of a draft: sandboxed, so nothing in it runs, and
/// with its own address, so it's never taken for this page.
fn preview_frame(preview_token: &str, state: &PreviewState) -> Markup {
    html! {
        figure class="template-preview" {
            figcaption { (state.name) }
            iframe
                sandbox=""
                loading="lazy"
                title=(format!("Your page when: {}", state.name))
                src=(crate::base::url(format!("/checkout-preview/{preview_token}/frame?state={}", state.slug))) {}
        }
    }
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
            (ui::Hint::new(format!("Template size {} of {}. These use a made-up order that can't be paid.", format_size(size), format_size(BUDGET))))
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
    use crate::checkout_pages::TEMPLATE;
    let value = TEMPLATE.value(token);
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

/// What the last check of a page's proof found, when it was asked for.
pub enum ApproveResult {
    NotChecked,
    Found,
    NotFound(String),
    CantCheck(String),
}

/// Step 3: the record or file that approves this version, and whether it's there.
pub fn approve_step(
    chrome: &PageChrome,
    store: &WizardStore,
    domain: &str,
    token: &str,
    result: &ApproveResult,
) -> Markup {
    let onion = domain_proof::is_onion(domain);
    let check = format!("{}/approve", store.page());
    let body = html! {
        (super::settings::plain_card("template-approve", "Approve this version", html! {
            @if onion {
                p { "This store has no web domain, so its onion address approves instead. An onion address has no DNS, so your shop serves a small file. We fetch it over Tor, never over the web. Each version has its own." }
                (ui::Hint::new("This is weaker than DNS: whoever can change your shop's files can approve a page."))
            } @else {
                p { "Add this record to " strong { (domain) } ". It proves this version is yours, so a stolen key can't change what customers see. Each version has its own." }
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
        @if matches!(result, ApproveResult::Found) {
            form method="post" action=(format!("{}/publish", store.page())) {
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(ui::Button::new("Use this template").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
                    ui::Slot::from(ui::LinkButton::new("Preview every state", format!("{}/preview", store.page()))),
                ])))
            }
        } @else {
            form method="post" action=(check) {
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(ui::Button::new(if onion { "Check for the file" } else { "Check for the record" }).variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)),
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
    /// Waiting for its record or file.
    Draft { domain: String, days_left: i64 },
    /// In use.
    Working {
        domain: String,
        checked: String,
        /// Why the last look got no answer: it keeps showing, which is worth saying.
        trouble: Option<String>,
        /// It approves on the web, so an onion address the store also has shows the
        /// layout instead.
        onion_gets_layout: bool,
        draft: bool,
    },
    /// In use, with its proof missing: still showing.
    Failing {
        domain: String,
        token: String,
        days_left: i64,
        draft: bool,
    },
    /// Published, but customers don't see it.
    NotShowing { draft: bool },
    /// This Monokulo's operator took it down.
    TurnedOff { reason: Option<String> },
}

/// The card on the Checkout tab.
pub fn page_card(store_id: &str, fallback: Layout, card: &PageCard) -> Markup {
    let page = crate::base::url(format!("/stores/{store_id}/settings/checkout-page"));
    let edit = || ui::LinkButton::new("Edit", page.clone());
    let recheck = || {
        html! {
            form method="post" action=(format!("{page}/recheck")) class="inline-form" {
                (ui::Button::new("Check again").kind(ui::ButtonType::Submit))
            }
        }
    };
    let stop = || {
        html! {
            form method="post" action=(format!("{page}/stop")) class="inline-form" {
                (ui::Button::new("Stop using").kind(ui::ButtonType::Submit))
            }
        }
    };
    let continue_draft = |draft: bool| {
        html! {
            @if draft { (ui::LinkButton::new("Continue your draft", format!("{page}/approve"))) }
        }
    };
    let (tag, body) = match card {
        PageCard::NeedsDomain => (
            "Not available yet",
            html! {
                p { "A custom page needs a web domain or onion address you've proved is yours. Verify one, then come back here." }
                (ui::LinkButton::new("Verify a domain", crate::base::url(format!("/stores/{store_id}/settings?tab=domains"))))
            },
        ),
        PageCard::None => (
            "Not set up",
            html! {
                p { "Write your own checkout with a template and optional CSS. Every version you publish is approved with a DNS record, so a leaked key can't change what customers see." }
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(ui::LinkButton::new("Create a custom page", page.clone()).variant(ui::ButtonVariant::Primary)),
                    ui::Slot::from(html! { a href=(super::docs_url("custom-pages")) { "Custom page guide" } }),
                ])))
            },
        ),
        PageCard::Draft { domain, days_left } => (
            "Waiting for approval",
            html! {
                p { "Customers don't see this draft until it's approved on " strong { (domain) } ". It's kept for " (days_left) " more days." }
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(ui::LinkButton::new("Continue", format!("{page}/approve")).variant(ui::ButtonVariant::Primary)),
                    ui::Slot::from(edit()),
                    ui::Slot::from(html! {
                        form method="post" action=(format!("{page}/discard")) class="inline-form" {
                            (ui::Button::new("Discard draft").kind(ui::ButtonType::Submit))
                        }
                    }),
                ])))
            },
        ),
        PageCard::Working {
            domain,
            checked,
            trouble,
            onion_gets_layout,
            draft,
        } => (
            "Working",
            html! {
                p { "Showing to customers where your site frames it. Checked " (checked) " on " strong { (domain) } "." }
                @if let Some(trouble) = trouble {
                    (ui::Hint::new(trouble.as_str()))
                }
                (ui::Hint::new(format!("Direct visits to a payment link show {}.", fallback.label())))
                @if *onion_gets_layout {
                    (ui::Hint::new(format!("Your onion address shows {} too: only the domain that approved a page shows it.", fallback.label())))
                }
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(continue_draft(*draft)),
                    ui::Slot::from(edit()),
                    ui::Slot::from(stop()),
                ])))
            },
        ),
        PageCard::Failing {
            domain,
            token,
            days_left,
            draft,
        } => (
            "Failing",
            html! {
                p {
                    "We can't find this version's proof on " strong { (domain) } ". Customers still see your page for "
                    (days_left) " more days, then " (fallback.label()) ". Put it back, then check again:"
                }
                (proof_details(domain, token))
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(recheck()),
                    ui::Slot::from(continue_draft(*draft)),
                    ui::Slot::from(edit()),
                    ui::Slot::from(stop()),
                ])))
            },
        ),
        PageCard::NotShowing { draft } => (
            "Not showing",
            html! {
                p { "Customers now see " strong { (fallback.label()) } " instead of your page. Open orders keep working. Your page comes back by itself once a check finds its record or file." }
                (ui::FormActions::new(ui::Slot::many([
                    ui::Slot::from(recheck()),
                    ui::Slot::from(continue_draft(*draft)),
                    ui::Slot::from(edit()),
                    ui::Slot::from(stop()),
                ])))
            },
        ),
        PageCard::TurnedOff { reason } => (
            "Turned off by admin",
            html! {
                p { "The operator of this Monokulo turned off your custom page. Customers see your layout, " strong { (fallback.label()) } "." }
                @if let Some(reason) = reason { p { strong { "Reason: " } (reason) } }
                (ui::Hint::new("You can edit a draft, but can't publish it while this is off."))
                (edit())
            },
        ),
    };
    super::settings::plain_card_with_meta("custom-page", "Custom page", Some(tag), body)
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
