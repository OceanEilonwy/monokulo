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

A merchant using the dashboard over the onion address is given a link on
the onion address, and told the app connects over Tor: the code is never on
the web address for them, so their phone isn't sent off Tor unasked.

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
your app's own connection (`mk:app`) and its push targets
(`mk:push-targets`), and carries the forms your key may use
(`create-order`, `disconnect`, `register-push-target`). Follow the links rather than building
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

There's no `sale.cancelled`: only the POS cancels an order, and only its
own, so an app's order never is. `sale.waiting` comes again after
`sale.double_spend` when the order is left with nothing received: wait for
a payment again.

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

The key stops working at once, its event stream ends and its push targets
are deleted. The merchant can do the same from the dashboard. Orders the
app made stay, and can still be paid.

## 6. Get pushes

An app that isn't running hears of the store's sales from pushes instead
of its event stream: [Web Push](https://www.rfc-editor.org/rfc/rfc8030),
encrypted to the device ([RFC 8291](https://www.rfc-editor.org/rfc/rfc8291))
and signed by the Monokulo ([VAPID](https://www.rfc-editor.org/rfc/rfc8292)),
to a UnifiedPush distributor or to a push relay that hands it on to APNs or
FCM. Only an app's key has push targets.

Follow the entry's `mk:push-targets`:

```
GET https://www.monokulo.com/api/v1/push-targets
Authorization: Bearer {app_key}
```

```json
{ "vapid_public_key": "BOz2…", "push_targets": [],
  "_links": { "self": { "href": "/api/v1/push-targets" }, "mk:app": { "href": "/api/v1/app" } },
  "_templates": { "register-push-target": { "method": "POST", "target": "/api/v1/push-targets" } } }
```

`vapid_public_key` is the Monokulo's own key, the same for every app. Give
it to your push service when you ask it for a subscription (a UnifiedPush
distributor's `VAPID` option, the relay's `vapid_public_key`), so it takes
pushes for that subscription from this Monokulo only. Read it each time you
subscribe, rather than keeping it.

Then register the subscription with the `register-push-target` form:

```
POST https://www.monokulo.com/api/v1/push-targets
Authorization: Bearer {app_key}
Content-Type: application/json

{ "client_id": "unifiedpush", "kind": "encrypted",
  "endpoint": "https://ntfy.sh/upYuGg6sBMEvqHs4?up=1",
  "keys": { "p256dh": "BCVx…iw4", "auth": "BTBZ…Igg" } }
```

- `client_id` is your app's own name for the subscription (visible ASCII,
  at most 64 characters). Registering again with the same one replaces that
  target's subscription and keeps its id: do so whenever your push service
  hands out a new endpoint.
- `kind` is `encrypted`, Web Push encrypted with the subscription's `keys`
  (`p256dh` and `auth`, base64url, as a `PushSubscription` or a UnifiedPush
  distributor gives them); or `live-activity`, plain JSON to the push relay
  for an iOS Live Activity, which takes no `keys`. A `live-activity` target
  may set `include_payment_uri` to have its pushes carry the payment URI,
  for the Dynamic Island to show the payment code.
- `endpoint` is the subscription's `https` URL, at most 2048 characters
  once non-ASCII characters are percent-escaped. Monokulo doesn't push over
  Tor, so it is never an onion address. Plain `http` is taken only for this
  machine (`127.0.0.1`, `[::1]`, `localhost`), which Monokulo pushes to only
  when its admin allows private addresses.
- `events` names the sale events to push (the names of
  [Follow them](#4-follow-them)); every one when left out.

It answers `201 Created` with the target and its `Location`. Its keys are
never shown again. A target carries the `unregister-push-target` form,
`DELETE` on the target itself, for when the app no longer wants its
pushes. An app may have 8 push targets.

| Problem | Status | What to do |
| --- | --- | --- |
| [`invalid-request`](../problems/invalid-request/) | 400 | The subscription isn't one Monokulo can push to: `detail` says why. |
| [`too-many-push-targets`](../problems/too-many-push-targets/) | 409 | Unregister a target you no longer use, or register again with the `client_id` of one. |
| [`forbidden`](../problems/forbidden/) | 403 | Only an app's own key has push targets. |
| [`not-found`](../problems/not-found/) | 404 | No such target for your app (another app's included). |

### What arrives

Each sale event of [Follow them](#4-follow-them) that a target wants is
pushed to it, for every order any of the store's apps makes. The
`sale.waiting` of a new order isn't pushed to the app that made it: it
shows the sale already.

An `encrypted` target gets the event's JSON, as the stream sends it, with
the `api_version` webhooks carry, encrypted to the subscription's keys
(`Content-Encoding: aes128gcm`, RFC 8291). Decrypt it with the
subscription's private key and `auth`, which never leave the device:

```json
{ "api_version": 2, "event": "sale.paid", "order_id": "order_9f3c…", "amount": "24.50",
  "currency": "USD", "address": "8…", "xmr_amount_piconero": 141900000000,
  "merchant_order_id": "#1042", "expires_at": 1761312000,
  "device_name": "Front counter", "at": 1761310260,
  "_links": { "mk:order": { "href": "/api/v1/orders/order_9f3c…" }, "curies": […] } }
```

A `live-activity` target gets the state an iOS Live Activity shows, plain
JSON (`Content-Type: application/json`), through the relay. It never holds
the order's id; `sale` is the app's own `merchant_order_id`, when it gave
one, and `paymentURI` is there only for a target registered with
`include_payment_uri`, and only while the sale takes a payment:

```json
{ "phase": "waiting", "sale": "#1042", "amount": "24.50", "currency": "USD",
  "xmr": "0.1419", "expiresAt": 1761312000, "waitingCount": 2,
  "paymentURI": "monero:8…?tx_amount=0.1419" }
```

`phase` is `waiting` (`sale.waiting`, and `sale.partial`: the rest can
still come), `seen`, `confirming`, `paid`, or `expired` (`sale.expired`,
and `sale.double_spend`: no payment). `waitingCount` is the store's app
orders still waiting for a payment, this one included while it is.

Every push about one order has the same `Topic`, so a push service holding
pushes for a device that is away keeps only the latest. It is a hash, not
the order's id, which a push service shouldn't learn:

| Event | `Urgency` | `TTL` |
| --- | --- | --- |
| `sale.waiting` | `normal` | until the sale expires |
| `sale.seen`, `sale.confirming` | `low` | until the sale expires |
| `sale.partial` | `high` | until the sale expires |
| `sale.paid`, `sale.expired`, `sale.double_spend` | `high` | 30 minutes |

"Until the sale expires" is at least a minute and at most 30 minutes (the
admin's `push.default_ttl_secs`). A push is never sent after its `TTL`, and
a newer one about the same order replaces one not sent yet. One that
fails is tried again 5, 20, 80 and 320 seconds later (or when the push
service's `Retry-After` says), five times at most.

A push service answering `404` or `410` has dropped the subscription:
Monokulo deletes the target. So it does after 20 failures in a row. Read
your targets (`mk:push-targets`) when the app starts, and register again
any that's gone.
