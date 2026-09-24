//! `http/checkout.rs` - the real, public checkout/payment page
//! (`GET /pay/{pk}/orders/{payment_id}`), its "order not found" fallback,
//! and the nav-bearing `/share` wrapper around it.
//!
//! **`checkout_page`/`not_found_page` deliberately carry no site nav at
//! all** - a merchant embeds `checkout_page` in an iframe inside their own
//! checkout flow; Monokulo's own site navigation has no business appearing
//! inside it. They still use the shared head partial for consistent
//! typography/color, just not [`super::layout`]'s nav bar - see
//! [`super::layout_bare_with_head`]/[`super::layout_bare`].
//!
//! **`checkout_page` carries no `<script>` element at all** - a real
//! customer paying real money must be able to trust and use this page with
//! JavaScript disabled. A `<meta http-equiv="refresh">` re-fetches the whole
//! page every 10s while the order is still in progress (stopped once
//! `is_terminal`), and every value on it (status, amounts, the progress
//! bar's fill, the relative "expires in" time, even the expiry pill's own
//! color) is already correct in the HTML `http::checkout::render_checkout_page`
//! returns - nothing here may depend on client-side JS to be meaningful.

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
    pub payment_id: String,
    pub status_label: String,
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
    pub pk: String,
    pub payments: Vec<CheckoutPaymentViewModel>,
}

/// Unchanged, verbatim, from the old `checkout.html.hbs`'s own `<style>`
/// block - it carried no handlebars syntax to begin with (confirmed - zero
/// `{{` in it), so there was nothing to convert.
const CHECKOUT_STYLE: &str = r#"
body { padding: 1.2rem; background: var(--paper-raised); }
.pay-wrap { max-width: 720px; margin: 0 auto; }
.pay-header { display: flex; align-items: center; justify-content: center; gap: 0.6em; flex-wrap: wrap; margin-bottom: 0.8em; }
.status-pending, .status-unconfirmed, .status-confirming, .status-partial { background: var(--tint-warning); }
.status-paid { background: var(--tint-success); }
.status-expired, .status-unknown { background: var(--tint-error); }
.expiry-pill {
  display: inline-block;
  border: 1px solid var(--line);
  color: var(--muted);
  padding: 0.05em 0.6em;
  font-size: 0.8em;
  font-weight: 700;
}
.expiry-pill.expiry-soon { border-color: var(--warning); color: var(--warning); background: var(--tint-warning); }
.expiry-pill.expiry-urgent { border-color: var(--error); color: var(--error); background: var(--tint-error); }
.pay-grid { display: grid; grid-template-columns: 1fr; gap: 0; text-align: center; }
@media (min-width: 700px) {
  .pay-grid { grid-template-columns: minmax(0, 300px) minmax(0, 1fr); gap: 2rem; text-align: left; }
  .pay-col-primary { text-align: center; }
  .pay-header { justify-content: space-between; }
}
.amount { font-size: 2.2rem; font-weight: 700; margin: 0.2em 0 0.1em; letter-spacing: -0.01em; }
.fiat-amount { color: var(--muted); margin-bottom: 1.2em; }
.qr-wrap { margin: 0 auto 1.2em; display: flex; justify-content: center; }
.qr-wrap svg { width: 208px; height: 208px; }
.address-card {
  display: block;
  text-align: left;
  border: 1px solid var(--line);
  background: var(--paper-raised);
  padding: 0.6em 0.7em;
  cursor: pointer;
  margin: 0 0 1.4em;
}
.address-card:focus-within { outline: 3px solid var(--accent); outline-offset: 0; }
.address-card-row { display: flex; align-items: center; gap: 0.5em; }
.address-copy-icon { flex: none; width: 1.1em; height: 1.1em; color: var(--muted); }
.address-label { display: block; color: var(--muted); font-size: 0.8em; margin: 0 0 0.35em; }
.address-text {
  flex: 1;
  width: 100%;
  font-size: 0.85em;
  resize: none;
  border: none;
  background: none;
  padding: 0;
  height: 2.6em;
  cursor: pointer;
}
.section {
  text-align: left;
  margin-top: 1.6em;
  padding-top: 1.4em;
  border-top: 1px solid var(--line);
}
.pay-grid > .pay-col-primary > .section:first-child,
.pay-grid > .pay-col-secondary > .section:first-child { margin-top: 0; padding-top: 0; border-top: none; }
.progress-row { display: flex; justify-content: space-between; font-size: 0.85em; margin-bottom: 0.4em; }
.progress-bar { border: 1px solid var(--line); border-radius: var(--radius-sm); overflow: hidden; height: 0.5em; background: var(--paper); }
.progress-fill { height: 100%; background: var(--accent); }
.payments-table { font-size: 0.8em; margin-top: 1.2em; }
.meta { margin-top: 1.4em; font-size: 0.8em; color: var(--muted); text-align: left; }
.visually-hidden {
  position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
  overflow: hidden; clip: rect(0,0,0,0); white-space: nowrap; border: 0;
}
"#;

