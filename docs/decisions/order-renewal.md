# Decisions: renewing an expired order

Made on 2026-11-10 without the owner to ask, while adding order renewal for
Monokular's "New code". Each says what was chosen, why, and what else was
possible, so any of them can be reversed knowingly.

**The problem.** Till's "New code", offered on a sale whose code ran out
unpaid, made a second Monokulo order for the same Square payment (its
idempotency key with `#2` after it). The dashboard then listed two orders for
one sale, which confused the merchant. The owner chose the fix: an API to
**renew** an expired, unpaid order, re-priced at the current rate with a new
expiry, keeping its id and its address, so one point-of-sale payment is always
one Monokulo order. Till's side is in monokular `docs/ledger.md` §7 and its
`docs/decisions/core.md` 167 to 174.

The API is in the docs' [Connected apps](../../xtask/src/pages/docs/apps.md)
page, "Renew an expired order", and in the reference (the order's
`renew-order` form).

1. **Only an expired order nothing has reached may be renewed.** The engine
   checks, in the transaction that renews it:
   - its status is `expired` (not `pending`, not `cancelled`: a cancelled
     order is the POS's, called off on purpose; not any paid or part-paid
     status);
   - `amount_received_piconero` is 0 **and** it has no payment rows at all,
     voided ones included, and no double spend was ever detected on it: a
     payment seen in the pool, a part payment, or one later reversed all
     refuse it. Renewing would put a new price over money that arrived, and
     a refused renew costs the cashier one more order at worst;
   - it is still in the scanner's window (`Order::in_scan_window`: closed no
     longer ago than `payment.expired_order_grace_period_minutes`, 360 by
     default). Past it the scanner stopped watching the address, so a payment
     sent since could be on chain unseen; renewing would ask the customer to
     pay an address whose history Monokulo doesn't know;
   - the row that scans it isn't behind (the same `scan_row_behind` that
     holds back expiry): while the store is catching up after an outage, a
     payment may be in a block not yet scanned for it. That refusal is
     `503`, worth retrying; the others are `409`;
   - it is still on the store's wallet (`scan_tenant_id == tenant_id`): after
     a wallet change the old address belongs to a wallet the merchant left.

   Considered: renewing a part-paid order for the rest (the checkout already
   asks for what's left; nothing needs a new quote), and renewing past the
   grace window after a rescan (needs the chain walked first; a new order is
   the honest answer then).
2. **Same order, same address, same subaddress index; new
   `xmr_amount_piconero` and `expires_at`.** The new expiry is
   `now + order_expiry_seconds`, exactly as a new order's. The status is
   recomputed in the same transaction (it becomes `pending`, `closed_at` is
   cleared, `next_due_at` is the new deadline) and the scanner watches it
   again as an open order. Nothing else changes: its id, `created_at`,
   merchant reference, description, confirmations override and idempotency
   key stay. No payment can be lost to the change, because the subaddress
   and the scan window are the same ones the order always had.
3. **No quote history: a payment is judged against the current quote.**
   `derive_status` keys off the payments' amounts and confirmations, the
   order's amount, and `now` against `expires_at`; it never looks at when a
   payment was made. So a payment sent at the old price and seen only after
   the renewal (the customer paid an expired code, or the second or so the
   mempool poll takes) is counted in full in `amount_received_piconero`, and
   the order is `partial` if the rate rose (the customer tops up at the same
   address, and the checkout asks for what's left), `overpaid` if it fell, or
   `paid` if they match. It is never lost, and never hidden: the merchant
   sees it on the order, the till sees the status. Keeping old quotes and
   accepting a payment that matches any of them would give a payer the
   better of two rates, make `derive_status` depend on history, and change
   the property tests' model for a window of about a second. A renewal is
   refused once anything is seen (1), which closes all but that window.
4. **`POST /api/v1/orders/{order_id}`, the order's own `renew-order` form,
   for the store's secret key and apps' keys.** A form on the order (not a
   route an app builds) is also how a client finds out the Monokulo can
   renew at all: an older Monokulo's orders carry no `renew-order`, so Till
   hides New code there rather than make a second order (Till's decision
   171). The public key may not renew: anyone has it, and a renew changes
   the price on someone's checkout. An app's key may renew only an order one
   of the store's apps made (any of them: any till may tap New code on a
   sale another till started), so a till can't re-price the shop's
   WooCommerce orders; the secret key may renew any of the store's orders.
5. **The precondition is the quote being renewed: `expires_at`.** The body
   is `{ "expires_at": … }`, the expiry of the quote the caller saw run out.
   The engine renews only while the order's `expires_at` still equals it,
   in the same transaction as the checks; otherwise it changes nothing and
   the order is answered as it is now (`200`). So two tills tapping New code
   at once, or a retry after a lost answer, get one renewal and the same
   answer, with no key to store. A caller holding an older quote than the
   order's present one gets the present order, which tells it what happened.
   Considered: an idempotency key per renewal (more state, and a second
   till's key would differ, so it wouldn't stop two renewals).
6. **No cap on renewals.** Each renewal is a fresh quote; the order is
   watched for exactly as long as an order made then would be, and the grace
   window (1) runs from the latest expiry. A sale whose customer keeps
   walking away is the cashier's to close.
7. **Pricing is the store's present rate for the order's own amount and
   currency.** The amount and currency are read from the order's record
   (`order_currency_metadata`); its `piconero_per_unit` and `provider` are
   updated with the new rate so the order page shows the rate it now asks
   for. The confirmations it needs stay as it was made with them: the
   amount in the store's currency didn't change, and the threshold is part
   of the order's terms. An order with no record (made before Monokulo kept
   them) can't be renewed: there's no price to re-quote. An XMR order
   renews at the same amount, with a new expiry.
8. **The engine's events carry the order's `expires_at`, and sale events
   take their amount and expiry from the event.** A renewal is announced by
   the status change it makes, `order.pending`, which apps already hear as
   `sale.waiting` and webhooks as `order.pending`. Every engine order event
   now carries `expires_at` beside the `xmr_amount_piconero` it always
   carried, both as they were when the event was written, and a sale event
   (stream and pushes) uses those rather than the copy in `app_orders`: the
   engine commits the renewal before monokulo updates its copy, and the
   order-event subscriber could otherwise read the old amount for the
   renewal's own `sale.waiting`. `app_orders` is updated too, for a stream
   that opens later and for the Live Activity's waiting count. A push's
   time to live runs to the event's expiry. Nothing else is emitted: no new
   event kind, so older apps and plugins see a renewed order as one waiting
   again, which it is.
9. **The engine route is `POST /api/v1/admin/tenant/orders/{order_id}/renew`,
   beside `/cancel`.** Monokulo prices and the engine renews, as for a new
   order. The engine answers `200` with `{ renewed, order_id, address,
   xmr_amount_piconero, expires_at }` (`renewed` false when the quote had
   moved on), `409` with why for a refusal, `503` while the store's scan is
   behind, and `404` for an order that isn't the store's.
