//! `http/checkout.rs` - the real, public checkout/payment page
//! (`GET /api/v1/checkout/{order_id}`), its "order not found" fallback,
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
    /// While part paid, what was due before the latest payment, struck through
    /// beside what's due now so the change is seen whenever the payer looks
    /// back. `None` if a reversal means it can't be known.
    pub previous_due_xmr: Option<String>,
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
    /// Which version of a custom page this is, if it's on one.
    pub page_token: Option<String>,
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

/// The order's event stream, routed part by part, with the page's own
/// options.
fn events_url(data: &CheckoutViewModel, template: &Template) -> String {
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
        showing_bits(&template.regions_showing(&template_state(data))),
    ) + &data
        .page_token
        .as_ref()
        .map_or_else(String::new, |token| format!("&page={token}"))
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
        Feature::Status if part.option == Some("icon") => live_status_icon(data, false),
        Feature::Status if part.option == Some("label") => live_status_icon(data, true),
        Feature::Status if part.option == Some("track") => live_track(data),
        Feature::Status => live_status(data),
        Feature::Message if part.option == Some("full") => live_message_full(data),
        Feature::Message => live_message(data),
        Feature::Amount => live_amount(data, part.option == Some("bare")),
        Feature::Fiat => live_fiat(data),
        Feature::Countdown => live_countdown(data, part.option == Some("inline")),
        Feature::Qr => live_qr(data, part.option),
        Feature::Address if part.option == Some("short") => live_address_short(data),
        Feature::Address => live_address(data, part.option == Some("plain")),
        Feature::Progress => live_progress(data, part.option != Some("text")),
        Feature::Refund => refund_part(data),
        Feature::Payments => live_payments(data, part.option == Some("folded")),
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

/// The Content-Security-Policy of a preview of a draft: nothing runs, nothing can
/// go anywhere, and it's framed only by this Monokulo's own pages. `sandbox` gives
/// it no origin of its own, so its typeface can't load, and it shows in the
/// customer's device font.
pub fn preview_policy() -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let fonts = base64::engine::general_purpose::STANDARD
        .encode(sha2::Sha256::digest(super::font_faces().as_bytes()));
    format!(
        "sandbox; default-src 'none'; style-src 'self' 'sha256-{fonts}'; font-src 'self'; img-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'"
    )
}

/// A made-up order on a draft's template, with its CSS. Runs nothing.
pub fn preview_page(
    chrome: &PageChrome,
    data: &CheckoutViewModel,
    template: &Template,
    page_css: Option<&str>,
) -> Markup {
    page(chrome, data, template, page_css, false)
}

pub fn checkout_page(chrome: &PageChrome, data: &CheckoutViewModel) -> Markup {
    page(chrome, data, data.layout.template(), None, true)
}

/// A store's custom page on a real order.
pub fn custom_page(
    chrome: &PageChrome,
    data: &CheckoutViewModel,
    template: &Template,
    page_css: Option<&str>,
) -> Markup {
    page(chrome, data, template, page_css, true)
}

