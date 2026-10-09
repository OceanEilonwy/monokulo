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
  "merchant_order_id": "gm-1042",
  "status": "paid",
  "amount": "0.041200000000",
  "currency": "XMR",
  "created_at": 1791400000
}
```

- `merchant_order_id` is your own id from `createOrder` (absent when the
  order was made without one).
- `amount` and `currency` are what the order asks for in XMR: Monokulo's
  engine prices every order in XMR, so `currency` is always `"XMR"` and
  `amount` has 12 decimals.
- A double-spend later gets its own `order.double_spend_detected` event (and
  `order.double_spend_reversed` if it was wrong), with the `txid`.

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
