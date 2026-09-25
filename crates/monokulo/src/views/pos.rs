//! `http/pos.rs::pos_page` - the terminal screen's own static shell. Every
//! live value (the entered amount, the shared checkout iframe, and
//! backgrounded payments stacked at the top) is
//! driven client-side by JS talking to `http::pos`'s JSON endpoints - see
//! `http::pos`'s own module doc comment for why this screen, unlike the
//! public checkout page, leans on JS rather than working around it.
//!
//! Deliberately full-screen (no nav bar, with a compact store title) via
//! [`super::layout_bare_with_head`] - meant to be left open on a
//! merchant's device at the counter all day, not navigated away from.

use maud::{html, Markup, PreEscaped};

use super::{layout_bare_with_head, store_breadcrumb, PageChrome};

pub struct PosViewModel {
    pub connection_id: String,
    pub public_key: String,
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
            div class="pos-topbar-start" {
                (store_breadcrumb(&data.connection_id, &data.display_name, false))
                span class="breadcrumb-sep" aria-hidden="true" { "›" }
                span class="pos-title" { "POS" }
            }
            button type="button" class="secondary-btn pos-topbar-action pos-screen-hidden" id="background-btn" { "Confirm in background" }
            details class="background-disclosure" id="background-disclosure" hidden {
                summary id="background-summary" { "Background orders (0)" }
                div class="bg-stack" id="bg-stack" {}
            }
            (super::status_indicator(chrome.health, "pos-status-link", false))
        }

        div class="pos-wrap" {
            div id="pos-config"
                data-connection-id=(data.connection_id)
                data-public-key=(data.public_key)
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
                    iframe id="payment-frame" title="Monero payment" {}
                    p id="payment-error" class="error" hidden {}
                    button type="button" class="secondary-btn pos-screen-hidden" id="dismiss-btn" { "Dismiss" }
                    button type="button" class="secondary-btn" id="cancel-btn" { "Cancel" }
                }
            }
        }

        noscript {
            style { (PreEscaped(".pos-wrap{display:none}.pos-no-js{padding-top:4rem}")) }
            div class="wrap pos-no-js" { p class="error" { "POS requires JavaScript. Use Create an order on the store page instead." } }
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
/// currency), and the shared checkout view once a sale is charged.
///
/// Unchanged, verbatim, from the old `pos.html.hbs`'s own `<style>` block -
/// it carried no handlebars syntax to begin with (confirmed - zero `{{` in
/// it), so there was nothing to convert; kept as a page-specific `<style>`
/// rather than folded into `views/head.html` since none of it applies to
/// any other page.
const POS_STYLE: &str = r#"
html, body { height: 100%; }
body { margin: 0; padding: 0; height: 100vh; height: 100dvh; display: flex; flex-direction: column; background: var(--paper); overscroll-behavior-y: contain; }

.pos-topbar {
  position: relative;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: calc(env(safe-area-inset-top, 0px) + 0.3rem) calc(env(safe-area-inset-right, 0px) + 0.7rem) 0.5rem calc(env(safe-area-inset-left, 0px) + 0.7rem);
  z-index: 20;
  pointer-events: none;
}
.pos-topbar > * { pointer-events: auto; }
.pos-topbar-start { display: flex; align-items: center; gap: 0.5em; min-width: 0; }
.pos-topbar-start .context-nav { margin: 0; min-width: 0; }
.pos-topbar-start .context-nav a { max-width: min(45vw, 18rem); }
.pos-title { color: var(--ink); font-size: 0.85rem; font-weight: 700; white-space: nowrap; }
.pos-topbar-action { flex: none; font-size: .72rem; padding: .4em .55em; margin-left: auto; }
.pos-status-link {
  display: inline-flex;
  align-items: center;
  padding: 0.5em;
  opacity: 0.65;
  text-decoration: none;
}
.pos-status-link:hover, .pos-status-link:active { opacity: 1; }
.pos-status-link .status-dot { margin-left: 0; }
.background-disclosure { position: relative; margin-left: .35em; font-size: .8rem; }
.background-disclosure summary { cursor: pointer; border: 1px solid var(--line); background: var(--paper-raised); padding: .35em .6em; }
.background-disclosure.has-error summary { border-color: var(--error); color: var(--error); }
.background-disclosure .bg-item { width: 100%; max-width: none; cursor: pointer; box-shadow: none; }

.pos-wrap {
  margin: 0 auto;
  width: 100%;
  flex: 1;
  min-height: 0;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  padding: 0.3rem calc(env(safe-area-inset-right, 0px) + 0.9rem) calc(env(safe-area-inset-bottom, 0px) + 0.9rem) calc(env(safe-area-inset-left, 0px) + 0.9rem);
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
.payment-panel iframe { width: 100%; flex: 1; min-height: 0; border: 0; background: var(--paper-raised); }
.payment-panel > button { flex: none; }
#payment-error { flex: none; margin: .3em 0; }
.bg-stack {
  position: absolute;
  top: 100%;
  right: 0;
  width: min(90vw, 360px);
  max-height: min(60vh, 28em);
  overflow-y: auto;
  background: var(--paper);
  border: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 0.4em;
  padding: 0.4em;
  align-items: center;
  z-index: 20;
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
  .pos-wrap { padding-top: 0.3rem; }
  .amount-display { font-size: clamp(1.1rem, 6vw, 1.9rem); }
  .note-input-row input { padding: 0.3em 0.5em; font-size: 0.9rem; }
  .charge-btn, .secondary-btn { padding: 0.3em 0; margin-top: 0.25em; font-size: 1rem; }
  .key-face { font-size: clamp(1rem, 5vw, 1.6rem); }
  .payment-panel { padding: 0.6em; }
}
"#;

/// Ported from the old `pos.html.hbs`'s own `<script>` block, reading its
/// three config values from `#pos-config`'s own `data-*` attributes rather
/// than `<body>`'s - a plain, invisible element is a cleaner seam for this
/// than special-casing `<body>`'s own attributes through the shared
/// [`super::page_shell`] every other page also renders through. Payment
/// status comes from one EventSource on `/pos/events` covering every
/// watched order, not a poll per order.
const POS_SCRIPT: &str = r#"
(function () {
  "use strict";
  var config = document.getElementById("pos-config");
  var connectionId = config.getAttribute("data-connection-id");
  var publicKey = config.getAttribute("data-public-key");
  var decimals = parseInt(config.getAttribute("data-decimals"), 10) || 2;
  var MAX_DIGITS = decimals + 9;
  // How long the status stream may be down before the screen says so -
  // EventSource reconnects on its own, so a brief blip shouldn't alarm.
  var CONNECTION_GRACE_MS = 6000;
  var STREAM_RETRY_MS = 5000;
  var MAX_NOTE_LENGTH = 120;

  var digits = "0";
  var amountDisplay = document.getElementById("amount-display");
  var chargeBtn = document.getElementById("charge-btn");
  var posError = document.getElementById("pos-error");
  var noteInput = document.getElementById("note-input");
  var keypadScreen = document.getElementById("keypad-screen");
  var paymentScreen = document.getElementById("payment-screen");
  var paymentFrame = document.getElementById("payment-frame");
  var paymentError = document.getElementById("payment-error");
  var cancelBtn = document.getElementById("cancel-btn");
  var backgroundBtn = document.getElementById("background-btn");
  var dismissBtn = document.getElementById("dismiss-btn");
  var bgStack = document.getElementById("bg-stack");
  var bgDisclosure = document.getElementById("background-disclosure");
  var bgSummary = document.getElementById("background-summary");

  var foreground = null; // { orderId, note }
  var backgrounded = {}; // orderId -> { el, note, markLost }

  // One EventSource carries status for every order being watched - the
  // one on screen and every backgrounded one. The server sends each order's
  // status on connect and again only when it changes; the stream is
  // reopened with the new id list whenever that set changes.
  var watched = {}; // orderId -> function (status)
  var stream = null;
  var reopenTimer = null;
  var lostTimer = null;
  var connectionLost = false;

  function watch(orderId, handler) {
    watched[orderId] = handler;
    reopenStream();
  }

  function unwatch(orderId) {
    if (!watched[orderId]) return;
    delete watched[orderId];
    reopenStream();
  }

  function reopenStream(delay) {
    if (reopenTimer) return;
    reopenTimer = setTimeout(function () {
      reopenTimer = null;
      if (stream) { stream.close(); stream = null; }
      var ids = Object.keys(watched);
      if (!ids.length) { setConnectionLost(false); return; }
      var source = new EventSource("/dashboard/stores/" + connectionId + "/pos/events?orders=" + ids.map(encodeURIComponent).join(","));
      stream = source;
      source.addEventListener("open", function () { setConnectionLost(false); });
      source.addEventListener("status", function (event) {
        var result;
        try { result = JSON.parse(event.data); } catch (err) { return; }
        setConnectionLost(false);
        var handler = watched[result.order_id];
        if (!handler) return;
        if (result.is_terminal) unwatch(result.order_id);
        handler(result);
      });
      source.addEventListener("error", function () {
        if (stream !== source) return;
        // A rejected request (signed out, server error) is not retried by
        // EventSource itself.
        if (source.readyState === EventSource.CLOSED) reopenStream(STREAM_RETRY_MS);
        if (!lostTimer) {
          lostTimer = setTimeout(function () {
            lostTimer = null;
            if (stream && stream.readyState !== EventSource.OPEN) setConnectionLost(true);
          }, CONNECTION_GRACE_MS);
        }
      });
    }, delay || 0);
  }

  function setConnectionLost(lost) {
    if (lost === connectionLost) return;
    connectionLost = lost;
    if (lost) {
      paymentError.textContent = "Lost connection to payment status. Retrying...";
      paymentError.hidden = !foreground;
      Object.keys(backgrounded).forEach(function (id) { backgrounded[id].markLost(); });
      updateBackgroundSummary();
    } else {
      paymentError.hidden = true;
    }
  }

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

  function shortId(orderId) {
    return orderId.length <= 14 ? orderId : orderId.slice(0, 6) + "…" + orderId.slice(-4);
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
    openOrder(order.order_id, note);
  }

  function openOrder(orderId, note) {
    paymentFrame.src = "/pay/" + encodeURIComponent(publicKey) + "/orders/" + encodeURIComponent(orderId) + "?view=compact";
    paymentError.hidden = true;
    backgroundBtn.classList.add("pos-screen-hidden");
    dismissBtn.classList.add("pos-screen-hidden");
    showScreen(paymentScreen);
    startForegroundPoll(orderId, note);
  }

  function resetToKeypad() {
    paymentFrame.removeAttribute("src");
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
    if (foreground) unwatch(foreground.orderId);
    foreground = null;
    backgroundBtn.classList.add("pos-screen-hidden");
    dismissBtn.classList.add("pos-screen-hidden");
  }

  function startForegroundPoll(orderId, note) {
    foreground = { orderId: orderId, note: note || "" };
    watch(orderId, function (result) {
      if (!foreground || foreground.orderId !== orderId) return;

      if (result.status === "pending") {
        backgroundBtn.classList.add("pos-screen-hidden");
        dismissBtn.classList.add("pos-screen-hidden");
        return;
      }

      var success = !result.error && (result.status === "paid" || (result.status === "overpaid" && !result.error));
      var canBackground = !result.is_terminal && result.confirmations_required > 0 && !result.error;
      backgroundBtn.classList.toggle("pos-screen-hidden", !canBackground);
      dismissBtn.classList.toggle("pos-screen-hidden", !(result.is_terminal && !!result.error));

      if (result.is_terminal && success) {
        setTimeout(function () {
          if (foreground && foreground.orderId === orderId) {
            stopForegroundPoll();
            resetToKeypad();
          }
        }, 3500);
      }
    });
  }

  dismissBtn.addEventListener("click", function () {
    stopForegroundPoll();
    resetToKeypad();
  });

  backgroundBtn.addEventListener("click", function () {
    if (!foreground) return;
    var orderId = foreground.orderId;
    var note = foreground.note;
    stopForegroundPoll();
    resetToKeypad();
    startBackgroundPoll(orderId, note);
  });

  function updateBackgroundSummary() {
    var entries = Object.keys(backgrounded);
    bgDisclosure.hidden = entries.length === 0;
    bgSummary.textContent = "Background orders (" + entries.length + ")";
    bgDisclosure.classList.toggle("has-error", entries.some(function (id) { return backgrounded[id].el.classList.contains("is-error"); }));
    if (!entries.length) bgDisclosure.open = false;
  }

  function startBackgroundPoll(orderId, note) {
    var el = document.createElement("button");
    el.type = "button";
    el.className = "bg-item";
    el.innerHTML =
      '<span class="bg-id"></span>' +
      '<span class="bg-bar"><span class="bg-bar-fill"></span></span>' +
      '<span class="bg-dismiss pos-screen-hidden" aria-label="Dismiss">&times;</span>';
    el.querySelector(".bg-id").textContent = shortId(orderId);
    var fill = el.querySelector(".bg-bar-fill");
    var dismiss = el.querySelector(".bg-dismiss");
    bgStack.appendChild(el);

    var entry = {
      el: el,
      note: note || "",
      markLost: function () {
        el.classList.add("is-error");
        dismiss.classList.remove("pos-screen-hidden");
      },
    };
    backgrounded[orderId] = entry;
    updateBackgroundSummary();

    el.addEventListener("click", function () {
      unwatch(orderId);
      delete backgrounded[orderId];
      el.remove();
      updateBackgroundSummary();
      if (foreground) stopForegroundPoll();
      openOrder(orderId, entry.note);
      bgDisclosure.open = false;
    });

    watch(orderId, function (result) {
      if (!backgrounded[orderId]) return;
      var percent = result.confirmations_required > 0
        ? Math.min(100, Math.round((result.confirmations / result.confirmations_required) * 100))
        : 100;
      fill.style.width = percent + "%";
      el.classList.toggle("is-error", !!result.error);
      updateBackgroundSummary();
      if (result.error) dismiss.classList.remove("pos-screen-hidden");

      if (result.is_terminal && !result.error) {
        el.classList.add("is-paid");
        el.querySelector(".bg-id").textContent = shortId(orderId) + " — paid";
        setTimeout(function () {
          if (backgrounded[orderId]) {
            delete backgrounded[orderId];
            el.remove();
            updateBackgroundSummary();
          }
        }, 4000);
      }
    });
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
            public_key: "pk_test".to_string(),
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
            html.contains(r#"<nav class="context-nav" aria-label="Breadcrumb"><a href="/dashboard/stores/conn-1" title="example.com">example.com</a></nav><span class="breadcrumb-sep" aria-hidden="true">›</span><span class="pos-title">POS</span>"#),
            "expected the shared labeled store breadcrumb in the POS top bar"
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

    #[test]
    fn pos_uses_shared_checkout_and_top_background_stack_without_nfc() {
        let html = page(&chrome(), &data()).into_string();
        assert!(html.contains("id=\"payment-frame\""));
        assert!(html.contains("?view=compact"));
        assert!(html.contains("id=\"background-disclosure\""));
        assert!(html.contains("id=\"background-btn\""));
        assert!(html.contains("POS requires JavaScript"));
        assert!(!html.contains("NDEFReader"));
        assert!(!html.contains("id=\"tick-overlay\""));
    }
}
