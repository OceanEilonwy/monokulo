//! `http/pos.rs::pos_page` - the terminal screen's own static shell. Every
//! live value (the entered amount, the QR/URI/NFC payment view, the
//! tick/progress overlay, backgrounded payments stacked at the bottom) is
//! driven client-side by JS talking to `http::pos`'s JSON endpoints - see
//! `http::pos`'s own module doc comment for why this screen, unlike the
//! public checkout page, leans on JS rather than working around it.
//!
//! Deliberately full-screen (no nav bar, no store title) via
//! [`super::layout_bare_with_head`] - meant to be left open on a
//! merchant's device at the counter all day, not navigated away from.

use maud::{html, Markup, PreEscaped};

use super::{layout_bare_with_head, PageChrome};

pub struct PosViewModel {
    pub connection_id: String,
    pub display_name: String,
    pub base_currency: String,
    /// How many decimal places the keypad's digit-shift should keep before
    /// inserting a decimal point - `2` for every fiat currency (matching
    /// `shared::exchange_rate::compute_xmr_amount`'s own 2-decimal-place
    /// limit), `12` when this store's `base_currency` is itself `"XMR"`
    /// (matching `shared::exchange_rate::parse_xmr_to_piconero`'s own native
    /// precision) - see `http::pos::pos_page`'s own doc comment.
    pub base_currency_decimals: u8,
}

const VIEWPORT: &str = "width=device-width, initial-scale=1, maximum-scale=1, viewport-fit=cover";

pub fn page(chrome: &PageChrome, data: &PosViewModel) -> Markup {
    let title = format!("POS - {} - Monokulo", data.display_name);
    let extra_head = html! { style { (PreEscaped(POS_STYLE)) } };
    let body = html! {
        div class="pos-topbar" {
            a href=(format!("/dashboard/stores/{}", data.connection_id)) class="pos-back" aria-label="Back to dashboard" title="Back to dashboard" { "←" }
            a href="/status" class="pos-status-link" id="pos-status-link" title="checking..." {
                span id="pos-status-dot" class="status-dot status-dot-unknown" {}
            }
        }

        div class="pos-wrap" {
            div id="pos-config"
                data-connection-id=(data.connection_id)
                data-base-currency=(data.base_currency)
                data-decimals=(data.base_currency_decimals)
                style="display:none" {}

            div id="keypad-screen" class="pos-screen" {
                div class="amount-display" id="amount-display" { "0.00" }
                div class="keypad" {
                    div class="key-grid" {
                        @for digit in ["1", "2", "3", "4", "5", "6", "7", "8", "9"] {
                            button type="button" class="key" data-digit=(digit) { span class="key-face" { (digit) } }
                        }
                        button type="button" class="key key-clear" id="key-clear" aria-label="Clear" { span class="key-face" { "C" } }
                        button type="button" class="key" data-digit="0" { span class="key-face" { "0" } }
                        button type="button" class="key key-backspace" id="key-backspace" aria-label="Backspace" {
                            span class="key-face" {
                                svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" {
                                    path d="M8 5 L2 12 L8 19 H21 A1 1 0 0 0 22 18 V6 A1 1 0 0 0 21 5 Z" fill="none" stroke-width="2" stroke-linejoin="round" {}
                                    path d="M12.2 9.3 L17.5 14.7 M17.5 9.3 L12.2 14.7" stroke-width="2" stroke-linecap="round" {}
                                }
                            }
                        }
                    }
                }
                div class="note-input-row" {
                    input type="text" id="note-input" placeholder="Reference (optional) — e.g. a name" maxlength="120" autocomplete="off";
                }
                button type="button" class="charge-btn" id="charge-btn" disabled { "Charge" }
                p id="pos-error" {}
            }

            div id="payment-screen" class="pos-screen pos-screen-hidden" {
                div class="payment-panel" {
                    div class="payment-amount" id="payment-amount" {}
                    div class="qr-holder" id="qr-holder" {}
                    div class="address-row" { code id="payment-address" {} }
                    p class="nfc-status" id="nfc-status" {}
                    button type="button" class="secondary-btn" id="cancel-btn" { "Cancel" }
                }
            }
        }

        div class="tick-overlay pos-screen-hidden" id="tick-overlay" {
            div class="tick-ring" {
                svg viewBox="0 0 120 120" class="ring-svg" aria-hidden="true" focusable="false" {
                    circle class="ring-track" cx="60" cy="60" r="54" {}
                    circle class="ring-progress" id="ring-progress" cx="60" cy="60" r="54" {}
                }
                div class="tick-mark" id="tick-mark" { "✓" }
            }
            p class="tick-label" id="tick-label" { "Paid" }
            p class="tick-note" id="tick-note" {}
            p class="tick-error" id="tick-error" {}
            div class="tick-actions" {
                button type="button" class="secondary-btn pos-screen-hidden" id="background-btn" { "Confirm in background" }
                button type="button" class="secondary-btn pos-screen-hidden" id="dismiss-btn" { "Dismiss" }
            }
        }

        div class="bg-stack" id="bg-stack" {}

        noscript {
            div class="wrap" { p class="error" { "This screen needs JavaScript for the live keypad, payment status and NFC tap payments. Use the plain \"create an order\" form on this store's own page instead." } }
        }

        script { (PreEscaped(POS_SCRIPT)) }
    };
    layout_bare_with_head(chrome, &title, VIEWPORT, extra_head, body)
}

