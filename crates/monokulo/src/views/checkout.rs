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
    /// pre-computed server-side - the progress bar's fill width is a plain
    /// `style="width: {progress_percent}%"`, not something a `<script>`
    /// sets after load (this page has none - see this module's own doc
    /// comment).
    pub progress_percent: u8,
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
    /// one via the form below - shown read-only from then on. `None` shows
    /// the form instead.
    pub refund_address: Option<String>,
    /// Set only when the refund-address form below was just rejected (empty
    /// submission, or a real engine failure) - `None` on a plain page load.
    pub refund_address_error: Option<String>,
    /// `?view=compact`: a single-column layout with a full-screen paid
    /// state, for small frames. A generic presentation option - the page
    /// knows nothing about who embeds it (the POS terminal is one user).
    pub is_compact: bool,
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

/// The checkout's own styling, on top of the shared theme.
const CHECKOUT_STYLE: &str = r#"
html { background: var(--paper-raised); }
body { min-height: 100vh; min-height: 100dvh; padding: 1.2rem; background: var(--paper-raised); }
.pay-wrap { max-width: 720px; margin: 0 auto; }
/* One column, the stage above the code, whenever it fits the screen. */
.pay-grid { display: grid; grid-template-columns: minmax(0, 1fr); grid-template-areas: "stage" "primary" "secondary"; align-items: start; gap: 0; max-width: 460px; margin-inline: auto; text-align: center; }
.pay-grid > * { min-width: 0; }
.stage-slot { grid-area: stage; margin-bottom: .9em; }
/* Who is asking, over everything, as wide as the column(s) under it. */
.merchant { display: flex; align-items: center; gap: var(--space-md); max-width: 460px; margin: 0 auto .9em; padding: var(--space-md); border: 1px solid var(--line); border-radius: var(--radius-md); background: var(--surface-sunken); text-align: left; }
.merchant:has(.merchant-for) { align-items: flex-start; }
.merchant-about { display: grid; gap: 2px; min-width: 0; }
.merchant h2 { margin: 0; padding: 0; border: 0; font-size: var(--text-base); font-weight: 800; overflow-wrap: anywhere; }
.merchant .hint { margin: 0; font-size: var(--text-xs); overflow-wrap: anywhere; }
.merchant .hint svg { width: 1em; height: 1em; color: var(--success); vertical-align: -.1em; }
.merchant-for { margin: 0; font-size: var(--text-sm); overflow-wrap: anywhere; }
/* The amount sizes itself to this column (`cqi`). */
.pay-col-primary { grid-area: primary; container-type: inline-size; }
.pay-col-secondary { grid-area: secondary; }
/* Two columns only where one can't fit: a short landscape screen. The stage
   then heads the second column, beside the code. */
@media (orientation: landscape) and (max-height: 760px) and (min-width: 560px) {
  .pay-grid { max-width: 760px; grid-template-columns: minmax(0, 290px) minmax(0, 1fr); grid-template-areas: "primary stage" "primary secondary"; column-gap: 2rem; text-align: left; }
  .merchant { max-width: 760px; }
  .pay-col-primary { text-align: center; }
  .qr-wrap svg { width: min(208px, 46vh); height: auto; }
}
/* One line, number and unit together: the size follows the column's width
   (cqi), between a floor that still fits a 280px frame and the full size. */
