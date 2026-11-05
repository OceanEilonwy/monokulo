//! `http/checkout.rs` - the real, public checkout/payment page
//! (`GET /checkout/{order_id}`), its "order not found" fallback,
//! and the nav-bearing `/share` wrapper around it.
//!
//! **`checkout_page`/`not_found_page` deliberately carry no site nav at
//! all** - a merchant embeds `checkout_page` in an iframe inside their own
//! checkout flow; Monokulo's own site navigation has no business appearing
//! inside it. They still use the shared head for consistent
//! typography/color, without the site nav - see
//! [`super::layout_bare_with_head`]/[`super::layout_bare`].
//!
//! The payment details and refund form are server-rendered and usable with
//! JavaScript disabled: a `<noscript>` meta-refresh reloads the page once a
//! minute and the refund form has a plain Save button. With JavaScript,
//! `checkout.js` instead streams re-rendered [`live_fragment`] regions over
//! Server-Sent Events, auto-saves the refund address, and locally decodes
//! refund QR images without sending those images to the server.

use checkout_template::{Feature, Part, State, Status, Template};

use super::checkout_layouts::Layout;
use maud::{html, Markup, PreEscaped};

use super::OrderStatus;

use super::{layout_bare, layout_bare_with_head, script, Load, PageChrome};

/// One payment row on the checkout page's payments table - mirrors the
/// engine's own `PaymentViewModel` field-for-field.
pub struct CheckoutPaymentViewModel {
    pub txid: String,
    pub amount_xmr: String,
    pub confirmations: u64,
    pub is_zero_conf: bool,
}

/// The view model [`checkout_page`] takes.
pub struct CheckoutViewModel {
    pub order_id: crate::db::OrderId,
    /// Who the payer is paying: the store's name.
    pub store_name: String,
    /// What the payment is for: the order's description, or the store's
    /// payment description when the order has none; `None` when neither
    /// has one. Text the merchant typed, shown escaped.
    pub description: Option<String>,
    /// The order's merchant reference (`merchant_order_id`), only when its
    /// creator chose to show it to the payer.
    pub merchant_reference: Option<String>,
    /// The store's site (`shop.example`), only while a verified domain
    /// covers it (`crate::embed_domains::verified_site`).
    pub merchant_site: Option<crate::stores::Site>,
    /// Whether the page may show the site's own icon, loaded from the site:
    /// never for a visitor on Tor, who would be sent off Tor for it.
    pub merchant_icon: bool,
    pub status_label: String,
    pub status: OrderStatus,
    pub address: String,
    /// The QR code (`http::checkout::payment_qr_svg`); asks for
    /// `amount_due_xmr` while one is due.
    pub qr_code_svg: crate::qr::QrSvg,
    /// What the code holds: `monero:...`, which a small frame links to.
    pub payment_uri: String,
    pub xmr_amount: String,
    /// What is still owed: the whole amount, or the rest after a partial
    /// payment; zero once covered.
    pub amount_due_xmr: String,
    pub amount_received_xmr: String,
    pub amount: String,
    pub currency: String,
    pub confirmations: u64,
    pub confirmations_required: u64,
    /// `min(100, round(confirmations / confirmations_required * 100))`,
    /// pre-computed server-side - the progress bar is a plain `<progress>`,
    /// so it needs no script, and no style attribute the page's
    /// Content-Security-Policy would refuse.
    pub progress_percent: u8,
    /// Whether the order can't change any more
    /// (`engine_client::OrderView::is_final`): an expired or cancelled one
    /// the engine still watches isn't, and the page keeps following it.
    pub is_terminal: bool,
    /// When a double spend was detected: gates the banner, which shows the
    /// time with `clock`.
    pub double_spend_detected_at: Option<i64>,
    /// The zone times are shown in: `?timezone=`, else UTC.
    pub clock: super::time::Clock,
    /// No `?timezone=`: the script shows each time in the customer's own
    /// zone instead (`data-local`).
    pub local_times: bool,
    /// A moment.js-style relative duration ("12h", "4h 15m", "2d 4h"),
    /// pre-rendered server-side (`crate::templates::format_duration_until`) -
    /// this page is customer-facing and must stay fully meaningful with
    /// JavaScript disabled. Only shown while the order can be paid.
    pub expires_in_display: String,
    /// When the order was made and when it expires (Unix seconds): the
    /// countdown's whole span, and with `clock`, what's left of it.
    pub created_at: i64,
    pub expires_at: i64,
    /// `Some` once a customer (or their storefront, on their behalf) has set
    /// one via the form below, which shows it saved and can still change it
    /// until `refund_address_locked`.
    pub refund_address: Option<String>,
    /// The order has received money and has an address on record, which the
    /// engine keeps from then on: shown read-only, with no form.
    pub refund_address_locked: bool,
    /// Set only when the refund-address form below was just rejected (empty
    /// submission, or a real engine failure) - `None` on a plain page load.
    pub refund_address_error: Option<String>,
    /// Which layout this page is in. A generic presentation option - the page
    /// knows nothing about who embeds it (the POS terminal is one user).
    pub layout: Layout,
    /// The store's stylesheet, by the hash of its content, if it has one.
    pub style_hash: Option<String>,
    pub refund_enabled: bool,
    pub query_suffix: String,
    /// Whether the no-JavaScript meta refresh is on (`?refresh=false` turns
    /// it off), and the query string of the same page with it flipped, for
    /// the no-JavaScript "Auto Refresh" toggle.
    pub auto_refresh: bool,
    pub toggled_refresh_suffix: String,
    pub payment_error: Option<String>,
    pub payments: Vec<CheckoutPaymentViewModel>,
}

/// The URL of the order's checkout page (`crate::api::checkout`).
fn checkout_url(order_id: &crate::db::OrderId) -> String {
    crate::base::url(
        crate::api::checkout::Path {
            order_id: order_id.clone(),
        }
        .to_string(),
    )
}

/// The URL the order's refund address form posts to.
fn refund_url(order_id: &crate::db::OrderId) -> String {
    crate::base::url(
        crate::http::paths::RefundAddressPath {
            order_id: order_id.clone(),
        }
        .to_string(),
    )
}

/// The checkout's own stylesheet, on top of the shared theme: a file, as the
/// page's Content-Security-Policy ([`content_security_policy`]) allows no
/// inline style of its own.
pub(crate) const CHECKOUT_CSS: static_assets::Baked =
    static_assets::bake!("src/views/checkout.css");

/// The Content-Security-Policy every checkout page is sent with. The page
/// loads only from this Monokulo, so nothing a store adds to it can reach
/// another site, and a customer on Tor is never sent to the clearnet. The
/// one inline style allowed, by its hash, is the typeface rules each page
/// starts with, which carry the request's base path (`crate::base`).
/// `blob:` images are the refund form reading a photo of a QR code where
/// `createImageBitmap` is missing. The one thing from elsewhere is the page's
/// merchant icon, from the verified site it names and never on Tor
/// ([`CheckoutViewModel::merchant_icon`]).
pub fn content_security_policy(data: &CheckoutViewModel) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let fonts = base64::engine::general_purpose::STANDARD
        .encode(sha2::Sha256::digest(super::font_faces().as_bytes()));
    let icon = merchant_icon_site(data)
        .map(|site| format!(" {}", site.link()))
        .unwrap_or_default();
    format!(
        "default-src 'none'; script-src 'self'; style-src 'self' 'sha256-{fonts}'; font-src 'self'; img-src 'self' blob:{icon}; connect-src 'self'; form-action 'self'; base-uri 'none'"
    )
}

/// Who is asking to be paid, and for what: the store's name, its site once
/// a verified domain covers it, what the order is for and the merchant's
/// reference, when they're set. A payment link opened from a message
/// would otherwise be anyone asking for money. In a compact frame (a shop's
/// own checkout, which says all this already) it's one line.
fn merchant_header(data: &CheckoutViewModel) -> Markup {
    html! {
        header class="merchant" {
            @if data.layout == Layout::Compact {
                (ui::Hint::new(html! {
                    strong { (data.store_name) }
                    @if let Some(reference) = &data.merchant_reference { " · " (reference) }
                }))
            } @else {
                // The site's own icon once a verified domain covers it, its
                // name's letter until then (or if it has none, or the
                // visitor is on Tor).
                (ui::Avatar::of(&data.store_name).maybe_src(merchant_icon_site(data).map(|site| {
                    format!("{}/favicon.ico", site.link())
                })))
                div class="merchant-about" {
                    (ui::Heading::new(data.store_name.as_str()))
                    @if let Some(site) = &data.merchant_site {
                        (ui::Hint::new(html! {
                            (site) " " (PreEscaped(ui::assets::icons::CHECK))
                            span class="visually-hidden" { " (a verified domain)" }
                        }).attr("data-merchant", "site"))
                    }
                    @if let Some(description) = &data.description {
                        p class="merchant-for" { (description) }
                    }
                    @if let Some(reference) = &data.merchant_reference {
                        (ui::Hint::new(html! { "Reference " span class="mk-mono" { (reference) } }))
                    }
                }
            }
        }
    }
}

