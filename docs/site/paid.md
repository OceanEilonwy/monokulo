# When is an order really paid?

Three places hear about a payment. Only two of them can be trusted with
shipping.

## In the page: a nicety

`onPaid` and `onStatusChange` ([JS library](../js-library/)) run in the
customer's browser. Use them to say thanks or move to a "Thanks" page. Never
ship on them: anyone can call `onPaid` from their browser's console.

## On your server: the signed webhook

In the store's settings, under *Webhooks*, add your server's address. Monokulo
then sends a `POST` for every change of an order's status
(`order.confirming`, `order.paid`, `order.expired`, …) with a JSON body:

```json
{
  "event": "order.paid",
  "event_id": "evt_6f1c…",
  "order_id": "pay_91b2…",
  "status": "paid",
  "created_at": 1791400000
}
```

- `order_id` is Monokulo's id for the order: `createOrder` gave it to your
  page as `orderId`. Keep it with your own order when you make one, so the
  webhook can find it.
- A double-spend later gets its own `order.double_spend_detected` event (and
  `order.double_spend_reversed`, with the `txid`, if it was wrong).
- Richer fields (the shop's own order id, the fiat price with its exchange-rate source and rate, the store) arrive when Monokulo takes over webhook delivery from the engine, which is planned.

Before trusting a delivery:

1. **Check `X-Monokulo-Signature`**: `t=<unix seconds>,v1=<hex>`, where the
   hex is HMAC-SHA256 of `"<t>.<body>"` keyed by the webhook's signing
   secret. Compare in constant time, over the exact bytes received.
2. **Refuse an old one**: `t` more than five minutes from your clock.
3. **Ignore an `event_id` you've seen**: a delivery whose answer was lost is
   sent again, byte for byte.
4. **Then ship** on `order.paid` (or `order.overpaid`).

Answer with any `2xx` once it's handled; anything else is retried with
growing gaps.

## No server: the dashboard

A shop with no server of its own (a static page, like
[Geomart](../walkthrough-geomart/)) has nowhere for a webhook to arrive. It
shows "Thanks" from `onPaid` and confirms each order in the Monokulo
dashboard, on the store's page, before it sends anything.
