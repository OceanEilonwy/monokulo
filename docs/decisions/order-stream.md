# Decisions: the store's order stream

Made on 2026-11-12 for Monokular's Till, at the owner's request. Each says
what was chosen, why, and what else was possible, so any of them can be
reversed knowingly.

**The problem.** A connected app followed its orders on `mk:events`
(`GET /api/v1/events`), whose `sale.*` events carry the sale (amount,
address, expiry, device) but not the order: no `confirmations`, no
`confirmations_required`, no `amount_received_piconero`, and only one
`sale.confirming`, at the first block. Till read every confirming order
again every 20 seconds to fill its ring ("4 of 10"), and kept those numbers
optional because an event never had them. The owner chose to fix it in
Monokulo: a store-wide stream whose events are the order itself. Till's
side is in monokular `docs/decisions/core.md` (the payment backend, 228 and
after).

The API is in the docs' [Connected apps](../../xtask/src/pages/docs/apps.md)
page, "4. Follow them", and in the reference (`mk:orders-events`).

1. **`GET /api/v1/orders/events`, rel `orders-events`, linked from the
   entry.** It sits beside one order's stream
   (`/api/v1/orders/{order_id}/events`, `mk:order-events`), as the
   collection is beside one order. The path can't be mistaken for an order:
   every order id starts `order_`, and the router prefers the fixed segment
   anyway (a test reads both). Rejected: a query on `mk:events`
   (`?carry=order`), one URL with two shapes of event.
2. **Its events: `ready`, `order`, `lagged`.** `order`'s data is the order
   as `GET /api/v1/orders/{order_id}` answers it, links and forms included
   as the key reads them, built by the same code (`api::v1::order::load`)
   and read as the event is sent: never a copy kept from earlier. One parser
   reads both streams; an app needs no second model of an order.
3. **Same scope and keys as `mk:events`.** The orders the store's
   connected apps make, for the store's secret key and its apps' keys
   (`server`); never an order the store's own server or checkout made, nor
   another store's. It ends once its key stops working, like `mk:events`.
4. **One event per change, a new confirmation included.** The engine's
   per-store change hints (`GET /api/v1/admin/tenant/events`) already fire
   for each change to an order's status, confirmations, amount received
   or payments; the stream shares the live hub's one engine connection per
   store (`live::LiveHub::subscribe_store`) with the checkout and POS pages.
   Each hint reads the order again; one that reads the same as what was
   last sent sends nothing, so a confirming order sends one event a block
   and an idle one none. Unlike one order's stream, there's no 30-second
   re-send of an unchanged order: nothing in the body moves with the
   clock. An app's order just made, and one renewed, wake the stream from
   monokulo itself (`EngineClient::order_changed`): the engine says nothing
   of an order it makes.
5. **`ready` waits for Monokulo's own view of the store's changes.** It's
   sent once the live hub's engine connection for the store is open (at
   most 5 s), so a change after `ready` is never missed silently. When that
   connection opens again later, or fell behind, the stream says `lagged`
   (`missed` 0 when how many isn't known): the app reads its orders again.
   With the engine's stream unavailable the hub retries every 10 s at most,
   and each retry is a `lagged`: the app degrades to reading its orders
   that often, not to hearing nothing.
6. **The keep-alive comment every 25 s**, as `mk:events`.
7. **`mk:events` stays, unchanged, marked deprecated** in its rel's
   description, the entry's, the docs and the explorer, for Till builds
   already installed. Nothing it sends changed.
8. **An `encrypted` push carries the order too** (owner's choice): the
   sale event as before, plus `order`, the order as reading it answers when
   the push is queued (the subscriber reads it with the store's key, as a
   stream does; an app's `sale.waiting` push takes the order as just made).
   A push stays one sale event (its kind, TTL, urgency and topic are the
   sale's); the order only says where it stands now.
9. **An order that would make the push too large is left out, said in the
   log.** A Web Push body is 4096 bytes, 3993 of plaintext
   (`push::MAX_PLAINTEXT`), but through a push relay the ciphertext goes to
   APNs or FCM as base64 in a JSON envelope inside their own 4096 bytes
   (`push.md` 112), which leaves about 2912 bytes of plaintext. Monokulo
   doesn't know which way a target's pushes go, so it keeps every push
   under `push::payload::RELAYED_PLAINTEXT` (2900). A till's sale with its
   order is about 2200 bytes (measured, with a long proxy prefix and an
   onion address); the largest sale an app may make with the largest order
   (140-character description and 120-character reference of characters
   JSON escapes, the longest amount, currency, address and device name) is
   about 5300, past even a direct Web Push. Such a push goes without
   `order` (and `tracing` warns) rather than failing at every attempt; the
   app reads the order from its link. Rejected: truncating fields of the
   order (a body that isn't what reading it answers), or dropping the sale
   fields to make room (installed apps read them).
10. **The order says which device made it, and when its payment was spent
    twice.** Two things a sale event told an app that the order didn't:
    `device_name` (the device that made it, for an order one of the store's
    apps made, as the merchant named it; left out for any other) and
    `double_spend_detected_at` (left out unless a payment to it was found
    spent twice). Both are in the order wherever it's read (one order, the
    list, the stream, a push), so the stream loses nothing `mk:events` said:
    Till shows another till's sale with the till that rang it up, and a
    reversed payment as reversed. Both are additions: a client that doesn't
    know them ignores them.
