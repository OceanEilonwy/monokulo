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
| `width`, `height` | CSS length | 420px × 900px | The iframe's size. | |
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
