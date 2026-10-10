# Webhooks

A webhook tells your server when one of the store's orders changes: it's
paid, it's confirming, it expired, a payment to it was double-spent. Ship on
`order.paid` (see [When is an order really paid?](../paid/)).

## Add one

In Monokulo, the store's settings, *Webhooks*: *Add a webhook* with your
server's address, and any headers of your own you want sent with every
delivery (an API key for your endpoint, say). Monokulo shows the webhook's
signing secret once, there and then: keep it on your server. Lost it?
Delete the webhook and add a new one.

The WooCommerce plugin adds its own webhook when it connects, and
disconnecting it deletes it.

**A new webhook gets only the events that happen from the moment it's
added.** Nothing from before is sent to it: no earlier status changes, not
even for orders still open.

There is no switching a webhook off. To stop one, delete it; to start
again, add it again (with a new signing secret).

## What arrives

A `POST` with `Content-Type: application/json` and these headers:

| Header | |
| --- | --- |
| `X-Monokulo-Signature` | `t=<unix seconds>,v1=<hex>`: see [Check it](#check-it). |
| `X-Monokulo-Event` | The event, as in the body. |
| `X-Monokulo-Event-Id` | The event's id, as in the body. |
| `traceparent` | For tracing; ignore it if you don't trace. |

plus the headers you added. The body:

```json
{
  "api_version": 2,
  "event_id": "evt_4be2d07a85c113e9",
  "event": "order.paid",
  "created_at": 1760020320,
  "order_id": "5f01c9a7d2e14b88a3e0f9c6d1e21b07",
  "status": "paid",
  "merchant_order_id": "gm-1042",
  "amount": "12.50",
  "currency": "EUR",
  "xmr_amount": "0.081245310000",
  "fx_source": "coingecko",
  "fx_rate": "153.84615385",
  "store": { "id": "917701c2-15c7-4bcb-bbc8-72d9ab28e785", "name": "Bakery" }
}
```

| Field | What it is |
| --- | --- |
| `api_version` | `2`. |
| `event_id` | The event's id. The same on every retry of it: dedupe on this. |
| `event` | `order.<status>` when the order's status changes (`order.unconfirmed`, `order.confirming`, `order.paid`, `order.partial`, `order.overpaid`, `order.expired`); `order.double_spend_detected` when a payment to it was double-spent; `order.double_spend_reversed` when that turned out to be wrong. |
| `created_at` | When the event happened, in Unix seconds. |
| `order_id` | Monokulo's id for the order: `createOrder` gave it to your page as `orderId`. |
| `status` | The order's new status. On `order.<status>` events only. |
| `txid` | The transaction concerned. On the two double-spend events only. |
| `merchant_order_id` | Your own order id, when you passed one (`merchantOrderId`). |
| `amount`, `currency` | The price as you set it, fiat or XMR, as a decimal string. |
| `xmr_amount` | What the customer was asked to pay, in XMR with 12 decimals. |
| `fx_source`, `fx_rate` | Who priced the order and the rate used: 1 XMR in `currency`, a decimal string with up to 8 decimals. Not there for orders priced in XMR. |
| `store` | The store: `{ "id", "name" }`. |

A field with no value is left out, never sent as `null`.

## Check it

Before trusting a delivery:

1. **Check `X-Monokulo-Signature`**: `t=<unix seconds>,v1=<hex>`, where the
   hex is HMAC-SHA256 of `"<t>.<body>"` keyed by the webhook's signing
   secret. Compute it over the exact bytes received, before parsing them,
   and compare in constant time.
2. **Refuse an old one**: `t` more than five minutes from your clock.
3. **Ignore an `event_id` you've seen**: a delivery whose answer was lost is
   sent again, byte for byte, under a fresh signature.

[Geomart's handler](../walkthrough-geomart/#step-5) shows all three in a few
lines of JavaScript.

## Answer it

Answer with any `2xx` once it's handled, within 5 seconds (the defaults
are under [Settings](#settings)). Anything else, or no answer, is tried
again: up to 8 attempts, waiting 1, 2, 4, 8, 16, 32
and 64 minutes between them, so the last is about 2 hours after the first.
Then Monokulo gives up on it.

Each order's events are sent one at a time, oldest first: the next waits
until the one before is delivered or given up on.

## See how it's doing

The store's settings, *Webhooks*, show each webhook with one line on how
it's doing: delivering, retrying (and when it tries next), or gave up. Under
it are its deliveries, newest first, 20 at a time, open by themselves when
one is retrying or gave up. *Older →* and *← Newer* page through them (in
place, or as the page *All deliveries for this webhook* without
JavaScript).

Deliveries don't stay forever: one that arrived is deleted after 30 days,
one that gave up after 90 (`webhooks.keep_delivered_days`,
`webhooks.keep_given_up_days`). Deliveries still being tried are kept.

- *Details* on a delivery shows its attempts, the request sent (the
  signature header, never the secret; your own headers' values masked) and
  the start of your server's last answer.
- *Send again* on a delivery that gave up sends it again now; *Retry failed*
  does it for all of a webhook's.
- *Delete…* removes the webhook and its deliveries.

## Settings

Whoever runs your Monokulo sets these on its admin settings page
(*Payments*, *Webhooks*) or in its options file:

| Setting | Default | |
| --- | --- | --- |
| `webhooks.max_attempts` | 8 | Attempts per delivery before giving up. |
| `webhooks.delivery_timeout_ms` | 5000 | How long your server has to answer each one. |
| `webhooks.keep_delivered_days` | 30 | Days a delivery that arrived stays in the list. |
| `webhooks.keep_given_up_days` | 90 | Days a delivery that gave up stays in the list, where it can still be sent again. |
| `webhooks.allow_private_urls` | false | Webhooks are never sent to private or loopback addresses (`localhost`, `192.168.…`) unless this is on. Only for testing on your own network. |
