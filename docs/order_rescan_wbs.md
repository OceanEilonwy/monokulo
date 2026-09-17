# Expired-Order Rescan — Work Breakdown Structure

Lets a merchant trigger a one-off, background chain rescan for a *single*
order, starting from around that order's creation time — for the real
support case this exists to solve: a customer says they sent funds to an
order that has since expired, and the normal scanner has stopped watching
that order's subaddress (see "Why this is needed" below), so the payment
was never matched.

This file exists because the change touches the engine's scanner, its
daemon-RPC layer, a genuinely new background-job shape, a new admin
endpoint, and control-plane's dashboard/order-detail pages — read this
before starting any leaf below, and keep `work_notes.md` in sync as each
phase lands, same convention `docs/fx_refactor.md` established.

**Nothing below is implemented yet. This is a plan for review — several
items under "Open questions" need your decision before Phase 1 starts.**

## Why this is needed (confirmed by reading the code, not assumed)

The scanner (`src/scanner.rs`) is a single global, per-network loop with one
"last scanned height" per network (`Store::max_scanned_height`,
`store.rs:1077`). Each tick, it walks every block against every *active*
tenant's full subaddress range — "active" meaning the tenant currently has
at least one order in `Pending`/`Unconfirmed`/`Confirming`/`Partial`
(`Store::active_tenant_ids`, `store.rs:486`). This exclusion is **per
tenant, not per order**: one still-open order keeps a tenant's whole
subaddress range in scope, including an already-expired order's own
address. A subaddress only truly stops being watched once *every* order on
that tenant is terminal (`Paid`/`Overpaid`/`Expired`).

