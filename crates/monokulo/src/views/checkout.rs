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

use super::{layout_bare, layout_bare_with_head, layout_with_head, PageChrome};

/// One payment row on the checkout page's payments table - mirrors the
/// engine's own `PaymentViewModel` field-for-field.
pub struct CheckoutPaymentViewModel {
    pub txid_short: String,
    pub amount_xmr: String,
    pub confirmations: u64,
    pub is_zero_conf: bool,
}

/// The view model [`checkout_page`] takes.
pub struct CheckoutViewModel {
    pub order_id: String,
    pub status_label: String,
    pub status: String,
    pub status_class: String,
    pub address: String,
    /// Trusted, pre-rendered SVG markup (`http::checkout::qr_svg_for_html`) -
    /// rendered raw, unescaped.
    pub qr_code_svg: String,
    pub xmr_amount: String,
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
    /// Presence only - gates the double-spend banner. The actual text comes
    /// from `double_spend_detected_at_display`, pre-rendered server-side.
    pub double_spend_detected_at: Option<i64>,
    pub double_spend_detected_at_display: String,
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
.pay-header { display: flex; align-items: center; justify-content: center; gap: 0.6em; flex-wrap: wrap; margin-bottom: 0.8em; }
.status-pending, .status-unconfirmed, .status-confirming, .status-partial, .status-overpaid { background: var(--tint-warning); }
.status-paid { background: var(--tint-success); }
.status-expired, .status-unknown { background: var(--tint-error); }
.expiry-pill {
  display: inline-flex;
  align-items: center;
  gap: 0.35em;
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  color: var(--muted);
  padding: 0.05em 0.6em;
  font-size: 0.8em;
  font-weight: 700;
}
.expiry-pill svg { flex: none; width: 1em; height: 1em; }
.expiry-pill.expiry-soon { border-color: var(--warning); color: var(--warning); background: var(--tint-warning); }
.expiry-pill.expiry-urgent { border-color: var(--error); color: var(--error); background: var(--tint-error); }
.pay-grid { display: grid; grid-template-columns: 1fr; gap: 0; text-align: center; }
@media (min-width: 700px) {
  .pay-grid { grid-template-columns: minmax(0, 300px) minmax(0, 1fr); gap: 2rem; text-align: left; }
  .pay-col-primary { text-align: center; }
  .pay-header { justify-content: space-between; }
}
.amount { font-size: 2.2rem; font-weight: 700; margin: 0.2em 0 0.1em; letter-spacing: -0.01em; }
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
.payments-table { font-size: 0.8em; margin-top: 1.2em; }
.meta { margin-top: 1.4em; font-size: 0.8em; color: var(--muted); text-align: left; }
.visually-hidden {
  position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
  overflow: hidden; clip: rect(0,0,0,0); white-space: nowrap; border: 0;
}
.payment-state { border: 1px solid var(--line); padding: 1em; margin: 0 0 1em; text-align: center; background: var(--paper); }
.payment-state.is-error { border-color: var(--error); background: var(--tint-error); }
.payment-state.is-warning { border-color: var(--warning); background: var(--tint-warning); }
.payment-state.is-paid { border-color: var(--success); background: var(--tint-success); }
.payment-state-title { margin: 0 0 .3em; font-size: 1.35em; }
.payment-state p { margin: .3em 0; }
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
.refund-camera { width: 100%; max-height: 16em; background: var(--ink); }
.scan-error { color: var(--error); }
.checkout-compact { max-width: 100%; }
.checkout-compact .pay-grid { grid-template-columns: 1fr; gap: 0; text-align: center; }
.checkout-compact .pay-col-secondary { font-size: .9em; }
.checkout-compact .qr-wrap { margin-bottom: .5em; }
.checkout-compact .qr-wrap svg { width: min(42vh, 208px); height: auto; }
.checkout-compact .section { margin-top: .7em; padding-top: .7em; }
.checkout-compact .payment-state.is-paid { position: fixed; inset: 0; z-index: 5; display: flex; flex-direction: column; align-items: center; justify-content: center; }
.checkout-compact .meta, .checkout-compact .payments-table { display: none; }
.checkout-compact .pay-header { margin-bottom: .4em; }
.checkout-compact .amount { font-size: 1.8rem; font-weight: 800; margin: .1em 0; }
.checkout-compact .fiat-amount { font-size: .8em; margin-bottom: .4em; }
.checkout-compact .qr-wrap { margin-bottom: .3em; }
.checkout-compact .qr-wrap svg { width: min(29vh, 158px); height: auto; }
.checkout-compact .address-block { margin-bottom: .5em; }
.checkout-compact .address-row { border: 1px solid #999; border-radius: 4px; padding: .3em; }
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
    };
    let body = html! {
        div class=(if data.is_compact { "pay-wrap checkout-compact" } else { "pay-wrap" }) id="checkout-root" data-order-id=(data.order_id) data-status=(data.status) data-confirmations=(data.confirmations) data-error=(data.payment_error.as_deref().unwrap_or("")) {
            h1 class="visually-hidden" { "Monero payment of " (data.xmr_amount) " XMR" }
            @if !data.is_terminal {
                noscript {
                    p class="refresh-toggle" {
                        a href=(format!("/pay/{}/orders/{}{}", data.pk, data.order_id, data.toggled_refresh_suffix)) {
                            "Auto Refresh: " (if data.auto_refresh { "ON" } else { "OFF" })
                        }
                    }
                }
            }

            (live_status(data))

            div class="pay-grid" {
                div class="pay-col-primary" {
                    (amount_label(data))
                    div class="amount" id="xmr-amount" aria-labelledby="amount-label" { (data.xmr_amount) " XMR" }
                    div class="fiat-amount" { "≈ " (data.amount) " " (data.currency) }

                    div class="qr-wrap" { (PreEscaped(&data.qr_code_svg)) }

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
                        form method="post" id="refund-form" action=(format!("/pay/{}/orders/{}/refund-address{}", data.pk, data.order_id, data.query_suffix)) {
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
        @if data.refund_enabled { script src="/static/jsQR.js" {} }
        script src="/static/checkout.js" {}
    };
    layout_bare_with_head(chrome, "Pay with Monero", "width=device-width, initial-scale=1", extra_head, body)
}

/// The checkout page's parts that change while it's open, each an element
/// with an `id` and `data-live`. The live-update stream sends these,
/// re-rendered, and `checkout.js` swaps each into place by id - everything
/// else (QR code, address, the refund form mid-edit) is left untouched.
pub fn live_fragment(data: &CheckoutViewModel) -> Markup {
    html! {
        (live_status(data))
        (amount_label(data))
        (address_label(data))
        (live_progress(data))
        (live_payments(data))
    }
}

fn live_status(data: &CheckoutViewModel) -> Markup {
    let awaiting_payment = matches!(data.status.as_str(), "pending" | "partial");
    let amount_mismatch = matches!(data.status.as_str(), "partial" | "overpaid");
    let payment_state_class = if data.double_spend_detected_at.is_some() || (data.payment_error.is_some() && !amount_mismatch) {
        "payment-state is-error"
    } else if amount_mismatch {
        "payment-state is-warning"
    } else if data.status == "paid" {
        "payment-state is-paid"
    } else {
        "payment-state"
    };
    html! {
        div id="live-status" data-live {
            @if data.status != "pending" {
                div id="payment-state" class=(payment_state_class) role="status" {
                    h2 class="payment-state-title" { (data.status_label) }
                    @if let Some(error) = &data.payment_error {
                        p { (error) }
                    } @else if data.status == "paid" {
                        p { "Payment confirmed." }
                    } @else {
                        p { (data.confirmations) " of " (data.confirmations_required) " confirmations" }
                    }
                }
            }

            div class="pay-header" {
                span id="status-badge" class=(format!("tag {}", data.status_class)) { (data.status_label) }
                @if awaiting_payment {
                    span class=(format!("expiry-pill {}", data.expiry_urgency_class)) {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            path d="M6 2h12M6 22h12M7 2v4a5 5 0 0 0 2 4l3 2-3 2a5 5 0 0 0-2 4v4m10-20v4a5 5 0 0 1-2 4l-3 2 3 2a5 5 0 0 1 2 4v4" {}
                        }
                        "Send payment within " (data.expires_in_display)
                    }
                }
            }

            @if data.double_spend_detected_at.is_some() {
                div id="double-spend-banner" class="error" role="alert" {
                    "A payment toward this order was reversed by a blockchain double-spend, detected at "
                    span id="double-spend-time" { (data.double_spend_detected_at_display) }
                    "."
                    @if !data.is_terminal {
                        " This order's status above reflects only still-valid payments."
                    }
                }
            }
        }
    }
}

fn amount_label(data: &CheckoutViewModel) -> Markup {
    let amount_mismatch = matches!(data.status.as_str(), "partial" | "overpaid");
    html! {
        span class=(if amount_mismatch { "amount-label" } else { "visually-hidden" }) id="amount-label" data-live {
            @if amount_mismatch { "Order total" } @else { "Amount due" }
        }
    }
}

fn address_label(data: &CheckoutViewModel) -> Markup {
    html! {
        label class="address-label" id="address-label" for="address" data-live {
            @if data.status == "partial" { "Send the remaining amount to" }
            @else if data.status == "pending" { "Send exactly this amount to" }
            @else { "Payment address" }
        }
    }
}

fn live_progress(data: &CheckoutViewModel) -> Markup {
    html! {
        div class="section" id="live-progress" data-live {
            div class="progress-row" {
                span id="confirmations-label" { (data.confirmations) " / " (data.confirmations_required) " confirmations" }
                span id="received-label" { (data.amount_received_xmr) " XMR received" }
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
                        caption class="visually-hidden" { "Individual payments received toward this order" }
                        thead { tr { th scope="col" { "Tx" } th scope="col" { "Amount" } th scope="col" { "Confirmations" } } }
                        tbody id="payments-body" {
                            @for payment in &data.payments {
                                tr {
                                    td { (payment.txid_short) }
                                    td { (payment.amount_xmr) " XMR" }
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
pub struct CheckoutShareViewModel {
    pub pk: String,
    pub order_id: String,
    /// Whether the order actually exists - `false` renders a real
    /// not-found state (still with the site's own nav around it, unlike
    /// [`not_found_page`]'s bare equivalent), rather than a page whose only
    /// content is a broken iframe.
    pub found: bool,
}

const SHARE_STYLE: &str = r#"
.share-wrap { max-width: 840px; margin: 2.4rem auto; }
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
                h1 { "Pay with Monero" }
                p class="hint" { "Complete the payment below - this page stays up to date on its own, so it's safe to bookmark or come back to later." }
                iframe class="share-frame" id="checkout-frame" src=(format!("/pay/{}/orders/{}", data.pk, data.order_id)) title="Monero payment" {}
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
            order_id: "pay_abc123".to_string(),
            status_label: if is_terminal { "Paid".to_string() } else { "Waiting for payment".to_string() },
            status: if is_terminal { "paid".to_string() } else { "pending".to_string() },
            status_class: if is_terminal { "status-paid".to_string() } else { "status-pending".to_string() },
            address: "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC".to_string(),
            qr_code_svg: "<svg></svg>".to_string(),
            xmr_amount: "0.500000000000".to_string(),
            amount_received_xmr: "0.000000000000".to_string(),
            amount: "0.5".to_string(),
            currency: "XMR".to_string(),
            confirmations: 0,
            confirmations_required: 10,
            progress_percent: 0,
            is_terminal,
            double_spend_detected_at: None,
            double_spend_detected_at_display: "-".to_string(),
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
        assert!(html.contains("/static/checkout.js"));
        assert!(html.contains("id=\"scan-refund\""));
        assert!(html.contains("/static/jsQR.js"));
        assert!(html.contains(r#"<noscript><meta http-equiv="refresh" content="60" id="checkout-refresh"></noscript>"#));
        assert!(html.contains(r#"<noscript><p class="refresh-toggle"><a href="/pay/pk_abc123/orders/pay_abc123?refresh=false">Auto Refresh: ON</a></p></noscript>"#), "got: {html}");
        assert!(html.contains(r#"<noscript><button type="submit" class="btn-secondary">Save</button></noscript>"#));
        assert!(!html.contains(r#"<nav class="site-nav""#), "the checkout page must not carry the site nav, got: {html}");
        assert!(!html.contains("Monokulo"), "the checkout page must not carry the site brand/logo, got: {html}");
    }

    #[test]
    fn checkout_page_stops_meta_refreshing_once_the_order_is_terminal() {
        let html = checkout_page(&chrome(), &test_checkout_view_model(true)).into_string();
        assert!(html.contains("/static/checkout.js"));
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
        assert!(!html.contains(r#"http-equiv="refresh""#), "auto refresh off must drop the meta refresh, got: {html}");
        assert!(html.contains(r#"<a href="/pay/pk_abc123/orders/pay_abc123?view=compact">Auto Refresh: OFF</a>"#), "got: {html}");
        assert!(html.contains(r#"action="/pay/pk_abc123/orders/pay_abc123/refund-address?view=compact&amp;refresh=false""#), "saving must keep auto refresh off, got: {html}");

        let terminal = checkout_page(&chrome(), &test_checkout_view_model(true)).into_string();
        assert!(!terminal.contains("Auto Refresh"), "a terminal order never refreshes, so it has nothing to toggle, got: {terminal}");
    }

    #[test]
    fn checkout_page_meta_refreshes_once_a_minute_only_without_javascript() {
        let mut data = test_checkout_view_model(false);
        data.refund_address = Some(data.address.clone());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<noscript><meta http-equiv="refresh" content="60" id="checkout-refresh"></noscript>"#));
        assert_eq!(html.matches("http-equiv=\"refresh\"").count(), 1, "the refresh must only exist inside noscript, got: {html}");
    }

    #[test]
    fn live_fragment_carries_every_changing_region_and_nothing_else() {
        let mut data = test_checkout_view_model(false);
        data.status = "partial".to_string();
        data.payments = vec![CheckoutPaymentViewModel {
            txid_short: "abcd1234…ef5678".to_string(),
            amount_xmr: "0.200000000000".to_string(),
            confirmations: 0,
            is_zero_conf: true,
        }];
        let fragment = live_fragment(&data).into_string();
        for id in ["live-status", "amount-label", "address-label", "live-progress", "live-payments"] {
            assert!(fragment.contains(&format!(r#"id="{id}""#)), "missing {id} in {fragment}");
        }
        assert_eq!(fragment.matches("data-live").count(), 5);
        assert!(fragment.contains("mempool"));
        assert!(!fragment.contains("refund_address"), "the refund form must never be swapped mid-edit");
        assert!(!fragment.contains("<svg role=\"presentation\""), "the QR code never changes and is not re-sent");
        // Every region in the fragment is the same markup the page itself renders.
        let page = checkout_page(&chrome(), &data).into_string();
        assert!(page.contains(&fragment[fragment.find("<span").unwrap()..fragment.find("</span>").unwrap()]));
    }

    #[test]
    fn pos_view_uses_the_same_payment_markup_with_a_prominent_success_state() {
        let mut data = test_checkout_view_model(true);
        data.is_compact = true;
        data.query_suffix = "?view=compact".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("pay-wrap checkout-compact"));
        assert!(html.contains("payment-state is-paid"));
        assert!(html.contains("id=\"copy-address\""));
        assert!(html.contains("class=\"address-block\""));
        assert!(html.contains("id=\"refund_address\""));
        assert!(html.contains("refund-address?view=compact"));
    }

    #[test]
    fn partial_payment_shows_remaining_amount_and_deadline() {
        let mut data = test_checkout_view_model(false);
        data.status = "partial".to_string();
        data.status_label = "Partial payment received".to_string();
        data.status_class = "status-partial".to_string();
        data.amount_received_xmr = "0.200000000000".to_string();
        data.payment_error = Some("0.200000000000 XMR received of 0.500000000000 XMR. Send the remaining 0.300000000000 XMR to the address below.".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("payment-state is-warning"));
        assert!(html.contains("0.200000000000 XMR received of 0.500000000000 XMR. Send the remaining 0.300000000000 XMR"));
        assert!(html.contains(r#"class="amount-label" id="amount-label" data-live>Order total</span>"#));
        assert!(html.contains("Send the remaining amount to"));
        assert!(html.contains("Send payment within 30m"));
        assert!(!html.contains("the customer sent"));
    }

    #[test]
    fn full_payment_hides_deadline_and_overpayment_explains_excess() {
        let mut data = test_checkout_view_model(false);
        data.status = "confirming".to_string();
        data.status_label = "Confirming".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(!html.contains("Send payment within"));
        assert!(!html.contains("Expires in"));
        assert!(html.contains("Payment address"));

        data.status = "overpaid".to_string();
        data.status_label = "Overpaid".to_string();
        data.status_class = "status-overpaid".to_string();
        data.is_terminal = true;
        data.amount_received_xmr = "0.600000000000".to_string();
        data.payment_error = Some("0.600000000000 XMR received for a 0.500000000000 XMR order (0.100000000000 XMR extra). Do not send more. Contact the merchant about the extra amount.".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("payment-state is-warning"));
        assert!(html.contains("0.600000000000 XMR received for a 0.500000000000 XMR order (0.100000000000 XMR extra). Do not send more."));
        assert!(html.contains(r#"class="amount-label" id="amount-label" data-live>Order total</span>"#));
        assert!(!html.contains("Send payment within"));
        assert!(!html.contains("the customer sent"));
    }

    #[test]
    fn refund_flag_hides_the_entire_refund_section() {
        let mut data = test_checkout_view_model(false);
        data.refund_enabled = false;
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(!html.contains("id=\"refund_address\""));
        assert!(!html.contains("/static/jsQR.js"));
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
    fn checkout_page_shows_the_double_spend_banner_only_when_one_was_detected() {
        let mut data = test_checkout_view_model(false);
        data.double_spend_detected_at = Some(1_700_000_000);
        data.double_spend_detected_at_display = "2023-11-14".to_string();
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("double-spend-banner"));
        assert!(html.contains("2023-11-14"));
    }

    #[test]
    fn checkout_page_lists_payments_when_present() {
        let mut data = test_checkout_view_model(false);
        data.payments = vec![
            CheckoutPaymentViewModel { txid_short: "abc…def".to_string(), amount_xmr: "0.25".to_string(), confirmations: 3, is_zero_conf: false },
            CheckoutPaymentViewModel { txid_short: "ghi…jkl".to_string(), amount_xmr: "0.25".to_string(), confirmations: 0, is_zero_conf: true },
        ];
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains("abc…def"));
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
        let data = CheckoutShareViewModel { pk: "pk_abc123".to_string(), order_id: "pay_abc123".to_string(), found: true };
        let html = share_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="site-nav""#));
        assert!(html.contains("Monokulo"));
        assert!(html.contains(r#"src="/pay/pk_abc123/orders/pay_abc123""#));
        assert!(!html.contains("Order not found"));
    }

    #[test]
    fn share_page_shows_a_not_found_state_with_the_site_nav_when_not_found() {
        let data = CheckoutShareViewModel { pk: "pk_abc123".to_string(), order_id: "pay_abc123".to_string(), found: false };
        let html = share_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="site-nav""#));
        assert!(html.to_lowercase().contains("not found"));
        assert!(!html.contains("<iframe"));
    }
}
