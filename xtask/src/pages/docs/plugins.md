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
  "webhook_signing_secret": "whsec_…"
}
```

Keep all four. `secret_token` is the store's key for your server, in
`Authorization: Bearer` on the [HTTP API](../api/); `public_key` is for the
shop's pages, if they embed the checkout with the [JS library](../js-library/).
`api` is the API's entry, a path: resolve it against the address you called
`/finish` at. A `503` with `{"error": …}` means the Monokulo can't connect
plugins yet; show the error. Any other failure is a `401`: start again.

### 2. Create an order

`GET /api/v1` with the key lists what it may do; the `create-order` form is:

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
the problem's page.

### 3. Send the customer to pay

To the order's `mk:checkout` link (`/checkout/{order_id}`, resolved against
the address you call Monokulo at), or show that page in an iframe on yours.
The [JS library](../js-library/) can mount it too. Read the order again with
its `self` link (`GET /api/v1/orders/{order_id}`), or follow its changes on
`mk:order-events`; but mark the shop's order paid from the webhook.

### 4. Take webhooks

Monokulo posts each change to `webhook_url`, signed. [Webhooks](../webhooks/)
has the body and how to check it. Mark the shop's order paid on
`order.paid` and `order.overpaid`; never undo a payment on your own.

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
