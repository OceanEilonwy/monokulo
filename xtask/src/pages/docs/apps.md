# Connected apps

A connected app is a device that takes payments for one store with a key
of its own: a till app beside a point of sale, say. A store has any number
of them, each disconnected on its own. Monokulo keeps no list of apps: any
app that follows this page works with any Monokulo.

An app differs from a [shop plugin](../plugins/): a store has one plugin,
which connects from the shop and is found by the shop's address; apps
connect from the store's dashboard and have no shop.

## 1. Connect

The merchant opens the store's settings, **Connections › Apps › Connect an
app**, and Monokulo shows a code: a QR code of a link, and the same link to
copy.

```
https://pay.example/apps/connect#token=ct_…
```

Read it with your app's scanner, or let the merchant paste it. The token is
in the `#` part, which a browser never sends to a server. It works once,
for 10 minutes. Redeem it:

```
POST https://pay.example/apps/connect
Monokulo-Client: {slug}/{your version}
Content-Type: application/json

{ "token": "ct_…", "slug": "till-app", "name": "Till App",
  "device_name": "Pixel 8", "platform": "android" }
```

- `slug` names your app, with a plugin's rules: a lowercase letter, then 1
  to 31 lowercase letters, digits or hyphens.
- `name`, `device_name` and `platform` are optional. `name` is what the
  dashboard calls your app; `device_name` is used when the merchant gave
  the device none.

It answers once per token:

```json
{
  "public_key": "pk_…",
  "app_key": "ak_…",
  "endpoint": "https://pay.example",
  "app_id": "app_…",
  "device_name": "Front counter",
  "store": { "name": "Bakery", "base_currency": "USD" },
  "wallet": { "name": "Shop wallet", "network": "mainnet",
              "address": "4AdUndXH…", "address_short": "4AdUndXH…pQ8Lmb7s" }
}
```

`wallet` is where the store's payments go: `address` is its primary
address, whole, for the app to show or copy, and `address_short` the same
address shortened for a narrow screen. A Monokulo from before `address`
was added sends only `address_short`.

Keep `app_key` somewhere safe on the device: it isn't shown again. An
`onion_endpoint` is added when the Monokulo has an onion address. A `503`
with `{"error": …}` means the Monokulo has no public address yet (the token
is still good); any other failure is a `401`.

## 2. Make orders

The [plugins' order API](../plugins/#2-create-an-order), with the app's key:

```
POST {endpoint}/pay/{public_key}/orders
Authorization: Bearer {app_key}
Monokulo-Client: {slug}/{your version}

{ "amount": "24.50", "currency": "USD", "merchant_order_id": "#1042",
  "idempotency_key": "till:payment-9f2c" }
```

The order is the app's on the dashboard, with the device's name. Two
devices that send the same `idempotency_key` get the same order. Anyone with
the payment link can read an order's `description`, so leave it out if what
was bought is private.

## 3. Follow them

```
GET {endpoint}/pay/{public_key}/events
Authorization: Bearer {app_key}
```

Server-sent events for every order the store's apps make: `ready` first,
then `sale.waiting`, `sale.seen`, `sale.confirming`, `sale.paid`,
`sale.partial`, `sale.expired` and `sale.double_spend`, each with JSON data:

```json
{ "event": "sale.paid", "order_id": "…", "amount": "24.50", "currency": "USD",
  "address": "8…", "xmr_amount_piconero": 141900000000,
  "merchant_order_id": "#1042", "expires_at": 1761312000,
  "device_name": "Front counter", "at": 1761310260 }
```

A comment arrives every 25 s. After `lagged`, or a reconnect, read the
status of the orders you're showing again:
`GET {endpoint}/pay/{public_key}/orders/{order_id}/status`.

## 4. Disconnect

```
DELETE {endpoint}/pay/{public_key}/app
Authorization: Bearer {app_key}
```

The key stops working at once. The merchant can do the same from the
dashboard. Orders the app made stay, and can still be paid.
