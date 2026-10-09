//! `http/checkout.rs` - the real, public checkout/payment page
//! (`GET /pay/{pk}/orders/{order_id}`), its "order not found" fallback,
//! and the nav-bearing `/share` wrapper around it.
//!
//! **`checkout_page`/`not_found_page` deliberately carry no site nav at
//! all** - a merchant embeds `checkout_page` in an iframe inside their own
//! checkout flow; Monokulo's own site navigation has no business appearing
//! inside it. They still use the shared head partial for consistent
//! typography/color, just not [`super::layout`]'s nav bar - see
//! [`super::layout_bare_with_head`]/[`super::layout_bare`].
//!
//! The payment details and refund form are server-rendered and usable with
//! JavaScript disabled: a `<noscript>` meta-refresh reloads the page once a
//! minute and the refund form has a plain Save button. With JavaScript,
//! `checkout.js` instead streams re-rendered [`live_fragment`] regions over
//! Server-Sent Events, auto-saves the refund address, and locally decodes
//! refund QR images without sending those images to the server.

use maud::{html, Markup, PreEscaped};

use super::{DisplayStatus, OrderStatus};

use super::{layout_bare, layout_bare_with_head, layout_with_head, script, Load, PageChrome};

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
    pub status_label: String,
    pub status: super::DisplayStatus,
    pub status_class: String,
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
    /// JavaScript disabled. Only shown when `!is_terminal`.
    pub expires_in_display: String,
    /// `""`, `"expiry-soon"`, or `"expiry-urgent"` - a CSS class picked
    /// server-side from how much time is actually left, so the timer can
    /// shift color as expiry nears with no client-side timer/JS of its own.
    pub expiry_urgency_class: String,
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
    pub pk: String,
    pub payments: Vec<CheckoutPaymentViewModel>,
}

/// Checkout styling, adapted from the old `checkout.html.hbs` style block.
const CHECKOUT_STYLE: &str = r#"
html { background: var(--paper-raised); }
body { min-height: 100vh; min-height: 100dvh; padding: 1.2rem; background: var(--paper-raised); }
.pay-wrap { max-width: 720px; margin: 0 auto; }
/* One column, the stage above the code, whenever it fits the screen. */
.pay-grid { display: grid; grid-template-columns: minmax(0, 1fr); grid-template-areas: "stage" "primary" "secondary"; align-items: start; gap: 0; max-width: 460px; margin-inline: auto; text-align: center; }
.pay-grid > * { min-width: 0; }
.stage-slot { grid-area: stage; margin-bottom: .9em; }
/* The amount sizes itself to this column (`cqi`). */
.pay-col-primary { grid-area: primary; container-type: inline-size; }
.pay-col-secondary { grid-area: secondary; }
/* Two columns only where one can't fit: a short landscape screen. The stage
   then heads the second column, beside the code. */
@media (orientation: landscape) and (max-height: 760px) and (min-width: 560px) {
  .pay-grid { max-width: 760px; grid-template-columns: minmax(0, 290px) minmax(0, 1fr); grid-template-areas: "primary stage" "primary secondary"; column-gap: 2rem; text-align: left; }
  .pay-col-primary { text-align: center; }
  .qr-wrap svg { width: min(208px, 46vh); height: auto; }
}
/* The stage: where the order is (Send, Confirm, Paid; no Confirm when one
   payment is enough) and one line on what to do now, in the state's colours.
   Always present, so the code under it never moves as the order changes. */
.stage-track { text-align: left; padding: .65em .8em; border: 1px solid var(--state-border); border-radius: var(--radius-md); background: var(--state-bg); color: var(--state-ink); }
.track { list-style: none; margin: 0 0 .5em; padding: 0; display: grid; grid-auto-flow: column; grid-auto-columns: minmax(0, 1fr); position: relative; }
.track::before { content: ""; position: absolute; top: .8em; left: 16.67%; right: 16.67%; height: 2px; background: color-mix(in srgb, currentColor 30%, transparent); }
.track-2::before { left: 25%; right: 25%; }
.track li { position: relative; display: grid; justify-items: center; gap: .2em; font-size: .75em; font-weight: 700; }
.track-dot { display: grid; place-items: center; width: 2.13em; height: 2.13em; border: 2px solid color-mix(in srgb, currentColor 40%, transparent); border-radius: 50%; background: var(--paper-raised); }
.track-dot svg { width: 1.2em; height: 1.2em; }
.step-done .track-dot { background: var(--state-paid-border); border-color: var(--state-paid-border); color: var(--paper-raised); }
.step-now .track-dot { border-color: currentColor; box-shadow: 0 0 0 3px color-mix(in srgb, currentColor 22%, transparent); }
.step-part .track-dot { border-color: currentColor; background: conic-gradient(currentColor 0 50%, var(--paper-raised) 50% 100%); }
.step-fail .track-dot { background: var(--state-border); border-color: var(--state-border); color: var(--paper-raised); }
.step-todo { opacity: .6; }
.track-msg { margin: 0; font-size: .85em; line-height: 1.4; }
.stage-expiry { white-space: nowrap; }
.stage-expiry.expiry-soon { color: var(--warning); font-weight: 800; }
.stage-expiry.expiry-urgent { color: var(--error); font-weight: 800; }
/* One line, number and unit together: the size follows the column's width
   (cqi), between a floor that still fits a 280px frame and the full size. */
