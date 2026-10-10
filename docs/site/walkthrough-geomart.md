# Walkthrough: take Monero at Geomart

Geomart is a pretend shop that sells shapes: triangles, squares, circles,
hexagons. It's one HTML file with a catalogue, a cart and a checkout page,
and a fake API in the same file standing in for a shop's server. It has no
way to pay. In six steps it will.

**Source:** [`examples/geomart/index.html`](https://github.com/OceanEilonwy/monokulo/blob/main/examples/geomart/index.html)
in the repo, and the finished shop beside it,
[`examples/geomart/done.html`](https://github.com/OceanEilonwy/monokulo/blob/main/examples/geomart/done.html).
**Try it:** [before](../geomart/) and [after](../geomart/done.html), or open the
files straight from disk.

### Step 1 · Make a store for Geomart

In Monokulo: *Add a store*. Name it **Geomart**, with the website
`geomart.example`, and take payments into a **stagenet** wallet for
now (under *More options* when you add the wallet). Copy the public key from
the store's page.

### Step 2 · Add the library

```diff
# examples/geomart/index.html
   </main>
+  <script src="https://pay.example/static/monokulo-client.js"></script>
   <script>
```

Load it with a plain `<script src>`, not a module or a bundle: it finds
Monokulo's address from its own URL.

### Step 3 · Create an order at checkout

```diff
# examples/geomart/index.html · checkout()
   async function checkout() {
     const receipt = await fakeApi.placeOrder(cart);   // Geomart's own order id
+    const order = await Monokulo.createOrder({
+      publicKey: "pk_3f9a…c21e",
+      amount: receipt.total,          // "13.50"
+      currency: "EUR",
+      merchantOrderId: receipt.id,    // "gm-1042", for step 5
+    });
```

`merchantOrderId` is Geomart's own order id. The webhook in step 5 carries
it back as `merchant_order_id`, so the shop finds its order without keeping
Monokulo's.

### Step 4 · Show the checkout

```diff
# examples/geomart/index.html · checkout()
     showPage("checkout");
+    Monokulo.mount("#pay", order, {
+      theme: "light",
+      onStatusChange: (status) => setBanner(status),
+      onPaid: () => showPage("thanks"),          // a nicety only: see step 5
+      onExpired: () => showPage("cart"),
+    });
```

### Step 5 · Mark it paid from the webhook

A browser callback can be faked, so the shop's server decides. In
Monokulo, the store's settings, *Webhooks*: add
`https://geomart.example/hooks/monokulo` and keep the signing secret.
Geomart's fake API shows the handler a real server needs:

```diff
# examples/geomart/index.html · fakeApi (stands in for your server)
+  // POST /hooks/monokulo   X-Monokulo-Signature: t=…,v1=…
+  // body: {"api_version":2,"event_id":"evt_…","event":"order.paid",
+  //        "order_id":"…","status":"paid","merchant_order_id":"gm-1042",
+  //        "amount":"13.50","currency":"EUR",…}
+  async onMonokuloWebhook(headers, body) {
+    if (!(await signatureOk(headers["x-monokulo-signature"], body, WEBHOOK_SECRET))) return 400;
+    const event = JSON.parse(body);
+    if (seen.has(event.event_id)) return 200;          // deliveries can repeat
+    seen.add(event.event_id);
+    if (event.event === "order.paid") this.markPaid(event.merchant_order_id);
+    return 200;
+  },
```

The signature is HMAC-SHA256 of `"<t>.<body>"` with the signing secret;
accept it only within five minutes of `t`. The handler finds Geomart's
order by `merchant_order_id`, the id it passed in step 3. The body also
carries the price as Geomart set it (`amount`, `currency`), what the
customer paid in XMR, the exchange rate and the store: every field is on
[Webhooks](../webhooks/). If a delivery fails, Monokulo tries again for
about two hours; the store's settings, *Webhooks*, show each one.

> **No server? (Geomart is one HTML file.)** A webhook needs somewhere to
> arrive, and `onPaid` runs in the customer's browser, so it can be faked. A
> static shop shows "Thanks" from `onPaid` and confirms each order in the
> Monokulo dashboard (or wherever it already handles fulfilment) before
> sending anything. A shop with a backend points the webhook at it, as
> above. See [When is an order really paid?](../paid/)

### Step 6 · Verify, test, go live

Verify `geomart.example` (a DNS TXT record, in the store's settings), buy a
triangle with stagenet coins, watch the webhook arrive, then change the
store's wallet to a mainnet one.