/// A merchant-operated terminal screen, not the customer-facing checkout
/// page (`views::checkout`'s own JS-free hard requirement does not apply
/// here - see `http::pos`'s module doc comment). Square-Terminal-like: one
/// big amount readout, a numeric keypad with no decimal key (digits shift
/// in from the right, the decimal point is fixed by the store's own
/// currency), and a full-bleed payment/tick overlay once a sale is charged.
///
/// Unchanged, verbatim, from the old `pos.html.hbs`'s own `<style>` block -
/// it carried no handlebars syntax to begin with (confirmed - zero `{{` in
/// it), so there was nothing to convert; kept as a page-specific `<style>`
/// rather than folded into `views/head.html` since none of it applies to
/// any other page.
const POS_STYLE: &str = r#"
html, body { height: 100%; }
body { margin: 0; padding: 0; background: var(--paper); overscroll-behavior-y: contain; }

.pos-topbar {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: calc(env(safe-area-inset-top, 0px) + 0.5rem) calc(env(safe-area-inset-right, 0px) + 0.7rem) 0.3rem calc(env(safe-area-inset-left, 0px) + 0.7rem);
  z-index: 20;
  pointer-events: none;
}
.pos-topbar > * { pointer-events: auto; }
.pos-back {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 2em;
  height: 2em;
  font-size: 1.3rem;
  line-height: 1;
  color: var(--muted);
  text-decoration: none;
  opacity: 0.6;
}
.pos-back:hover, .pos-back:active { opacity: 1; color: var(--ink); }
.pos-status-link {
  display: inline-flex;
  align-items: center;
  padding: 0.5em;
  opacity: 0.65;
  text-decoration: none;
}
.pos-status-link:hover, .pos-status-link:active { opacity: 1; }
.pos-status-link .status-dot { margin-left: 0; }

.pos-wrap {
  margin: 0 auto;
  height: 100vh;
  height: 100dvh;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  padding: calc(env(safe-area-inset-top, 0px) + 2.6rem) calc(env(safe-area-inset-right, 0px) + 0.9rem) calc(env(safe-area-inset-bottom, 0px) + 0.9rem) calc(env(safe-area-inset-left, 0px) + 0.9rem);
}
.pos-screen { flex: 1; min-height: 0; }
.pos-screen-hidden { display: none !important; }
#keypad-screen { display: grid; grid-template-rows: auto minmax(0, 1fr) auto auto auto; gap: 1em; }
#keypad-screen > * { margin: 0; }

.amount-display {
  font-size: clamp(2.6rem, 12vw, 4.4rem);
  line-height: clamp(2.6rem, 12vw, 4.4rem);
  font-weight: 700;
  text-align: center;
  overflow-x: auto;
  white-space: nowrap;
}