/// The order's event stream, routed part by part, with the page's own
/// options.
fn events_url(data: &CheckoutViewModel) -> String {
    let join = if data.query_suffix.is_empty() {
        '?'
    } else {
        '&'
    };
    format!(
        "{}{}{join}routed=true&parts=2&layout={}&r={}",
        crate::base::url(
            crate::http::paths::CheckoutEventsPath {
                order_id: data.order_id.clone()
            }
            .to_string()
        ),
        data.query_suffix,
        data.layout.name(),
        showing_bits(
            &data
                .layout
                .template()
                .regions_showing(&template_state(data))
        ),
    )
}

/// The refund address: the form, or once a payment has arrived, what's recorded.
/// Not live: a customer may be typing in it.
fn refund_part(data: &CheckoutViewModel) -> Markup {
    html! {
        div data-part="refund" {
                    @if data.refund_enabled && data.refund_address_locked { div class="section" {
                        // Not the form's ids: `checkout.js` leaves this alone.
                        label for="refund-address-fixed" { "Refund address" }
                        (ui::Input::new("refund_address")
                            .id("refund-address-fixed")
                            .value(data.refund_address.as_deref().unwrap_or(""))
                            .readonly(true)
                            .autocomplete("off")
                            .attr("spellcheck", "false"))
                        (ui::FieldNote::new(html! { "Recorded for the merchant to use if this order needs a refund. "
                            "It can't be changed now that a payment has arrived." }))
                    } } @else if data.refund_enabled { div class="section" {
                        form method="post" id="refund-form" action=(format!("{}{}", refund_url(&data.order_id), data.query_suffix))
                            fx-action=(format!("{}{}", refund_url(&data.order_id), data.query_suffix))
                            fx-method="POST" fx-trigger="refund:save" fx-swap="none" fx-own-errors
                            // The QR decoder, fetched by checkout.js only
                            // when a customer scans or picks an image.
                            data-qr-decoder=(crate::assets::url("jsQR.js")) {
                                label for="refund_address" { "Refund address " (ui::Hint::new("(optional)").inline(true)) }
                                @if let Some(error) = &data.refund_address_error {
                                    (ui::Alert::new(error.as_str(), ui::Tone::Error))
                                }
                                div id="refund-field" class=(if data.refund_address.is_some() { "refund-field is-saved" } else { "refund-field" }) {
                                    (ui::Input::new("refund_address")
                                        .id("refund_address")
                                        .value(data.refund_address.as_deref().unwrap_or(""))
                                        .placeholder("Your Monero refund address")
                                        .autocomplete("off")
                                        .attr("spellcheck", "false")
                                        .actions(refund_actions(data)))
                                    input type="file" id="refund-image" accept="image/*" hidden;
                                }
                                video id="refund-camera" class="refund-camera" autoplay playsinline hidden {}
                                p id="scan-error" class="scan-error" role="alert" hidden {}
                                (ui::FieldNote::new(html! { "If this order needs a refund, this address is recorded for the merchant to use. "
                                    "Refunds are not sent automatically." }))
                                noscript { (ui::FormActions::new(ui::Button::new("Save").kind(ui::ButtonType::Submit))) }
                        }
                    } }
        }
    }
}

/// One feature of a template, for this order.
fn render_part(data: &CheckoutViewModel, part: &Part) -> Markup {
    match part.feature {
        Feature::Store => html! { span data-part="store" { (data.store_name) } },
        Feature::OrderId => html! { span data-part="order-id" { (data.order_id) } },
        Feature::Merchant => merchant_header(data),
        Feature::Status if part.option == Some("icon") => live_status_icon(data),
        Feature::Status => live_status(data),
        Feature::Message => live_message(data),
        Feature::Amount => live_amount(data, part.option == Some("bare")),
        Feature::Fiat => live_fiat(data),
        Feature::Countdown => live_countdown(data),
        Feature::Qr => live_qr(data, part.option),
        Feature::Address if part.option == Some("short") => live_address_short(data),
        Feature::Address => live_address(data),
        Feature::Progress => live_progress(data),
        Feature::Refund => refund_part(data),
        Feature::Payments => live_payments(data),
    }
}

/// The element a live update replaces for a feature, if it has one.
fn live_id(feature: Feature) -> Option<&'static str> {
    match feature {
        Feature::Status => Some("live-status"),
        Feature::Message => Some("live-message"),
        Feature::Amount => Some("live-amount"),
        Feature::Fiat => Some("live-fiat"),
        Feature::Countdown => Some("live-countdown"),
        Feature::Qr => Some("live-qr"),
        Feature::Address => Some("live-address"),
        Feature::Progress => Some("live-progress"),
        Feature::Payments => Some("live-payments"),
        Feature::Store | Feature::OrderId | Feature::Merchant | Feature::Refund => None,
    }
}

/// What a template's conditions can ask about this order.
pub fn template_state(data: &CheckoutViewModel) -> State {
    State {
        status: match data.status {
            OrderStatus::Pending => Status::Pending,
            OrderStatus::Partial => Status::Partial,
            OrderStatus::Unconfirmed => Status::Unconfirmed,
            OrderStatus::Confirming => Status::Confirming,
            OrderStatus::Paid => Status::Paid,
            OrderStatus::Overpaid => Status::Overpaid,
            OrderStatus::Expired => Status::Expired,
            OrderStatus::Cancelled => Status::Cancelled,
        },
        double_spend: data.double_spend_detected_at.is_some(),
        fiat: priced_in_fiat(data),
        refund_locked: data.refund_address_locked,
    }
}

/// The template's part of the page.
fn page_body(template: &Template, data: &CheckoutViewModel) -> String {
    template.render(&template_state(data), &mut |part| {
        render_part(data, part).into_string()
    })
}

/// Which of a template's regions show, as the page's event stream says it:
/// `r=1001`.
pub fn showing_bits(showing: &[bool]) -> String {
    showing
        .iter()
        .map(|&on| if on { '1' } else { '0' })
        .collect()
}

/// What a page's stream starts from: the bits the page sent as this stream's
/// last event, if it's reconnecting, otherwise those in its URL, which are only
/// true of when it loaded.
pub fn page_showing(
    last_event_id: Option<&str>,
    in_url: Option<&str>,
    regions: usize,
) -> Option<Vec<bool>> {
    last_event_id
        .and_then(|bits| parse_showing(bits, regions))
        .or_else(|| in_url.and_then(|bits| parse_showing(bits, regions)))
}

/// Reads [`showing_bits`], for a template with `regions` regions.
pub fn parse_showing(bits: &str, regions: usize) -> Option<Vec<bool>> {
    (bits.len() == regions && bits.bytes().all(|b| b == b'0' || b == b'1'))
        .then(|| bits.bytes().map(|b| b == b'1').collect())
}

/// What the page's live stream sends now, by the id each replaces: a region
/// only when its condition has flipped (it may hold a form being typed in),
/// then each part showing. `showing` is what the page last had.
pub fn live_updates(
    template: &Template,
    data: &CheckoutViewModel,
    showing: &mut Vec<bool>,
) -> Vec<(String, String)> {
    let state = template_state(data);
    let now = template.regions_showing(&state);
    let mut updates = Vec::new();
    for (id, on) in now.iter().enumerate() {
        if showing.get(id) != Some(on) {
            let region = template.render_region(id, &state, &mut |part| {
                render_part(data, part).into_string()
            });
            if let Some(region) = region {
                updates.push((format!("mk-if-{id}"), region));
            }
        }
    }
    *showing = now;
    for part in template.parts_showing(&state) {
        if let Some(id) = live_id(part.feature) {
            updates.push((id.to_string(), render_part(data, &part).into_string()));
        }
    }
    updates
}

/// The verified site whose icon this page shows, if any.
fn merchant_icon_site(data: &CheckoutViewModel) -> Option<&crate::stores::Site> {
    data.merchant_site.as_ref().filter(|_| data.merchant_icon)
}

