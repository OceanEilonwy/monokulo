# Connected apps

A connected app is a device that takes payments for one store with a key
of its own: a till app beside a point of sale, say. A store has any number
of them, each disconnected on its own. Monokulo keeps no list of apps: any
app that follows this page works with any Monokulo.

An app differs from a [shop plugin](../plugins/): a store has one plugin,
which connects from the shop and is found by the shop's address; apps
connect from the store's dashboard and have no shop.

Once connected, an app uses the [HTTP API](../api/) with its own key.

## 1. Connect

The merchant opens the store's settings, **Connections › Apps › Connect an
app**, and Monokulo shows a code: a QR code of a link, and the same link to
copy.

```
https://www.monokulo.com/apps/connect#token=ct_…
```

Read it with your app's scanner, or let the merchant paste it. The token is
in the `#` part, which a browser never sends to a server. It works once,
for 10 minutes. Redeem it at the link's address:

```
POST https://www.monokulo.com/apps/connect
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
{ "app_key": "ak_…", "api": "/api/v1", "onion_api": "http://2gzy…wid.onion/api/v1" }
```

Keep `app_key` somewhere safe on the device: it isn't shown again. `api` is
the API's entry, a path (`/api/v1`, or `/monokulo/api/v1` for a Monokulo
served [under a path](../api/#under-a-path)): resolve it against the
address you connected at (an app that connected over the onion address
stays on it). `onion_api` is
there only while the Monokulo has an onion address: the same entry on it,
for an app that can reach Monokulo over Tor (see [Tor](../api/#tor)).

Every error is `application/problem+json`; show its `detail`. Its `type`
is one of these:

| Problem | Status | What to do |
| --- | --- | --- |
| [`invalid-connect-code`](../problems/invalid-connect-code/) | 401 | The code is unknown, used or expired: ask the merchant for a new one. |
| [`no-public-address`](../problems/no-public-address/) | 503 | The Monokulo's admin hasn't set its address yet. The code isn't spent: try again later. |
| [`invalid-request`](../problems/invalid-request/) | 400 | The body isn't what this takes, or `slug` isn't a slug. |
| [`unsupported-media-type`](../problems/unsupported-media-type/) | 415 | Send `Content-Type: application/json`. |
| [`too-large`](../problems/too-large/) | 413 | The body is too large. |
| [`rate-limited`](../problems/rate-limited/) | 429 | Wait `retry_after` seconds. |
| [`challenge-required`](../problems/challenge-required/) | 429 | Solve the proof of work in `challenge` and send the request again with `Monokulo-Proof`. |
| [`internal`](../problems/internal/) | 500 | Something failed on the Monokulo: try again later. |

## 2. Find your way

Every request carries the key:

```
GET https://www.monokulo.com/api/v1
Authorization: Bearer {app_key}
Monokulo-Client: {slug}/{your version}
```

The entry links to the store (`mk:store`: its name, base currency and the
wallet its payments go to, with the full address), its orders, its events,
and your app's own connection (`mk:app`), and carries the forms your key may
use (`create-order`, `disconnect`). Follow the links rather than building
paths: they're relative, so resolve them against the URL you used. Keep the
entry as its `Cache-Control` says, and read it again when it expires or a
link from it answers `401`, `404` or `405` (see
[Find your way](../api/#find-your-way)). Read the store (`mk:store`) each
time you show it; the merchant can rename it or move it to another wallet.

## 3. Make orders

The `create-order` form:

```
POST https://www.monokulo.com/api/v1/orders
Authorization: Bearer {app_key}
Monokulo-Client: {slug}/{your version}
Content-Type: application/json

{ "amount": "24.50", "currency": "USD", "merchant_order_id": "#1042",
  "idempotency_key": "till:payment-9f2c" }
```

It answers `201 Created` with the order and its `Location`. The order is
the app's on the dashboard, with the device's name. Two devices that send the
same `idempotency_key` get the same order. Anyone with the payment link can
read an order's `description`, so leave it out if what was bought is
private.

## 4. Follow them

```
GET https://www.monokulo.com/api/v1/events
Authorization: Bearer {app_key}
```

Server-sent events for every order the store's apps make: `ready` first,
then `sale.waiting`, `sale.seen`, `sale.confirming`, `sale.paid`,
`sale.partial`, `sale.expired` and `sale.double_spend`, each with JSON data
that links its order (`mk:order`):

```json
{ "event": "sale.paid", "order_id": "order_9f3c…", "amount": "24.50", "currency": "USD",
  "address": "8…", "xmr_amount_piconero": 141900000000,
  "merchant_order_id": "#1042", "expires_at": 1761312000,
  "device_name": "Front counter", "at": 1761310260,
  "_links": {
    "mk:order": { "href": "/api/v1/orders/order_9f3c…", "title": "One order: what to pay, where, and how far the payment has got." },
    "curies": [{ "name": "mk", "href": "https://docs.monokulo.com/rels/{rel}", "templated": true }]
  } }
```

A comment arrives every 25 s. After `lagged`, or a reconnect, read the
orders you're showing again: follow each one's `mk:order` link (or the
order's own `self`), or follow one order's own stream (`mk:order-events`).
Never build an order's URL from its id.

## 5. Disconnect

The `disconnect` form on your app's connection:

```
DELETE https://www.monokulo.com/api/v1/app
Authorization: Bearer {app_key}
```

The key stops working at once, and its event stream ends. The merchant can
do the same from the dashboard. Orders the app made stay, and can still be
paid.