pub fn checkout_page(chrome: &PageChrome, data: &CheckoutViewModel) -> Markup {
    let extra_head = html! {
        @if !data.is_terminal {
            meta http-equiv="refresh" content="10";
        }
        style { (PreEscaped(CHECKOUT_STYLE)) }
    };
    let body = html! {
        div class="pay-wrap" {
            h1 class="visually-hidden" { "Monero payment of " (data.xmr_amount) " XMR" }

            div class="pay-header" {
                span id="status-badge" class=(format!("tag {}", data.status_class)) { (data.status_label) }
                @if !data.is_terminal {
                    span class=(format!("expiry-pill {}", data.expiry_urgency_class)) { "Expires in " (data.expires_in_display) }
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

            div class="pay-grid" {
                div class="pay-col-primary" {
                    span class="visually-hidden" id="amount-label" { "Amount due" }
                    div class="amount" id="xmr-amount" aria-labelledby="amount-label" { (data.xmr_amount) " XMR" }
                    div class="fiat-amount" { "≈ " (data.amount) " " (data.currency) }

                    div class="qr-wrap" { (PreEscaped(&data.qr_code_svg)) }

                    label class="address-card" for="address" {
                        span class="address-label" id="address-label" { "Send exactly this amount to - tap to select, then copy" }
                        span class="address-card-row" {
                            svg class="address-copy-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
                                rect x="9" y="9" width="12" height="12" rx="1" {}
                                path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1" {}
                            }
                            textarea class="address-text" id="address" readonly aria-labelledby="address-label" { (data.address) }
                        }
                    }
                }
                div class="pay-col-secondary" {
                    div class="section" {
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

                    div class="section" {
                        @if let Some(refund_address) = &data.refund_address {
                            span class="address-label" id="refund-address-label" { "Refund address on file" }
                            textarea class="address-text" id="refund-address" readonly aria-labelledby="refund-address-label" style="border: 1px solid var(--line); padding: 0.6em; height: 3.4em;" { (refund_address) }
                        } @else {
                            form method="post" action=(format!("/pay/{}/orders/{}/refund-address", data.pk, data.payment_id)) {
                                label for="refund_address" { "Refund address " span class="field-help" style="display:inline" { "(optional)" } }
                                @if let Some(error) = &data.refund_address_error {
                                    div class="error" { (error) }
                                }
                                input type="text" id="refund_address" name="refund_address" placeholder="Your Monero refund address";
                                span class="field-help" {
                                    "If this order is ever overpaid, expires with funds already sent, or otherwise needs a "
                                    "refund, we'll send it back to this address - not to the address above. Only you can set this; the merchant "
                                    "never sees or chooses it."
                                }
                                button type="submit" class="btn-secondary" { "Save" }
                            }
                        }
                    }

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

                    div class="meta" {
                        "Order ID: " (data.payment_id) br;
                        @if !data.is_terminal {
                            "Expires in " (data.expires_in_display)
                        }
                    }
                }
            }
        }
    };
    layout_bare_with_head(chrome, "Pay with Monero", "width=device-width, initial-scale=1", extra_head, body)
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

/// The view model [`share_page`] takes - unlike [`CheckoutViewModel`] this
/// page *does* carry the site nav (via [`super::layout`]).
pub struct CheckoutShareViewModel {
    pub pk: String,
    pub payment_id: String,
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
                iframe class="share-frame" id="checkout-frame" src=(format!("/pay/{}/orders/{}", data.pk, data.payment_id)) title="Monero payment" {}
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
            payment_id: "pay_abc123".to_string(),
            status_label: if is_terminal { "Paid".to_string() } else { "Waiting for payment".to_string() },
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
            pk: "pk_abc123".to_string(),
            payments: vec![],
        }
    }

    #[test]
    fn checkout_page_carries_no_script_and_meta_refreshes_a_still_in_progress_order() {
        let html = checkout_page(&chrome(), &test_checkout_view_model(false)).into_string();
        assert!(!html.contains("<script"), "the checkout page must carry no JavaScript at all, got: {html}");
        assert!(
            html.contains(r#"<meta http-equiv="refresh" content="10">"#),
            "expected a meta-refresh directive on a still-in-progress order, got: {html}"
        );
        assert!(!html.contains(r#"<nav class="site-nav""#), "the checkout page must not carry the site nav, got: {html}");
        assert!(!html.contains("Monokulo"), "the checkout page must not carry the site brand/logo, got: {html}");
    }

    #[test]
    fn checkout_page_stops_meta_refreshing_once_the_order_is_terminal() {
        let html = checkout_page(&chrome(), &test_checkout_view_model(true)).into_string();
        assert!(!html.contains("<script"), "the checkout page must carry no JavaScript at all, got: {html}");
        assert!(
            !html.contains(r#"<meta http-equiv="refresh""#),
            "a paid/terminal order must not keep re-fetching itself, got: {html}"
        );
    }

    #[test]
    fn checkout_page_shows_the_refund_form_until_an_address_is_on_file() {
        let mut data = test_checkout_view_model(false);
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"id="refund_address""#));

        data.refund_address = Some("86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC".to_string());
        let html = checkout_page(&chrome(), &data).into_string();
        assert!(!html.contains(r#"id="refund_address""#));
        assert!(html.contains("Refund address on file"));
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
        let data = CheckoutShareViewModel { pk: "pk_abc123".to_string(), payment_id: "pay_abc123".to_string(), found: true };
        let html = share_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="site-nav""#));
        assert!(html.contains("Monokulo"));
        assert!(html.contains(r#"src="/pay/pk_abc123/orders/pay_abc123""#));
        assert!(!html.contains("Order not found"));
    }

    #[test]
    fn share_page_shows_a_not_found_state_with_the_site_nav_when_not_found() {
        let data = CheckoutShareViewModel { pk: "pk_abc123".to_string(), payment_id: "pay_abc123".to_string(), found: false };
        let html = share_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="site-nav""#));
        assert!(html.to_lowercase().contains("not found"));
        assert!(!html.contains("<iframe"));
    }
}