.amount { font-size: clamp(1.15rem, 9cqi, 2.2rem); font-weight: 700; margin: 0.2em 0 0.1em; letter-spacing: -0.01em; white-space: nowrap; }
.amount-unit { font-size: 0.7em; font-weight: 700; color: var(--muted); }
.amount-label { display: block; color: var(--muted); font-size: 0.8em; }
.fiat-amount { color: var(--muted); margin-bottom: 1.2em; }
/* The time left belongs to the amount: under it, before the code. */
.countdown-slot { margin: -.6em 0 1.2em; text-align: left; }
.qr-wrap { margin: 0 auto 1.2em; display: flex; justify-content: center; }
.qr-wrap svg { width: 208px; height: 208px; }
.address-block { text-align: left; margin: 0 0 1.4em; }
.address-label { display: block; color: var(--muted); font-size: 0.8em; margin: 0 0 0.35em; }
.section {
  text-align: left;
  margin-top: 1.6em;
  padding-top: 1.4em;
  border-top: 1px solid var(--line);
}
.pay-grid > .pay-col-primary > .section:first-child,
.pay-grid > .pay-col-secondary > .section:first-child { margin-top: 0; padding-top: 0; border-top: none; }
.progress-row { display: flex; justify-content: space-between; font-size: 0.85em; margin-bottom: 0.4em; }
.progress-bar { border: 1px solid var(--line); border-radius: var(--radius-sm); overflow: hidden; height: 1em; background: var(--paper); }
.progress-fill { height: 100%; background: var(--accent); }
/* Paid: the progress bar takes the paid colour. Follows the live status. */
#checkout-root[data-status="paid"] .progress-fill { background: var(--state-paid-border); }
#checkout-root[data-status="overpaid"] .progress-fill { background: var(--state-overpaid-border); }
.qr-wrap svg { background: var(--qr-bg); }
/* A code that shouldn't be paid (the payment is in, or the order is over):
   still plainly the same order, too faint for a wallet's camera. The address
   is struck through with it (GroupedValue's `void`). */
.qr-wrap.is-spent svg { opacity: .14; filter: grayscale(1); }
/* Partial: a new code for the rest. */
.qr-new svg { outline: 2px solid var(--state-partial-border); outline-offset: 6px; border-radius: 2px; animation: qr-new 1.4s ease-out 2; }
@keyframes qr-new { from { box-shadow: 0 0 0 6px color-mix(in srgb, var(--state-partial-border) 55%, transparent); } to { box-shadow: 0 0 0 22px transparent; } }
@media (prefers-reduced-motion: reduce) { .qr-new svg { animation: none; } }
.qr-note { margin: -.6em 0 1.2em; font-size: .75em; color: var(--muted); }
[data-table="payments"] { font-size: 0.8em; margin-top: 1.2em; }
.meta { margin-top: 1.4em; font-size: 0.8em; color: var(--muted); text-align: left; }
.refresh-toggle { margin: 0 0 .5em; text-align: right; font-size: .85em; }
/* The scan and image buttons wait, hidden, for the script that works them;
   the saved tick sits beside them, so the text stops short of all three. */