The reorg/double-spend reconciliation pass (`scanner.rs:161-705`) only
re-examines payments *already recorded* against an order
(`find_payments_at_or_after_height`, `store.rs:881`) — it never discovers a
brand-new payment to a cold subaddress. So once a tenant's every order is
terminal, a late payment to any of those orders' addresses is invisible to
every existing code path, forever, with no mechanism to notice it. That's
the real gap this feature closes — not a bug in confirmation counting or
status derivation (`derive_status`, `status.rs:70`, and `record_scan_match`,
`scanner.rs:85`, already handle a late match correctly *once* it's found).

## Open questions (need your decision before Phase 1)

1. **Scope: one-shot bounded rescan, or indefinite re-activation?**
   Recommendation: **one-shot**. The rescan walks from a computed start
   height up to the chain tip *as of when it's triggered*, then stops —
   it does not re-enroll the order in ongoing live scanning. Matches
   "trigger a sync" (a specific, on-demand action) rather than "make this
   order live again," and avoids open-ended questions about when a
   reactivated order should go back to sleep. If the customer's payment
   still hasn't landed by the time the rescan finishes, the merchant can
   trigger it again later.
2. **Where does rescan job/progress state live: a new durable table, or an
   in-memory map (the existing `ScannerStatusMap`,
   `scanner_status.rs:38`, is the only precedent, and it's ephemeral,
   per-network operational telemetry, not merchant-facing job state)?**
   Recommendation: **a new durable table** (survives an engine restart
   mid-job with an honest "interrupted" state; a merchant may check back
   on progress after closing the dashboard). This is more state than the
   engine has ever owned for a background operation, so flagging it
   explicitly rather than assuming.
3. **How does the dashboard cheaply know "is anything rescanning right
   now" across every order, for the "dashboard should also refresh"
   requirement — without an N+1 status check per listed order?**
   Recommendation: one new lightweight endpoint,
   `GET /api/v1/admin/tenant/rescans` (active rescans for this tenant,
   typically zero or one), that control-plane's dashboard-home handler
   calls once per render.
4. **Guardrails: how far back can a merchant reach, and can they run more
   than one rescan at once?**
   Recommendation: **one rescan job per tenant at a time** (a second
   trigger while one is running returns the existing job's status rather
   than starting a duplicate — real load-safety, not just a UX nicety,
   since this walks real historical blocks against a real daemon), and a
   **configurable maximum lookback** (e.g. reject a rescan whose computed
   start height is more than N days back) so this can't be turned into an
   accidental full-chain rescan by an old order.
5. **UI: only offer the button on `Expired` orders, or any order?**
   Recommendation: **`Expired` only** — every other status is already
   covered by live scanning, so a rescan there would just be redundant
   daemon load for no benefit. Matches the stated use case exactly.

Everything below assumes these five land as recommended; each phase notes
where it would change if you decide differently.

---

## 0. Foundations — daemon gains a timestamp→height lookup

- 0.1 Parse the block timestamp the daemon already sends
  - outcome: `BlockHeader` (`daemon_rpc.rs:270`) gains a `timestamp: u64`
    field alongside its existing `hash`
  - what: monerod's real `get_block` response already carries `timestamp`
    (confirmed against the live RPC shape this session already exercises
    for `hash`) — today's code simply doesn't deserialize it. Purely
    additive, no existing caller's behavior changes.
  - test: existing `get_block_hash`/`get_block_transactions` tests extended
    to assert a real, plausible timestamp comes back too
- 0.2 New `MoneroDaemonClient` method: block height for a given timestamp
  - outcome: `find_height_at_or_before(&self, timestamp: u64) -> Result<u64>`
    (exact name TBD) added to the `MoneroDaemonClient` trait
    (`daemon.rs:34`), implemented for `RpcDaemonClient`
  - what: binary search over `[0, tip_height]` using 0.1's timestamp field
    (`get_height()` at `daemon_rpc.rs:374` already gives a correct tip,
    off-by-one handling included — reuse verbatim, don't re-derive it).
    Monero block timestamps are **not** strictly monotonic (a later block
    can carry an earlier declared timestamp than one before it, within
    normal tolerance) — the search must be tolerant of that (a standard
    approach: binary search to a close height, then walk backward by a
    fixed safety margin — see 1.1's own note on this) rather than trust an
    exact hit.
  - what (fallback wrapper): decide whether this goes through
    `daemon_fallback.rs`'s multi-daemon corroboration (used today only for
    `is_key_image_spent_corroborated`) or a single daemon is acceptable for
    a lookup this coarse — a wrong-by-a-few-blocks start height is
    self-correcting (the rescan just does a few extra blocks of work), so
    probably doesn't need corroboration; confirm this reasoning before
    building it either way
  - test: against a real stagenet node (or whatever this codebase's
    existing scanner tests already run against - confirm at start of this
    task, not assumed here), a handful of known-good `(timestamp, expected
    height)` pairs, plus the boundary cases (timestamp before genesis,
    timestamp after the current tip)

## 1. Engine: a bounded, one-order historical rescan primitive

- 1.1 A function that rescans one tenant's one `minor_index` across a
  historical height range
  - outcome: something in `scanner.rs`'s own shape - e.g.
    `rescan_order(store, daemon, tenant_id, minor_index, from_height,
    to_height, on_progress: impl Fn(u64))` - that walks that range block by
    block, feeding every block's transactions through the **existing**
    `scan_transaction`/`record_scan_match` primitives (`scanner.rs:85`) the
    live scanner already uses, narrowed to this one minor_index instead of
    the tenant's whole range
  - what: this is the piece the research confirmed is genuinely reusable,
    not a rewrite - `scan_transaction`/`record_scan_match` are already
    narrow enough to call directly for one order. The safety margin from
    0.2 belongs here: subtract a fixed cushion (e.g. a few hundred blocks,
    or however many correspond to a comfortable multiple of Monero's
    ~2-minute block time relative to real-world timestamp jitter) from the
    computed start height before the walk begins, so a slightly-late
    binary-search hit can never cause a missed payment right at the
    boundary
  - why a new function rather than parameterizing the real scan loop: the
    live loop (`run_scan_tick`, `scanner.rs:869`) is block-driven and
    network-wide by design (one high-water mark, every active tenant, every
    tick) - retrofitting a single-order historical bound onto it would
    complicate the one loop every other order in the system depends on for
    correctness. A separate, narrow function keeps the live scanner
    untouched.
  - test: a real spawned test daemon (or whatever fixture the existing
    scanner tests use - confirm the pattern before building this) with a
    known payment sent to a specific minor_index at a specific historical
    height *after* that order's own `expires_at` - the rescan must find and
    record it, and `derive_status`/`record_scan_match`'s own existing tests
    already prove what happens correctly from there
- 1.2 A durable rescan-job table and its state machine
  - outcome: a new table (e.g. `order_rescans`, migration next in sequence)
    - one row per triggered job: `tenant_id`, `payment_id`, `status`
    (`running`/`completed`/`failed`/`interrupted`), `from_height`,
    `to_height`, `current_height` (the simple progress metric - "blocks
    scanned / blocks to scan" is just `(current_height - from_height) /
    (to_height - from_height)`), `started_at`, `finished_at`
  - what: `interrupted` exists specifically for "the engine restarted
    mid-job" (open question 2's durability requirement) - on boot, any row
    still `running` gets marked `interrupted` rather than silently
    forgotten or silently resumed (resuming automatically on every restart
    is its own decision - simplest correct default is "the merchant
    triggers it again," matching the one-shot framing in open question 1)
  - what (one-job-per-tenant guardrail, open question 4): a partial unique
    index or an application-level check on `(tenant_id) WHERE status =
    'running'` - a second trigger while one is active returns the existing
    row's status, not a new job
  - test: real sqlite round-trip tests (same style every other table in
    this codebase already gets), plus a real "engine restart mid-job marks
    it interrupted" test
- 1.3 The actual background job runner
  - outcome: a genuinely new spawn shape - `shared::supervise::supervise`
    (used everywhere else in this codebase) is loop-only and wraps a `Fn`
    that never returns; this needs a **bounded**, run-once-to-completion
    task instead
  - what: a plain `tokio::spawn` per triggered job is probably sufficient
    (not every background task in this codebase needs the same
    infinite-retry supervision the scanner/webhook/reorg loops do) -
    updates 1.2's row as it progresses (say, once every N blocks or every
    few seconds, not every single block - real write load on every block
    would be wasteful for a rescan that might cover tens of thousands of
    blocks), catches a panic and marks the row `failed` rather than
    silently vanishing
  - why this needs its own decision, not reuse: flagged explicitly in the
    research because no bounded-job precedent exists anywhere in this
    codebase today - this is genuinely new shape, budget real design time
    here rather than assuming a quick fit into `supervise`

## 2. Engine: admin API surface

- 2.1 `POST /api/v1/admin/tenant/orders/{payment_id}/rescan` - trigger
  - outcome: computes `from_height` (0.2's lookup against the order's own
    `created_at`, minus 1.1's safety margin), `to_height` (current tip via
    `get_height()`), inserts a 1.2 row, spawns 1.3's runner, returns the
    job's initial state (`202 Accepted` with a small JSON body - job
    already-running is not an error, it's the same response with the
    existing job's current progress)
  - what (guardrail, open question 4): reject with a clear `400` if the
    order isn't `Expired` (open question 5) or if the computed lookback
    exceeds the configured maximum (open question 4's second half - a new
    `PaymentConfig` knob, e.g. `max_rescan_lookback_days`)
  - test: real end-to-end - trigger, poll to completion, assert the
    previously-invisible late payment is now recorded and the order's
    status reflects it; a second trigger while the first is still running
    returns the same job, not a new one; a non-expired order is rejected;
    an over-the-lookback-limit order is rejected
- 2.2 `GET /api/v1/admin/tenant/orders/{payment_id}/rescan` - status/progress
  - outcome: current 1.2 row for this order (or `404` if none was ever
    triggered) - status, percent complete (derived from
    `current_height`/`from_height`/`to_height`), started/finished times
  - test: real poll-during-a-real-rescan test asserting percent complete
    increases monotonically and reaches 100 on completion
- 2.3 `GET /api/v1/admin/tenant/rescans` - the dashboard-wide check (open
  question 3)
  - outcome: every currently-`running` rescan for this tenant (in practice
    almost always zero or one, given 1.2's one-job-per-tenant guardrail) -
    lets control-plane's dashboard-home page answer "is anything syncing
    right now" with one call instead of one per listed order
  - test: real test asserting an empty list with nothing running, and the
    real in-progress job while one is active

## 3. Control-plane: trigger UI and progress display

- 3.1 `EngineClient` gains the three new calls
  - outcome: `trigger_rescan`, `get_rescan_status`, `list_active_rescans` -
    same thin-wrapper shape `get_order_detail`/`set_confirmations_required`
    (`engine_client.rs:104`/`214`) already establish: build the URL,
    `bearer_auth(sk)`, send, parse
  - test: same style as every other `EngineClient` method's own tests
    (mirrors the existing coverage for `get_order_detail`)
- 3.2 Order-detail page: the trigger button and progress display
  - outcome: on `order_detail.html.hbs`, for an `Expired` order with no
    rescan ever triggered, a plain form button ("Sync from order creation
    date", confirmed per open question 5's recommendation - only shown for
    `Expired`). Once one exists, the same simple server-computed progress
    bar pattern `checkout.html.hbs` already established (a real
    `progress_percent` computed in the Rust handler, not client JS - this
    page already uses a `<meta http-equiv="refresh">`, not JavaScript, for
    its own "stay current" behavior, so this follows the same convention
    rather than introducing a new one)
  - what: this is a real, direct reuse of a pattern this session already
    built and proved out (the checkout page's progress bar / meta-refresh
    approach) - not a new UI idiom
  - test: real HTTP-level test - trigger via the form, assert the
    in-progress page shows a real percentage; poll through to completion,
    assert the button/progress UI is gone and (if the rescan found the
    payment) the order's own status/payments table reflects it
- 3.3 Tightened meta-refresh while a rescan is active
  - outcome: `order_detail.html.hbs`'s existing static `content="15"`
    becomes conditional - a shorter interval (e.g. `5`) while this order's
    own rescan is `running`, the normal `15` otherwise
  - what: the handler already loads this order's own detail via the engine
    - 3.1's `get_rescan_status` is one more call on the same request,
    already-paid-for round trip
  - test: real test asserting the shorter interval appears exactly when a
    rescan is genuinely in progress, and the normal one otherwise
- 3.4 Dashboard-home: awareness of any in-progress rescan
  - outcome: `dashboard_home.html.hbs` also tightens its own meta-refresh
    when 2.3's `list_active_rescans` (called once per store connection, or
    once if 2.3 is made connection-agnostic - confirm shape once 2.3 is
    built) reports anything running for any of this merchant's stores,
    plus a small "syncing" indicator so a merchant landing on the plain
    dashboard (not the specific order page) can tell something is
    happening
  - test: real test - dashboard page during a real in-progress rescan shows
    the shorter interval and the indicator; a plain dashboard with nothing
    running doesn't

## 4. Documentation

- 4.1 `docs/DESIGN.md`: new subsection under Data Model (the `order_rescans`
  table), under HTTP API Surface (the three new admin routes), and under
  Configuration Surface (`max_rescan_lookback_days` and whatever 1.1's
  safety-margin constant ends up being, if it becomes configurable rather
  than fixed)
- 4.2 `work_notes.md`: a real entry once each phase lands, same practice
  every other multi-session piece of work in this repo already gets
