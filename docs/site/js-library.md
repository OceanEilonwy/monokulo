# JS library

`<script src="https://pay.example/static/monokulo-client.js"></script>` adds
one global, `window.Monokulo`, with two functions. No build step, no
dependencies. Monokulo's address is taken from the script's own URL unless
you pass `endpoint`.

## `Monokulo.createOrder(params) → Promise<Order>`

| Param | Type | What it is |
| --- | --- | --- |
| `publicKey` | string, required | The store's `pk_…`. |
| `amount` | number or string, required | The price, in `currency`. |
| `currency` | string, required | `"XMR"`, or a currency the store's exchange rate provider prices (`"EUR"`). |
| `merchantOrderId` | string, optional | Your own order or cart id, kept with the order and sent back in its webhooks. |
| `endpoint` | string, optional | Monokulo's address, if it can't be read from the script tag. |

Resolves to `{ orderId, address, xmrAmountPiconero, amount, currency,
merchantOrderId, expiresAt, endpoint, publicKey }`. Rejects with an `Error`
carrying Monokulo's message. If Monokulo asks a busy visitor for a proof of
work, the library solves it and tries again by itself.

## `Monokulo.mount(target, orderOrId, options?) → { iframe, destroy() }`

`target` is a selector or an element; its content is replaced by the
checkout's iframe. `orderOrId` is the object from `createOrder`, or an order
id (then pass `endpoint` and `publicKey` in `options`, say after a reload).
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
| `endpoint`, `publicKey` | string | Only when mounting a bare order id. |

Status values: `pending`, `unconfirmed`, `confirming`, `partial`, `paid`,
`overpaid`, `expired`. Updates stop at `paid`, `overpaid` or `expired`.

## When is an order really paid?

- **In the page** (`onPaid`, `onStatusChange`): for the customer's
  experience only. It runs in their browser, so it can be faked.
- **On your server**: the signed `order.paid` webhook. See
  [When is an order really paid?](../paid/)
- **No server**: confirm the order in the Monokulo dashboard before you
  send anything.
