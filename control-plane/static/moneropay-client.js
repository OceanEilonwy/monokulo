/*
 * moneropay-client.js - the thin embed library for a static merchant site.
 * Moved here from the engine per `docs/fx_refactor.md` decision 3 - order
 * creation and the checkout page are both control-plane responsibilities
 * now, so this talks to control-plane's own `/pay/{pk}/orders` endpoint,
 * not the engine directly (the engine no longer has any concept of fiat at
 * all, and no checkout page of its own to iframe - see that document's
 * Phase 3/4). Deliberately dependency-free, no build step, ES5-ish syntax:
 * it has to run unmodified on an arbitrary third-party page (a GitHub Pages
 * site, most plausibly) that whoever maintains this project does not
 * control the toolchain of.
 *
 * All payment logic and status rendering lives server-side in the
 * `/pay/{pk}/orders/{payment_id}` page this library iframes; this file only
 * creates orders, mounts that iframe, and polls control-plane's own
 * `/status` endpoint directly to drive `onStatusChange`/`onPaid`/
 * `onExpired`. The iframed page itself is deliberately plain, script-free
 * HTML (a `<meta http-equiv="refresh">` re-fetches it on its own) - a real
 * customer paying real money must be able to trust and use it with
 * JavaScript disabled, so it never posts a message back to this library;
 * this file's own polling (running here, in the merchant's page, not
 * inside the frame) is what makes the callbacks below fire.
 */