pub fn checkout_page(chrome: &PageChrome, data: &CheckoutViewModel) -> Markup {
    let template = data.layout.template();
    let extra_head = html! {
        // Without JavaScript this is the only update path; with it,
        // `checkout.js` streams changes in place instead (`noscript` content
        // is never parsed as markup when scripting is on).
        @if !data.is_terminal && data.auto_refresh {
            noscript { meta http-equiv="refresh" content="60" id="checkout-refresh"; }
        }
        (super::stylesheet("checkout.css"))
        // The store's colours and shape, over the theme.
        @if let Some(hash) = &data.style_hash {
            link rel="stylesheet" href=(crate::base::url(format!("/static-style/{hash}.css")));
        }
        // Live updates and the refund address save (structured_logging.md
        // part 8), in this order: the glue sets fixi's defaults.
        (script("fx-glue.js", Load::Defer))
        (script("fixi.js", Load::Defer))
        (script("ssexi.js", Load::Defer))
        // The shared components' islands: the address's Copy button.
        (script("ui-islands.js", Load::Module))
        // Problem reports, only from a store that opted in to client logs
        // (off by default: a payment page, D8).
        @if chrome.browser_reports {
            (script("telemetry.js", Load::Defer))
        }
    };
    let body = html! {
        div class=(match data.layout { Layout::Detailed => "pay-wrap", Layout::Compact => "pay-wrap checkout-compact", Layout::Tiny => "pay-wrap checkout-tiny" }) id="checkout-root" data-order-id=(data.order_id) data-status=(data.status) data-confirmations=(data.confirmations) data-error=(data.payment_error.as_deref().unwrap_or("")) {
            h1 class="sr-only" { "Monero payment of " (data.xmr_amount) " XMR" }
            // Always here, and only its text replaced, so a screen reader
            // announces each change of status.
            p id="checkout-announce" class="sr-only" role="status" { (data.status_label) }
            @if !data.is_terminal {
                noscript {
                    p class="refresh-toggle" {
                        a href=(format!("{}{}", checkout_url(&data.order_id), data.toggled_refresh_suffix)) {
                            "Auto Refresh: " (if data.auto_refresh { "ON" } else { "OFF" })
                        }
                    }
                }
            }

            // With JavaScript, fixi opens the order's event stream as soon
            // as it sees this, and ssexi routes each changed part of the
            // page into place. Not for a final order: nothing will change.
            @if !data.is_terminal {
                span id="checkout-stream" hidden fx-action=(events_url(data)) fx-trigger="fx:inited" fx-swap="none" fx-sse-reconnect fx-own-errors {}
            }

            (PreEscaped(page_body(template, data)))
        }
        (script("checkout.js", Load::Now))
    };
    layout_bare_with_head(
        chrome,
        "Pay with Monero",
        "width=device-width, initial-scale=1",
        extra_head,
        body,
    )
}

/// The checkout page's parts that change while it's open, each an element
/// with an `id` and `data-live`. The live-update stream sends these,
/// re-rendered, and `checkout.js` swaps each into place by id - everything
/// else (QR code, address, the refund form mid-edit) is left untouched.
pub fn live_fragment(data: &CheckoutViewModel) -> Markup {
    html! {
        (live_status(data))
        (live_pay(data))
        (live_address(data))
        (live_progress(data))
        (live_payments(data))
    }
}