.refund-field { margin: .5em 0; }
#refund_address { padding-right: 7.8em; }
.refund-field button[hidden] { display: none; }
.refund-save-state svg { width: 1.25em; height: 1.25em; }
.refund-save-state { display: none; align-items: center; justify-content: center; width: 2em; color: var(--success); }
.refund-field.is-saving .refund-save-state, .refund-field.is-saved .refund-save-state { display: inline-flex; }
.refund-field.is-saving .refund-save-state svg { display: none; }
.refund-field.is-saving .refund-save-state::after { content: ''; width: 1em; height: 1em; border: 2px solid var(--line); border-top-color: var(--success); border-radius: 50%; animation: refund-spin .7s linear infinite; }
.refund-field.is-saved input { border-color: var(--success); background: var(--tint-success); }
.refund-field.is-invalid input { border-color: var(--error); background: var(--tint-error); }
.refund-field.is-invalid .refund-save-state { display: inline-flex; color: var(--error); }
.refund-field.is-invalid .refund-save-state svg { display: none; }
.refund-field.is-invalid .refund-save-state::after { content: '!'; font-weight: 800; border: 2px solid currentColor; border-radius: 50%; width: 1.1em; height: 1.1em; line-height: 1em; text-align: center; }
@keyframes refund-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) { .refund-field.is-saving .refund-save-state::after { animation-duration: 1.5s; } }
.refund-camera { width: 100%; max-height: 16em; background: var(--media-bg); }
.scan-error { color: var(--error); }
.checkout-compact { max-width: 100%; }
.checkout-compact .pay-grid { grid-template-columns: 1fr; grid-template-areas: "stage" "primary" "secondary"; gap: 0; max-width: none; text-align: center; }
.checkout-compact .stage-slot { margin-bottom: .5em; }
.checkout-compact .merchant { max-width: none; margin-bottom: .5em; padding: var(--space-sm) var(--space-md); }
.checkout-compact .merchant .hint { font-size: var(--text-sm); }
.checkout-compact .pay-col-secondary { font-size: .9em; }
.checkout-compact .qr-wrap { margin-bottom: .5em; }
.checkout-compact .qr-wrap svg { width: min(42vh, 208px); height: auto; }
.checkout-compact .section { margin-top: .7em; padding-top: .7em; }
.checkout-compact .meta, .checkout-compact [data-table="payments"] { display: none; }
.checkout-compact .amount { font-size: clamp(1.1rem, 8cqi, 1.8rem); font-weight: 800; margin: .1em 0; }
.checkout-compact .fiat-amount { font-size: .8em; margin-bottom: .4em; }
.checkout-compact .qr-wrap { margin-bottom: .3em; }
.checkout-compact .qr-wrap svg { width: min(29vh, 158px); height: auto; }
.checkout-compact .address-block { margin-bottom: .5em; }
.checkout-compact .countdown-slot { margin-bottom: .5em; }
.checkout-compact .field-help { font-size: .7em; }
"#;

