# Webpages

Put Monokulo's checkout on any web page with one script tag: two
functions, no build step, no dependencies.

## Quick start

Load `monokulo-client.js` from your Monokulo's address, make an order and
mount the checkout. The script adds one global, `window.Monokulo`. Ship when
the signed `order.paid` webhook arrives, never on the page's word.

```html Your checkout page
<script src="https://pay.example/static/monokulo-client.js"></script>
<div id="pay"></div>
<script type="module">
  const order = await Monokulo.createOrder({
    publicKey: "pk_…",
    amount: "12.50",
    currency: "EUR",
  });
  Monokulo.mount("#pay", order, { onPaid: () => showThanks() });
</script>
```

## Choosing a layout

The store's **Checkout** tab sets its layout: Detailed, Compact or Tiny, with its
colours and corners. To choose one for a single embed, pass `view`, which also
sizes the frame:

```js One embed
Monokulo.mount("#pay", order, { view: "tiny" });
```

Passing `view` always shows that layout. To write the page yourself, see
[Custom checkout pages](../custom-pages/).

## Order statuses

The status `onStatusChange` is called with. Updates stop at a final
status.

| Status | Meaning | Final |
| --- | --- | --- |
| `pending` | Waiting for its payment. | no |
| `unconfirmed` | A payment was seen in the pool, not yet in a block. | no |
| `confirming` | The payment is in a block, with too few confirmations yet. | no |
| `partial` | Less than the amount has arrived; the rest can still come. | no |
| `paid` | Paid in full, with enough confirmations. | yes |
| `overpaid` | Paid more than the amount, with enough confirmations. | yes |
| `expired` | It expired before it was paid. | yes |
| `cancelled` | The merchant cancelled it before anything was paid. Only the POS cancels, its own orders. | yes |

## Shops on Tor

On a page served from a `.onion` address, the library makes the order,
frames the checkout and follows it on Monokulo's own onion address, so the
customer never leaves Tor. `monokulo` and `checkoutUrl` are then onion
addresses.