(function (global) {
  "use strict";

  // Orders remember the endpoint/publicKey they were created against, so
  // `mount(selector, paymentId, options)` can be called with just the id
  // without the caller re-stating both. A page that reloads and wants to
  // remount a paymentId from a previous visit (no createOrder call this
  // pageload) can still pass `endpoint`/`publicKey` explicitly via `options`.
  var knownOrders = {};

  // Every element that currently has a live mount, so a re-mount can tear the
  // previous one down first. `mount()` only removes its `message` listener from
  // the returned `destroy()`, which nothing calls automatically - so remounting
  // the same container (a SPA re-render, a "try again" button, a customer
  // switching between two order widgets) would otherwise stack a second
  // listener on top of the first and fire every callback once per past mount.
  // A Map keyed by the element avoids stashing bookkeeping on the merchant's
  // own DOM node; the WeakMap variant is used where available so a detached
  // container doesn't pin its entry forever.
  var activeMounts = typeof WeakMap === "function" ? new WeakMap() : new Map();

  // Captured here, while this file is still the executing script, rather than
  // read on demand inside createOrder/mount. `document.currentScript` names
  // whatever script is running *right now*, so by the time a merchant calls
  // into this library it is their code, not ours: null when called from an
  // event handler or a module, and their own page's origin when called from
  // another classic <script src>. The endpoint inference then either fails
  // outright or - worse - silently points the payment API at the merchant's
  // own static host.
  var SCRIPT_ORIGIN = (function () {
    var script = document.currentScript;
    if (!script || !script.src) return null;
    try {
      return new URL(script.src).origin;
    } catch (e) {
      return null;
    }
  })();

  function scriptOrigin() {
    return SCRIPT_ORIGIN;
  }

  function createOrder(params) {
    params = params || {};
    var endpoint = (params.endpoint || scriptOrigin() || "").replace(/\/+$/, "");
    var publicKey = params.publicKey;
    if (!endpoint) {
      return Promise.reject(new Error("MoneroPay.createOrder: endpoint is required (could not infer it from the script tag)"));
    }
    if (!publicKey) {
      return Promise.reject(new Error("MoneroPay.createOrder: publicKey is required"));
    }
    if (params.amount === undefined || params.amount === null || !params.currency) {
      return Promise.reject(new Error("MoneroPay.createOrder: amount and currency are required (currency: \"XMR\", or any fiat currency this store's exchange rate provider supports)"));
    }

    return fetch(endpoint + "/pay/" + encodeURIComponent(publicKey) + "/orders", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        amount: String(params.amount),
        currency: params.currency,
        // Optional - a storefront's own order/cart id, undefined/omitted
        // when the caller doesn't have one (the server's own `#[serde(default)]`
        // treats a missing key the same as an explicit `null`).
        merchant_order_id: params.merchantOrderId || undefined,
      }),
    })
      .then(function (response) {
        if (!response.ok) {
          return response
            .json()
            .catch(function () {
              return { error: "HTTP " + response.status };
            })
            .then(function (body) {
              throw new Error(body.error || "HTTP " + response.status);
            });
        }
        return response.json();
      })
      .then(function (data) {
        var order = {
          paymentId: data.payment_id,
          address: data.address,
          xmrAmountPiconero: data.xmr_amount_piconero,
          amount: data.amount,
          currency: data.currency,
          merchantOrderId: data.merchant_order_id,
          expiresAt: data.expires_at,
          endpoint: endpoint,
          publicKey: publicKey,
        };
        knownOrders[order.paymentId] = { endpoint: endpoint, publicKey: publicKey };
        return order;
      });
  }

  function resolveTarget(selector) {
    if (typeof selector === "string") return document.querySelector(selector);
    return selector || null;
  }

  function mount(selector, paymentIdOrOrder, options) {
    options = options || {};
    var el = resolveTarget(selector);
    if (!el) {
      throw new Error("MoneroPay.mount: target element not found: " + selector);
    }

    var paymentId, endpoint, publicKey;
    if (paymentIdOrOrder && typeof paymentIdOrOrder === "object") {
      paymentId = paymentIdOrOrder.paymentId;
      endpoint = paymentIdOrOrder.endpoint;
      publicKey = paymentIdOrOrder.publicKey;
    } else {
      paymentId = paymentIdOrOrder;
    }
    endpoint = options.endpoint || endpoint || (knownOrders[paymentId] || {}).endpoint || scriptOrigin();
    publicKey = options.publicKey || publicKey || (knownOrders[paymentId] || {}).publicKey;

    if (!paymentId || !endpoint || !publicKey) {
      throw new Error(
        "MoneroPay.mount: could not resolve paymentId/endpoint/publicKey - pass an order object " +
          "from createOrder(), or {endpoint, publicKey} in options for a paymentId from a previous visit"
      );
    }

    // Tear down whatever this element was showing before, so a re-mount replaces
    // the previous mount rather than accumulating alongside it.
    var previous = activeMounts.get(el);
    if (previous) {
      previous.destroy();
    }

    var iframeSrc = endpoint + "/pay/" + encodeURIComponent(publicKey) + "/orders/" + encodeURIComponent(paymentId);
    var statusUrl = iframeSrc + "/status";
    try {
      new URL(iframeSrc, global.location.href);
    } catch (e) {
      throw new Error("MoneroPay.mount: endpoint is not a valid URL: " + endpoint);
    }

    var iframe = document.createElement("iframe");
    iframe.src = iframeSrc;
    iframe.title = "Monero Payment";
    iframe.style.border = "none";
    iframe.style.width = options.width || "420px";
    iframe.style.height = options.height || "640px";
    // No "allow-scripts" - the page this iframes carries none by design
    // (see this file's own doc comment above); this mount()'s own polling
    // below, running in the merchant's page rather than inside the frame,
    // is what drives the callbacks now.
    iframe.setAttribute("sandbox", "allow-same-origin allow-popups");

    el.innerHTML = "";
    el.appendChild(iframe);

    var TERMINAL_STATUSES = { paid: true, overpaid: true, expired: true };
    var destroyed = false;
    var pollTimer = null;
    var lastStatus = null;

    function poll() {
      if (destroyed) return;
      fetch(statusUrl)
        .then(function (r) {
          // A non-2xx response (a transient rate limit or 5xx) still has a
          // JSON body, so `.json()` alone would "succeed" with `data.status`
          // simply `undefined`. Checking `r.ok` first routes any non-2xx
          // into the same catch/retry path as a network failure.
          if (!r.ok) { throw new Error("status check failed: " + r.status); }
          return r.json();
        })
        .then(function (data) {
          if (destroyed) return;
          if (data.status !== lastStatus) {
            lastStatus = data.status;
            if (typeof options.onStatusChange === "function") options.onStatusChange(data.status, data);
            if ((data.status === "paid" || data.status === "overpaid") && typeof options.onPaid === "function") {
              options.onPaid(data);
            }
            if (data.status === "expired" && typeof options.onExpired === "function") {
              options.onExpired(data);
            }
            // The mounted iframe only refreshes itself on its own
            // meta-refresh timer - reload it here too so what the customer
            // *sees* catches up with the status change this poll just
            // detected, rather than waiting for the frame's own next tick.
            iframe.src = iframeSrc;
          }
          if (!TERMINAL_STATUSES[data.status]) {
            pollTimer = setTimeout(poll, 3000);
          }
        })
        .catch(function () {
          if (!destroyed) pollTimer = setTimeout(poll, 5000);
        });
    }
    pollTimer = setTimeout(poll, 3000);

    var handle = {
      iframe: iframe,
      destroy: function () {
        if (destroyed) return;
        destroyed = true;
        if (pollTimer) { clearTimeout(pollTimer); pollTimer = null; }
        if (activeMounts.get(el) === handle) activeMounts["delete"](el);
        if (iframe.parentNode) iframe.parentNode.removeChild(iframe);
      },
    };
    activeMounts.set(el, handle);
    return handle;
  }

  global.MoneroPay = { createOrder: createOrder, mount: mount };
})(window);