/// The same parts one by one, with the id each replaces, for the stream
/// that routes each to its element (`http::checkout::checkout_events`).
pub fn live_parts(data: &CheckoutViewModel) -> [(&'static str, Markup); 5] {
    [
        ("live-status", live_status(data)),
        ("live-pay", live_pay(data)),
        ("live-address", live_address(data)),
        ("live-progress", live_progress(data)),
        ("live-payments", live_payments(data)),
    ]
}

/// Whether the customer still owes something: the whole amount, or the rest
/// after a partial payment. Only then is the code live; otherwise it fades.
fn awaiting_payment(data: &CheckoutViewModel) -> bool {
    matches!(data.status, OrderStatus::Pending | OrderStatus::Partial)
}

/// When a double spend reversed a payment to the order, if that decides what
/// the payer is told: it does unless another payment covers the order in
/// full (paid or overpaid; the engine keeps the flag set either way), when
/// the stage follows the status and only a note mentions the reversal.
/// While a covering payment still confirms, the page stays cautious. The
/// stage ([`stage_steps`], [`stage_message`], [`live_status`]) and the
/// status event's message (`http::checkout`) all decide by this.
pub fn reversed_at(status: OrderStatus, double_spend_detected_at: Option<i64>) -> Option<i64> {
    double_spend_detected_at
        .filter(|_| !matches!(status, OrderStatus::Paid | OrderStatus::Overpaid))
}

/// The track: Send, Confirm, Paid - without Confirm when the store counts a
/// payment as soon as it's seen (`confirmations_required == 0`).
fn stage_steps(data: &CheckoutViewModel) -> Vec<ui::ProgressStep> {
    use ui::StepProgress as Step;
    let [send, confirm, paid] = if reversed_at(data.status, data.double_spend_detected_at).is_some()
        && !awaiting_payment(data)
    {
        [Step::Fail, Step::Todo, Step::Todo]
    } else {
        match data.status {
            OrderStatus::Pending => [Step::Now, Step::Todo, Step::Todo],
            OrderStatus::Partial => [Step::Part, Step::Todo, Step::Todo],
            OrderStatus::Unconfirmed | OrderStatus::Confirming => {
                [Step::Done, Step::Now, Step::Todo]
            }
            OrderStatus::Paid | OrderStatus::Overpaid => [Step::Done, Step::Done, Step::Done],
            OrderStatus::Expired | OrderStatus::Cancelled => [Step::Fail, Step::Todo, Step::Todo],
        }
    };
    let steps = if data.confirmations_required == 0 {
        vec![
            ("Send", send),
            (
                "Paid",
                if confirm == Step::Now {
                    Step::Now
                } else {
                    paid
                },
            ),
        ]
    } else {
        vec![("Send", send), ("Confirm", confirm), ("Paid", paid)]
    };
    steps
        .into_iter()
        .map(|(label, progress)| ui::ProgressStep::new(label, progress))
        .collect()
}

/// The line under the track, only when something is wrong: a short title, then
/// what to do now. The track, amounts and code say the rest.
fn stage_message(data: &CheckoutViewModel) -> Option<(String, Markup)> {
    let received = super::trim_xmr(&data.amount_received_xmr);
    let due = super::trim_xmr(&data.amount_due_xmr);
    let when = |at: i64| html! { span id="double-spend-time" { @if data.local_times { (data.clock.time_local(at)) } @else { (data.clock.time(at)) } } };
    if let Some(at) = reversed_at(data.status, data.double_spend_detected_at) {
        let when = when(at);
        return Some(if awaiting_payment(data) {
            (
                "A payment was reversed".to_string(),
                html! { "A double spend was detected at " (when) ", so it doesn't count. Send " (due) " XMR to pay this order." },
            )
        } else {
            (
                "Payment reversed".to_string(),
                html! { "A double spend was detected at " (when) ", so that payment doesn't count. Contact the merchant before sending anything." },
            )
        });
    }
    // Covered anyway: the paid stage, with a note of the reversed payment.
    let reversed_note = data.double_spend_detected_at.map(|at| {
        html! { " A double spend was detected at " (when(at)) ", so that payment doesn't count; another one covers this order." }
    });
    Some(match data.status {
        OrderStatus::Pending
        | OrderStatus::Partial
        | OrderStatus::Unconfirmed
        | OrderStatus::Confirming => return None,
        // Covered, but a payment was reversed on the way: worth a line.
        OrderStatus::Paid => ("Paid".to_string(), html! { (reversed_note?) }),
        OrderStatus::Overpaid => (
            "Paid, with extra".to_string(),
            html! {
                @if let Some(message) = &data.payment_error { (message) } @else { "Don't send more. Contact the merchant about the extra amount." }
                @if let Some(note) = &reversed_note { (note) }
            },
        ),
        // Not final yet (`is_terminal`): the engine still watches the order,
        // so a payment already sent can still arrive and count.
        OrderStatus::Expired => (
            "This payment expired".to_string(),
            html! {
                @if received == "0" && !data.is_terminal { "Don't send to this address. If you already sent it, keep this page open: it updates when the payment arrives." }
                @else if received == "0" { "Nothing arrived in time. Don't send to this address; go back to the shop to start again." }
                @else { (received) " XMR arrived before it expired. Don't send more; contact the merchant about it." }
            },
        ),
        OrderStatus::Cancelled => (
            "This order was cancelled".to_string(),
            html! {
                @if received == "0" && !data.is_terminal { "Don't send to this address. If you already sent it, keep this page open: it updates when the payment arrives." }
                @else if received == "0" { "Don't send to this address; ask the merchant for a new order if you still want to pay." }
                @else { (received) " XMR arrived after it was cancelled. Don't send more; contact the merchant about a refund." }
            },
        ),
    })
}

/// The track above the code, in every state, so nothing moves as the order
/// changes (the badge is for screen readers).
fn live_status(data: &CheckoutViewModel) -> Markup {
    let steps = stage_steps(data);
    let steps = ui::ProgressSteps::new("Payment progress", steps);
    let steps = match stage_message(data) {
        Some((title, message)) => steps.title(title).message(message),
        None => steps,
    };
    let shown = super::order_state(data.status).1;
    let state = if reversed_at(data.status, data.double_spend_detected_at).is_some() {
        ui::OrderState::DoubleSpend
    } else {
        shown
    };
    html! {
        div id="live-status" class="stage-slot" data-live data-part="status" {
            span id="status-badge" class=(format!("sr-only state-{shown}")) { (data.status_label) }
            (steps.state(state).plain(true))
        }
    }
}

/// The status as one icon, for a small frame: its words for a screen reader.
fn live_status_icon(data: &CheckoutViewModel) -> Markup {
    let shown = super::order_state(data.status).1;
    let reversed = reversed_at(data.status, data.double_spend_detected_at).is_some();
    let state = if reversed {
        ui::OrderState::DoubleSpend
    } else {
        shown
    };
    let icon = if reversed {
        "M12 7v7M12 17.5v.5"
    } else {
        match data.status {
            OrderStatus::Pending | OrderStatus::Unconfirmed | OrderStatus::Confirming => {
                "M12 7v5l3 2M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z"
            }
            OrderStatus::Partial => "M12 3a9 9 0 1 0 0 18V3z",
            OrderStatus::Paid => "m5 12 5 5 9-10",
            OrderStatus::Overpaid => "M12 7v7M12 17.5v.5",
            OrderStatus::Expired | OrderStatus::Cancelled => "m6 6 12 12M18 6 6 18",
        }
    };
    html! {
        span id="live-status" class=(format!("tiny-icon state-{state}")) data-live data-part="status" {
            span id="status-badge" class=(format!("sr-only state-{shown}")) { (data.status_label) }
            svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true" { path d=(icon) {} }
        }
    }
}

/// A few words on where the order stands, once there's no code to pay. Says
/// less than the status line of a bigger page, for a small frame.
fn live_message(data: &CheckoutViewModel) -> Markup {
    let received = super::trim_xmr(&data.amount_received_xmr);
    let message = if reversed_at(data.status, data.double_spend_detected_at).is_some() {
        if awaiting_payment(data) {
            "A payment was reversed. It doesn't count.".to_string()
        } else {
            "Payment reversed. Contact the merchant.".to_string()
        }
    } else {
        match data.status {
            OrderStatus::Pending | OrderStatus::Partial => String::new(),
            OrderStatus::Unconfirmed => "Seen on the network. Don't send it again.".to_string(),
            OrderStatus::Confirming => format!(
                "{} of {} confirmations. Don't send it again.",
                data.confirmations, data.confirmations_required
            ),
            OrderStatus::Paid => "Paid. Nothing more to do.".to_string(),
            OrderStatus::Overpaid => {
                "Paid, with extra. Don't send more; contact the merchant.".to_string()
            }
            OrderStatus::Expired | OrderStatus::Cancelled => {
                let what = if data.status == OrderStatus::Expired {
                    "This payment expired."
                } else {
                    "This order was cancelled."
                };
                if received != "0" {
                    format!("{what} Don't send more; contact the merchant.")
                } else if data.is_terminal {
                    format!("{what} Don't send to this address.")
                } else {
                    format!("{what} Don't send to this address. If you already sent it, keep this page open.")
                }
            }
        }
    };
    html! {
        p id="live-message" class="tiny-message" data-live data-part="message" role="status" { (message) }
    }
}

/// The address shortened, for a small frame, with Copy giving the whole of it.
/// Without JavaScript, it's selectable and focus shows it in full. Once it
/// shouldn't be paid, nothing to copy it with, as for the whole address.
fn live_address_short(data: &CheckoutViewModel) -> Markup {
    if !awaiting_payment(data) {
        let chars: Vec<char> = data.address.chars().collect();
        let first: String = chars.iter().take(6).collect();
        let last: String = chars[chars.len().saturating_sub(6)..].iter().collect();
        return html! {
            div id="live-address" class="address-short is-spent" data-live data-part="address" {
                span class="mk-mono" { (first) "…" (last) }
            }
        };
    }
    html! {
        div id="live-address" class="address-short" data-live data-part="address" {
            (ui::CopyValue::new(data.address.as_str())
                .copy_label("Copy the full address")
                .truncate(ui::Truncate::Middle)
                .head(6usize)
                .tail(6usize))
        }
    }
}

/// Inside the refund field: the saved tick, then scanning a code and choosing
/// its picture, both hidden until `checkout.js` can decode one.
fn refund_actions(data: &CheckoutViewModel) -> ui::Slot {
    let saved = html! {
        span id="refund-save-state" class="refund-save-state" role="status" aria-label=(if data.refund_address.is_some() { "Refund address saved" } else { "" }) {
            svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" { path d="m4 12 5 5L20 6" {} }
        }
    };
    let scan = html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true" { path d="M3 9V3h6M15 3h6v6M21 15v6h-6M9 21H3v-6M6 6h2v2H6zM16 6h2v2h-2zM6 16h2v2H6zM12 11h2v2h-2zM16 16h2v2h-2z" {} }
    };
    let image = html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" { rect x="2" y="3" width="20" height="18" rx="2" {} circle cx="8.5" cy="8.5" r="1.5" {} path d="m2 17 6-6 4 4 3-3 7 7" {} }
    };
    ui::Slot::many([
        saved.into(),
        ui::IconButton::new("Scan refund QR", scan)
            .attr("id", "scan-refund")
            .flag("hidden")
            .into(),
        ui::IconButton::new("Choose QR image", image)
            .attr("id", "upload-refund")
            .flag("hidden")
            .into(),
    ])
}

/// The amount, always in the same place: what's left after a partial payment,
/// the order's total once nothing more is owed. `bare` is the number alone.
fn live_amount(data: &CheckoutViewModel, bare: bool) -> Markup {
    let due = super::trim_xmr(&data.amount_due_xmr);
    let total = super::trim_xmr(&data.xmr_amount);
    let (label, amount) = if data.status == OrderStatus::Partial {
        ("Still to pay", due)
    } else if awaiting_payment(data) {
        ("Amount due", total)
    } else {
        ("Order total", total)
    };
    html! {
        span id="live-amount" data-live data-part="amount" {
            @if !bare { span class="amount-label" id="amount-label" { (label) } }
            // The number and its unit never split; the font shrinks with
            // the frame instead (see `.amount`).
            span class="amount" id="xmr-amount" aria-labelledby=[(!bare).then_some("amount-label")] {
                span class="amount-value" { (amount) } " " span class="amount-unit" { "XMR" }
            }
        }
    }
}

/// A fiat equivalent, only for an order priced in fiat.
fn live_fiat(data: &CheckoutViewModel) -> Markup {
    html! {
        span id="live-fiat" data-live data-part="fiat" {
            @if data.status != OrderStatus::Partial && priced_in_fiat(data) {
                span class="fiat-amount" { "≈ " (super::display_amount(&data.amount, &data.currency)) }
            }
        }
    }
}

/// The time left to pay, while money is owed.
fn live_countdown(data: &CheckoutViewModel) -> Markup {
    html! {
        div id="live-countdown" data-live data-part="countdown" {
            @if awaiting_payment(data) {
                div class="countdown-slot" { (countdown(data)) }
            }
        }
    }
}

