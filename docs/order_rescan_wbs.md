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

**Nothing below is implemented yet. This is a plan for review.**

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

## Decisions (resolved)

All five were open questions in this doc's first draft; the user resolved
each directly before work started. Recorded here for anyone picking this
up later, same convention `docs/fx_refactor.md` already established.

1. **One-shot bounded rescan, not indefinite re-activation.** The rescan
   walks from a computed start height up to the chain tip *as of when it's
   triggered*, then stops — it does not re-enroll the order in ongoing live
   scanning. If the customer's payment still hasn't landed by the time the
   rescan finishes, the merchant triggers it again.
2. **Job/progress state is durable** (a new table, not the existing
   in-memory-only `ScannerStatusMap`, `scanner_status.rs:38`, which is
   per-network operational telemetry, not merchant-facing job state) —
   **and a restart mid-job resumes, it doesn't discard progress.** The
   whole point of persisting `current_height` durably is that a restart
   doesn't throw away real, already-done work — see 1.2/1.3's resume design
   below for exactly how.
3. **One lightweight, real-HTTP-cached endpoint** —
   `GET /api/v1/admin/tenant/rescans`, using genuine `ETag`/`Cache-Control`/
   `If-None-Match` semantics (not a bespoke in-process TTL cache dressed up
   to look like one) — see 2.3/3.1 below. **Broadened while resolving
   this**: the HTTP-cache-aware client becomes control-plane's *default*
   transport for every outbound call (`EngineClient` and
   `CoingeckoRateProvider` both), not hand-wired for this one endpoint —
   see 3.1's own note on why that's safe by construction, and on keeping
   memory bounded.
4. **Guardrails, plus a real two-mode trigger UI.** One rescan job per
   tenant at a time (a second trigger while one is running returns the
   existing job's status, not a new one — real load-safety against a real
   daemon, not just a UX nicety). Triggering offers **simple** (rescan the
   last `X` days) and **advanced** (pick an explicit date range, capped at
   `N` days wide) modes; in both, the earliest reachable date is
   `max(order.created_at, now − N days)` — a rescan can never reach earlier
   than the order's own creation (there's nothing to find before an order
   exists) or further back than the configured cap — and the UI makes that
   bound visible, not just enforced server-side. See 2.1/3.2 below.