.amount { font-size: clamp(1.15rem, 9cqi, 2.2rem); font-weight: 700; margin: 0.2em 0 0.1em; letter-spacing: -0.01em; white-space: nowrap; }
.amount-unit { font-size: 0.7em; font-weight: 700; color: var(--muted); }
.amount-label { display: block; color: var(--muted); font-size: 0.8em; }
.fiat-amount { color: var(--muted); margin-bottom: 1.2em; }
.qr-wrap { margin: 0 auto 1.2em; display: flex; justify-content: center; }
.qr-wrap svg { width: 208px; height: 208px; }
.address-block { text-align: left; margin: 0 0 1.4em; }
.address-row { display: flex; align-items: flex-start; gap: 0.5em; }
.address-copy-btn { flex: none; display: inline-flex; align-items: flex-start; justify-content: center; width: 2em; height: 2em; padding: 0.2em 0 0; margin: 0; border: 0; background: transparent; color: var(--muted); cursor: pointer; }
.address-copy-btn[hidden] { display: none; }
.address-copy-btn:hover { color: var(--ink); background: transparent; }
.address-copy-btn svg { width: 1.1em; height: 1.1em; }
.address-label { display: block; color: var(--muted); font-size: 0.8em; margin: 0 0 0.35em; }
.address-text {
  flex: 1;
  width: 100%;
  min-width: 0;
  font-size: 0.85em;
  resize: none;
  border: none;
  background: none;
  padding: 0;
  min-height: 1.5em;
  height: auto;
  field-sizing: content;
  overflow: hidden;
  cursor: text;
  outline: none;
}
.address-text:focus { outline: none; }
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
   fades with it and loses its copy button. */
.qr-wrap.is-spent svg { opacity: .14; filter: grayscale(1); }
#checkout-root:has(.qr-wrap.is-spent) .address-block { opacity: .55; }
#checkout-root:has(.qr-wrap.is-spent) .address-copy-btn { display: none; }
/* Partial: a new code for the rest. */
.qr-new svg { outline: 2px solid var(--state-partial-border); outline-offset: 6px; border-radius: 2px; animation: qr-new 1.4s ease-out 2; }
@keyframes qr-new { from { box-shadow: 0 0 0 6px color-mix(in srgb, var(--state-partial-border) 55%, transparent); } to { box-shadow: 0 0 0 22px transparent; } }
@media (prefers-reduced-motion: reduce) { .qr-new svg { animation: none; } }
.qr-note { margin: -.6em 0 1.2em; font-size: .75em; color: var(--muted); }
.payments-table { font-size: 0.8em; margin-top: 1.2em; }
.meta { margin-top: 1.4em; font-size: 0.8em; color: var(--muted); text-align: left; }
.refresh-toggle { margin: 0 0 .5em; text-align: right; font-size: .85em; }
.refund-field { display: grid; align-items: center; margin: .5em 0; }
.refund-field input[type=text] { grid-area: 1 / 1; display: block; width: 100%; padding-right: 7.8em; margin: 0; }
.refund-tools { grid-area: 1 / 1; justify-self: end; z-index: 1; display: flex; align-items: center; gap: .15em; margin-right: .35em; }
.refund-icon-btn { display: inline-flex; align-items: center; justify-content: center; width: 2.2em; height: 2.2em; margin: 0; padding: .3em; border: 0; background: transparent; color: var(--muted); cursor: pointer; }
.refund-icon-btn[hidden] { display: none; }
.refund-icon-btn:hover, .refund-icon-btn:focus-visible { color: var(--ink); background: var(--paper); }
.refund-icon-btn svg, .refund-save-state svg { width: 1.25em; height: 1.25em; }
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
.checkout-compact .pay-col-secondary { font-size: .9em; }
.checkout-compact .qr-wrap { margin-bottom: .5em; }
.checkout-compact .qr-wrap svg { width: min(42vh, 208px); height: auto; }
.checkout-compact .section { margin-top: .7em; padding-top: .7em; }
.checkout-compact .meta, .checkout-compact .payments-table { display: none; }
.checkout-compact .amount { font-size: clamp(1.1rem, 8cqi, 1.8rem); font-weight: 800; margin: .1em 0; }
.checkout-compact .fiat-amount { font-size: .8em; margin-bottom: .4em; }
.checkout-compact .qr-wrap { margin-bottom: .3em; }
.checkout-compact .qr-wrap svg { width: min(29vh, 158px); height: auto; }
.checkout-compact .address-block { margin-bottom: .5em; }
.checkout-compact .address-row { border: 1px solid var(--control-border); border-radius: var(--radius-sm); padding: .3em; }
.checkout-compact .address-text { font-size: .7em; }
.checkout-compact .refund-field { display: flex; flex-wrap: wrap; gap: .3em; }
.checkout-compact .refund-field input[type=text] { padding-right: .6em; flex: 1 1 100%; }
.checkout-compact .refund-tools { justify-self: auto; margin: 0; gap: .4em; }
.checkout-compact .refund-icon-btn { width: auto; height: 2.3em; gap: .3em; padding: .25em .5em; border: 1px solid var(--line); border-radius: 4px; background: var(--paper); color: var(--ink); font-size: .75em; font-weight: 700; }
.checkout-compact .refund-icon-btn[hidden] { display: none; }
.refund-icon-btn span { display: none; }
.checkout-compact .refund-icon-btn span { display: inline; }
.checkout-compact .field-help { font-size: .7em; }
"#;