/// The code, always in the same place. After a partial payment it's redrawn
/// for what's left; once it shouldn't be paid (the payment is in, or the order
/// is over) it fades (`.is-spent`), so nothing invites a second payment.
fn live_qr(data: &CheckoutViewModel, size: Option<&str>) -> Markup {
    let partial = data.status == OrderStatus::Partial;
    let state = if partial {
        " qr-new"
    } else if awaiting_payment(data) {
        ""
    } else {
        " is-spent"
    };
    let size = size.map_or(String::new(), |size| format!(" qr-{size}"));
    html! {
        div id="live-qr" data-live data-part="qr" {
            // In a small frame, a tap opens the customer's wallet, with or
            // without JavaScript: `_blank`, as an embed's sandbox lets a popup
            // leave the frame but not the frame itself.
            @if size == " qr-small" && awaiting_payment(data) {
                a class=(format!("qr-wrap{state}{size}")) href=(data.payment_uri) target="_blank" rel="noopener noreferrer" aria-label="Open in your wallet" { (data.qr_code_svg) }
            } @else {
                div class=(format!("qr-wrap{state}{size}")) { (data.qr_code_svg) }
            }
            @if partial {
                p class="qr-note" { "Wallets that read payment codes fill in " (super::trim_xmr(&data.amount_due_xmr)) " XMR for you." }
            }
        }
    }
}

/// What a page opened before a release's split of `live-pay` still asks for by
/// that id, until those pages are gone.
fn live_pay(data: &CheckoutViewModel) -> Markup {
    html! {
        div id="live-pay" data-live {
            (live_amount(data, false))
            (live_fiat(data))
            (live_countdown(data))
            (live_qr(data, None))
        }
    }
}

/// Whether the order was priced in a currency other than XMR: the amount
/// then has its fiat equivalent, and its XMR price is what time runs out on.
fn priced_in_fiat(data: &CheckoutViewModel) -> bool {
    !data.currency.is_empty() && data.currency != "XMR"
}

/// The time left to pay: counted down on the page, stepping the bar each
/// minute; without JavaScript, the server's words, moved on by the refresh.
/// It turns the warning colour at five minutes and the error colour at two.
fn countdown(data: &CheckoutViewModel) -> ui::Countdown {
    let remaining = u64::try_from(data.expires_at - data.clock.now()).unwrap_or(0);
    let total = u64::try_from(data.expires_at - data.created_at)
        .unwrap_or(0)
        .max(remaining);
    let at = if data.local_times {
        data.clock.time_local(data.expires_at)
    } else {
        data.clock.time(data.expires_at)
    };
    let note = if priced_in_fiat(data) {
        html! { "Price held until " (at) }
    } else {
        html! { "Until " (at) }
    };
    ui::Countdown::new(total, remaining, data.expires_in_display.as_str())
        .suffix("left to pay")
        .note(note)
        .urgent(120u64)
        .toned()
}

/// The address, whole, in groups to check by eye, under what to send to
/// it. Muted, with nothing to copy it with, once it shouldn't be paid: a live
/// part, so that follows the order.
fn live_address(data: &CheckoutViewModel) -> Markup {
    const GROUP: usize = 6;
    let open = awaiting_payment(data);
    let chars: Vec<char> = data.address.chars().collect();
    let last = match chars.len() % GROUP {
        0 => GROUP,
        rest => rest,
    };
    let first: String = chars.iter().take(GROUP).collect();
    let end: String = chars[chars.len().saturating_sub(last)..].iter().collect();
    let mut address = ui::GroupedValue::of(data.address.as_str(), GROUP)
        .copy_label("Copy address")
        .void(!open);
    if open {
        address = address.hint(html! {
            "Check your wallet shows " strong { (first) } " at the start and " strong { (end) } " at the end."
        });
    }
    html! {
        div class="address-block" id="live-address" data-live data-part="address" {
            span class="address-label" id="address-label" {
                @if data.status == OrderStatus::Partial { "Send the remaining amount to" }
                @else if data.status == OrderStatus::Pending { "Send exactly this amount to" }
                @else if matches!(data.status, OrderStatus::Unconfirmed | OrderStatus::Confirming) { "Payment seen. Don't send it again" }
                @else { "Payment address" }
            }
            (address)
        }
    }
}

fn live_progress(data: &CheckoutViewModel) -> Markup {
    html! {
        div class="section" id="live-progress" data-live data-part="progress" {
            div class="progress-row" {
                span id="confirmations-label" { (data.confirmations) " / " (data.confirmations_required) " confirmations" }
                span id="received-label" { (super::trim_xmr(&data.amount_received_xmr)) " XMR received" }
            }
            progress class="progress-bar" id="progress-bar" aria-labelledby="confirmations-label"
                max="100" value=(data.progress_percent)
                aria-valuetext=(format!("{} of {} confirmations", data.confirmations, data.confirmations_required)) {
                (data.progress_percent) "%"
            }
        }
    }
}

fn live_payments(data: &CheckoutViewModel) -> Markup {
    let rows = data.payments.iter().map(|payment| {
        ui::Slot::from(ui::Row::new(ui::Slot::many([
            ui::Cell::new(super::short_id(&payment.txid)).into(),
            ui::Cell::new(
                html! { span class="nowrap" { (super::trim_xmr(&payment.amount_xmr)) " XMR" } },
            )
            .into(),
            ui::Slot::from(ui::Cell::new(if payment.is_zero_conf {
                "mempool".to_string()
            } else {
                payment.confirmations.to_string()
            })),
        ])))
    });
    html! {
        div id="live-payments" data-live data-part="payments" {
            @if !data.payments.is_empty() {
                div class="section" {
                    (ui::Table::new(ui::Slot::many(rows))
                        .head(ui::Row::new(ui::Slot::many(
                            ["Tx", "Amount", "Confirmations"]
                                .map(|name| ui::Slot::from(ui::Cell::new(name).header(true).attr("scope", "col"))),
                        )))
                        .caption("Individual payments received toward this order")
                        .scroll(false)
                        .attr("data-table", "payments"))
                }
            }
        }
    }
}

pub fn not_found_page(chrome: &PageChrome) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Order not found" }
            p { "This payment link is invalid, or the order it refers to no longer exists." }
        }
    };
    layout_bare(chrome, "Order not found", body)
}

/// Shown instead of the checkout when a store that only allows its checkout
/// on its verified domains has an order a browser page created, and it is
/// opened as a full page rather than inside the shop's own frame
/// (`http::checkout::must_open_from_shop`). Styled like [`not_found_page`],
/// plain HTML with nothing that needs JavaScript.
pub fn open_from_shop_page(chrome: &PageChrome) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Open this payment from the shop's website" }
            p {
                "This store only shows its checkout inside its own website. Go back to the shop and continue "
                "your payment there."
            }
        }
    };
    layout_bare(chrome, "Open this payment from the shop's website", body)
}

/// The view model [`share_page`] takes - unlike [`CheckoutViewModel`] this
/// page *does* carry the site nav (via [`super::layout`]).
/// The checkout the share page frames, in the viewer's own theme when they
/// are signed in and chose one (the framed page can't see their account).
fn share_frame_src(chrome: &PageChrome, data: &CheckoutShareViewModel) -> String {
    // Someone opening a link has no shop around it, so always the whole page,
    // whatever the store's layout: and never a custom page, which is its
    // merchant's words under this Monokulo's own address.
    let mut params = vec!["view=detailed".to_string()];
    match chrome.theme {
        crate::db::Theme::System => {}
        crate::db::Theme::Light => params.push("theme=light".to_string()),
        crate::db::Theme::Dark => params.push("theme=dark".to_string()),
    }
    // A signed-in viewer who chose a zone sees times in it; otherwise the
    // checkout shows the browser's own.
    if chrome.logged_in && !chrome.clock.is_automatic() {
        params.push(format!(
            "timezone={}",
            url::form_urlencoded::byte_serialize(chrome.clock.name().as_bytes())
                .collect::<String>()
        ));
    }
    format!("{}?{}", checkout_url(&data.order_id), params.join("&"))
}

pub struct CheckoutShareViewModel {
    pub order_id: crate::db::OrderId,
    /// Whether the order actually exists - `false` renders a real
    /// not-found state (still with the site's own nav around it, unlike
    /// [`not_found_page`]'s bare equivalent), rather than a page whose only
    /// content is a broken iframe.
    pub found: bool,
}

/// The title and the framed checkout are one card on the page. The card's
/// surface is the checkout's own background (`--paper-raised`), so the
/// frame's edge doesn't show and the payment reads as one panel.
const SHARE_STYLE: &str = r#"
.share-wrap { max-width: 840px; }
.share-card { margin-top: var(--space-md); background: var(--paper-raised); border: 1px solid var(--line); border-radius: var(--radius-lg); overflow: hidden; }
.share-frame {
  width: 100%;
  display: block;
  border: none;
  height: 620px;
}
"#;

