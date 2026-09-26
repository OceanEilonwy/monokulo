/*
 * monokulo-client.js - the thin embed library for a static merchant site.
 * Moved here from the engine per `docs/fx_refactor.md` decision 3 - order
 * creation and the checkout page are both monokulo responsibilities
 * now, so this talks to monokulo's own `/pay/{pk}/orders` endpoint,
 * not the engine directly (the engine no longer has any concept of fiat at
 * all, and no checkout page of its own to iframe - see that document's
 * Phase 3/4). Deliberately dependency-free, no build step, ES5-ish syntax:
 * it has to run unmodified on an arbitrary third-party page (a GitHub Pages
 * site, most plausibly) that whoever maintains this project does not
 * control the toolchain of.
 *
 * All payment logic and status rendering lives server-side in the
 * `/pay/{pk}/orders/{order_id}` page this library iframes; this file only
 * creates orders, mounts that iframe, and follows monokulo's own
 * `/events` stream (Server-Sent Events, falling back to polling `/status`
 * where EventSource is unavailable or refused) to drive `onStatusChange`/
 * `onPaid`/`onExpired`. The iframed page keeps itself up to date and has a
 * no-JavaScript fallback. It does not post messages back to this library;
 * this file's own subscription (running here, in the merchant's page, not
 * inside the frame) is what makes the callbacks below fire.
 *
 * Abuse protection: a visitor making a lot of requests may be asked to
 * prove it's a real browser. monokulo then answers `createOrder` or the
 * status poll with `429` and a JSON body
 * `{ "error": ..., "challenge": { "challenge": "<token>", "difficulty": <bits>, ... } }`
 * (also in the `Monokulo-Challenge` response header). This library solves it
 * by itself - finding a nonce such that SHA-256(challenge + nonce) starts with
 * `difficulty` zero bits, with Web Crypto - and retries once with the header
 * `Monokulo-Proof: <challenge>.<nonce>`. A `429` without a challenge (with
 * `Retry-After`) means "past the hard limit": the call fails and the caller
 * may retry later. Merchants don't need to change anything.
 */