.keypad {
  display: flex;
  align-items: center;
  justify-content: center;
  container-type: size;
  min-height: 0;
  min-width: 0;
}
.key-grid {
  width: min(100cqw, 100cqh * 3 / 4);
  height: min(100cqh, 100cqw * 4 / 3);
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  grid-template-rows: repeat(4, 1fr);
  gap: 0;
  background: var(--pos-frame);
  border-radius: 6cqmin;
}
.key {
  width: 100%;
  height: 100%;
  margin: 0;
  padding: 6%;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  border: none;
  background: none;
  cursor: pointer;
  -webkit-tap-highlight-color: transparent;
}
.key:hover { background: none; border-color: transparent; }
.key-face {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-family: inherit;
  font-size: clamp(1.4rem, 6vw, 2.6rem);
  font-weight: 700;
  color: var(--ink);
  border-radius: 22%;
  border: 2px solid var(--line);
  background: var(--paper-raised);
  transition: background 0.06s ease, color 0.06s ease, border-color 0.06s ease;
}
.key:hover .key-face { border-color: var(--accent); }
.key:active .key-face {
  background: var(--accent);
  color: var(--accent-ink);
  border-color: var(--accent);
}
.key-clear .key-face { color: var(--error); }
.key-clear:active .key-face { background: var(--error); color: var(--error-ink); border-color: var(--error); }
.key-backspace .key-face { color: var(--warning); }
.key-backspace:active .key-face { background: var(--warning); color: var(--warning-ink); border-color: var(--warning); }
.key-backspace svg { display: block; flex: none; width: 1em; height: 1em; }
.key-backspace svg, .key-backspace svg * { stroke: currentColor; }

.note-input-row input {
  width: 100%;
  box-sizing: border-box;
  font-family: inherit;
  font-size: 1rem;
  padding: var(--space-sm) var(--space-md);
  border: 2px solid var(--line);
  border-radius: var(--radius-sm);
  background: var(--paper-raised);
  color: var(--ink);
}
.note-input-row input:focus { outline: 3px solid var(--accent); outline-offset: 0; }

.charge-btn, .secondary-btn {
  display: block;
  width: 100%;
  flex: none;
  font-family: inherit;
  font-size: 1.1rem;
  font-weight: 700;
  padding: var(--space-sm) 0;
  border: 2px solid var(--line);
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: var(--accent-ink);
  cursor: pointer;
}
.charge-btn:disabled { background: var(--paper-raised); color: var(--muted); cursor: not-allowed; }
.charge-btn:not(:disabled):hover { filter: brightness(0.95); }
.secondary-btn { background: var(--paper-raised); color: var(--ink); }
.secondary-btn:hover { background: var(--accent); color: var(--accent-ink); border-color: var(--accent); }

#pos-error:empty { display: none; }
#pos-error { color: var(--error); font-weight: 700; margin: 0.6em 0 0; }

#payment-screen { display: flex; flex-direction: column; }
.payment-panel {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  justify-content: center;
  border: 2px solid var(--line);
  border-radius: var(--radius-md);
  background: var(--paper-raised);
  padding: 1em;
  text-align: center;
  box-shadow: 0 0.4em 1em rgba(var(--shadow-rgb), 0.1);
}
.payment-amount { font-size: 1.8rem; font-weight: 700; margin-bottom: 0.5em; }
.qr-holder { max-width: 260px; margin: 0 auto; }
.qr-holder svg { width: 100%; height: auto; display: block; }
.address-row { margin: 0.6em 0; word-break: break-all; font-size: 0.8rem; }
.nfc-status { color: var(--muted); font-size: 0.85rem; min-height: 1.2em; }

.tick-overlay {
  position: fixed;
  inset: 0;
  background: var(--paper);
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 0.6em;
  padding: 1.5em;
  text-align: center;
  z-index: 10;
}
.tick-ring { position: relative; width: 160px; height: 160px; }
.ring-svg { width: 100%; height: 100%; transform: rotate(-90deg); }
.ring-track { fill: none; stroke: var(--line); stroke-width: 6; opacity: 0.25; }
.ring-progress {
  fill: none;
  stroke: var(--success);
  stroke-width: 6;
  stroke-linecap: round;
  transition: stroke-dashoffset 0.4s linear;
}
.tick-overlay.is-error .ring-progress { stroke: var(--error); }
.tick-mark {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 4rem;
  font-weight: 700;
  color: var(--success);
}
.tick-overlay.is-error .tick-mark { color: var(--error); }
.tick-label { font-size: 1.3rem; font-weight: 700; margin: 0; }
.tick-note { font-size: 1rem; color: var(--muted); margin: 0; max-width: 22em; }
.tick-note:empty { display: none; }
.tick-error { color: var(--error); font-weight: 700; max-width: 22em; margin: 0; }
.tick-error:empty { display: none; }
.tick-actions { display: flex; flex-direction: column; gap: 0.5em; width: 100%; max-width: 20em; }