/// Progressive enhancement only - `.share-frame`'s CSS `height` above is a
/// real, working fixed value either way. Same-origin (this page and the
/// framed checkout page are both served by monokulo), so reading the
/// frame's own content height directly is safe and reliable.
const SHARE_SCRIPT: &str = r#"
(function () {
  var frame = document.getElementById("checkout-frame");
  if (!frame) return;
  // The checkout's own box, not the frame's document: that is at least as
  // tall as the frame, so measuring it would never let the frame shrink.
  function content(doc) {
    return doc && (doc.getElementById("checkout-root") || doc.body);
  }
  function resize() {
    try {
      var el = content(frame.contentDocument);
      if (!el) return;
      var win = frame.contentWindow;
      var below = parseFloat(win.getComputedStyle(el).marginBottom) || 0;
      // The body's own padding and border under the checkout are part of
      // the page too; without them the frame stays that much short and
      // scrolls inside.
      if (el !== el.ownerDocument.body) {
        var body = win.getComputedStyle(el.ownerDocument.body);
        below += (parseFloat(body.paddingBottom) || 0) + (parseFloat(body.borderBottomWidth) || 0);
      }
      var height = Math.ceil(el.getBoundingClientRect().bottom + win.scrollY + below);
      if (height) frame.style.height = height + "px";
    } catch (e) { /* not ready yet, or something unexpected - keep the CSS fallback */ }
  }
  frame.addEventListener("load", function () {
    resize();
    try {
      var el = content(frame.contentDocument);
      if (el && typeof ResizeObserver === "function") {
        new ResizeObserver(resize).observe(el);
      }
    } catch (e) {}
  });
})();
"#;