/// Who is asking to be paid, and for what: the store's name, its site once
/// a verified domain covers it, what the order is for and the merchant's
/// reference, when they're set. A payment link opened from a message
/// would otherwise be anyone asking for money. In a compact frame (a shop's
/// own checkout, which says all this already) it's one line.
fn merchant_header(data: &CheckoutViewModel) -> Markup {
    html! {
        header class="merchant" {
            @if data.is_compact {
                (ui::Hint::new(html! {
                    strong { (data.store_name) }
                    @if let Some(reference) = &data.merchant_reference { " · " (reference) }
                }))
            } @else {
                // The site's own icon once a verified domain covers it, its
                // name's letter until then (or if it has none, or the
                // visitor is on Tor).
                (ui::Avatar::of(&data.store_name).maybe_src(data.merchant_site.as_ref().filter(|_| data.merchant_icon).map(|site| {
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
        "{}{}{join}routed=true",
        crate::base::url(
            crate::http::paths::CheckoutEventsPath {
                order_id: data.order_id.clone()
            }
            .to_string()
        ),
        data.query_suffix
    )
}

pub fn checkout_page(chrome: &PageChrome, data: &CheckoutViewModel) -> Markup {
    let extra_head = html! {
        // Without JavaScript this is the only update path; with it,
        // `checkout.js` streams changes in place instead (`noscript` content
        // is never parsed as markup when scripting is on).
        @if !data.is_terminal && data.auto_refresh {
            noscript { meta http-equiv="refresh" content="60" id="checkout-refresh"; }
        }
        style { (PreEscaped(CHECKOUT_STYLE)) }
        @if data.is_compact { style { (PreEscaped("body{padding:.4rem}")) } }
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
        div class=(if data.is_compact { "pay-wrap checkout-compact" } else { "pay-wrap" }) id="checkout-root" data-order-id=(data.order_id) data-status=(data.status) data-confirmations=(data.confirmations) data-error=(data.payment_error.as_deref().unwrap_or("")) {
            h1 class="sr-only" { "Monero payment of " (data.xmr_amount) " XMR" }
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

            (merchant_header(data))
            div class="pay-grid" {
                (live_status(data))
                div class="pay-col-primary" {
                    (live_pay(data))

                    (live_address(data))
                }
                div class="pay-col-secondary" {
                    (live_progress(data))

                    @if data.refund_enabled { div class="section" {
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

                    (live_payments(data))

                    div class="meta" {
                        "Order ID: " (data.order_id)
                    }
                }
            }
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

/// The track: Send, Confirm, Paid - without Confirm when the store counts a
/// payment as soon as it's seen (`confirmations_required == 0`).
fn stage_steps(data: &CheckoutViewModel) -> Vec<ui::ProgressStep> {
    use ui::StepProgress as Step;
    let [send, confirm, paid] = if data.double_spend_detected_at.is_some()
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
        .map(|(label, progress)| ui::ProgressStep {
            label: label.to_string(),
            progress,
        })
        .collect()
}

/// The stage's words: a short title, then what to do now.
fn stage_message(data: &CheckoutViewModel) -> (String, Markup) {
    let received = super::trim_xmr(&data.amount_received_xmr);
    let due = super::trim_xmr(&data.amount_due_xmr);
    let total = super::trim_xmr(&data.xmr_amount);
    if let Some(at) = data.double_spend_detected_at {
        let when = html! { span id="double-spend-time" { @if data.local_times { (data.clock.time_local(at)) } @else { (data.clock.time(at)) } } };
        return if awaiting_payment(data) {
            (
                "A payment was reversed".to_string(),
                html! { "A double spend was detected at " (when) ", so it doesn't count. Send " (due) " XMR to pay this order." },
            )
        } else {
            (
                "Payment reversed".to_string(),
                html! { "A double spend was detected at " (when) ", so that payment doesn't count. Contact the merchant before sending anything." },
            )
        };
    }
    match data.status {
        OrderStatus::Pending => (
            "Send the exact amount below".to_string(),
            html! { "Scan the code with your wallet, or copy the address." },
        ),
        OrderStatus::Partial => (
            format!("Send the remaining {due} XMR"),
            html! { (received) " of " (total) " XMR received. This new code asks for the rest." },
        ),
        OrderStatus::Unconfirmed => (
            "Waiting for confirmation".to_string(),
            html! { (received) " XMR seen on the network. Don't send it again; this page updates by itself." },
        ),
        OrderStatus::Confirming => (
            "Confirming".to_string(),
            html! { (data.confirmations) " of " (data.confirmations_required) " confirmations. Don't send it again; this page updates by itself." },
        ),
        OrderStatus::Paid => (
            "Paid".to_string(),
            html! { (received) " XMR received" @if data.confirmations_required > 0 { " and confirmed" } ". Nothing more to send." },
        ),
        OrderStatus::Overpaid => (
            "Paid, with extra".to_string(),
            html! {
                @if let Some(message) = &data.payment_error { (message) } @else { "Don't send more. Contact the merchant about the extra amount." }
            },
        ),
        OrderStatus::Expired => (
            "This payment expired".to_string(),
            html! {
                @if received == "0" { "Nothing arrived in time. Don't send to this address; go back to the shop to start again." }
                @else { (received) " XMR arrived before it expired. Don't send more; contact the merchant about it." }
            },
        ),
        OrderStatus::Cancelled => (
            "This order was cancelled".to_string(),
            html! {
                @if received == "0" { "Don't send to this address; ask the merchant for a new order if you still want to pay." }
                @else { (received) " XMR arrived after it was cancelled. Don't send more; contact the merchant about a refund." }
            },
        ),
    }
}

/// The stage above the code, in every state: the same place and shape, so
/// nothing moves as the order changes (the badge is for screen readers).
fn live_status(data: &CheckoutViewModel) -> Markup {
    let steps = stage_steps(data);
    let (title, message) = stage_message(data);
    let shown = super::order_state(data.status).1;
    let state = if data.double_spend_detected_at.is_some() {
        ui::OrderState::DoubleSpend
    } else {
        shown
    };
    html! {
        div id="live-status" class="stage-slot" data-live {
            span id="status-badge" class=(format!("sr-only state-{shown}")) { (data.status_label) }
            (ui::ProgressSteps::new("Payment progress", steps, title, message).state(state))
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

/// The part of the payment column that follows the order: the amount and
/// its QR code, always in the same place. After a partial payment the
/// amount is what's left and the code is redrawn for it; once the code
/// shouldn't be paid (the payment is in, or the order is over) it fades
/// (`.is-spent`), so nothing invites a second payment.
fn live_pay(data: &CheckoutViewModel) -> Markup {
    let due = super::trim_xmr(&data.amount_due_xmr);
    let total = super::trim_xmr(&data.xmr_amount);
    let partial = data.status == OrderStatus::Partial;
    let (label, amount) = if partial {
        ("Still to pay", due)
    } else if awaiting_payment(data) {
        ("Amount due", total)
    } else {
        ("Order total", total)
    };
    html! {
        div id="live-pay" data-live {
            span class="amount-label" id="amount-label" { (label) }
            // The number and its unit never split; the font shrinks with
            // the frame instead (see `.amount`).
            div class="amount" id="xmr-amount" aria-labelledby="amount-label" {
                span class="amount-value" { (amount) } " " span class="amount-unit" { "XMR" }
            }
            // A fiat equivalent only for an order priced in fiat.
            @if !partial && priced_in_fiat(data) {
                div class="fiat-amount" { "≈ " (super::display_amount(&data.amount, &data.currency)) }
            }
            @if awaiting_payment(data) {
                div class="countdown-slot" { (countdown(data)) }
            }
            @if partial {
                div class="qr-wrap qr-new" { (data.qr_code_svg) }
                p class="qr-note" { "Wallets that read payment codes fill in " (due) " XMR for you." }
            } @else if awaiting_payment(data) {
                div class="qr-wrap" { (data.qr_code_svg) }
            } @else {
                div class="qr-wrap is-spent" { (data.qr_code_svg) }
            }
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
/// it. Struck through, with nothing to copy it with, once it shouldn't be
/// paid: a live part, so that follows the order.
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
        div class="address-block" id="live-address" data-live {
            span class="address-label" id="address-label" {
                @if data.status == OrderStatus::Partial { "Send the remaining amount to" }
                @else if data.status == OrderStatus::Pending { "Send exactly this amount to" }
                @else { "Payment address" }
            }
            (address)
        }
    }
}

fn live_progress(data: &CheckoutViewModel) -> Markup {
    html! {
        div class="section" id="live-progress" data-live {
            div class="progress-row" {
                span id="confirmations-label" { (data.confirmations) " / " (data.confirmations_required) " confirmations" }
                span id="received-label" { (super::trim_xmr(&data.amount_received_xmr)) " XMR received" }
            }
            div class="progress-bar" id="progress-bar" role="progressbar" aria-labelledby="confirmations-label"
                aria-valuemin="0" aria-valuemax=(data.confirmations_required) aria-valuenow=(data.confirmations)
                aria-valuetext=(format!("{} of {} confirmations", data.confirmations, data.confirmations_required)) {
                div class="progress-fill" id="progress-fill" style=(format!("width: {}%", data.progress_percent)) {}
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
        div id="live-payments" data-live {
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
    let mut params = Vec::new();
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
    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    format!("{}{query}", checkout_url(&data.order_id))
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
            refund_address_error: None,
            is_compact: false,
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
            html.contains(r#"<div class="fiat-amount">≈ 12.50 USD</div>"#),
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
        assert_eq!(fragment.matches("data-live").count(), 5);
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
        assert!(
            html.contains("<strong>Send the remaining 0.3 XMR.</strong>"),
            "{html}"
        );
        assert!(html.contains("0.2 of 0.5 XMR received. This new code asks for the rest."));
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
        assert!(html.contains("Payment address"), "{html}");

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

        data.status = engine_protocol::order_status::OrderStatus::Paid;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains("XMR received. Nothing more to send."),
            "{html}"
        );
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
    }
}