.bg-stack {
  position: fixed;
  left: 0;
  right: 0;
  bottom: 0;
  display: flex;
  flex-direction: column-reverse;
  gap: 0.4em;
  padding: 0.6em;
  align-items: center;
  z-index: 5;
  pointer-events: none;
}
.bg-item {
  pointer-events: auto;
  display: flex;
  align-items: center;
  gap: 0.6em;
  border: 2px solid var(--line);
  border-radius: var(--radius-sm);
  background: var(--paper-raised);
  padding: 0.4em 0.7em;
  font-size: 0.85rem;
  max-width: 360px;
  width: 100%;
  box-shadow: 0 0.3em 0.7em rgba(var(--shadow-rgb), 0.1);
}
.bg-item.is-paid { border-color: var(--success); }
.bg-item.is-error { border-color: var(--error); color: var(--error); }
.bg-item .bg-id { font-weight: 700; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }
.bg-item .bg-bar {
  flex: 1;
  height: 0.5em;
  border: 1px solid var(--line);
  background: var(--paper);
  position: relative;
  overflow: hidden;
}
.bg-item .bg-bar-fill { position: absolute; inset: 0; width: 0%; background: var(--success); }
.bg-item.is-error .bg-bar-fill { background: var(--error); }
.bg-item .bg-dismiss { border: none; background: none; cursor: pointer; font-weight: 700; color: inherit; }

@media (max-height: 480px) {
  .pos-wrap { padding-top: calc(env(safe-area-inset-top, 0px) + 1.1rem); }
  .amount-display { font-size: clamp(1.1rem, 6vw, 1.9rem); }
  .note-input-row input { padding: 0.3em 0.5em; font-size: 0.9rem; }
  .charge-btn, .secondary-btn { padding: 0.3em 0; margin-top: 0.25em; font-size: 1rem; }
  .key-face { font-size: clamp(1rem, 5vw, 1.6rem); }
  .payment-panel { padding: 0.6em; }
  .payment-amount { font-size: 1.2rem; margin-bottom: 0.25em; }
  .qr-holder { max-width: min(260px, 30vh); }
  .address-row { margin: 0.3em 0; font-size: 0.7rem; }
  .nfc-status { font-size: 0.75rem; min-height: 0; }
}
"#;