/// The order's event stream, routed part by part, with the page's own
/// options.
fn events_url(data: &CheckoutViewModel) -> String {
    let join = if data.query_suffix.is_empty() {
        '?'
    } else {
        '&'
    };
    format!(
        "/pay/{}/orders/{}/events{}{join}routed=true",
        data.pk, data.order_id, data.query_suffix
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
                        a href=(format!("/pay/{}/orders/{}{}", data.pk, data.order_id, data.toggled_refresh_suffix)) {
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

            div class="pay-grid" {
                (live_status(data))
                div class="pay-col-primary" {
                    (live_pay(data))

                    // Faded with the code once it shouldn't be paid.
                    div class="address-block" {
                        (address_label(data))
                        div class="address-row" {
                            button type="button" id="copy-address" class="address-copy-btn" aria-label="Copy payment address" title="Copy payment address" hidden {
                                svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
                                    rect x="9" y="9" width="12" height="12" rx="1" {}
                                    path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1" {}
                                }
                            }
                            textarea class="address-text" id="address" readonly aria-labelledby="address-label" { (data.address) }
                        }
                    }
                }
                div class="pay-col-secondary" {
                    (live_progress(data))

                    @if data.refund_enabled { div class="section" {
                        form method="post" id="refund-form" action=(format!("/pay/{}/orders/{}/refund-address{}", data.pk, data.order_id, data.query_suffix))
                            fx-action=(format!("/pay/{}/orders/{}/refund-address{}", data.pk, data.order_id, data.query_suffix))
                            fx-method="POST" fx-trigger="refund:save" fx-swap="none" fx-own-errors {
                                label for="refund_address" { "Refund address " span class="field-help" style="display:inline" { "(optional)" } }
                                @if let Some(error) = &data.refund_address_error {
                                    div class="error" { (error) }
                                }
                                div id="refund-field" class=(if data.refund_address.is_some() { "refund-field is-saved" } else { "refund-field" }) {
                                    input type="text" id="refund_address" name="refund_address" value=(data.refund_address.as_deref().unwrap_or("")) placeholder="Your Monero refund address" autocomplete="off" spellcheck="false";
                                    span class="refund-tools" {
                                    span id="refund-save-state" class="refund-save-state" role="status" aria-label=(if data.refund_address.is_some() { "Refund address saved" } else { "" }) {
                                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" { path d="m4 12 5 5L20 6" {} }
                                    }
                                    button type="button" id="scan-refund" class="refund-icon-btn" aria-label="Scan refund QR" title="Scan refund QR" hidden {
                                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true" { path d="M3 9V3h6M15 3h6v6M21 15v6h-6M9 21H3v-6M6 6h2v2H6zM16 6h2v2h-2zM6 16h2v2H6zM12 11h2v2h-2zM16 16h2v2h-2z" {} }
                                        span { "Scan refund QR" }
                                    }
                                    button type="button" id="upload-refund" class="refund-icon-btn" aria-label="Choose QR image" title="Choose QR image" hidden {
                                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" { rect x="2" y="3" width="20" height="18" rx="2" {} circle cx="8.5" cy="8.5" r="1.5" {} path d="m2 17 6-6 4 4 3-3 7 7" {} }
                                        span { "Choose QR image" }
                                    }
                                    }
                                    input type="file" id="refund-image" accept="image/*" hidden;
                                }
                                video id="refund-camera" class="refund-camera" autoplay playsinline hidden {}
                                p id="scan-error" class="scan-error" role="alert" hidden {}
                                span class="field-help" {
                                    "If this order needs a refund, this address is recorded for the merchant to use. "
                                    "Refunds are not sent automatically."
                                }
                                noscript { button type="submit" class="btn-secondary" { "Save" } }
                        }
                    } }

                    (live_payments(data))

                    div class="meta" {
                        "Order ID: " (data.order_id)
                    }
                }
            }
        }
        @if data.refund_enabled { (script("jsQR.js", Load::Now)) }
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
        (address_label(data))
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
        ("address-label", address_label(data)),
        ("live-progress", live_progress(data)),
        ("live-payments", live_payments(data)),
    ]
}