(function (global) {
  "use strict";

  // Orders remember the endpoint/publicKey they were created against, so
  // `mount(selector, orderId, options)` can be called with just the id
  // without the caller re-stating both. A page that reloads and wants to
  // remount an orderId from a previous visit (no createOrder call this
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

  function leadingZeroBits(bytes) {
    var bits = 0;
    for (var i = 0; i < bytes.length; i++) {
      if (bytes[i] === 0) { bits += 8; continue; }
      var b = bytes[i];
      while ((b & 0x80) === 0) { bits++; b <<= 1; }
      return bits;
    }
    return bits;
  }

  // Resolves to "<challenge>.<nonce>" (see the header comment).
  function solveChallenge(challenge, difficulty) {
    if (!global.crypto || !global.crypto.subtle || typeof TextEncoder !== "function") {
      return Promise.reject(new Error("this browser can't solve monokulo's challenge (no Web Crypto)"));
    }
    var encoder = new TextEncoder();
    var nonce = 0;
    function batch() {
      var tries = [];
      for (var i = 0; i < 256; i++) tries.push(String(nonce + i));
      nonce += 256;
      return Promise.all(tries.map(function (n) {
        return global.crypto.subtle.digest("SHA-256", encoder.encode(challenge + n)).then(function (hash) {
          return leadingZeroBits(new Uint8Array(hash)) >= difficulty ? n : null;
        });
      })).then(function (results) {
        for (var j = 0; j < results.length; j++) if (results[j] !== null) return challenge + "." + results[j];
        return batch();
      });
    }
    return batch();
  }

  // `fetch`, retried once with a solved challenge if monokulo asks for one.
  function fetchSolvingChallenges(url, init) {
    init = init || {};
    return fetch(url, init).then(function (response) {
      if (response.status !== 429) return response;
      return response.clone().json().catch(function () { return null; }).then(function (body) {
        var issued = body && body.challenge;
        if (!issued || !issued.challenge) return response;
        return solveChallenge(issued.challenge, issued.difficulty).then(function (proof) {
          var headers = {};
          var original = init.headers || {};
          for (var key in original) if (Object.prototype.hasOwnProperty.call(original, key)) headers[key] = original[key];
          headers["Monokulo-Proof"] = proof;
          var retry = {};
          for (var k in init) if (Object.prototype.hasOwnProperty.call(init, k)) retry[k] = init[k];
          retry.headers = headers;
          return fetch(url, retry);
        });
      });
    });
  }

  function createOrder(params) {
    params = params || {};
    var endpoint = (params.endpoint || scriptOrigin() || "").replace(/\/+$/, "");
    var publicKey = params.publicKey;
    if (!endpoint) {
      return Promise.reject(new Error("Monokulo.createOrder: endpoint is required (could not infer it from the script tag)"));
    }
    if (!publicKey) {
      return Promise.reject(new Error("Monokulo.createOrder: publicKey is required"));
    }
    if (params.amount === undefined || params.amount === null || !params.currency) {
      return Promise.reject(new Error("Monokulo.createOrder: amount and currency are required (currency: \"XMR\", or any fiat currency this store's exchange rate provider supports)"));
    }

    return fetchSolvingChallenges(endpoint + "/pay/" + encodeURIComponent(publicKey) + "/orders", {
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
          orderId: data.order_id,
          address: data.address,
          xmrAmountPiconero: data.xmr_amount_piconero,
          amount: data.amount,
          currency: data.currency,
          merchantOrderId: data.merchant_order_id,
          expiresAt: data.expires_at,
          endpoint: endpoint,
          publicKey: publicKey,
        };
        knownOrders[order.orderId] = { endpoint: endpoint, publicKey: publicKey };
        return order;
      });
  }

  function resolveTarget(selector) {
    if (typeof selector === "string") return document.querySelector(selector);
    return selector || null;
  }

  function mount(selector, orderIdOrOrder, options) {
    options = options || {};
    var el = resolveTarget(selector);
    if (!el) {
      throw new Error("Monokulo.mount: target element not found: " + selector);
    }

    var orderId, endpoint, publicKey;
    if (orderIdOrOrder && typeof orderIdOrOrder === "object") {
      orderId = orderIdOrOrder.orderId;
      endpoint = orderIdOrOrder.endpoint;
      publicKey = orderIdOrOrder.publicKey;
    } else {
      orderId = orderIdOrOrder;
    }
    endpoint = options.endpoint || endpoint || (knownOrders[orderId] || {}).endpoint || scriptOrigin();
    publicKey = options.publicKey || publicKey || (knownOrders[orderId] || {}).publicKey;

    if (!orderId || !endpoint || !publicKey) {
      throw new Error(
        "Monokulo.mount: could not resolve orderId/endpoint/publicKey - pass an order object " +
          "from createOrder(), or {endpoint, publicKey} in options for an orderId from a previous visit"
      );
    }

    // Tear down whatever this element was showing before, so a re-mount replaces
    // the previous mount rather than accumulating alongside it.
    var previous = activeMounts.get(el);
    if (previous) {
      previous.destroy();
    }

    var checkoutUrl = endpoint + "/pay/" + encodeURIComponent(publicKey) + "/orders/" + encodeURIComponent(orderId);
    var iframeSrc = checkoutUrl + (options.refund === false ? "?refund=false" : "");
    var statusUrl = checkoutUrl + "/status";
    try {
      new URL(iframeSrc, global.location.href);
    } catch (e) {
      throw new Error("Monokulo.mount: endpoint is not a valid URL: " + endpoint);
    }

    var iframe = document.createElement("iframe");
    iframe.src = iframeSrc;
    iframe.title = "Monero Payment";
    iframe.style.border = "none";
    iframe.style.width = options.width || "420px";
    // 900px, not the previous 640px: the checkout page's amount, QR, address
    // card, progress bar, and refund-address form alone already run past
    // 900px at this default 420px width - 640px was silently cutting real
    // content off into the frame's own inner scrollbar for every order, not
    // just wide/edge-case ones. True auto-sizing (matching the framed page's
    // real content height, the way checkout_share.html.hbs's own same-origin
    // script does) isn't available here - this iframe's origin is the
    // caller's own site, not monokulo's, so cross-origin restrictions block
    // reading its content height directly. A caller who wants a
    // specifically sized/no-scroll embed should still pass an explicit
    // `height` in `options`.
    iframe.style.height = options.height || "900px";
    // The checkout script and refund form need these sandbox capabilities;
    // this mount() still follows status independently for merchant callbacks.
    iframe.setAttribute("sandbox", "allow-same-origin allow-scripts allow-forms allow-popups");
    iframe.setAttribute("allow", "camera");

    el.innerHTML = "";
    el.appendChild(iframe);

    var TERMINAL_STATUSES = { paid: true, overpaid: true, expired: true };
    var destroyed = false;
    var pollTimer = null;
    var updates = null;
    var lastStatus = null;

    function stopUpdates() {
      if (pollTimer) { clearTimeout(pollTimer); pollTimer = null; }
      if (updates) { updates.close(); updates = null; }
    }

    function onStatus(data) {
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
      }
      if (TERMINAL_STATUSES[data.status]) stopUpdates();
    }

    function poll() {
      if (destroyed) return;
      fetchSolvingChallenges(statusUrl)
        .then(function (r) {
          // A non-2xx response (a transient rate limit or 5xx) still has a
          // JSON body, so `.json()` alone would "succeed" with `data.status`
          // simply `undefined`. Checking `r.ok` first routes any non-2xx
          // into the same catch/retry path as a network failure.
          if (!r.ok) { throw new Error("status check failed: " + r.status); }
          return r.json();
        })
        .then(function (data) {
          onStatus(data);
          if (!destroyed && !TERMINAL_STATUSES[data.status]) pollTimer = setTimeout(poll, 3000);
        })
        .catch(function () {
          if (!destroyed) pollTimer = setTimeout(poll, 5000);
        });
    }

    if (typeof EventSource === "function") {
      updates = new EventSource(checkoutUrl + "/events");
      updates.addEventListener("status", function (event) {
        var data;
        try { data = JSON.parse(event.data); } catch (e) { return; }
        onStatus(data);
      });
      // EventSource reconnects by itself after a dropped connection; only a
      // request refused outright (CLOSED) falls back to polling.
      updates.addEventListener("error", function () {
        if (updates && updates.readyState === EventSource.CLOSED) {
          updates = null;
          if (!destroyed) pollTimer = setTimeout(poll, 3000);
        }
      });
    } else {
      pollTimer = setTimeout(poll, 3000);
    }

    var handle = {
      iframe: iframe,
      destroy: function () {
        if (destroyed) return;
        destroyed = true;
        stopUpdates();
        if (activeMounts.get(el) === handle) activeMounts["delete"](el);
        if (iframe.parentNode) iframe.parentNode.removeChild(iframe);
      },
    };
    activeMounts.set(el, handle);
    return handle;
  }

  global.Monokulo = { createOrder: createOrder, mount: mount };
})(window);