fn page(
    chrome: &PageChrome,
    data: &CheckoutViewModel,
    template: &Template,
    page_css: Option<&str>,
    scripts: bool,
) -> Markup {
    let extra_head = html! {
        // Without JavaScript this is the only update path; with it,
        // `checkout.js` streams changes in place instead (`noscript` content
        // is never parsed as markup when scripting is on).
        @if scripts && !data.is_terminal && data.auto_refresh {
            noscript {
                // A custom page says so as it refreshes: to the browser that's no
                // longer a frame of another site.
                @if data.page_token.is_some() {
                    meta http-equiv="refresh" content=(format!("60;url={}{}", checkout_url(&data.order_id), data.query_suffix)) id="checkout-refresh";
                } @else {
                    meta http-equiv="refresh" content="60" id="checkout-refresh";
                }
            }
        }
        (super::stylesheet("status.css"))
        (super::stylesheet("checkout.css"))
        // The store's colours and shape, over the theme.
        @if let Some(hash) = &data.style_hash {
            link rel="stylesheet" href=(crate::base::url(format!("/static-style/{hash}.css")));
        }
        // A custom page's own, last, so it has the last word on style.
        @if let Some(hash) = page_css {
            link rel="stylesheet" href=(crate::base::url(format!("/static-style/{hash}.css")));
        }
        @if scripts {
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
        }
    };
    let body = html! {
        div class=(match data.layout { _ if data.page_token.is_some() => "pay-wrap checkout-custom", Layout::Detailed => "pay-wrap checkout-detailed", Layout::Compact => "pay-wrap checkout-compact", Layout::Tiny => "pay-wrap checkout-tiny" }) id="checkout-root" data-order-id=(data.order_id) data-status=(data.status) data-confirmations=(data.confirmations) data-error=(data.payment_error.as_deref().unwrap_or("")) {
            h1 class="sr-only" { "Monero payment of " (data.xmr_amount) " XMR" }
            // Always here, and only its text replaced, so a screen reader
            // announces each change of status.
            p id="checkout-announce" class="sr-only" role="status" { (data.status_label) }
            @if scripts && !data.is_terminal {
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
            @if scripts && !data.is_terminal {
                span id="checkout-stream" hidden fx-action=(events_url(data, template)) fx-trigger="fx:inited" fx-swap="none" fx-sse-reconnect fx-own-errors {}
            }

            // What the status symbol draws with.
            (PreEscaped(ui::assets::STATUS_SYMBOLS))
            (PreEscaped(page_body(template, data)))
        }
        @if scripts { (script("checkout.js", Load::Now)) }
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
        (live_address(data, false))
        (live_progress(data, true))
        (live_payments(data, false))
    }
}

/// The same parts one by one, with the id each replaces, for the stream
/// that routes each to its element (`http::checkout::checkout_events`).
pub fn live_parts(data: &CheckoutViewModel) -> [(&'static str, Markup); 5] {
    [
        ("live-status", live_status(data)),
        ("live-pay", live_pay(data)),
        ("live-address", live_address(data, false)),
        ("live-progress", live_progress(data, true)),
        ("live-payments", live_payments(data, false)),
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

/// How much of each step of the track is done, in percent, in its order.
fn track_fills(data: &CheckoutViewModel) -> [u8; 3] {
    let amount = |xmr: &str| xmr.parse::<f64>().unwrap_or(0.0);
    let total = amount(&data.xmr_amount);
    let share = |part: f64| {
        if total > 0.0 {
            (part / total * 100.0).round().clamp(0.0, 100.0) as u8
        } else {
            0
        }
    };
    if reversed_at(data.status, data.double_spend_detected_at).is_some() && !awaiting_payment(data)
    {
        return [100, 0, 0];
    }
    match data.status {
        OrderStatus::Pending => [0, 0, 0],
        OrderStatus::Partial | OrderStatus::Expired | OrderStatus::Cancelled => {
            [share(amount(&data.amount_received_xmr)), 0, 0]
        }
        OrderStatus::Unconfirmed => [100, 0, 0],
        OrderStatus::Confirming => [100, data.progress_percent.min(100), 0],
        OrderStatus::Paid | OrderStatus::Overpaid => [100, 100, 100],
    }
}

/// The track's steps, each filled by how much of it is done, with the count in
/// Confirm's name so nothing moves as a payment confirms. The fill is an SVG's own
/// attribute: the page's policy allows no `style`.
fn track_steps(data: &CheckoutViewModel) -> Vec<ui::ProgressStep> {
    let [send, confirm, paid] = track_fills(data);
    let fills = if data.confirmations_required == 0 {
        vec![send, paid]
    } else {
        vec![send, confirm, paid]
    };
    let reversed = reversed_at(data.status, data.double_spend_detected_at).is_some();
    let counting = data.status == OrderStatus::Confirming && !reversed;
    stage_steps(data)
        .into_iter()
        .zip(fills)
        .map(|(mut step, fill)| {
            if counting && step.label == "Confirm" {
                step.label = format!(
                    "Confirm ({}/{})",
                    data.confirmations, data.confirmations_required
                );
            }
            step.icon(html! {
                svg class="mk-track-fill" width="100%" height="100%" aria-hidden="true" { rect width=(format!("{fill}%")) height="100%" {} }
            })
        })
        .collect()
}

/// The track alone, each step a bar filled by how much of it is done. What's
/// gone wrong is `{{message full}}`'s.
fn live_track(data: &CheckoutViewModel) -> Markup {
    let reversed = reversed_at(data.status, data.double_spend_detected_at).is_some();
    let shown = super::order_state(data.status).1;
    let state = if reversed {
        ui::OrderState::DoubleSpend
    } else {
        shown
    };
    html! {
        div id="live-status" class="stage-slot" data-live data-part="status" {
            span id="status-badge" class=(format!("sr-only state-{shown}")) { (data.status_label) }
            (ui::ProgressSteps::new("Payment progress", track_steps(data)).state(state).plain(true).bars(true))
        }
    }
}

/// The status as the POS's symbol, for a small frame: with its word when
/// `word`, and otherwise the word only for a screen reader.
fn live_status_icon(data: &CheckoutViewModel, word: bool) -> Markup {
    let shown = super::order_state(data.status).1;
    let reversed = reversed_at(data.status, data.double_spend_detected_at).is_some();
    let state = if reversed {
        ui::OrderState::DoubleSpend
    } else {
        shown
    };
    let wedge = (state == ui::OrderState::Confirming).then(|| confirming_wedge(data));
    let short = if reversed {
        "Reversed"
    } else {
        super::order_state_short(data.status)
    };
    html! {
        span id="live-status" class=(format!("status-symbol state-{state}")) data-live data-part="status" {
            (ui::StatusIcon::new(state).maybe_progress(wedge))
            @if word {
                span id="status-badge" class="status-word" { (short) }
            } @else {
                span id="status-badge" class=(format!("sr-only state-{shown}")) { (data.status_label) }
            }
        }
    }
}

/// The POS's confirming wedge: at least 20%, rising in 10% steps, full at the
/// threshold.
fn confirming_wedge(data: &CheckoutViewModel) -> u32 {
    if data.confirmations_required == 0 {
        return 100;
    }
    let tenths = data
        .confirmations
        .saturating_mul(10)
        .div_ceil(data.confirmations_required);
    u32::try_from(tenths.saturating_mul(10).clamp(20, 100)).unwrap_or(100)
}

/// A few words on what to do now, for a small frame.
fn live_message(data: &CheckoutViewModel) -> Markup {
    let received = super::trim_xmr(&data.amount_received_xmr);
    let message = if reversed_at(data.status, data.double_spend_detected_at).is_some() {
        if awaiting_payment(data) {
            "A payment was reversed. It doesn't count.".to_string()
        } else {
            "Contact the merchant.".to_string()
        }
    } else {
        match data.status {
            OrderStatus::Pending => "Scan or copy to pay.".to_string(),
            OrderStatus::Partial => "Send the remaining amount".to_string(),
            OrderStatus::Unconfirmed => "Payment seen".to_string(),
            OrderStatus::Confirming => format!(
                "Payment confirmed ({} of {})",
                data.confirmations, data.confirmations_required
            ),
            OrderStatus::Paid => "Nothing more to do.".to_string(),
            OrderStatus::Overpaid => "Don't send more. Contact the merchant.".to_string(),
            OrderStatus::Expired | OrderStatus::Cancelled => {
                if received != "0" {
                    "Don't send more. Contact the merchant.".to_string()
                } else if data.is_terminal {
                    "Don't send to this address.".to_string()
                } else {
                    "Don't send!\nAlready sent? Keep this page open.".to_string()
                }
            }
        }
    };
    html! {
        p id="live-message" class="live-message" data-live data-part="message" role="status" { (message) }
    }
}

/// What's gone wrong and what to do now, in full, when it has; nothing otherwise.
fn live_message_full(data: &CheckoutViewModel) -> Markup {
    let reversed = reversed_at(data.status, data.double_spend_detected_at).is_some();
    let state = if reversed {
        ui::OrderState::DoubleSpend
    } else {
        super::order_state(data.status).1
    };
    html! {
        div id="live-message" class=(format!("stage-note state-{state}")) data-live data-part="message" {
            @if let Some((title, message)) = stage_message(data) {
                p role="status" { strong { (title) "." } " " (message) }
            }
        }
    }
}

/// The address shortened, for a small frame, with a copy symbol giving the
/// whole of it, in every state. Muted once it shouldn't be paid. Without
/// JavaScript, it's selectable and focus shows it in full.
fn live_address_short(data: &CheckoutViewModel) -> Markup {
    let class = if awaiting_payment(data) {
        "address-short"
    } else {
        "address-short is-spent"
    };
    html! {
        div id="live-address" class=(class) data-live data-part="address" {
            (ui::CopyValue::new(data.address.as_str())
                .copy_label("Copy the full address")
                .truncate(ui::Truncate::Middle)
                .head(6usize)
                .tail(6usize)
                .icon(true))
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
    // Without JavaScript nothing saves as you type: a Save button sits
    // where the scan and image buttons would.
    let save = html! {
        noscript { (ui::Button::new("Save").kind(ui::ButtonType::Submit).size(ui::ControlSize::Compact)) }
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
        save.into(),
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
            @if let (OrderStatus::Partial, Some(was)) = (data.status, &data.previous_due_xmr) {
                s class="amount-was" {
                    span class="visually-hidden" { "Was " }
                    (super::trim_xmr(was)) " XMR"
                }
            }
            // The number and its unit never split; the font shrinks with
            // the frame instead (see `.amount`).
            span class="amount" id="xmr-amount" aria-labelledby=[(!bare).then_some("amount-label")] {
                span class="amount-value" { (amount) } " " span class="amount-unit" { "XMR" }
                // With no label to say so; on screen, the struck amount does.
                @if bare && data.status == OrderStatus::Partial { " " span class="visually-hidden" { "still to pay" } }
            }
        }
    }
}

/// What's left of the quoted price, in proportion to the XMR left, written to
/// the same places as the quote.
fn remaining_fiat(data: &CheckoutViewModel) -> Option<String> {
    let total = data
        .xmr_amount
        .parse::<f64>()
        .ok()
        .filter(|total| *total > 0.0)?;
    let due = data.amount_due_xmr.parse::<f64>().ok()?;
    let quoted = data.amount.parse::<f64>().ok()?;
    let places = data
        .amount
        .split_once('.')
        .map_or(0, |(_, places)| places.len());
    Some(format!("{:.*}", places, quoted * due / total))
}

/// A fiat equivalent, only for an order priced in fiat: of what's left, while
/// part paid.
fn live_fiat(data: &CheckoutViewModel) -> Markup {
    let amount = if data.status == OrderStatus::Partial {
        remaining_fiat(data)
    } else {
        Some(data.amount.clone())
    };
    html! {
        span id="live-fiat" data-live data-part="fiat" {
            @if let Some(amount) = amount.filter(|_| priced_in_fiat(data)) {
                span class="fiat-amount" { "≈ " (super::display_amount(&amount, &data.currency)) }
            }
        }
    }
}

/// The time left to pay, while money is owed.
fn live_countdown(data: &CheckoutViewModel, inline: bool) -> Markup {
    html! {
        div id="live-countdown" data-live data-part="countdown" {
            @if awaiting_payment(data) {
                div class="countdown-slot" { (countdown(data).inline(inline)) }
            }
        }
    }
}

/// The code, always in the same place. After a partial payment it's redrawn
/// for what's left; once it shouldn't be paid (the payment is in, or the order
/// is over) it fades (`.is-spent`) under a symbol of why, so nothing invites a
/// second payment.
fn live_qr(data: &CheckoutViewModel, size: Option<&str>) -> Markup {
    let state = if awaiting_payment(data) {
        ""
    } else {
        " is-spent"
    };
    let small = matches!(size, Some("small" | "medium"));
    let size = size.map_or(String::new(), |size| format!(" qr-{size}"));
    html! {
        div id="live-qr" data-live data-part="qr" {
            // In a small frame, a tap opens the customer's wallet, with or
            // without JavaScript: `_blank`, as an embed's sandbox lets a popup
            // leave the frame but not the frame itself.
            @if small && awaiting_payment(data) {
                a class=(format!("qr-wrap{state}{size}")) href=(data.payment_uri) target="_blank" rel="noopener noreferrer" aria-label="Open in your wallet" { (data.qr_code_svg) }
            } @else {
                div class=(format!("qr-wrap{state}{size}")) {
                    (data.qr_code_svg)
                    @if !awaiting_payment(data) { (spent_mark(data)) }
                }
            }
        }
    }
}

/// Over a code that shouldn't be paid, why: a tick once the payment is in, a
/// cross once the order is over, an exclamation mark if a payment was reversed.
fn spent_mark_kind(data: &CheckoutViewModel) -> (&'static str, &'static str) {
    let reversed = reversed_at(data.status, data.double_spend_detected_at).is_some();
    match data.status {
        _ if reversed => ("reversed", "M12 6v8M12 18v.5"),
        OrderStatus::Expired | OrderStatus::Cancelled => ("over", "M7 7l10 10M17 7 7 17"),
        OrderStatus::Unconfirmed | OrderStatus::Confirming => ("confirming", "m4.5 12.5 5 5 10-11"),
        OrderStatus::Overpaid => ("overpaid", "m4.5 12.5 5 5 10-11"),
        _ => ("paid", "m4.5 12.5 5 5 10-11"),
    }
}

/// The mark itself: [`spent_mark_kind`] in its circle.
fn spent_mark(data: &CheckoutViewModel) -> Markup {
    let (kind, symbol) = spent_mark_kind(data);
    html! {
        span class="qr-mark" data-mark=(kind) aria-hidden="true" {
            svg viewBox="0 0 24 24" { path d=(symbol) {} }
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
            (live_countdown(data, false))
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
    let remaining = data
        .clock
        .now()
        .until(clock::UnixTime::from_secs(data.expires_at))
        .as_secs();
    let total = clock::UnixTime::from_secs(data.created_at)
        .until(clock::UnixTime::from_secs(data.expires_at))
        .as_secs()
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
/// it. Muted once it shouldn't be paid, still with its Copy button: a live
/// part, so that follows the order.
fn live_address(data: &CheckoutViewModel, plain: bool) -> Markup {
    const GROUP: usize = 6;
    let open = awaiting_payment(data);
    let mut address = ui::GroupedValue::of(data.address.as_str(), GROUP)
        .copy_label("Copy address")
        .plain(plain);
    if !open {
        address = address.attr("data-spent", "");
    }
    html! {
        div class="address-block" id="live-address" data-live data-part="address" {
            span class="address-label" id="address-label" {
                @if data.status == OrderStatus::Partial { "Send the remaining amount to" }
                @else if data.status == OrderStatus::Pending { "Send exactly this amount to" }
                @else { "Payment address" }
            }
            (address)
        }
    }
}

/// What's confirmed and what's come in, with its bar when `bar`.
fn live_progress(data: &CheckoutViewModel, bar: bool) -> Markup {
    html! {
        div class="section" id="live-progress" data-live data-part="progress" {
            div class="progress-row" {
                span id="confirmations-label" { (data.confirmations) " / " (data.confirmations_required) " confirmations" }
                span id="received-label" { (super::trim_xmr(&data.amount_received_xmr)) " XMR received" }
            }
            @if bar {
                progress class="progress-bar" id="progress-bar" aria-labelledby="confirmations-label"
                    max="100" value=(data.progress_percent)
                    aria-valuetext=(format!("{} of {} confirmations", data.confirmations, data.confirmations_required)) {
                    (data.progress_percent) "%"
                }
            }
        }
    }
}

/// The payments in, behind a disclosure when `folded`.
fn live_payments(data: &CheckoutViewModel, folded: bool) -> Markup {
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
                @let table = ui::Table::new(ui::Slot::many(rows))
                    .head(ui::Row::new(ui::Slot::many(
                        ["Tx", "Amount", "Confirmations"]
                            .map(|name| ui::Slot::from(ui::Cell::new(name).header(true).attr("scope", "col"))),
                    )))
                    .caption("Individual payments received toward this order")
                    .scroll(false)
                    .attr("data-table", "payments");
                div class="section" {
                    @if folded {
                        (ui::Disclosure::new(format!("Payments ({})", data.payments.len()), html! { (table) }))
                    } @else {
                        (table)
                    }
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
///
/// The page is as wide as the checkout's one column, and short of the 560px
/// where a short frame goes to two: this frame takes its content's height, so
/// it would always look short, and the page scrolls however tall it gets. The
/// fixed height is the frame the client gives Detailed, for no JavaScript.
const SHARE_STYLE: &str = r#"
.share-wrap { max-width: 500px; }
.share-card { margin-top: var(--space-md); background: var(--paper-raised); border: 1px solid var(--line); border-radius: var(--radius-lg); overflow: hidden; }
.share-frame {
  width: 100%;
  display: block;
  border: none;
  height: 880px;
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
        PageChrome::from_user(
            None,
            "/api/v1/checkout/pay_abc123",
            clock::Clock::System.now(),
        )
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
            previous_due_xmr: None,
            amount_received_xmr: "0.000000000000".to_string(),
            amount: "0.5".to_string(),
            currency: "XMR".to_string(),
            confirmations: 0,
            confirmations_required: 10,
            progress_percent: 0,
            is_terminal,
            double_spend_detected_at: None,
            clock: crate::views::time::Clock::utc(clock::UnixTime::EPOCH),
            local_times: true,
            expires_in_display: "30m".to_string(),
            created_at: -1800,
            expires_at: 1800,
            refund_address: None,
            refund_address_locked: false,
            refund_address_error: None,
            layout: Layout::Detailed,
            style_hash: None,
            page_token: None,
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
    /// it), the refund address has its own Save button inside its box, and
    /// there is no site nav or brand.
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
                    r#"<a href="/api/v1/checkout/pay_abc123?refresh=false">Auto Refresh: ON</a>"#
                ),
                "{html}"
            );
            assert!(html.contains(
                r#"<noscript><button class="mk-btn" type="submit" data-size="compact">Save</button></noscript></span></span>"#
            ), "{html}");
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
            html.contains(
                r#"<a href="/api/v1/checkout/pay_abc123?view=compact">Auto Refresh: OFF</a>"#
            ),
            "got: {html}"
        );
        assert!(
            html.contains(
                r#"action="/api/v1/checkout/pay_abc123/refund-address?view=compact&amp;refresh=false""#
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
        assert!(fragment.contains(r#"<div class="qr-wrap">"#), "{fragment}");
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
        assert!(html.contains("still to pay"), "{html}");
        assert!(
            html.contains(r#"<span class="amount-value">0.3</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<div class="qr-wrap">"#), "{html}");
        assert!(!html.contains("amount-was"), "{html}");
        data.previous_due_xmr = Some("0.500000000000".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("Was </span>0.5 "), "{html}");
        assert!(html.contains(" left to pay</span>"), "{html}");
        assert!(html.contains("Send the remaining amount to"), "{html}");
        assert!(!html.contains("Wallets that read"), "{html}");

        // Paid in full: no deadline.
        data.status = engine_protocol::order_status::OrderStatus::Confirming;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(!html.contains(" left to pay</span>"), "{html}");
        assert!(!html.contains("Don't send it again"), "{html}");

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
        let now = html.find(r#"<li class="step-now">"#).expect("a step now");
        assert!(
            html[now..].contains(r#"<span class="mk-track-name">Paid"#),
            "{html}"
        );
    }

    #[test]
    fn the_track_shows_how_much_of_each_step_is_done() {
        let mut data = test_checkout_view_model(false);
        data.xmr_amount = "1.000000000000".to_string();
        data.amount_received_xmr = "0.250000000000".to_string();
        data.status = OrderStatus::Partial;
        assert_eq!(track_fills(&data), [25, 0, 0]);

        data.status = OrderStatus::Confirming;
        data.confirmations = 2;
        data.confirmations_required = 10;
        data.progress_percent = 20;
        assert_eq!(track_fills(&data), [100, 20, 0]);
        let labels: Vec<String> = track_steps(&data).into_iter().map(|s| s.label).collect();
        assert_eq!(labels, ["Send", "Confirm (2/10)", "Paid"]);

        data.status = OrderStatus::Paid;
        assert_eq!(track_fills(&data), [100, 100, 100]);
    }

    #[test]
    fn part_paid_shows_what_is_left_of_the_quoted_price() {
        let mut data = test_checkout_view_model(false);
        data.status = OrderStatus::Partial;
        data.currency = "USD".to_string();
        data.amount = "84.00".to_string();
        data.xmr_amount = "1.000000000000".to_string();
        data.amount_due_xmr = "0.250000000000".to_string();
        assert!(live_fiat(&data).into_string().contains("21.00 USD"));
        data.status = OrderStatus::Pending;
        assert!(live_fiat(&data).into_string().contains("84.00 USD"));
    }

    #[test]
    fn an_overpaid_order_marks_its_code_apart_from_a_paid_one() {
        let mut data = test_checkout_view_model(true);
        data.status = OrderStatus::Overpaid;
        assert_eq!(spent_mark_kind(&data).0, "overpaid");
        data.status = OrderStatus::Paid;
        assert_eq!(spent_mark_kind(&data).0, "paid");
    }

    #[test]
    fn the_full_message_says_nothing_until_something_has_gone_wrong() {
        let mut data = test_checkout_view_model(false);
        data.status = OrderStatus::Confirming;
        assert!(stage_message(&data).is_none());
        data.status = OrderStatus::Expired;
        assert_eq!(stage_message(&data).unwrap().0, "This payment expired");
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
                "live-amount",
                "live-fiat",
                "live-status",
                "live-qr",
                "live-address",
                "live-message"
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
            let html = live_address(&data, false).into_string();
            let start = html.find("id=\"address-label\">").unwrap() + 19;
            html[start..start + html[start..].find('<').unwrap()].to_string()
        };
        assert_eq!(label(OrderStatus::Pending), "Send exactly this amount to");
        assert_eq!(label(OrderStatus::Unconfirmed), "Payment address");
        assert_eq!(label(OrderStatus::Confirming), "Payment address");
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

    /// Tiny keeps the same parts in every state, so nothing moves.
    #[test]
    fn tiny_shows_the_same_parts_in_every_state() {
        use engine_protocol::order_status::OrderStatus::*;
        for status in [
            Pending,
            Partial,
            Unconfirmed,
            Confirming,
            Paid,
            Overpaid,
            Expired,
            Cancelled,
        ] {
            let mut data = test_checkout_view_model(false);
            data.layout = Layout::Tiny;
            data.status = status;
            let mut showing = Vec::new();
            let ids: Vec<String> = live_updates(data.layout.template(), &data, &mut showing)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            assert_eq!(
                ids,
                [
                    "live-qr",
                    "live-amount",
                    "live-fiat",
                    "live-status",
                    "live-message",
                    "live-address"
                ],
                "{status:?}"
            );
        }
    }

    #[test]
    fn the_message_says_what_to_do_in_each_state_with_its_caveats() {
        use engine_protocol::order_status::OrderStatus::*;
        let said = |status, terminal: bool, received: &str, reversed: Option<i64>| {
            let mut data = test_checkout_view_model(terminal);
            data.status = status;
            data.amount_received_xmr = received.to_string();
            data.double_spend_detected_at = reversed;
            let html = live_message(&data).into_string();
            html[html.find('>').unwrap() + 1..html.rfind('<').unwrap()].to_string()
        };
        assert_eq!(said(Pending, false, "0", None), "Scan or copy to pay.");
        assert_eq!(
            said(Partial, false, "0.1", None),
            "Send the remaining amount"
        );
        assert_eq!(said(Paid, true, "0.5", None), "Nothing more to do.");
        assert_eq!(
            said(Expired, false, "0", None),
            "Don't send!\nAlready sent? Keep this page open."
        );
        assert_eq!(
            said(Expired, true, "0", None),
            "Don't send to this address."
        );
        assert_eq!(
            said(Cancelled, true, "0.1", None),
            "Don't send more. Contact the merchant."
        );
        assert_eq!(
            said(Unconfirmed, false, "0", Some(1)),
            "Contact the merchant."
        );
        assert_eq!(said(Paid, true, "0.5", Some(1)), "Nothing more to do.");
    }

    /// Over a code that shouldn't be paid, a symbol says why, and not over one
    /// that can be.
    #[test]
    fn a_spent_code_carries_the_symbol_of_why() {
        use engine_protocol::order_status::OrderStatus::*;
        let mark = |status, reversed: Option<i64>| {
            let mut data = test_checkout_view_model(false);
            data.status = status;
            data.double_spend_detected_at = reversed;
            let html = live_qr(&data, Some("small")).into_string();
            html.split_once("data-mark=\"")
                .map(|(_, rest)| rest[..rest.find('"').unwrap()].to_string())
        };
        assert_eq!(mark(Pending, None), None);
        assert_eq!(mark(Partial, None), None);
        assert_eq!(mark(Confirming, None).as_deref(), Some("confirming"));
        assert_eq!(mark(Paid, None).as_deref(), Some("paid"));
        assert_eq!(mark(Expired, None).as_deref(), Some("over"));
        assert_eq!(mark(Unconfirmed, Some(1)).as_deref(), Some("reversed"));
    }

    /// The POS's wedge: at least 20%, in 10% steps, full at the threshold.
    #[test]
    fn the_confirming_wedge_steps_as_the_poss_does() {
        let wedge = |confirmations, required| {
            let mut data = test_checkout_view_model(false);
            data.confirmations = confirmations;
            data.confirmations_required = required;
            confirming_wedge(&data)
        };
        assert_eq!(wedge(1, 10), 20);
        assert_eq!(wedge(3, 10), 30);
        assert_eq!(wedge(10, 10), 100);
        assert_eq!(wedge(0, 0), 100);
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
    fn a_reversed_payment_is_said_not_to_count_while_money_is_owed() {
        let mut data = test_checkout_view_model(false);
        data.status = OrderStatus::Partial;
        data.double_spend_detected_at = Some(1);
        assert!(
            live_message(&data)
                .into_string()
                .contains("A payment was reversed. It doesn&#39;t count.")
                || live_message(&data)
                    .into_string()
                    .contains("A payment was reversed. It doesn't count.")
        );
    }

    /// The address can be copied in every state, muted once it shouldn't be paid.
    #[test]
    fn a_short_address_keeps_its_copy_and_mutes_once_it_shouldnt_be_paid() {
        let mut data = test_checkout_view_model(false);
        let copy = "Copy the full address";
        let open = live_address_short(&data).into_string();
        assert!(open.contains(copy) && !open.contains("is-spent"), "{open}");
        data.status = OrderStatus::Paid;
        let spent = live_address_short(&data).into_string();
        assert!(
            spent.contains(copy) && spent.contains("is-spent"),
            "{spent}"
        );
    }
}