/// Whether the customer still owes something: the whole amount, or the rest
/// after a partial payment. Only then is the code live; otherwise it fades.
fn awaiting_payment(data: &CheckoutViewModel) -> bool {
    matches!(
        data.status,
        DisplayStatus::Order(OrderStatus::Pending | OrderStatus::Partial)
    )
}

/// One step on the stage's track.
#[derive(Clone, Copy, PartialEq)]
enum Step {
    Done,
    Now,
    Part,
    Fail,
    Todo,
}

impl Step {
    fn class(self) -> &'static str {
        match self {
            Step::Done => "step-done",
            Step::Now => "step-now",
            Step::Part => "step-part",
            Step::Fail => "step-fail",
            Step::Todo => "step-todo",
        }
    }
    /// For screen readers, after the step's name.
    fn words(self) -> &'static str {
        match self {
            Step::Done => "done",
            Step::Now => "now",
            Step::Part => "partly done",
            Step::Fail => "stopped",
            Step::Todo => "to come",
        }
    }
}

/// The track: Send, Confirm, Paid - without Confirm when the store counts a
/// payment as soon as it's seen (`confirmations_required == 0`).
fn stage_steps(data: &CheckoutViewModel) -> Vec<(&'static str, Step)> {
    let [send, confirm, paid] = if data.double_spend_detected_at.is_some()
        && !awaiting_payment(data)
    {
        [Step::Fail, Step::Todo, Step::Todo]
    } else {
        match data.status {
            DisplayStatus::Order(OrderStatus::Pending) => [Step::Now, Step::Todo, Step::Todo],
            DisplayStatus::Order(OrderStatus::Partial) => [Step::Part, Step::Todo, Step::Todo],
            DisplayStatus::Order(OrderStatus::Unconfirmed)
            | DisplayStatus::Order(OrderStatus::Confirming) => [Step::Done, Step::Now, Step::Todo],
            DisplayStatus::Order(OrderStatus::Paid)
            | DisplayStatus::Order(OrderStatus::Overpaid) => [Step::Done, Step::Done, Step::Done],
            DisplayStatus::Order(OrderStatus::Expired) | DisplayStatus::Cancelled => {
                [Step::Fail, Step::Todo, Step::Todo]
            }
        }
    };
    if data.confirmations_required == 0 {
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
    }
}

/// The stage's words: a short title, then what to do now.
fn stage_message(data: &CheckoutViewModel) -> (String, Markup) {
    let received = super::trim_xmr(&data.amount_received_xmr);
    let due = super::trim_xmr(&data.amount_due_xmr);
    let total = super::trim_xmr(&data.xmr_amount);
    let left = html! { span class=(format!("stage-expiry {}", data.expiry_urgency_class)) { (data.expires_in_display) " left" } };
    if let Some(at) = data.double_spend_detected_at {
        let when = html! { span id="double-spend-time" { @if data.local_times { (data.clock.time_local(at)) } @else { (data.clock.time(at)) } } };
        return if awaiting_payment(data) {
            (
                "A payment was reversed".to_string(),
                html! { "A double spend was detected at " (when) ", so it doesn't count. Send " (due) " XMR to pay this order. " (left) },
            )
        } else {
            (
                "Payment reversed".to_string(),
                html! { "A double spend was detected at " (when) ", so that payment doesn't count. Contact the merchant before sending anything." },
            )
        };
    }
    match data.status {
        DisplayStatus::Order(OrderStatus::Pending) => (
            format!("Send {total} XMR"),
            html! { "Scan the code or copy the address. " (left) },
        ),
        DisplayStatus::Order(OrderStatus::Partial) => (
            format!("Send the remaining {due} XMR"),
            html! { (received) " of " (total) " XMR received. This new code asks for the rest. " (left) },
        ),
        DisplayStatus::Order(OrderStatus::Unconfirmed) => (
            "Waiting for confirmation".to_string(),
            html! { (received) " XMR seen on the network. Don't send it again; this page updates by itself." },
        ),
        DisplayStatus::Order(OrderStatus::Confirming) => (
            "Confirming".to_string(),
            html! { (data.confirmations) " of " (data.confirmations_required) " confirmations. Don't send it again; this page updates by itself." },
        ),
        DisplayStatus::Order(OrderStatus::Paid) => (
            "Paid".to_string(),
            html! { (received) " XMR received" @if data.confirmations_required > 0 { " and confirmed" } ". Nothing more to send." },
        ),
        DisplayStatus::Order(OrderStatus::Overpaid) => (
            "Paid, with extra".to_string(),
            html! {
                @if let Some(message) = &data.payment_error { (message) } @else { "Don't send more. Contact the merchant about the extra amount." }
            },
        ),
        DisplayStatus::Order(OrderStatus::Expired) => (
            "This payment expired".to_string(),
            html! {
                @if received == "0" { "Nothing arrived in time. Don't send to this address; go back to the shop to start again." }
                @else { (received) " XMR arrived before it expired. Don't send more; contact the merchant about it." }
            },
        ),
        DisplayStatus::Cancelled => (
            "Cancelled".to_string(),
            html! { "This order was cancelled. Don't send to this address." },
        ),
    }
}