pub fn share_page(chrome: &PageChrome, data: &CheckoutShareViewModel) -> Markup {
    let extra_head = html! { style { (PreEscaped(SHARE_STYLE)) } };
    let header = ui::PageHeader::new(if data.found {
        "Pay with Monero"
    } else {
        "Order not found"
    });
    let body = html! {
        @if data.found {
            (ui::Hint::new("Complete the payment below - this page stays up to date on its own, so it's safe to bookmark or come back to later."))
            section class="share-card" {
                iframe class="share-frame" id="checkout-frame" src=(share_frame_src(chrome, data)) title="Monero payment" {}
            }
            script { (PreEscaped(SHARE_SCRIPT)) }
        } @else {
            p { "This payment link is invalid, or the order it refers to no longer exists." }
        }
    };
    super::Page::new("Pay with Monero", header)
        .class("share-wrap")
        .head(extra_head)
        .render(chrome, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/checkout/pay_abc123")
    }

    fn test_checkout_view_model(is_terminal: bool) -> CheckoutViewModel {
        CheckoutViewModel {
            order_id: engine_protocol::ids::OrderId::new("pay_abc123".to_string()),
            store_name: "Corner shop".to_string(),
            description: None,
            merchant_reference: None,
            merchant_site: None,
            merchant_icon: true,
            status_label: if is_terminal { "Paid".to_string() } else { "Waiting for payment".to_string() },
            status: if is_terminal { engine_protocol::order_status::OrderStatus::Paid } else { engine_protocol::order_status::OrderStatus::Pending },
            address: "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC".to_string(),
            payment_uri: "monero:4abc?tx_amount=0.5".to_string(),
            qr_code_svg: crate::qr::QrSvg::for_test("<svg></svg>"),
            xmr_amount: "0.500000000000".to_string(),
            amount_due_xmr: "0.500000000000".to_string(),
            amount_received_xmr: "0.000000000000".to_string(),
            amount: "0.5".to_string(),
            currency: "XMR".to_string(),
            confirmations: 0,
            confirmations_required: 10,
            progress_percent: 0,
            is_terminal,
            double_spend_detected_at: None,
            clock: crate::views::time::Clock::utc(0),
            local_times: true,
            expires_in_display: "30m".to_string(),
            created_at: -1800,
            expires_at: 1800,
            refund_address: None,
            refund_address_locked: false,
            refund_address_error: None,
            layout: Layout::Detailed,
            style_hash: None,
            refund_enabled: true,
            query_suffix: String::new(),
            auto_refresh: true,
            toggled_refresh_suffix: "?refresh=false".to_string(),
            payment_error: None,
            payments: vec![],
        }
    }

    /// Without JavaScript the checkout still works: it refreshes itself
    /// once a minute from inside `<noscript>` only (with a link to stop
    /// it), the refund address has its own Save button, and there is no
    /// site nav or brand.
    #[test]
    fn checkout_page_works_without_javascript() {
        let mut data = test_checkout_view_model(false);
        for refund_address in [None, Some(data.address.clone())] {
            data.refund_address = refund_address;
            let html = checkout_page(&chrome(), &data).into_string();
            assert!(html.contains(
                r#"<noscript><meta http-equiv="refresh" content="60" id="checkout-refresh"></noscript>"#
            ));
            assert_eq!(
                html.matches("http-equiv=\"refresh\"").count(),
                1,
                "the refresh must only exist inside noscript, got: {html}"
            );
            assert!(
                html.contains(
                    r#"<a href="/checkout/pay_abc123?refresh=false">Auto Refresh: ON</a>"#
                ),
                "{html}"
            );
            assert!(html.contains(
                r#"<noscript><div class="form-actions"><button class="mk-btn" type="submit">Save</button></div></noscript>"#
            ));
            assert!(!html.contains(r#"<nav class="site-nav""#), "{html}");
            assert!(!html.contains("Monokulo"), "{html}");
        }
    }

    #[test]
    fn a_fiat_price_sent_with_padding_reads_in_its_currencys_usual_decimals() {
        let mut data = test_checkout_view_model(false);
        data.amount = "12.50000000".to_string();
        data.currency = "USD".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"<span class="fiat-amount">≈ 12.50 USD</span>"#),
            "{html}"
        );
    }

    #[test]
    fn checkout_page_stops_meta_refreshing_once_the_order_is_terminal() {
        let html = checkout_page(&chrome(), &test_checkout_view_model(true)).into_string();
        assert!(html.contains(&crate::assets::url("checkout.js")));
        assert!(
            !html.contains(r#"<meta http-equiv="refresh""#),
            "a paid/terminal order must not keep re-fetching itself, got: {html}"
        );
    }

    #[test]
    fn checkout_page_auto_refresh_toggle_turns_the_meta_refresh_off_and_back_on() {
        let mut data = test_checkout_view_model(false);
        data.auto_refresh = false;
        data.query_suffix = "?view=compact&refresh=false".to_string();
        data.toggled_refresh_suffix = "?view=compact".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            !html.contains(r#"http-equiv="refresh""#),
            "auto refresh off must drop the meta refresh, got: {html}"
        );
        assert!(
            html.contains(r#"<a href="/checkout/pay_abc123?view=compact">Auto Refresh: OFF</a>"#),
            "got: {html}"
        );
        assert!(
            html.contains(
                r#"action="/checkout/pay_abc123/refund-address?view=compact&amp;refresh=false""#
            ),
            "saving must keep auto refresh off, got: {html}"
        );

        let terminal = checkout_page(&chrome(), &test_checkout_view_model(true)).into_string();
        assert!(
            !terminal.contains("Auto Refresh"),
            "a terminal order never refreshes, so it has nothing to toggle, got: {terminal}"
        );
    }

    #[test]
    fn live_fragment_carries_every_changing_region_and_nothing_else() {
        let mut data = test_checkout_view_model(false);
        data.status = engine_protocol::order_status::OrderStatus::Partial;
        data.payments = vec![CheckoutPaymentViewModel {
            txid: format!("abcd12{}ef5678", "0".repeat(52)),
            amount_xmr: "0.200000000000".to_string(),
            confirmations: 0,
            is_zero_conf: true,
        }];
        let fragment = live_fragment(&data).into_string();
        for id in [
            "live-status",
            "live-pay",
            "live-address",
            "live-progress",
            "live-payments",
        ] {
            assert!(
                fragment.contains(&format!(r#"id="{id}""#)),
                "missing {id} in {fragment}"
            );
        }
        // The parts `live-pay` holds for a page from before they were split,
        // and the other four.
        assert_eq!(fragment.matches("data-live").count(), 9);
        assert!(fragment.contains("mempool"));
        assert!(
            !fragment.contains("refund_address"),
            "the refund form must never be swapped mid-edit"
        );
        // The QR code follows the amount due, so it is a live part too.
        assert!(
            fragment.contains(r#"<div class="qr-wrap qr-new">"#),
            "{fragment}"
        );
        // Every region in the fragment is the same markup the page itself renders.
        let page = checkout_page(&chrome(), &data).into_string();
        assert!(page.contains(
            &fragment[fragment.find("<span").unwrap()..fragment.find("</span>").unwrap()]
        ));
    }

    /// What the merchant typed (the order's description, its reference)
    /// reaches the payer escaped.
    #[test]
    fn the_checkout_escapes_what_the_merchant_typed() {
        let mut data = test_checkout_view_model(false);
        data.description = Some("2× Yirgacheffe <250g>".to_string());
        data.merchant_reference = Some("<INV-2041>".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("2× Yirgacheffe &lt;250g&gt;"), "{html}");
        assert!(html.contains("&lt;INV-2041&gt;"), "{html}");
        assert!(
            !html.contains("<250g>") && !html.contains("<INV-2041>"),
            "{html}"
        );
    }

    /// In every state the stage leads and the code stays on the page; the
    /// code fades once the payment is seen or the order is over. A part
    /// payment asks for what's left under a new code with the time left;
    /// a full one drops the deadline; an overpaid one explains the extra.
    #[test]
    fn the_code_fades_once_the_payment_is_seen_or_the_order_is_over() {
        for (status, faded) in [
            ("pending", false),
            ("partial", false),
            ("unconfirmed", true),
            ("confirming", true),
            ("paid", true),
            ("overpaid", true),
            ("expired", true),
            ("cancelled", true),
        ] {
            let mut data = test_checkout_view_model(!matches!(
                status,
                "pending" | "partial" | "unconfirmed" | "confirming"
            ));
            data.status = status.parse().expect("a known status");
            let html = checkout_page(&chrome(), &data).into_string();
            assert_eq!(html.contains("qr-wrap is-spent"), faded, "{status}: {html}");
            let stage = html
                .find(r#"class="stage-slot""#)
                .unwrap_or_else(|| panic!("{status}: no stage in {html}"));
            let code = html
                .find(r#"<div class="qr-wrap"#)
                .unwrap_or_else(|| panic!("{status}: no code in {html}"));
            assert!(
                stage < code,
                "{status}: the stage must come before the code"
            );
        }

        // Part paid: what's left, a new code for it, and the time left.
        let mut data = test_checkout_view_model(false);
        data.status = engine_protocol::order_status::OrderStatus::Partial;
        data.amount_received_xmr = "0.200000000000".to_string();
        data.amount_due_xmr = "0.300000000000".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("Still to pay"), "{html}");
        assert!(
            html.contains(r#"<span class="amount-value">0.3</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<div class="qr-wrap qr-new">"#), "{html}");
        assert!(html.contains(" left to pay</span>"), "{html}");
        assert!(html.contains("Send the remaining amount to"), "{html}");

        // Paid in full: no deadline.
        data.status = engine_protocol::order_status::OrderStatus::Confirming;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(!html.contains(" left to pay</span>"), "{html}");
        assert!(html.contains("Payment seen"), "{html}");

        // Overpaid: the extra explained, over the order total.
        data.status = engine_protocol::order_status::OrderStatus::Overpaid;
        data.is_terminal = true;
        data.amount_received_xmr = "0.600000000000".to_string();
        data.payment_error = Some("0.6 XMR received for a 0.5 XMR order (0.1 XMR extra). Do not send more. Contact the merchant about the extra amount.".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains("<strong>Paid, with extra.</strong>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="amount-value">0.5</span>"#),
            "{html}"
        );
        assert!(html
            .contains("0.6 XMR received for a 0.5 XMR order (0.1 XMR extra). Do not send more."));
        assert!(!html.contains(" left to pay</span>"), "{html}");
    }

    /// A cancelled order tells the payer not to pay; money that arrived
    /// anyway is acknowledged, with whom to ask about a refund.
    #[test]
    fn a_cancelled_order_says_not_to_pay_and_owns_up_to_money_sent_anyway() {
        let mut data = test_checkout_view_model(false);
        data.status = OrderStatus::Cancelled;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("This order was cancelled"));
        assert!(
            html.contains("Don&#39;t send to this address")
                || html.contains("Don't send to this address")
        );
        data.amount_received_xmr = "0.100000000000".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("0.1 XMR arrived after it was cancelled"));
        assert!(html.contains("contact the merchant about a refund"));
    }

    #[test]
    fn the_track_has_no_confirm_step_when_one_payment_is_enough() {
        let mut data = test_checkout_view_model(false);
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"<ol class="mk-track" aria-label="Payment progress">"#),
            "{html}"
        );
        assert!(html.contains("Confirm<span"));

        data.confirmations_required = 0;
        data.status = engine_protocol::order_status::OrderStatus::Unconfirmed;
        let html = checkout_page(&chrome(), &data).into_string();
        assert_eq!(html.matches("<li class=\"step-").count(), 2, "{html}");
        assert!(!html.contains("Confirm<span"), "{html}");
        // Seen is the last step before Paid, so Paid is where the order is.
        assert!(html.contains(r#"<li class="step-now"><span class="mk-track-dot" aria-hidden="true"></span><span class="mk-track-name">Paid<span class="visually-hidden">, now</span>"#), "{html}");
    }

    #[test]
    fn refund_flag_hides_the_entire_refund_section() {
        let mut data = test_checkout_view_model(false);
        data.refund_enabled = false;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(!html.contains("id=\"refund_address\""));
        assert!(!html.contains(&crate::assets::url("jsQR.js")));
        assert!(html.contains("id=\"confirmations-label\""));
    }

    #[test]
    fn checkout_page_shows_the_double_spend_stage_only_when_one_was_detected() {
        let mut data = test_checkout_view_model(false);
        assert!(!checkout_page(&chrome(), &data)
            .into_string()
            .contains("A payment was reversed"));
        data.double_spend_detected_at = Some(1_700_000_000);
        let html = checkout_page(&chrome(), &data).into_string();
        // Still owed: the stage says so, and the code stays live.
        assert!(html.contains("A payment was reversed"), "{html}");
        assert!(!html.contains(r#"<div class="qr-wrap is-spent">"#));
        // Otherwise: contact the merchant, and the code fades.
        data.status = engine_protocol::order_status::OrderStatus::Unconfirmed;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains("Payment reversed")
                && html.contains("Contact the merchant before sending anything.")
                && html.contains(r#"<div class="qr-wrap is-spent">"#),
            "{html}"
        );
        // Another payment covers it in full: paid, with a note of the
        // reversed one, not the reversed stage (the engine keeps the flag).
        data.amount_received_xmr = "0.500000000000".to_string();
        data.confirmations = 10;
        data.is_terminal = true;
        for (status, title) in [
            (OrderStatus::Paid, "<strong>Paid.</strong>"),
            (OrderStatus::Overpaid, "<strong>Paid, with extra.</strong>"),
        ] {
            data.status = status;
            let html = checkout_page(&chrome(), &data).into_string();
            assert!(html.contains(title), "{status}: {html}");
            assert_eq!(
                html.matches(r#"<li class="step-done">"#).count(),
                3,
                "{status}: every step done, got: {html}"
            );
            assert!(
                !html.contains("step-fail")
                    && !html.contains("Payment reversed")
                    && !html.contains("state-double-spend"),
                "{status}: {html}"
            );
            assert!(
                html.contains("so that payment doesn&#39;t count; another one covers this order.")
                    || html
                        .contains("so that payment doesn't count; another one covers this order."),
                "{status}: {html}"
            );
        }
    }

    /// Images load from this Monokulo, and from the verified site whose icon
    /// the page shows: not a site that isn't verified, and not on Tor.
    #[test]
    fn the_policy_lets_in_only_the_verified_sites_icon() {
        let images = |data: &CheckoutViewModel| {
            content_security_policy(data)
                .split("; ")
                .find(|directive| directive.starts_with("img-src"))
                .unwrap()
                .to_string()
        };
        let mut data = test_checkout_view_model(false);
        data.merchant_site = None;
        assert_eq!(images(&data), "img-src 'self' blob:");
        data.merchant_site = crate::stores::Site::parse("shop.example:8443").ok();
        data.merchant_icon = true;
        assert_eq!(
            images(&data),
            "img-src 'self' blob: https://shop.example:8443"
        );
        data.merchant_icon = false;
        assert_eq!(images(&data), "img-src 'self' blob:");
    }

    /// A plain track, and a line only when something is wrong.
    #[test]
    fn the_status_says_something_only_when_something_is_wrong() {
        use engine_protocol::order_status::OrderStatus::*;
        let line = |status, reversed: Option<i64>| {
            let mut data = test_checkout_view_model(false);
            data.status = status;
            data.double_spend_detected_at = reversed;
            stage_message(&data).map(|(title, _)| title)
        };
        for status in [Pending, Partial, Unconfirmed, Confirming, Paid] {
            assert_eq!(line(status, None), None, "{status:?}");
        }
        assert_eq!(line(Overpaid, None).as_deref(), Some("Paid, with extra"));
        assert_eq!(line(Expired, None).as_deref(), Some("This payment expired"));
        assert_eq!(
            line(Cancelled, None).as_deref(),
            Some("This order was cancelled")
        );
        assert_eq!(
            line(Pending, Some(1)).as_deref(),
            Some("A payment was reversed")
        );
        assert_eq!(
            line(Unconfirmed, Some(1)).as_deref(),
            Some("Payment reversed")
        );
        assert_eq!(line(Paid, Some(1)).as_deref(), Some("Paid"));
    }

    /// Every status a stock layout can be in renders each part it asks for,
    /// once, and the stream sends those by their ids.
    #[test]
    fn a_stock_layout_sends_each_of_its_live_parts_by_id() {
        let mut data = test_checkout_view_model(false);
        data.layout = Layout::Compact;
        let template = data.layout.template();
        let mut showing = Vec::new();
        let ids: Vec<String> = live_updates(template, &data, &mut showing)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            [
                "live-status",
                "live-amount",
                "live-fiat",
                "live-countdown",
                "live-qr",
                "live-address"
            ]
        );
        data.layout = Layout::Detailed;
        let ids: Vec<String> = live_updates(data.layout.template(), &data, &mut showing)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(ids.contains(&"live-payments".to_string()), "{ids:?}");
        assert!(ids.contains(&"live-progress".to_string()), "{ids:?}");
    }

    /// A region is sent when its condition flips, never because something in
    /// it changed.
    #[test]
    fn a_region_is_sent_only_when_it_flips() {
        let template = Template::load(
            "{{status}}{{if awaiting}}<p>Pay</p>{{end}}{{if final}}<p>Done</p>{{end}}",
        )
        .unwrap();
        let mut data = test_checkout_view_model(false);
        let mut showing = vec![true, false];
        let regions = |updates: Vec<(String, String)>| {
            updates
                .into_iter()
                .filter(|(id, _)| id.starts_with("mk-if"))
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        };
        assert!(regions(live_updates(&template, &data, &mut showing)).is_empty());
        data.confirmations = 3;
        assert!(regions(live_updates(&template, &data, &mut showing)).is_empty());
        data.status = OrderStatus::Paid;
        assert_eq!(
            regions(live_updates(&template, &data, &mut showing)),
            ["mk-if-0", "mk-if-1"]
        );
        assert_eq!(showing, [false, true]);
    }

    /// A stream reconnecting starts from what it last sent, not what the page
    /// loaded with: otherwise each reconnect would replace a region that had
    /// flipped since, losing whatever was typed in it.
    #[test]
    fn a_reconnecting_stream_starts_from_what_it_last_sent() {
        assert_eq!(
            page_showing(Some("01"), Some("10"), 2),
            Some(vec![false, true])
        );
        assert_eq!(page_showing(None, Some("10"), 2), Some(vec![true, false]));
        assert_eq!(
            page_showing(Some("x"), Some("10"), 2),
            Some(vec![true, false])
        );
        assert_eq!(page_showing(None, None, 2), None);
    }

    /// Once seen, the address says not to pay it again: a compact page has
    /// nothing else that does.
    #[test]
    fn the_address_says_a_payment_was_seen_while_it_confirms() {
        let label = |status| {
            let mut data = test_checkout_view_model(false);
            data.status = status;
            let html = live_address(&data).into_string();
            let start = html.find("id=\"address-label\">").unwrap() + 19;
            html[start..start + html[start..].find('<').unwrap()].to_string()
        };
        assert_eq!(label(OrderStatus::Pending), "Send exactly this amount to");
        assert_eq!(
            label(OrderStatus::Unconfirmed),
            "Payment seen. Don't send it again"
        );
        assert_eq!(
            label(OrderStatus::Confirming),
            "Payment seen. Don't send it again"
        );
        assert_eq!(label(OrderStatus::Paid), "Payment address");
    }

    #[test]
    fn the_page_says_which_regions_it_showed() {
        assert_eq!(showing_bits(&[true, false, true]), "101");
        assert_eq!(parse_showing("101", 3), Some(vec![true, false, true]));
        assert_eq!(parse_showing("101", 2), None);
        assert_eq!(parse_showing("1x1", 3), None);
        assert_eq!(parse_showing("", 0), Some(vec![]));
    }

    /// Tiny swaps the code for words once nothing is owed, in the same place.
    #[test]
    fn tiny_shows_the_code_only_while_money_is_owed() {
        use engine_protocol::order_status::OrderStatus::*;
        let parts = |status, reversed: Option<i64>| {
            let mut data = test_checkout_view_model(false);
            data.layout = Layout::Tiny;
            data.status = status;
            data.double_spend_detected_at = reversed;
            let mut showing = Vec::new();
            live_updates(data.layout.template(), &data, &mut showing)
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| !id.starts_with("mk-if"))
                .collect::<Vec<_>>()
        };
        for status in [Pending, Partial] {
            assert_eq!(
                parts(status, None),
                [
                    "live-status",
                    "live-amount",
                    "live-fiat",
                    "live-qr",
                    "live-address"
                ]
            );
        }
        for status in [Unconfirmed, Confirming, Paid, Overpaid, Expired, Cancelled] {
            assert_eq!(
                parts(status, None),
                ["live-status", "live-amount", "live-fiat", "live-message"],
                "{status:?}"
            );
        }
    }

    #[test]
    fn tinys_message_says_what_to_do_in_each_state_with_its_caveats() {
        use engine_protocol::order_status::OrderStatus::*;
        let said = |status, terminal: bool, received: &str, reversed: Option<i64>| {
            let mut data = test_checkout_view_model(terminal);
            data.status = status;
            data.amount_received_xmr = received.to_string();
            data.double_spend_detected_at = reversed;
            let html = live_message(&data).into_string();
            html[html.find('>').unwrap() + 1..html.rfind('<').unwrap()].to_string()
        };
        assert_eq!(said(Pending, false, "0", None), "");
        assert_eq!(said(Paid, true, "0.5", None), "Paid. Nothing more to do.");
        assert_eq!(
            said(Expired, false, "0", None),
            "This payment expired. Don&#39;t send to this address. If you already sent it, keep this page open."
                .replace("&#39;", "'")
        );
        assert_eq!(
            said(Expired, true, "0", None),
            "This payment expired. Don't send to this address."
        );
        assert_eq!(
            said(Cancelled, true, "0.1", None),
            "This order was cancelled. Don't send more; contact the merchant."
        );
        assert_eq!(
            said(Unconfirmed, false, "0", Some(1)),
            "Payment reversed. Contact the merchant."
        );
        assert_eq!(
            said(Paid, true, "0.5", Some(1)),
            "Paid. Nothing more to do."
        );
    }

    /// In a small frame the code opens the wallet, only while it can be paid.
    #[test]
    fn a_small_code_links_to_the_wallet_only_while_it_can_be_paid() {
        let mut data = test_checkout_view_model(false);
        let link = r#"href="monero:4abc?tx_amount=0.5""#;
        assert!(live_qr(&data, Some("small")).into_string().contains(link));
        assert!(!live_qr(&data, None).into_string().contains(link));
        data.status = OrderStatus::Paid;
        assert!(!live_qr(&data, Some("small")).into_string().contains(link));
    }

    /// A payment reversed while money is still owed says so, beside the code.
    #[test]
    fn tiny_says_a_reversed_payment_doesnt_count_while_money_is_owed() {
        let mut data = test_checkout_view_model(false);
        data.layout = Layout::Tiny;
        data.status = OrderStatus::Partial;
        data.double_spend_detected_at = Some(1);
        let mut showing = Vec::new();
        let ids: Vec<String> = live_updates(data.layout.template(), &data, &mut showing)
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !id.starts_with("mk-if"))
            .collect();
        assert_eq!(
            ids,
            [
                "live-status",
                "live-amount",
                "live-fiat",
                "live-message",
                "live-qr",
                "live-address"
            ]
        );
        assert!(live_message(&data)
            .into_string()
            .contains("A payment was reversed. It doesn't count."));
    }

    /// Whatever a template does, a spent address has nothing to copy it with.
    #[test]
    fn a_short_address_stops_offering_copy_once_it_shouldnt_be_paid() {
        let mut data = test_checkout_view_model(false);
        let copy = "Copy the full address";
        assert!(live_address_short(&data).into_string().contains(copy));
        data.status = OrderStatus::Paid;
        assert!(!live_address_short(&data).into_string().contains(copy));
    }
}
