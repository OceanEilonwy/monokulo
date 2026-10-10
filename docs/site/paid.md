# When is an order really paid?

Three places hear about a payment. Only two of them can be trusted with
shipping.

## In the page: a nicety

`onPaid` and `onStatusChange` ([JS library](../js-library/)) run in the
customer's browser. Use them to say thanks or move to a "Thanks" page. Never
ship on them: anyone can call `onPaid` from their browser's console.

## On your server: the signed webhook

In the store's settings, under *Webhooks*, add your server's address. Monokulo
then sends a signed `POST` for every change of an order's status
(`order.confirming`, `order.paid`, `order.expired`, …) with a JSON body:

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

- `merchant_order_id` is your own order id, when you passed one to
  `createOrder` (`merchantOrderId`): find your order by it. `order_id` is
  Monokulo's.
- A double-spend later gets its own `order.double_spend_detected` event (and
  `order.double_spend_reversed` if it was wrong), each with the `txid`.
- Every field, and the retries, are on [Webhooks](../webhooks/).

Before trusting a delivery:

1. **Check `X-Monokulo-Signature`**: `t=<unix seconds>,v1=<hex>`, where the
   hex is HMAC-SHA256 of `"<t>.<body>"` keyed by the webhook's signing
   secret. Compare in constant time, over the exact bytes received.
2. **Refuse an old one**: `t` more than five minutes from your clock.
3. **Ignore an `event_id` you've seen**: a delivery whose answer was lost is
   sent again, byte for byte.
4. **Then ship** on `order.paid` (or `order.overpaid`).

Answer with any `2xx` once it's handled; anything else is retried with
growing gaps for about two hours, then given up on. The store's settings,
under *Webhooks*, show each delivery and can send one again.

## No server: the dashboard

A shop with no server of its own (a static page, like
[Geomart](../walkthrough-geomart/)) has nowhere for a webhook to arrive. It
shows "Thanks" from `onPaid` and confirms each order in the Monokulo
dashboard, on the store's page, before it sends anything.