/// The stage above the code, in every state: the same place and shape, so
/// nothing moves as the order changes (the badge is for screen readers).
fn live_status(data: &CheckoutViewModel) -> Markup {
    let steps = stage_steps(data);
    let (title, message) = stage_message(data);
    let state_class = if data.double_spend_detected_at.is_some() {
        "state-double-spend"
    } else {
        data.status_class.as_str()
    };
    html! {
        div id="live-status" class="stage-slot" data-live {
            span id="status-badge" class=(format!("tag {} sr-only", data.status_class)) { (data.status_label) }
            div class=(format!("stage-track {state_class}")) {
                ol class=(if steps.len() == 2 { "track track-2" } else { "track" }) aria-label="Payment progress" {
                    @for (name, step) in &steps {
                        li class=(step.class()) {
                            span class="track-dot" aria-hidden="true" {
                                @if *step == Step::Done { (check_icon()) } @else if *step == Step::Fail { (cross_icon()) }
                            }
                            span class="track-name" { (name) span class="sr-only" { ", " (step.words()) } }
                        }
                    }
                }
                p class="track-msg" role="status" { strong { (title) "." } " " (message) }
            }
        }
    }
}

/// The part of the payment column that follows the order: the amount and
/// its QR code, always in the same place. After a partial payment the
/// amount is what's left and the code is redrawn for it; once the code
/// shouldn't be paid (the payment is in, or the order is over) it fades
/// (`.is-spent`), so nothing invites a second payment.
fn live_pay(data: &CheckoutViewModel) -> Markup {
    let due = super::trim_xmr(&data.amount_due_xmr);
    let total = super::trim_xmr(&data.xmr_amount);
    let partial = data.status == DisplayStatus::Order(OrderStatus::Partial);
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
            @if !partial && !data.currency.is_empty() && data.currency != "XMR" {
                div class="fiat-amount" { "≈ " (data.amount) " " (data.currency) }
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

fn check_icon() -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
            path d="m4.5 12.5 5 5 10-11" {}
        }
    }
}

fn cross_icon() -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" aria-hidden="true" focusable="false" {
            path d="M7 7l10 10M17 7 7 17" {}
        }
    }
}