5. **The "sync" control only appears on `Expired` orders** — every other
   status is already covered by live scanning, so it would just be
   redundant daemon load there.

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
    (`running`/`completed`/`failed`), `mode` (`simple`/`advanced`, purely
    informational - what actually governs the walk is `from_height`/
    `to_height`, already resolved at trigger time), `from_height`,
    `to_height`, `current_height` (the simple progress metric - "blocks
    scanned / blocks to scan" is just `(current_height - from_height) /
    (to_height - from_height)`), `started_at`, `finished_at`, `updated_at`
    (bumped on every progress write - doubles as 2.3's cache-freshness
    signal)
  - what (restart-survives-and-resumes, decision 2): **no `interrupted`
    state** - a restart is not a terminal outcome. On boot, the engine
    queries for any row still `status = 'running'` and re-spawns 1.3's
    runner for each one, resuming from that row's own `current_height`
    (not `from_height` - the already-scanned prefix is real, durable work
    and must not be redone) up through the **same, original**
    `to_height` (never recomputed against a new "current tip" on resume -
    a fixed target chosen once at trigger time, so a job converges even
    across several restarts rather than chasing a moving tip forever). The
    resumed walk starts scanning *at* `current_height` again, not
    `current_height + 1` - guarantees at-least-once coverage of whatever
    block was mid-flight when the process stopped, relying on the same
    idempotent match-recording primitives (`record_scan_match`,
    `scanner.rs:85`) the live scanner's own block-by-block resumability
    already depends on - no new idempotency logic needed, this is an
    existing property being leaned on, not invented here.
  - what (one-job-per-tenant guardrail, decision 4): a partial unique index
    or an application-level check on `(tenant_id) WHERE status = 'running'`
    - a second trigger while one is active returns the existing row's
    status, not a new job
  - test: real sqlite round-trip tests (same style every other table in
    this codebase already gets); a real "kill the process mid-job, restart
    it, confirm the resumed job picks up from its last `current_height` and
    still reaches the same original `to_height`" test - the one genuinely
    new piece of behavior this whole feature adds, so it earns its own
    dedicated test rather than being asserted incidentally elsewhere
- 1.3 The actual background job runner
  - outcome: a genuinely new spawn shape - `shared::supervise::supervise`
    (used everywhere else in this codebase) is loop-only and wraps a `Fn`
    that never returns; this needs a **bounded**, run-once-to-completion
    task instead, callable both from the trigger endpoint (2.1) and from
    boot-time resume (1.2)
  - what: a plain `tokio::spawn` per running job is probably sufficient
    (not every background task in this codebase needs the same
    infinite-retry supervision the scanner/webhook/reorg loops do) -
    updates 1.2's row as it progresses (say, once every N blocks or every
    few seconds, not every single block - real write load on every block
    would be wasteful for a rescan that might cover tens of thousands of
    blocks), catches a panic and marks the row `failed` rather than
    silently vanishing (a `failed` job does not auto-resume on a later
    restart - `failed` is terminal, the merchant re-triggers deliberately;
    only a row that was genuinely still `running` when the process stopped
    gets picked back up)
  - why this needs its own decision, not reuse: flagged explicitly in the
    research because no bounded-job precedent exists anywhere in this
    codebase today - this is genuinely new shape, budget real design time
    here rather than assuming a quick fit into `supervise`

## 2. Engine: admin API surface

- 2.1 `POST /api/v1/admin/tenant/orders/{payment_id}/rescan` - trigger
  - outcome: request body carries `mode: "simple" | "advanced"` plus, for
    `advanced`, an explicit `from`/`to` (dates or unix timestamps - exact
    wire shape TBD); the handler resolves the real `(from_height,
    to_height)` pair, inserts a 1.2 row, spawns 1.3's runner, returns the
    job's initial state (`202 Accepted` with a small JSON body - job
    already-running is not an error, it's the same response with the
    existing job's current progress)
  - what (resolving the requested window, decision 4): two new
    `PaymentConfig` knobs - `default_rescan_lookback_days` (`X`, simple
    mode's fixed window, measured back from *now*) and
    `max_rescan_lookback_days` (`N`, the hard ceiling both modes share).
    `simple` resolves to `from = max(order.created_at, now − X days)`,
    `to = now`. `advanced` takes the caller's own `from`/`to`, clamped/
    rejected (a real `400`, not silent clamping - the caller asked for
    something invalid, say so) if `from < max(order.created_at, now − N
    days)`, if `to > now`, or if `to < from`. Both modes then run 0.2's
    timestamp→height lookup (minus 1.1's safety margin) to turn the
    resolved `from`/`to` timestamps into `from_height`/`to_height`.
  - what (guardrail, decision 5): reject with a clear `400` if the order
    isn't `Expired`
  - test: real end-to-end for both modes - `simple` trigger, poll to
    completion, assert the previously-invisible late payment is now
    recorded and the order's status reflects it; `advanced` trigger with an
    explicit range that includes the payment, same assertion; `advanced`
    with a `from` before the order's own `created_at` is rejected with a
    clear error, not silently clamped; `advanced` spanning more than `N`
    days is rejected; a second trigger while the first is still running
    returns the same job, not a new one; a non-expired order is rejected
- 2.2 `GET /api/v1/admin/tenant/orders/{payment_id}/rescan` - status/progress
  - outcome: current 1.2 row for this order (or `404` if none was ever
    triggered) - status, percent complete (derived from
    `current_height`/`from_height`/`to_height`), started/finished times
  - test: real poll-during-a-real-rescan test asserting percent complete
    increases monotonically and reaches 100 on completion
- 2.3 `GET /api/v1/admin/tenant/rescans` - the dashboard-wide check
  (decision 3, real HTTP caching)
  - outcome: every currently-`running` rescan for this tenant (in practice
    almost always zero or one, given 1.2's one-job-per-tenant guardrail) -
    lets control-plane's dashboard-home page answer "is anything syncing
    right now" with one call instead of one per listed order
  - what (real HTTP caching, not a bespoke cache): the handler computes a
    cheap `ETag` from 1.2's own state - `"none"` when nothing is running,
    otherwise something like `"{job_id}:{updated_at}"` (a single indexed
    query, no scan) - and sets `Cache-Control: max-age=<a few seconds>`.
    Honors `If-None-Match`: a matching `ETag` gets a bodyless `304 Not
    Modified`, cheap on both ends. This is the same mechanism a browser or
    CDN would use, applied here between control-plane and the engine.
  - test: real test asserting an empty list with nothing running, and the
    real in-progress job while one is active; a real conditional-request
    test (send `If-None-Match` with the current `ETag`, assert `304`; bump
    the job's progress, assert the same `If-None-Match` now gets a fresh
    `200` with a new `ETag`)

## 3. Control-plane: trigger UI and progress display

- 3.1 A shared HTTP-cache-aware client, adopted as control-plane's default
  transport - plus `EngineClient`'s three new calls
  - outcome: a single `reqwest-middleware`-wrapped client (the
    `http-cache-reqwest` crate, layered on plain `reqwest`) becomes the
    transport **every** outbound HTTP call in control-plane goes through -
    `EngineClient` (every method, not just the new ones) and
    `CoingeckoRateProvider` (`shared`, so this crate gains the new
    dependency too) - rather than something hand-wired for one endpoint.
    `EngineClient` also gains `trigger_rescan`, `get_rescan_status`,
    `list_active_rescans` - the first two the same thin-wrapper shape
    `get_order_detail`/`set_confirmations_required`
    (`engine_client.rs:104`/`214`) already establish.
  - what (this is safe as a blanket default, not just for the one new
    endpoint): standards-compliant HTTP caching only ever caches a
    response the server explicitly marked cacheable (`Cache-Control`/
    `ETag`/`Expires`/`Last-Modified`) - a response with none of those
    (every existing engine endpoint today, and real-world confirmation:
    Coingecko's own actual response headers should be checked once this
    phase starts, not assumed) is simply never cached, so switching the
    default transport doesn't silently start caching order/payment status
    or anything else that isn't explicitly marked as OK to cache. Confirm
    the exact cache-manager backend and its default `CacheMode` semantics
    against the crate's own docs for whatever version gets pinned before
    wiring this in - the intent (only cache what's explicitly marked
    cacheable, at RFC 7234's default strictness) needs to match what's
    actually configured, not just assumed from the crate's name.
  - what (memory - the user's own question, answered concretely rather
    than just reassured): back the cache with a **bounded** in-memory
    store (an entry-count and/or total-byte ceiling, not an unbounded
    map) - `http-cache-reqwest`'s pluggable `CacheManager` trait supports
    this (a `moka`-backed manager is the natural fit, `moka` already
    supports both max-entry and weighted-size eviction out of the box).
    In practice the whole cacheable surface here is tiny by construction -
    one rescan-status endpoint keyed by tenant, plus Coingecko's own rate
    lookups keyed by currency (a few dozen at most) - so a modest cap
    (e.g. a few hundred entries, or a low-single-digit-MB weight ceiling)
    would never realistically be approached; it exists as a hard backstop,
    not a limit this feature is expected to bump into.
  - test: same style as every other `EngineClient`/`CoingeckoRateProvider`
    method's own tests (mirrors the existing coverage), plus a real test
    proving a second `list_active_rescans` call within the
    `Cache-Control` window doesn't re-hit the engine at all (a call-count
    assertion against the test daemon/engine, same pattern this session's
    own Coingecko cache tests already used) - and a real test proving an
    ordinary, non-cache-control-bearing engine call (e.g.
    `get_order_detail`) is *not* cached, so the blanket adoption is
    proven safe, not just asserted
- 3.2 Order-detail page: the trigger form and progress display
  - outcome: on `order_detail.html.hbs`, for an `Expired` order with no
    rescan ever triggered, a plain form (decision 5 - only shown for
    `Expired`) offering both modes at once, no JS needed to switch between
    them: a radio choice between **"Rescan from &lt;date&gt;"** (simple -
    the label states the real, already-computed date (`max(order.created_at,
    now − X days)`) outright, not an abstract "last `X` days" the merchant
    has to do the math on themselves) and "Advanced - choose a range", with
    two native `<input type="date" min="..." max="...">` fields for the
    advanced case. The handler computes each field's real `min`
    (`max(order.created_at, now − N days)`, decision 4) and `max` (`today`)
    server-side and renders them as real HTML attributes - the browser
    itself refuses an out-of-range pick, and a line of plain text states
    the same bound in words ("orders can only be rescanned from their own
    creation date (<real date>) or the last `N` days, whichever is later")
    - decision 4's own "make this apparent to them," not just enforced
    silently by the `400` 2.1 already gives an out-of-range submission
    regardless. Once a job exists, the same simple
    server-computed progress bar pattern `checkout.html.hbs` already
    established (a real `progress_percent` computed in the Rust handler,
    not client JS).
  - what: the progress-bar/meta-refresh half of this is a real, direct
    reuse of a pattern this session already built and proved out on the
    checkout page - not a new UI idiom. The two-mode form is new, but
    deliberately still zero-JS (both modes' fields are always present in
    one plain form; the server reads whichever the submitted `mode` radio
    selected and ignores the other's fields), consistent with this
    codebase's general preference for plain HTML forms over client-side
    show/hide.
  - test: real HTTP-level test for each mode - trigger via the form,
    assert the in-progress page shows a real percentage; poll through to
    completion, assert the button/progress UI is gone and (if the rescan
    found the payment) the order's own status/payments table reflects it;
    a real test asserting the rendered date inputs' `min` attribute is the
    real, computed bound (not a hardcoded guess) for both an order younger
    than `N` days and one older than `N` days
- 3.3 Order-detail page: the in-progress indicator, and a tightened
  meta-refresh
  - outcome: while this order's own rescan is `running`, its status area
    (next to the existing status `.tag`) gains a second small badge - e.g.
    `<span class="tag tag-syncing">Syncing 42%</span>` - and the page's
    existing static `<meta http-equiv="refresh" content="15">` becomes
    conditional, a shorter interval (e.g. `5`) while `running`, the normal
    `15` otherwise. Below it, the real progress bar 3.2 already describes
    (server-computed `progress_percent`, same component `checkout.html.hbs`
    established) carries the detail - the badge is the at-a-glance summary,
    the bar is the "how far along, really" answer for someone who scrolls
    to it.
  - what (design - reuses an existing component rather than inventing a
    new one): `.tag`/the accent color are already this site's established
    "something real is happening" language (`_styles.html.hbs`'s own
    `.tag-ok`/`.tag-error` family, and separately the pulsing
    `.status-dot`/`@keyframes status-pulse` the nav's own live health
    indicator already uses) - a new `.tag-syncing` (accent-colored,
    optionally reusing the same pulse animation) stays visually consistent
    with both rather than introducing a third visual language for "in
    progress"
  - test: real test asserting the shorter interval and the badge appear
    exactly when a rescan is genuinely in progress, and neither does
    otherwise
- 3.4 Dashboard-home: awareness of any in-progress rescan
  - outcome: `dashboard_home.html.hbs` also tightens its own meta-refresh
    when 2.3's `list_active_rescans` (called once per store connection, or
    once if 2.3 is made connection-agnostic - confirm shape once 2.3 is
    built) reports anything running for any of this merchant's stores. A
    per-listed-order badge (mirroring 3.3's) would be noisy on a page that
    can list many orders across many stores for what's realistically at
    most one or two active jobs at a time (decision 4's one-per-tenant
    guardrail) - instead, one small banner near the top of the page,
    reusing the same `.tag-syncing` component: "Syncing 1 order for
    possible late payments - <a href="...">pay_abc123 &rarr;</a>" (plural
    phrasing, one link per active job, if a merchant has more than one
    store each mid-rescan) - always says *why* the page is refreshing
    faster than usual, never a silent behavior change, consistent with
    every other auto-refreshing page on this site already stating its own
    interval in plain text
  - test: real test - dashboard page during a real in-progress rescan shows
    the shorter interval and the real banner text/link; two simultaneous
    rescans across two different stores both listed; a plain dashboard
    with nothing running shows neither

## 4. Documentation

- 4.1 `docs/DESIGN.md`: new subsection under Data Model (the `order_rescans`
  table), under HTTP API Surface (the three new admin routes), and under
  Configuration Surface (`default_rescan_lookback_days`,
  `max_rescan_lookback_days`, and whatever 1.1's safety-margin constant
  ends up being, if it becomes configurable rather than fixed)
- 4.2 `work_notes.md`: a real entry once each phase lands, same practice
  every other multi-session piece of work in this repo already gets
