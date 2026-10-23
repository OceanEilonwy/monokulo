# Shop plugins

A shop plugin takes Monero in a shop platform's own checkout. You install
it in the shop, not in Monokulo, and connect it with one button. Monokulo
keeps no list of plugins: any plugin that follows the steps on this page
works with any Monokulo.

## Plugins

| Shop | Plugin |
| --- | --- |
| WooCommerce | [monokulo-woocommerce](https://github.com/OceanEilonwy/monokulo-woocommerce) |

Each plugin's own repository says how to install it and what it needs.

## Writing a plugin

A plugin does five things. Everything else about its shop platform stays
in the plugin.

### 1. Connect

Send the merchant's browser to:

```
GET {monokulo}/plugins/connect/{slug}?site_url=…&return_url=…&nonce=…&name=…
```

- `slug` names your plugin: a lowercase letter, then 1 to 31 lowercase
  letters, digits or hyphens (`prestashop`, `open-cart`). Anything else is
  not found.
- `site_url` is the shop's address. Monokulo finds the merchant's store by
  its host, or sets one up for it. One host is one store.
- `return_url` is where Monokulo sends the browser back. It must be on the
  same host as `site_url`, over https (plain http only for `localhost` or a
  `.onion` address).
- `nonce` is yours: keep it, and check it when the browser comes back.
- `name` is optional: what Monokulo calls your plugin on its pages
  ("Connect your PrestaShop shop"), up to 40 characters. Without it,
  Monokulo shows the slug.

The merchant signs in and confirms, and the browser comes back to
`return_url` with `token` and `nonce` added. Check the nonce, then, from your
server, within 10 minutes:

```
POST {monokulo}/plugins/connect/{slug}/finish
Monokulo-Client: {slug}/{your version}
Content-Type: application/json

{ "token": "…", "webhook_url": "https://shop.example/your-receiver", "name": "PrestaShop" }
```

It answers once per token:

```json
{
  "public_key": "pk_…",
  "secret_token": "sk_…",
  "api": "/api/v1",
  "onion_api": "http://2gzy…wid.onion/api/v1",
  "webhook_signing_secret": "whsec_…"
}
```

`onion_api` is there only while the Monokulo has an onion address, and
`webhook_signing_secret` only when you sent a `webhook_url`. Keep them all. `secret_token` is the store's key for your server, in
`Authorization: Bearer` on the [HTTP API](../api/); `public_key` is for the
shop's pages, if they embed the checkout with the [JS library](../js-library/).
`api` is the API's entry, a path (`/api/v1`, or `/monokulo/api/v1` for a
Monokulo served [under a path](../api/#under-a-path)): resolve it against
the address you called `/finish` at, and keep the result.

Every error is `application/problem+json`; show its `detail`. Its `type`
is one of these:

| Problem | Status | What to do |
| --- | --- | --- |
| [`invalid-connect-code`](../problems/invalid-connect-code/) | 401 | The token is unknown, used or expired: start connecting again. |
| [`no-public-address`](../problems/no-public-address/) | 503 | The Monokulo's admin hasn't set its address yet. The token isn't spent: try again later. |
| [`invalid-request`](../problems/invalid-request/) | 400 | The body isn't what `/finish` takes, or `webhook_url` isn't an http(s) URL. The token isn't spent: fix it and send it again. |
| [`unsupported-media-type`](../problems/unsupported-media-type/) | 415 | Send `Content-Type: application/json`. |
| [`too-large`](../problems/too-large/) | 413 | The body is too large. |
| [`not-found`](../problems/not-found/) | 404 | `slug` isn't a slug. |
| [`rate-limited`](../problems/rate-limited/) | 429 | Wait `retry_after` seconds. |
| [`internal`](../problems/internal/) | 500 | Something failed on the Monokulo after the token was spent: start again. |

### 2. Create an order

The entry (`GET {api}` with the key) lists what it may do. Keep it as its
`Cache-Control` says and read it again when it expires or a form from it is
refused with `401`, `404` or `405` (see [Find your way](../api/#find-your-way)),
so making an order is one request. Its `create-order` form is:

```
POST {monokulo}/api/v1/orders
Authorization: Bearer {secret_token}
Monokulo-Client: {slug}/{your version}
Content-Type: application/json

{
  "amount": "12.50",
  "currency": "EUR",
  "merchant_order_id": "1042",
  "show_merchant_order_id": true,
  "description": "Order #1042: Blue mug + 2 more",
  "idempotency_key": "ps:1042:12.50:EUR"
}
```

It answers `201 Created` with the order and its `Location`. Monokulo prices
fiat in XMR itself. `amount` is a decimal string with up to 12 decimals in
any currency, and trailing zeros don't count, so a shop that pads its prices
(`12.50000000`) can send them as they are. Webhooks give `amount` back
exactly as you sent it. A retry with the same `idempotency_key` gets the same
order. While your plugin is the store's connected one, orders it makes this
way show as yours on the dashboard. A refused order is
`application/problem+json`: its `detail` says why, and its `type` links to
the problem's page. Two are worth retrying rather than showing the
customer as final: [`rates-unavailable`](../problems/rates-unavailable/)
(503: the exchange-rate providers are down or rate limiting Monokulo and it
has no recent rate for the currency) and
[`engine-unavailable`](../problems/engine-unavailable/) (503: the payment
engine is busy). Nothing was made; wait `retry_after` seconds (also in
`Retry-After`) when given, and send the same request with the same
`idempotency_key`.

### 3. Send the customer to pay

To the order's `mk:checkout` link (`/checkout/{order_id}`, resolved against
the address you call Monokulo at), or show that page in an iframe on yours.

A customer browsing the shop's own `.onion` address must stay on Tor: send
them to the order's `mk:onion-checkout` instead, an absolute URL on
Monokulo's onion address, never to `mk:checkout` resolved against the web
address you called. Tell them apart by the host their request came to. If
the order has no `mk:onion-checkout`, the Monokulo has no onion address;
say so to the merchant rather than sending a Tor customer to the web.
The [JS library](../js-library/) can mount it too. Read the order again with
its `self` link (`GET /api/v1/orders/{order_id}`), or follow its changes on
`mk:order-events`; but mark the shop's order paid from the webhook.

On the shop's own order page, link the merchant to the order in Monokulo
with its [`mk:dashboard-order`](../rels/dashboard-order/) link (resolved
like the others): its page on the merchant's dashboard. Only your secret
key is given it; keep it with the order rather than building it. Every
webhook carries it too, absolute, in its `_links` (with `mk:order` and
`mk:checkout`), so an order made before your plugin kept its links gets
them from its next event.

### 4. Take webhooks

Monokulo posts each change to `webhook_url`, signed. [Webhooks](../webhooks/)
has the body and how to check it. Mark the shop's order paid on
`order.paid` and `order.overpaid`; never undo a payment on your own.
`order.expired` and `order.cancelled` both mean the order takes no payment
any more; an order your plugin made is never cancelled (only the POS
cancels, its own orders), but a receiver that sees `order.cancelled` should
treat it as it treats `order.expired`.

### 5. Forward your errors (optional)

Follow the entry's `send-logs` form; your `sk_` key is the only one offered
it:

```
POST /api/v1/logs
Authorization: Bearer {secret_token}
Monokulo-Client: {slug}/{your version}
Content-Type: application/json

{ "entries": [ { "level": "error", "message": "…", "trace_id": "…" } ] }
```

Kept only when the store turned on client logs (otherwise a `403`
`logs-off` problem). Levels are PSR-3's (`error`, `warning`, …); at most 20
entries a time.

## Test your receiver

[webhook-vectors.json](webhook-vectors.json) holds one signed delivery for
each event, and deliveries a receiver must refuse. Monokulo's own tests
build it from the code that sends real webhooks and check real deliveries
against it, so a receiver that passes on it handles what Monokulo sends.