/// Unchanged, verbatim, from the old `pos.html.hbs`'s own `<script>` block,
/// except reading its three config values from `#pos-config`'s own
/// `data-*` attributes rather than `<body>`'s - a plain, invisible element
/// is a cleaner seam for this than special-casing `<body>`'s own attributes
/// through the shared [`super::page_shell`] every other page also renders
/// through.
const POS_SCRIPT: &str = r#"
(function () {
  "use strict";
  var config = document.getElementById("pos-config");
  var connectionId = config.getAttribute("data-connection-id");
  var decimals = parseInt(config.getAttribute("data-decimals"), 10) || 2;
  var MAX_DIGITS = decimals + 9;
  var POLL_INTERVAL_MS = 2000;
  var POLL_FAILURE_LIMIT = 3;
  var RING_CIRCUMFERENCE = 2 * Math.PI * 54;
  var MAX_NOTE_LENGTH = 120;

  var digits = "0";
  var amountDisplay = document.getElementById("amount-display");
  var chargeBtn = document.getElementById("charge-btn");
  var posError = document.getElementById("pos-error");
  var noteInput = document.getElementById("note-input");
  var keypadScreen = document.getElementById("keypad-screen");
  var paymentScreen = document.getElementById("payment-screen");
  var paymentAmount = document.getElementById("payment-amount");
  var qrHolder = document.getElementById("qr-holder");
  var paymentAddress = document.getElementById("payment-address");
  var nfcStatus = document.getElementById("nfc-status");
  var cancelBtn = document.getElementById("cancel-btn");
  var tickOverlay = document.getElementById("tick-overlay");
  var tickLabel = document.getElementById("tick-label");
  var tickNote = document.getElementById("tick-note");
  var tickError = document.getElementById("tick-error");
  var ringProgress = document.getElementById("ring-progress");
  var backgroundBtn = document.getElementById("background-btn");
  var dismissBtn = document.getElementById("dismiss-btn");
  var bgStack = document.getElementById("bg-stack");
  var statusDot = document.getElementById("pos-status-dot");
  var statusLink = document.getElementById("pos-status-link");

  ringProgress.style.strokeDasharray = String(RING_CIRCUMFERENCE);

  (function pollHealth() {
    fetch("/status/summary").then(function (r) { return r.json(); }).then(function (data) {
      if (data && data.healthy) {
        statusDot.className = "status-dot status-dot-ok";
        statusLink.title = "all systems healthy";
      } else {
        statusDot.className = "status-dot status-dot-error";
        statusLink.title = "an issue was detected - see the status page";
      }
    }).catch(function () {
      statusDot.className = "status-dot status-dot-unknown";
      statusLink.title = "could not check status";
    });
  })();

  var foreground = null; // { paymentId, timer, failures, note }
  var backgrounded = {}; // paymentId -> { el, timer, failures }

  function formatAmount(rawDigits) {
    var padded = rawDigits.padStart(decimals + 1, "0");
    var whole = decimals > 0 ? padded.slice(0, padded.length - decimals) : padded;
    var frac = decimals > 0 ? padded.slice(padded.length - decimals) : "";
    whole = whole.replace(/^0+(?=\d)/, "");
    return decimals > 0 ? whole + "." + frac : whole;
  }

  function groupThousands(whole) {
    return whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  }

  function displayAmount() {
    var plain = formatAmount(digits);
    var dot = plain.indexOf(".");
    if (dot === -1) return groupThousands(plain);
    return groupThousands(plain.slice(0, dot)) + plain.slice(dot);
  }

  function isZeroAmount() {
    return /^0+$/.test(digits);
  }

  function renderAmount() {
    amountDisplay.textContent = displayAmount();
    chargeBtn.disabled = isZeroAmount();
  }

  document.querySelectorAll(".key[data-digit]").forEach(function (btn) {
    btn.addEventListener("click", function () { pushDigit(btn.getAttribute("data-digit")); });
  });
  document.getElementById("key-backspace").addEventListener("click", backspace);
  document.getElementById("key-clear").addEventListener("click", clearAmount);

  function pushDigit(d) {
    var next = digits + d;
    if (next.length > MAX_DIGITS) next = next.slice(next.length - MAX_DIGITS);
    digits = next.replace(/^0+(?=\d)/, "");
    if (digits === "") digits = "0";
    renderAmount();
  }

  function backspace() {
    digits = digits.length > 1 ? digits.slice(0, -1) : "0";
    renderAmount();
  }

  function clearAmount() {
    digits = "0";
    renderAmount();
  }

  document.addEventListener("keydown", function (event) {
    if (keypadScreen.classList.contains("pos-screen-hidden")) return;
    if (document.activeElement === noteInput) {
      if (event.key === "Enter") chargeBtn.click();
      return;
    }
    if (event.key >= "0" && event.key <= "9") { pushDigit(event.key); }
    else if (event.key === "Backspace") { backspace(); }
    else if (event.key === "Escape") { clearAmount(); }
    else if (event.key === "Enter") { chargeBtn.click(); }
  });

  function setPosError(message) {
    posError.textContent = message || "";
  }

  function showScreen(el) {
    [keypadScreen, paymentScreen].forEach(function (s) { s.classList.add("pos-screen-hidden"); });
    el.classList.remove("pos-screen-hidden");
  }

  function shortId(paymentId) {
    return paymentId.length <= 14 ? paymentId : paymentId.slice(0, 6) + "…" + paymentId.slice(-4);
  }

  async function attemptNfcWrite(moneroUri) {
    if (!("NDEFReader" in window)) {
      nfcStatus.textContent = "Tap-to-pay (NFC) isn't available on this device - use the QR code.";
      return;
    }
    try {
      var reader = new NDEFReader();
      await reader.write({ records: [{ recordType: "url", data: moneroUri }] });
      nfcStatus.textContent = "Ready for tap-to-pay, or scan the QR code.";
    } catch (err) {
      nfcStatus.textContent = "Tap-to-pay unavailable - use the QR code.";
    }
  }

  async function chargeAmount() {
    setPosError("");
    var amount = formatAmount(digits);
    var note = noteInput.value.trim().slice(0, MAX_NOTE_LENGTH);
    chargeBtn.disabled = true;
    var response;
    try {
      response = await fetch("/dashboard/stores/" + connectionId + "/pos/orders", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ amount: amount, merchant_order_id: note ? note : null }),
      });
    } catch (err) {
      setPosError("Couldn't reach the server - check your connection and try again.");
      chargeBtn.disabled = isZeroAmount();
      return;
    }
    if (!response.ok) {
      var body = await response.json().catch(function () { return {}; });
      setPosError(body.error || "Something went wrong creating this order. Please try again.");
      chargeBtn.disabled = isZeroAmount();
      return;
    }
    var order = await response.json();
    openPaymentScreen(order, note);
  }
  chargeBtn.addEventListener("click", chargeAmount);

  function openPaymentScreen(order, note) {
    paymentAmount.textContent = order.amount + " " + order.currency;
    qrHolder.innerHTML = order.qr_code_svg;
    paymentAddress.textContent = order.address;
    nfcStatus.textContent = "";
    showScreen(paymentScreen);
    attemptNfcWrite(order.monero_uri);
    startForegroundPoll(order.payment_id, note);
  }

  function resetToKeypad() {
    digits = "0";
    renderAmount();
    setPosError("");
    noteInput.value = "";
    showScreen(keypadScreen);
  }

  cancelBtn.addEventListener("click", function () {
    stopForegroundPoll();
    resetToKeypad();
  });

  function stopForegroundPoll() {
    if (foreground && foreground.timer) clearTimeout(foreground.timer);
    foreground = null;
    tickOverlay.classList.add("pos-screen-hidden");
    tickOverlay.classList.remove("is-error");
  }

  function setRingProgress(percent) {
    var clamped = Math.max(0, Math.min(100, percent));
    ringProgress.style.strokeDashoffset = String(RING_CIRCUMFERENCE * (1 - clamped / 100));
  }

  function showTick(state) {
    tickOverlay.classList.remove("pos-screen-hidden");
    tickOverlay.classList.toggle("is-error", state.isError);
    setRingProgress(state.percent);
    tickLabel.textContent = state.isError ? "Needs attention" : "Paid";
    tickNote.textContent = state.note || "";
    tickError.textContent = state.isError ? state.message : "";
    backgroundBtn.classList.toggle("pos-screen-hidden", !state.canBackground);
    dismissBtn.classList.toggle("pos-screen-hidden", !(state.finished && state.isError));
  }

  async function pollOnce(paymentId) {
    var response;
    try {
      response = await fetch("/dashboard/stores/" + connectionId + "/pos/orders/" + paymentId + "/status");
    } catch (err) {
      return { networkError: true };
    }
    if (!response.ok) return { networkError: true };
    return await response.json();
  }

  function startForegroundPoll(paymentId, note) {
    foreground = { paymentId: paymentId, timer: null, failures: 0, note: note || "" };
    tickOverlay.classList.remove("is-error");
    var tick = async function () {
      if (!foreground || foreground.paymentId !== paymentId) return;
      var result = await pollOnce(paymentId);
      if (!foreground || foreground.paymentId !== paymentId) return;

      if (result.networkError) {
        foreground.failures += 1;
        if (foreground.failures >= POLL_FAILURE_LIMIT) {
          showTick({ percent: 100, isError: true, message: "Lost connection to the payment status - the order itself is unaffected. Retrying...", canBackground: false, finished: false, note: foreground.note });
        }
        foreground.timer = setTimeout(tick, POLL_INTERVAL_MS);
        return;
      }
      foreground.failures = 0;

      if (result.status === "pending") {
        tickOverlay.classList.add("pos-screen-hidden");
        foreground.timer = setTimeout(tick, POLL_INTERVAL_MS);
        return;
      }

      var percent = result.confirmations_required > 0
        ? Math.min(100, Math.round((result.confirmations / result.confirmations_required) * 100))
        : 100;
      var success = !result.error && (result.status === "paid" || (result.status === "overpaid" && !result.error));
      var canBackground = !result.is_terminal && result.confirmations_required > 0 && !result.error;

      showTick({
        percent: percent,
        isError: !!result.error,
        message: result.error || "",
        canBackground: canBackground,
        finished: result.is_terminal,
        note: foreground.note,
      });

      if (result.is_terminal) {
        if (success) {
          setTimeout(function () {
            if (foreground && foreground.paymentId === paymentId) {
              stopForegroundPoll();
              resetToKeypad();
            }
          }, 2500);
        }
        return;
      }

      foreground.timer = setTimeout(tick, POLL_INTERVAL_MS);
    };
    tick();
  }

  dismissBtn.addEventListener("click", function () {
    stopForegroundPoll();
    resetToKeypad();
  });

  backgroundBtn.addEventListener("click", function () {
    if (!foreground) return;
    var paymentId = foreground.paymentId;
    stopForegroundPoll();
    resetToKeypad();
    startBackgroundPoll(paymentId);
  });

  function startBackgroundPoll(paymentId) {
    var el = document.createElement("div");
    el.className = "bg-item";
    el.innerHTML =
      '<span class="bg-id"></span>' +
      '<span class="bg-bar"><span class="bg-bar-fill"></span></span>' +
      '<button type="button" class="bg-dismiss pos-screen-hidden" aria-label="Dismiss">&times;</button>';
    el.querySelector(".bg-id").textContent = shortId(paymentId);
    var fill = el.querySelector(".bg-bar-fill");
    var dismiss = el.querySelector(".bg-dismiss");
    bgStack.appendChild(el);

    var entry = { el: el, timer: null, failures: 0 };
    backgrounded[paymentId] = entry;

    dismiss.addEventListener("click", function () {
      if (entry.timer) clearTimeout(entry.timer);
      delete backgrounded[paymentId];
      el.remove();
    });

    var tick = async function () {
      if (!backgrounded[paymentId]) return;
      var result = await pollOnce(paymentId);
      if (!backgrounded[paymentId]) return;

      if (result.networkError) {
        entry.failures += 1;
        if (entry.failures >= POLL_FAILURE_LIMIT) {
          el.classList.add("is-error");
          dismiss.classList.remove("pos-screen-hidden");
        }
        entry.timer = setTimeout(tick, POLL_INTERVAL_MS);
        return;
      }
      entry.failures = 0;

      var percent = result.confirmations_required > 0
        ? Math.min(100, Math.round((result.confirmations / result.confirmations_required) * 100))
        : 100;
      fill.style.width = percent + "%";
      el.classList.toggle("is-error", !!result.error);
      if (result.error) dismiss.classList.remove("pos-screen-hidden");

      if (result.is_terminal) {
        if (!result.error) {
          el.classList.add("is-paid");
          el.querySelector(".bg-id").textContent = shortId(paymentId) + " — paid";
          setTimeout(function () {
            if (backgrounded[paymentId]) {
              delete backgrounded[paymentId];
              el.remove();
            }
          }, 4000);
        }
        return;
      }
      entry.timer = setTimeout(tick, POLL_INTERVAL_MS);
    };
    tick();
  }

  renderAmount();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/stores/conn-1/pos")
    }

    fn data() -> PosViewModel {
        PosViewModel {
            connection_id: "conn-1".to_string(),
            display_name: "example.com".to_string(),
            base_currency: "XMR".to_string(),
            base_currency_decimals: 12,
        }
    }

    #[test]
    fn renders_no_nav_and_carries_the_stores_base_currency_and_connection_id() {
        let html = page(&chrome(), &data()).into_string();
        assert!(
            html.contains("XMR"),
            "expected the store's own base currency shown, got: {html}"
        );
        assert!(html.contains(r#"data-connection-id="conn-1""#));
        assert!(html.contains(r#"data-decimals="12""#));
        assert!(
            html.contains("/dashboard/stores/conn-1"),
            "expected the back link to this connection's dashboard"
        );
        assert!(
            !html.contains("<nav class=\"site-nav\""),
            "the POS terminal must render with no site nav element at all"
        );
    }

    #[test]
    fn title_includes_the_display_name() {
        let html = page(&chrome(), &data()).into_string();
        assert!(html.contains("<title>POS - example.com - Monokulo</title>"));
    }
}
