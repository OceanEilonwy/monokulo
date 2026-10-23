# JS library

`<script src="https://www.monokulo.com/static/monokulo-client.js"></script>` adds
one global, `window.Monokulo`, with two functions. No build step, no
dependencies. Monokulo's address is taken from the script's own URL, less
its `/static/monokulo-client.js` (so a Monokulo served under a path keeps
it), unless you pass it as `monokulo`.

## `Monokulo.createOrder(params) → Promise<Order>`

| Param | Type | What it is |
| --- | --- | --- |
| `publicKey` | string, required | The store's `pk_…`. |
| `amount` | number or string, required | The price, in `currency`: up to 12 decimals, trailing zeros not counted (`"12.50000000"` is fine). A string keeps it exact. |
| `currency` | string, required | `"XMR"`, or a currency the store's exchange rate provider prices (`"EUR"`). |
| `merchantOrderId` | string, optional | Your own order or cart id, kept with the order and shown on its page in the dashboard. |
| `description` | string, optional | What the order is for, shown to the payer under your store's name. Up to 140 characters, plain text. Anyone with the payment link can read it. |
| `showMerchantOrderId` | boolean, optional, default true | Set false to keep `merchantOrderId` off the checkout; it still shows in the dashboard and in webhooks. |
| `monokulo` | string, optional | Monokulo's address (`https://pay.example.com`, or `https://example.com/monokulo` under a path), if it can't be read from the script tag. The library finds the API and the checkout under it. |

Resolves to `{ orderId, address, xmrAmountPiconero, amount, currency,
merchantOrderId, description, expiresAt, monokulo, publicKey, checkoutUrl, orderUrl }`, `monokulo` being the
address the order was made at. It posts to [the API](../api/)'s `create-order` form with the public key
(`Authorization: Bearer pk_…`). Rejects with an `Error`
carrying the problem's `detail`. If Monokulo asks a busy visitor for a proof of
work, the library solves it and tries again by itself.

## `Monokulo.mount(target, orderOrId, options?) → { iframe, destroy() }`

`target` is a selector or an element; its content is replaced by the
checkout's iframe. `orderOrId` is the object from `createOrder`, or an order
id (then pass `monokulo`, and `publicKey` for the polling fallback, in
`options`, say after a reload).
Mounting the same element again replaces the old checkout; `destroy()`
removes it and stops listening.

| Option | Type | What it does |
| --- | --- | --- |
| `onStatusChange` | `(status, data) => void` | Each new status, from the order's live updates (server-sent events, polling as a fallback). |
| `onPaid` | `(data) => void` | The status became `paid` or `overpaid`. A hint for the page: trust the webhook. |
| `onExpired` | `(data) => void` | The status became `expired`. |
| `theme` | `"light"` or `"dark"` | Pins the checkout's theme; left out, it follows the customer's device. |
| `refund` | `false` | Hides the refund-address form. |
| `width`, `height` | CSS length | The iframe's size (420px × 900px unless set). |
| `monokulo`, `publicKey` | string | Only when mounting a bare order id; `publicKey` lets it poll the order where server-sent events can't be used. |

Status values: `pending`, `unconfirmed`, `confirming`, `partial`, `paid`,
`overpaid`, `expired`, `cancelled` (the merchant cancelled it before anything
was paid; only the POS cancels, its own orders). Updates stop at `paid`,
`overpaid`, `expired` or `cancelled`.

## Shops on Tor

On a page served from a `.onion` address, the library makes the order,
frames the checkout and follows it on Monokulo's own onion address, so the
customer never leaves Tor. It finds that address once, from the API's entry
(`mk:onion-api`, see [Tor](../api/#tor)); load the script from the onion
address instead
(`<script src="http://….onion/static/monokulo-client.js">`) and it needn't
look. `monokulo` and `checkoutUrl` are then onion addresses. A Monokulo with
no onion address has nowhere else to send them, so its web address is used.

## When is an order really paid?

- **In the page** (`onPaid`, `onStatusChange`): for the customer's
  experience only. It runs in their browser, so it can be faked.
- **On your server**: the signed `order.paid` webhook. See
  [When is an order really paid?](../paid/)
- **No server**: confirm the order in the Monokulo dashboard before you
  send anything.
