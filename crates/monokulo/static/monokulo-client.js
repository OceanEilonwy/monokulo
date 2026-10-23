/*
 * monokulo-client.js - the thin embed library for a static merchant site.
 * It talks to monokulo's own API (`POST /api/v1/orders`, with the store's
 * public key as `Authorization: Bearer pk_...`), never the engine, and
 * frames the order's checkout page (`/checkout/{order_id}`). Deliberately
 * dependency-free, no build step, ES5-ish syntax: it has to run unmodified
 * on an arbitrary third-party page (a GitHub Pages site, most plausibly)
 * that whoever maintains this project does not control the toolchain of.
 *
 * All payment logic and status rendering lives server-side in the
 * checkout page this library iframes; this file only creates orders,
 * mounts that iframe, and follows the checkout's own `/events` stream
 * (Server-Sent Events, falling back to polling the order,
 * `GET /api/v1/orders/{order_id}` with the public key, where EventSource is
 * unavailable or refused) to drive `onStatusChange`/`onPaid`/`onExpired`.
 * The iframed page keeps itself up to date and has a no-JavaScript
 * fallback. It does not post messages back to this library; this file's
 * own subscription (running here, in the merchant's page, not inside the
 * frame) is what makes the callbacks below fire.
 *
 * Errors are `application/problem+json`: `detail` says what went wrong.
 *
 * Abuse protection: a visitor making a lot of requests may be asked to
 * prove it's a real browser. monokulo then answers `createOrder` or the
 * order poll with `429` and a problem body with a `challenge` member,
 * `{ "type": ".../problems/challenge-required", "detail": ..., "challenge": { "challenge": "<token>", "difficulty": <bits>, ... } }`
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

    return fetchSolvingChallenges(endpoint + "/api/v1/orders", {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: "Bearer " + publicKey },
      body: JSON.stringify({
        amount: String(params.amount),
        currency: params.currency,
        // Optional - a storefront's own order/cart id, undefined/omitted
        // when the caller doesn't have one (the server's own `#[serde(default)]`
        // treats a missing key the same as an explicit `null`).
        merchant_order_id: params.merchantOrderId || undefined,
        // Optional - what the order is for, shown to the payer under the
        // store's name; the store's payment description when left out.
        description: params.description || undefined,
        // Optional - false keeps merchantOrderId off the checkout; the
        // server shows it when this is left out.
        show_merchant_order_id: typeof params.showMerchantOrderId === "boolean" ? params.showMerchantOrderId : undefined,
      }),
    })
      .then(function (response) {
        if (!response.ok) {
          return response
            .json()
            .catch(function () {
              return { detail: "HTTP " + response.status };
            })
            .then(function (body) {
              throw new Error(body.detail || "HTTP " + response.status);
            });
        }
        return response.json();
      })
      .then(function (data) {
        // The order's links are paths on monokulo: resolved against the
        // endpoint the order was made at.
        var links = data._links || {};
        var link = function (rel) {
          return links[rel] && links[rel].href ? new URL(links[rel].href, endpoint + "/").href : null;
        };
        var order = {
          orderId: data.order_id,
          address: data.address,
          xmrAmountPiconero: data.xmr_amount_piconero,
          amount: data.amount,
          currency: data.currency,
          merchantOrderId: data.merchant_order_id,
          description: data.description,
          expiresAt: data.expires_at,
          endpoint: endpoint,
          publicKey: publicKey,
          // The payer's page, to link to or frame.
          checkoutUrl: link("mk:checkout"),
          // The order itself, to read its status.
          orderUrl: link("self"),
        };
        knownOrders[order.orderId] = {
          endpoint: endpoint,
          publicKey: publicKey,
          checkoutUrl: order.checkoutUrl,
          orderUrl: order.orderUrl,
        };
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

    var orderId, endpoint, publicKey, known;
    if (orderIdOrOrder && typeof orderIdOrOrder === "object") {
      orderId = orderIdOrOrder.orderId;
      known = orderIdOrOrder;
    } else {
      orderId = orderIdOrOrder;
      known = knownOrders[orderId] || {};
    }
    endpoint = (options.endpoint || known.endpoint || scriptOrigin() || "").replace(/\/+$/, "");
    // Only needed to poll the order where Server-Sent Events can't be used.
    publicKey = options.publicKey || known.publicKey;

    if (!orderId || !endpoint) {
      throw new Error(
        "Monokulo.mount: could not resolve orderId/endpoint - pass an order object " +
          "from createOrder(), or {endpoint} in options for an orderId from a previous visit"
      );
    }

    // Tear down whatever this element was showing before, so a re-mount replaces
    // the previous mount rather than accumulating alongside it.
    var previous = activeMounts.get(el);
    if (previous) {
      previous.destroy();
    }

    // The order's own links when this page made it; for an order from an
    // earlier visit, the same paths built from its id.
    var checkoutUrl = known.checkoutUrl || endpoint + "/checkout/" + encodeURIComponent(orderId);
    var orderUrl = known.orderUrl || endpoint + "/api/v1/orders/" + encodeURIComponent(orderId);
    // `theme: "light" | "dark"` pins the checkout's theme to match the
    // merchant's site; left out, it follows the customer's device.
    var query = [];
    if (options.refund === false) query.push("refund=false");
    if (options.theme === "light" || options.theme === "dark") query.push("theme=" + options.theme);
    var iframeSrc = checkoutUrl + (query.length ? "?" + query.join("&") : "");
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
      fetchSolvingChallenges(orderUrl, { headers: { Authorization: "Bearer " + publicKey } })
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

    // Polling the order needs the store's public key; without it, a refused
    // stream is opened again a little later instead.
    function fallBack(delay) {
      if (destroyed) return;
      pollTimer = setTimeout(publicKey ? poll : listen, delay);
    }

    function listen() {
      if (destroyed) return;
      updates = new EventSource(checkoutUrl + "/events");
      updates.addEventListener("status", function (event) {
        var data;
        try { data = JSON.parse(event.data); } catch (e) { return; }
        onStatus(data);
      });
      // EventSource reconnects by itself after a dropped connection; only a
      // request refused outright (CLOSED) falls back.
      updates.addEventListener("error", function () {
        if (updates && updates.readyState === EventSource.CLOSED) {
          updates = null;
          fallBack(publicKey ? 3000 : 10000);
        }
      });
    }

    if (typeof EventSource === "function") {
      listen();
    } else if (publicKey) {
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