It finds that address once, from the API's entry (`mk:onion-api`, see
[Tor](../api/#tor)). Load the script from the onion address and it needn't
look. A Monokulo with no onion address has nowhere else to send them, so
its web address is used.

```html From the onion address
<script src="http://….onion/static/monokulo-client.js"></script>
```

## Trusting a payment

The page's callbacks run in the customer's browser, so they can be faked.
See [When is an order really paid?](../paid/)

| Where | Trust it for |
| --- | --- |
| In the page | `onPaid`, `onStatusChange`: the customer's experience only. |
| On your server | The signed `order.paid` webhook. Ship on this. |
| No server | Confirm the order in the Monokulo dashboard before you send anything. |

## JS reference

Everything on `window.Monokulo`. Monokulo's address is read from the
script's own URL, less its `/static/monokulo-client.js`, so a Monokulo
served under a path keeps it. Pass `monokulo` where it can't be.

### createOrder

Makes an order for the store and resolves with it. It posts to [the
API](../api/)'s `create-order` form with the public key
(`Authorization: Bearer pk_…`).

**Parameters**, one object:

| Name | Type | Default | Description | Note |
| --- | --- | --- | --- | --- |
| `publicKey` | string | required | The store's `pk_…` key. | |
| `amount` | number or string | required | The price, in `currency`. Up to 12 decimals. | Pass a string to keep it exact; trailing zeros don't count (`"12.50000000"` is fine). |
| `currency` | string | required | `"XMR"`, or a currency the store's exchange rate provider prices (`"EUR"`). | |
| `merchantOrderId` | string | optional | Your own order or cart id, kept with the order. | Shown on its page in the dashboard. |
| `description` | string | optional | What the order is for, shown to the payer under your store's name. Plain text, up to 140 characters. | Public: anyone with the payment link can read it. |
| `showMerchantOrderId` | boolean | `true` | Shows `merchantOrderId` on the checkout. | The dashboard and webhooks always have it. |
| `monokulo` | string | optional | Monokulo's address (`https://pay.example.com`, or `https://example.com/monokulo` under a path). | Only if it can't be read from the script tag. |

- Rejects with an `Error` carrying the problem's `detail`.
- If Monokulo asks a busy visitor for a proof of work, it solves it and
  tries again by itself.

```text Signature
Monokulo.createOrder(params) → Promise<Order>
```

```js Example
const order = await Monokulo.createOrder({
  publicKey: "pk_…",
  amount: "12.50",
  currency: "EUR",
  merchantOrderId: "cart-1042",
});
```

```js Resolves to (Order)
{
  orderId, address, xmrAmountPiconero,
  amount, currency, merchantOrderId, description,
  expiresAt, checkoutUrl, orderUrl,
  monokulo,  // the address the order was made at
  publicKey,
}
```

### mount

Replaces `target`'s content with the checkout's iframe and follows the
order's status: server-sent events, polling as a fallback.

**Parameters**:

| Name | Type | Default | Description | Note |
| --- | --- | --- | --- | --- |
| `target` | string or Element | required | A selector or an element. | |
| `orderOrId` | Order or string | required | The object from `createOrder`, or an order id. | With an id (say after a reload), pass `monokulo` and `publicKey` in the options. |

**Options**, the third argument:

| Name | Type | Default | Description | Note |
| --- | --- | --- | --- | --- |
| `onStatusChange` | `(status, data) => void` | optional | Called with each new status. | |
| `onPaid` | `(data) => void` | optional | Called when the status becomes `paid` or `overpaid`. | A hint for the page: ship on the webhook. |
| `onExpired` | `(data) => void` | optional | Called when the status becomes `expired`. | |
| `theme` | `"light"` or `"dark"` | the device's | Pins the checkout's theme. | |
| `refund` | `false` | optional | Hides the refund-address form. | |
| `width`, `height` | CSS length | the layout's: 420px × 880px, or 280px × 380px for `compact` and 280px × 152px for `tiny` | The iframe's size. | |
| `monokulo`, `publicKey` | string | optional | Monokulo's address and the store's key, for a bare order id. | `publicKey` lets it poll where server-sent events can't be used. |

- Mounting the same element again replaces the old checkout.
- `destroy()` removes it and stops listening.

```text Signature
Monokulo.mount(target, orderOrId, options?) → { iframe, destroy() }
```

```js Example
const checkout = Monokulo.mount("#pay", order, {
  theme: "dark",
  onStatusChange: (status) => setBadge(status),
  onPaid: () => showThanks(),
});

// later
checkout.destroy();
```

## Without the script: a plain iframe

The script is optional. The checkout is a page for people at
`/api/v1/checkout/{order_id}`, and an `<iframe>` on it is all a page needs. It
takes no key and works without JavaScript; the script adds `onPaid` and
`onExpired` callbacks, and a frame can [follow the status](#order-statuses)
itself with a few lines of your own. An order links to its page as `mk:checkout` (and
`mk:onion-checkout` while Monokulo has an onion address): build the URL from
that link, not by hand.

```html A plain frame
<iframe
  title="Monero payment"
  src="https://pay.example/api/v1/checkout/order_…?theme=light"
  sandbox="allow-same-origin allow-scripts allow-forms allow-popups"
  allow="camera"
  referrerpolicy="strict-origin"
  style="width:100%;border:0;height:880px"></iframe>
```

- `sandbox`: the page's script, its refund form and its links out need exactly these.
- `allow="camera"`: the refund form scans a QR code of an address.
- `referrerpolicy="strict-origin"`: says which site is framing the page, even from a page that sends no `Referer`. A store with verified domains only lets those sites frame it (`Content-Security-Policy: frame-ancestors`, set on its Domains tab); anywhere else the frame is refused.
- Height: start from the layout's size: `tiny` 152px, `compact` 380px, `detailed` 880px. The page is cross-origin, so your page can't measure it, but it tells you: inside a frame it posts `{ type: "monokulo:resize", height }` to its parent whenever its height changes. Apply it to the frame it came from, ignoring any other source:

```js Follow the checkout's height
window.addEventListener("message", (event) => {
  const frame = document.querySelector("#pay iframe");
  const data = event.data;
  if (!frame || event.source !== frame.contentWindow) return;
  if (data?.type === "monokulo:resize" && Number.isFinite(data.height) && data.height > 0) {
    frame.style.height = Math.min(Math.ceil(data.height), 5000) + "px";
  }
});
```

- Status: inside a frame the page also posts `{ type: "monokulo:status", status }` to its parent when it loads and each time the [status](#order-statuses) changes, so your page can react, say by offering a new order once one has expired. Those two messages are all it posts, and it listens to none. Like the script's callbacks, this only tells you what the customer is seeing: see [Trusting a payment](#trusting-a-payment).

```js Follow the checkout's status
window.addEventListener("message", (event) => {
  const frame = document.querySelector("#pay iframe");
  const data = event.data;
  if (!frame || event.source !== frame.contentWindow) return;
  if (data?.type === "monokulo:status" && data.status === "expired") {
    showNewOrderButton();
  }
});
```

- A site with a Content-Security-Policy needs `frame-src` for the Monokulo address, and nothing else.

**Query options**, all optional:

| Name | Values | Description |
| --- | --- | --- |
| `view` | `detailed`, `compact`, `tiny` | The layout. Left out, the store's own choice (its Checkout tab). |
| `theme` | `light`, `dark` | Pins the theme. Left out, the device's. |
| `refund` | `false` | Hides the refund-address form. |
| `refresh` | `false` | Without JavaScript, stops the page reloading itself every minute. |

**Routes**: all under `/api/v1/checkout/{order_id}`. The order id is
unguessable and whoever has it may pay, so there is no key; they are pages for
people, not the JSON API.

| Route | What it is |
| --- | --- |
| `GET /api/v1/checkout/{order_id}` | The page: the amount, the address and its QR code, live progress. |
| `GET /api/v1/checkout/{order_id}/events` | Its live updates, as server-sent events. |
| `POST /api/v1/checkout/{order_id}/refund-address` | Where its refund form posts. |
| `GET /api/v1/checkout/{order_id}/share` | The link a merchant sends: the page in a frame inside Monokulo's own. |

Without the script nothing tells the page it was paid. Ship on the signed
`order.paid` webhook, or read the order (`GET /api/v1/orders/{order_id}`) with
the store's secret key; a shop can poll its own server for that and reload the
page once it answers.