fn address_label(data: &CheckoutViewModel) -> Markup {
    html! {
        label class="address-label" id="address-label" for="address" data-live {
            @if data.status == DisplayStatus::Order(OrderStatus::Partial) { "Send the remaining amount to" }
            @else if data.status == DisplayStatus::Order(OrderStatus::Pending) { "Send exactly this amount to" }
            @else { "Payment address" }
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
    html! {
        div id="live-payments" data-live {
            @if !data.payments.is_empty() {
                div class="section" {
                    table class="payments-table" {
                        caption class="sr-only" { "Individual payments received toward this order" }
                        thead { tr { th scope="col" { "Tx" } th scope="col" { "Amount" } th scope="col" { "Confirmations" } } }
                        tbody id="payments-body" {
                            @for payment in &data.payments {
                                tr {
                                    td { (super::short_id(&payment.txid)) }
                                    td class="nowrap" { (super::trim_xmr(&payment.amount_xmr)) " XMR" }
                                    td { @if payment.is_zero_conf { "mempool" } @else { (payment.confirmations) } }
                                }
                            }
                        }
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
    format!("/pay/{}/orders/{}{query}", data.pk, data.order_id)
}

pub struct CheckoutShareViewModel {
    pub pk: String,
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
.share-wrap { max-width: 840px; margin: 1.6rem auto; }
.share-card { background: var(--paper-raised); border: 1px solid var(--line); border-radius: var(--radius-lg); overflow: hidden; }
.share-card-head { padding: var(--space-xl) 1.2rem 0; text-align: center; }
.share-card-head h1 { margin: 0 0 .25em; border: 0; padding: 0; }
.share-card-head .hint { margin: 0 auto; max-width: 34em; }
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
  function resize() {
    try {
      var doc = frame.contentDocument;
      var height = doc && doc.documentElement && doc.documentElement.scrollHeight;
      if (height) frame.style.height = height + "px";
    } catch (e) { /* not ready yet, or something unexpected - keep the CSS fallback */ }
  }
  frame.addEventListener("load", function () {
    resize();
    try {
      var doc = frame.contentDocument;
      if (doc && typeof ResizeObserver === "function") {
        new ResizeObserver(resize).observe(doc.documentElement);
      }
    } catch (e) {}
  });
})();
"#;

pub fn share_page(chrome: &PageChrome, data: &CheckoutShareViewModel) -> Markup {
    let extra_head = html! { style { (PreEscaped(SHARE_STYLE)) } };
    let body = html! {
        div class="wrap share-wrap" {
            @if data.found {
                section class="share-card" {
                    header class="share-card-head" {
                        h1 { "Pay with Monero" }
                        p class="hint" { "Complete the payment below - this page stays up to date on its own, so it's safe to bookmark or come back to later." }
                    }
                    iframe class="share-frame" id="checkout-frame" src=(share_frame_src(chrome, data)) title="Monero payment" {}
                }
                script { (PreEscaped(SHARE_SCRIPT)) }
            } @else {
                h1 { "Order not found" }
                p { "This payment link is invalid, or the order it refers to no longer exists." }
            }
        }
    };
    layout_with_head(chrome, "Pay with Monero", extra_head, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/pay/pk_abc123/orders/pay_abc123")
    }

    fn test_checkout_view_model(is_terminal: bool) -> CheckoutViewModel {
        CheckoutViewModel {
            order_id: shared::ids::OrderId::new("pay_abc123".to_string()),
            status_label: if is_terminal { "Paid".to_string() } else { "Waiting for payment".to_string() },
            status: if is_terminal { shared::order_status::OrderStatus::Paid.into() } else { shared::order_status::OrderStatus::Pending.into() },
            status_class: if is_terminal { "state-paid".to_string() } else { "state-pending".to_string() },
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
            expiry_urgency_class: String::new(),
            refund_address: None,
            refund_address_error: None,
            is_compact: false,
            refund_enabled: true,
            query_suffix: String::new(),
            auto_refresh: true,
            toggled_refresh_suffix: "?refresh=false".to_string(),
            payment_error: None,
            pk: "pk_abc123".to_string(),
            payments: vec![],
        }
    }

    #[test]
    fn checkout_page_preserves_no_js_refund_entry_and_adds_local_qr_capture() {
        let html = checkout_page(&chrome(), &test_checkout_view_model(false)).into_string();
        assert!(html.contains(&crate::assets::url("checkout.js")));
        assert!(html.contains("id=\"scan-refund\""));
        assert!(html.contains(&crate::assets::url("jsQR.js")));
        assert!(html.contains(
            r#"<noscript><meta http-equiv="refresh" content="60" id="checkout-refresh"></noscript>"#
        ));
        assert!(html.contains(r#"<noscript><p class="refresh-toggle"><a href="/pay/pk_abc123/orders/pay_abc123?refresh=false">Auto Refresh: ON</a></p></noscript>"#), "got: {html}");
        assert!(html.contains(
            r#"<noscript><button type="submit" class="btn-secondary">Save</button></noscript>"#
        ));
        assert!(
            !html.contains(r#"<nav class="site-nav""#),
            "the checkout page must not carry the site nav, got: {html}"
        );
        assert!(
            !html.contains("Monokulo"),
            "the checkout page must not carry the site brand/logo, got: {html}"
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
                r#"<a href="/pay/pk_abc123/orders/pay_abc123?view=compact">Auto Refresh: OFF</a>"#
            ),
            "got: {html}"
        );
        assert!(html.contains(r#"action="/pay/pk_abc123/orders/pay_abc123/refund-address?view=compact&amp;refresh=false""#), "saving must keep auto refresh off, got: {html}");

        let terminal = checkout_page(&chrome(), &test_checkout_view_model(true)).into_string();
        assert!(
            !terminal.contains("Auto Refresh"),
            "a terminal order never refreshes, so it has nothing to toggle, got: {terminal}"
        );
    }

    #[test]
    fn checkout_page_meta_refreshes_once_a_minute_only_without_javascript() {
        let mut data = test_checkout_view_model(false);
        data.refund_address = Some(data.address.clone());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains(
            r#"<noscript><meta http-equiv="refresh" content="60" id="checkout-refresh"></noscript>"#
        ));
        assert_eq!(
            html.matches("http-equiv=\"refresh\"").count(),
            1,
            "the refresh must only exist inside noscript, got: {html}"
        );
    }

    #[test]
    fn live_fragment_carries_every_changing_region_and_nothing_else() {
        let mut data = test_checkout_view_model(false);
        data.status = shared::order_status::OrderStatus::Partial.into();
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
            "address-label",
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

    #[test]
    fn compact_view_uses_the_same_payment_markup_and_stage() {
        let mut data = test_checkout_view_model(true);
        data.is_compact = true;
        data.query_suffix = "?view=compact".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("pay-wrap checkout-compact"));
        // Paid: the stage says so over the faded code, as in the full view.
        assert!(
            html.contains("<strong>Paid.</strong>")
                && html.contains(r#"<div class="qr-wrap is-spent">"#),
            "{html}"
        );
        assert!(!html.contains(r#"id="payment-state""#) && !html.contains(r#"class="receipt"#));
        assert!(html.contains("id=\"copy-address\""));
        assert!(html.contains("class=\"address-block\""));
        assert!(html.contains("id=\"refund_address\""));
        assert!(html.contains("refund-address?view=compact"));
    }

    #[test]
    fn partial_payment_shows_remaining_amount_and_deadline() {
        let mut data = test_checkout_view_model(false);
        data.status = shared::order_status::OrderStatus::Partial.into();
        data.status_label = "Partial payment received".to_string();
        data.status_class = "state-partial".to_string();
        data.amount_received_xmr = "0.200000000000".to_string();
        data.payment_error = Some(
            "0.2 XMR received of 0.5 XMR. Send the remaining 0.3 XMR to the address below."
                .to_string(),
        );
        data.amount_due_xmr = "0.300000000000".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        // The stage says what arrived and what's left, with the time left;
        // the badge stays for screen readers only.
        assert!(html.contains(r#"class="tag state-partial sr-only""#));
        assert!(
            html.contains(r#"<div class="stage-track state-partial">"#),
            "{html}"
        );
        assert!(html.contains(r#"<li class="step-part">"#));
        assert!(html.contains("<strong>Send the remaining 0.3 XMR.</strong>"));
        assert!(html.contains("0.2 of 0.5 XMR received. This new code asks for the rest."));
        assert!(
            html.contains(r#"<span class="stage-expiry ">30m left</span>"#),
            "{html}"
        );
        // The amount is what's left, under a new code for it.
        assert!(html.contains(r#"<span class="amount-value">0.3</span>"#));
        assert!(html.contains(r#"<div class="qr-wrap qr-new">"#));
        assert!(html.contains("Send the remaining amount to"));
        assert!(!html.contains("the customer sent"));
    }

    #[test]
    fn the_stage_leads_the_page_in_every_state_and_the_code_never_leaves() {
        for status in [
            "pending",
            "partial",
            "unconfirmed",
            "confirming",
            "paid",
            "overpaid",
            "expired",
            "cancelled",
        ] {
            let mut data = test_checkout_view_model(!matches!(
                status,
                "pending" | "partial" | "unconfirmed" | "confirming"
            ));
            data.status = super::DisplayStatus::named(status);
            let html = checkout_page(&chrome(), &data).into_string();
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
            assert!(
                !html.contains(r#"id="payment-state""#)
                    && !html.contains(r#"class="receipt"#)
                    && !html.contains(r#"class="expiry-pill"#),
                "{status}: {html}"
            );
        }
    }

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
            let mut data = test_checkout_view_model(false);
            data.status = super::DisplayStatus::named(status);
            let html = checkout_page(&chrome(), &data).into_string();
            assert_eq!(html.contains("qr-wrap is-spent"), faded, "{status}: {html}");
        }
    }

    #[test]
    fn the_track_has_no_confirm_step_when_one_payment_is_enough() {
        let mut data = test_checkout_view_model(false);
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"<ol class="track" aria-label="Payment progress">"#),
            "{html}"
        );
        assert!(html.contains("Confirm<span"));

        data.confirmations_required = 0;
        data.status = shared::order_status::OrderStatus::Unconfirmed.into();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"<ol class="track track-2" aria-label="Payment progress">"#),
            "{html}"
        );
        assert!(!html.contains("Confirm<span"), "{html}");
        // Seen is the last step before Paid, so Paid is where the order is.
        assert!(html.contains(r#"<li class="step-now"><span class="track-dot" aria-hidden="true"></span><span class="track-name">Paid<span class="sr-only">, now</span>"#), "{html}");

        data.status = shared::order_status::OrderStatus::Paid.into();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains("XMR received. Nothing more to send."),
            "{html}"
        );
    }

    #[test]
    fn full_payment_hides_deadline_and_overpayment_explains_excess() {
        let mut data = test_checkout_view_model(false);
        data.status = shared::order_status::OrderStatus::Confirming.into();
        data.status_label = "Confirming".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(!html.contains(" left</span>"));
        assert!(!html.contains("Expires in"));
        assert!(html.contains("Payment address"));

        data.status = shared::order_status::OrderStatus::Overpaid.into();
        data.status_label = "Overpaid".to_string();
        data.status_class = "state-overpaid".to_string();
        data.is_terminal = true;
        data.amount_received_xmr = "0.600000000000".to_string();
        data.payment_error = Some("0.6 XMR received for a 0.5 XMR order (0.1 XMR extra). Do not send more. Contact the merchant about the extra amount.".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        // The stage explains the extra over a faded code for the order total.
        assert!(
            html.contains("<strong>Paid, with extra.</strong>")
                && html.contains(r#"<span class="amount-value">0.5</span>"#),
            "{html}"
        );
        assert!(html
            .contains("0.6 XMR received for a 0.5 XMR order (0.1 XMR extra). Do not send more."));
        assert!(html.contains(r#"<div class="qr-wrap is-spent">"#));
        assert!(!html.contains(" left</span>"));
        assert!(!html.contains("the customer sent"));
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
    fn checkout_page_keeps_the_refund_field_editable_after_saving() {
        let mut data = test_checkout_view_model(false);
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"id="refund_address""#));

        data.refund_address = Some("86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"id="refund_address""#));
        assert!(html.contains("refund-field is-saved"));
        assert!(html.contains("Refund address saved"));
        assert!(!html.contains("id=\"refund-address\""));
    }

    #[test]
    fn checkout_page_shows_the_double_spend_stage_only_when_one_was_detected() {
        let mut data = test_checkout_view_model(false);
        assert!(!checkout_page(&chrome(), &data)
            .into_string()
            .contains(r#"<div class="stage-track state-double-spend">"#));
        data.double_spend_detected_at = Some(1_700_000_000);
        let html = checkout_page(&chrome(), &data).into_string();
        // Still owed: the stage says so, and the code stays live.
        assert!(
            html.contains(r#"<div class="stage-track state-double-spend">"#)
                && html.contains("A payment was reversed"),
            "{html}"
        );
        assert!(!html.contains(r#"<div class="qr-wrap is-spent">"#));
        data.status = shared::order_status::OrderStatus::Unconfirmed.into();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains("Payment reversed")
                && html.contains("Contact the merchant before sending anything.")
                && html.contains(r#"<div class="qr-wrap is-spent">"#),
            "{html}"
        );
        assert!(html.contains(r#"<li class="step-fail">"#));
        data.status = shared::order_status::OrderStatus::Pending.into();
        // UTC, marked for the script to show in the customer's own zone.
        assert!(html.contains(r#"<time class="when" datetime="2023-11-14T22:13:20Z" title="Tuesday 14 November 2023, 22:13:20 (UTC)" data-local>14 Nov 2023, 22:13</time>"#), "{html}");
        // With `?timezone=`, that zone, and nothing for the script to change.
        data.clock = crate::views::time::Clock::new(Some("Australia/Perth"), None, 0);
        data.local_times = false;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(
            html.contains(">15 Nov 2023, 06:13</time>") && !html.contains("data-local"),
            "{html}"
        );
    }

    #[test]
    fn checkout_page_lists_payments_when_present() {
        let mut data = test_checkout_view_model(false);
        data.payments = vec![
            CheckoutPaymentViewModel {
                txid: format!("3f9a1c{}e21b07", "9".repeat(52)),
                amount_xmr: "0.25".to_string(),
                confirmations: 3,
                is_zero_conf: false,
            },
            CheckoutPaymentViewModel {
                txid: "short-txid".to_string(),
                amount_xmr: "0.25".to_string(),
                confirmations: 0,
                is_zero_conf: true,
            },
        ];
        let html = checkout_page(&chrome(), &data).into_string();
        // Each txid as `views::short_id` shows it, the whole in reach.
        assert!(html.contains(&format!(
            r#"<td><span title="3f9a1c{0}e21b07"><span aria-hidden="true">3f9a1c…e21b07</span><span class="sr-only">3f9a1c{0}e21b07</span></span></td>"#,
            "9".repeat(52)
        )), "{html}");
        assert!(html.contains("<td>short-txid</td>"), "{html}");
        assert!(!html.contains("..."), "{html}");
        assert!(html.contains("mempool"));
    }

    #[test]
    fn not_found_page_renders_with_no_nav() {
        let html = not_found_page(&chrome()).into_string();
        assert!(html.to_lowercase().contains("not found"));
        assert!(!html.contains(r#"<nav class="site-nav""#));
    }

    #[test]
    fn share_page_wraps_the_iframe_with_the_site_nav_when_found() {
        let data = CheckoutShareViewModel {
            pk: "pk_abc123".to_string(),
            order_id: shared::ids::OrderId::new("pay_abc123".to_string()),
            found: true,
        };
        let html = share_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="site-nav""#));
        assert!(html.contains("Monokulo"));
        assert!(html.contains(r#"src="/pay/pk_abc123/orders/pay_abc123""#));
        assert!(!html.contains("Order not found"));
    }

    #[test]
    fn share_page_shows_a_not_found_state_with_the_site_nav_when_not_found() {
        let data = CheckoutShareViewModel {
            pk: "pk_abc123".to_string(),
            order_id: shared::ids::OrderId::new("pay_abc123".to_string()),
            found: false,
        };
        let html = share_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="site-nav""#));
        assert!(html.to_lowercase().contains("not found"));
        assert!(!html.contains("<iframe"));
    }
}
