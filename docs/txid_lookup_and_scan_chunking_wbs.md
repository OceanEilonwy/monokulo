# Replace manual chain rescan with direct txid lookup; dynamic chunk sizing for live-scanner catch-up

Work breakdown for the user's decision, replacing `docs/rescan_batching_proposal.md`'s
two options with a chosen direction:

1. Implement memory-budget-based, EWMA-adaptive chunk sizing (as proposed), but
   retarget it at the **live scanner's own catch-up walk** rather than the
   rescan feature - see "why the target moved" below.
2. **Remove the manual chain-rescan feature entirely** (`docs/order_rescan_wbs.md`,
   phases 0-3 and the rescan-specific parts of phase 5) and replace it with a
   direct **look up one payment by txid** action: O(1) daemon calls (locate +
   fetch one transaction), no block-range walk, no background job, no
   abuse/scaling surface at all - the user's own stated reasoning: letting
   merchants trigger repeated full-range chain scans doesn't scale the way a
   single targeted lookup does.

Same convention `docs/order_rescan_wbs.md` and `docs/rescan_batching_proposal.md`
already established: **nothing below is implemented yet - this is the plan for
review before the sequenced implementation begins.**

## Why the target moved (chunk sizing no longer serves rescan)

`MoneroDaemonClient::get_blocks_range` (`daemon.rs:39-61`, real override
`daemon_rpc.rs:614-` via `get_blocks.bin`) has exactly one caller today:
`rescan_order` (`scanner.rs:870`) - confirmed by inventory, not assumed. If
rescan is removed and this method has no other caller, the entire batching
mechanism just built becomes dead code. It has a real second home, though: the
**live scanner's own per-tick catch-up walk** (`run_scan_tick`'s block loop)
still fetches one block at a time even after a long time offline - if the
process is down for a day, the very next tick faces `scan_from..=scan_to`
spanning ~720 blocks, each still costing a separate `get_block`+
`get_transactions` round trip today. This is a real, still-standing
scalability concern, independent of - and arguably more important than -
rescan ever was (it's on the core live-detection path, not a
merchant-triggerable action). Retargeting the already-built batching
mechanism here means this work keeps its value instead of being deleted
alongside rescan.

**Scope limit, deliberate**: `get_blocks_range` only ever returns
transactions, never block hashes - and the live tick's own loop also needs
each block's hash (`get_block_hash`) to persist `scanned_blocks` for reorg
tracking. Monero's real block-hash computation is a real cryptographic
operation with its own encoding rules; `monero` 0.22.0 (the crate this whole
codebase already decodes everything with) doesn't implement it (confirmed -
no `Hashable` impl on `Block`/`BlockHeader` in that crate), and hand-rolling
it here would be a real, unjustified new risk for a problem this proposal
doesn't need to solve. **This phase batches transaction-fetching only** -
`get_block_hash` stays one call per height, unbatched, exactly as today. On a
long catch-up range this still cuts round trips roughly in half (from ~2N to
~N+1: N unbatched hash calls plus a small number of batched tx-chunk calls
instead of N separate `get_block`+`get_transactions` pairs) without touching
block-hashing at all.

---

## Part A - dynamic, memory-budget-based chunk sizing (live-scanner catch-up)

Same mechanism `docs/rescan_batching_proposal.md`'s Part 1 already designed,
retargeted:

### A.1 New setting

`payment.scan_chunk_memory_budget_mb` (`u32`) - deliberately renamed from any
"rescan_*" name proposed earlier, since this now governs the live scanner, not
a merchant-facing rescan action. Same `scalar_settings!` macro entry every
other knob uses (`settings.rs:95-121`'s pattern), e.g.:

```rust
PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB => { key: "payment.scan_chunk_memory_budget_mb", env: "SCANNER_PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB", default: "8" },
```

Appearing in the admin settings page requires **no monokulo UI code at all** -
confirmed by reading `instance_admin.rs:135-143`: `GET /api/v1/admin/settings`
already iterates `settings::ALL_SCALAR` generically, and monokulo's own
`admin_settings_page` view renders whatever `scanner_fields` that response
carries (confirmed via `admin_settings_page_shows_scanner_fields_and_networks_
when_reachable`'s own test, `crates/monokulo/src/views/admin.rs:405-432`) -
adding the setting to the macro list is the entire "add this as a setting to
the admin page" task.

- test: `settings::tests::all_knobs_load_with_defaults` (`settings.rs:158-161`'s
  neighborhood) gains this knob, same pattern as its siblings.

### A.2 EWMA-based chunk sizing, applied to `run_scan_tick`'s block loop

`run_scan_tick`'s `'heights: for height in scan_from..=scan_to` loop
(`scanner.rs`, the block referenced in `rescan_order`'s own tip-side doc
comment) restructured to walk in chunks:

- Maintain `avg_bytes_per_block: f64` as a local across the chunk loop
  (per-tick, not persisted - a catch-up range is walked within one tick's own
  call to `run_scan_tick`, so there's no cross-tick state to carry, unlike a
  multi-hour rescan job this mechanism no longer needs to survive a restart
  for).
- Each chunk: `chunk_size = (budget_bytes / avg_bytes_per_block).clamp(SCAN_CHUNK_MIN_BLOCKS, SCAN_CHUNK_MAX_BLOCKS).min(remaining)`,
  fetch via `daemon.get_blocks_range(height, chunk_size)`, then for each
  returned height still call `daemon.get_block_hash(height)` individually (see
  "scope limit" above) and record matches exactly as today.
- Update `avg_bytes_per_block` after each chunk from the chunk's own
  transactions' `monero::consensus::encode::serialize(tx).len()` sum ÷ block
  count - identical formula `rescan_batching_proposal.md` already specified,
  just now a live-tick-scoped local instead of a rescan-job-scoped one.
- Cold start: a fixed conservative constant (`SCAN_CHUNK_INITIAL_AVG_BYTES`),
  same reasoning as the proposal's own cold-start section.
- `SCAN_CHUNK_MIN_BLOCKS`/`SCAN_CHUNK_MAX_BLOCKS`/`SCAN_CHUNK_EWMA_ALPHA`/
  `SCAN_CHUNK_INITIAL_AVG_BYTES`: new constants alongside `run_scan_tick`,
  same "needs a real, deliberately-picked value, not a guess" treatment
  `RESCAN_START_HEIGHT_CUSHION_BLOCKS` originally got - **open question,
  not decided in this WBS**, see below.
- Ordinary tick behavior (the overwhelmingly common case: `scan_from..
  scan_to` spans 1-2 blocks) is essentially unaffected - one small chunk,
  computed the same way, no meaningfully different behavior from today for
  the steady state this mechanism isn't really for.

- test: a real test proving a wide catch-up range (many blocks queued up after
  simulated downtime) issues fewer `get_blocks_range` calls than blocks in the
  range (a call-counting spy, same style `run_scan_tick_never_calls_key_
  custody_for_a_tenant_with_no_pending_orders` already uses); a real test
  proving the chunk size shrinks when fed large blocks and grows when fed
  small ones within the same tick (a scripted fake with varying tx sizes per
  block); a real test proving `get_block_hash` is still called exactly once
  per height regardless of chunking (the scope-limit guarantee, worth its own
  explicit assertion given it's the one deliberately-not-batched piece).

---

## Part B - a direct "look up a payment by its transaction ID" action

### B.1 New daemon capability: fetch one transaction by hash

`MoneroDaemonClient` gains `async fn get_transaction(&self, txid: &str) ->
Result<Transaction, DaemonError>` (`daemon.rs`, alongside `locate_transaction`)
- no default, every implementor defines it, same treatment
`get_block_transactions` already gets (this is core, not an optional
convenience).

- `RpcDaemonClient`: reuses the existing private `fetch_transactions` helper
  (`daemon_rpc.rs:127-135`, already used by `get_block_transactions`) with a
  single-element hash list - genuinely no new RPC-format work, this endpoint
  is already fully proven in this codebase.
- `FallbackDaemonClient`: same per-node failover loop every other method
  already has (`daemon_fallback.rs`'s established pattern).
- Every test double (`daemon.rs`'s `fake` module, `scanner-test-support`'s
  `FakeDaemonClient`, `NoopDaemonClient`) gets a real implementation - scripted
  from the same fixture data `locate_transaction`'s fake already tracks, so a
  test can script "this txid exists, here's its `Transaction`" directly.

- test: `RpcDaemonClient::get_transaction` against the existing hermetic
  fixture (`daemon_rpc.rs`'s own `FIXTURE_TX_HEX`); a live `#[ignore]`d test
  fetching a known real mainnet txid and asserting it matches
  `get_block_transactions`'s own decoding of the same transaction from its
  block (parity, same spirit as the `get_blocks_range` live parity test just
  added).

### B.2 New engine endpoint

`POST /api/v1/admin/tenant/payments/lookup` (naming open - see below), body
`{ "txid": "<64 hex chars>" }`, handler in `http/admin.rs` (or a new small
module if that file is getting large after the rescan removal - a real call
to make once the removal's diff is visible):

1. Validate `txid` is 64 hex characters - `400` otherwise, same
   fail-fast-with-a-clear-error style `trigger_rescan`'s own validation used.
2. `daemon.locate_transaction(txid)` (`state.daemons`, the live scanner's own
   pool - see B.4 for why this doesn't need its own separate pool the way
   rescan did).
3. `TxLocation::NotFound` → `200` with `{ "outcome": "not_found_on_chain" }` -
   an ordinary, expected result, not an error (same "guardrail states are
   real responses, not just errors" precedent `AlreadyRunning` already set for
   the feature this replaces).
4. `InPool` or `InBlock(height)` → `daemon.get_transaction(txid)` for the real
   transaction, then `resolve_wallet_handle` (`http/mod.rs:307-322`, already
   reused everywhere) and `scan_transaction_for_tenant` (`scanner.rs:133-145`,
   **already exists, already exactly this shape** - reused unmodified) against
   `0..tenant.next_minor_index` - the tenant's whole address range, not one
   specific order's `minor_index`. This is the one real, deliberate design
   difference from the old rescan (which was always triggered from, and
   scoped to, one specific already-`Expired` order): **this lookup is
   tenant-wide, not order-scoped** - a merchant enters a txid without first
   having to guess which order it belongs to, matching how a customer
   actually reports a problem ("I paid, here's my txid," not "I paid order
   X"). No `Expired`-only restriction either - a real match is a real match
   regardless of the order's current status.
5. Empty touched set → `200` with `{ "outcome": "no_matching_order" }` (found
   on-chain, doesn't pay any of this tenant's known subaddresses).
6. Non-empty touched set → `recompute_and_notify` per touched order
   (`scanner.rs:502-517`, already exists, already reused everywhere) → `200`
   with `{ "outcome": "matched", "order_ids": [...] }` (a list, not a single
   id - a transaction can in principle pay more than one of a tenant's
   subaddresses; correctness over the common case's simplicity here).
7. Already-recorded case needs no special handling - `record_scan_match`'s
   existing idempotency (`UNIQUE(order_id, txid, output_index)`) already makes
   a second lookup of an already-applied txid a safe no-op that still reports
   `"matched"` with the same `order_ids` truthfully.

No new rate-limiting infrastructure - this sits under the existing
`admin_rate_limiter` middleware every other `/api/v1/admin/tenant/*` route
already has, and it requires the tenant's own secret token (not a public
surface) the same way `trigger_rescan` did.

- test: found-and-matched (order recomputes, webhook enqueued - same
  end-to-end assertion style `run_scan_tick_matches_mempool_tx_recomputes_
  status_and_enqueues_a_webhook` already uses); found-but-unrelated-tx
  (no match, no side effect); not-found-on-chain; a mempool-only match
  (`InPool`, no block height, matches the same "recorded at zero
  confirmations" treatment the live scanner's own mempool path already gets);
  malformed txid rejected with `400`; a second lookup of an already-recorded
  txid is a safe no-op that still reports success.

### B.3 Control-plane: `EngineClient` method + UI

- `EngineClient::lookup_payment(txid: &str) -> Result<PaymentLookupOutcome, EngineClientError>`,
  same thin-wrapper shape `get_order_detail`/`trigger_rescan` already
  established (`engine_client.rs:148-165`'s neighborhood, replacing it).
- UI home: **not** the order-detail page (this lookup doesn't start from a
  known order) - a small, plain-HTML-form tool, zero JS (same standing
  preference this codebase already states explicitly for the form it's
  replacing - "consistent with this codebase's general preference for plain
  HTML forms over client-side show/hide"). Natural home: the orders-list page
  header, or a small dedicated `/dashboard/payments/lookup` page linked from
  it - exact placement is a real UI call worth making with the actual page
  layout in front of you, not asserted here. On submit: render the outcome
  inline (found → a link to the now-updated order; not found → a plain,
  honest message) - server-computed, same convention every other page in this
  codebase already follows.

- test: submitting a real txid that matches shows the order link and its
  updated status; a txid that doesn't match shows the plain "not found"
  message; a malformed txid re-shows the form with a clear error - mirroring
  the removed feature's own test shapes
  (`triggering_a_rescan_against_a_non_expired_order_reshows_the_form_with_the_
  engines_real_error`'s pattern) for the new one.

### B.4 Why this reuses `daemons`, not a dedicated pool

The contention bug `AppState::rescan_daemons` exists to prevent
(`http/mod.rs:108-119`) was about **sustained, high-volume** request traffic -
a rescan issuing thousands of sequential block fetches competing with the live
scanner's own latency-sensitive calls for the same connection pool. A txid
lookup is two quick calls (`locate_transaction`, `get_transaction`), not a
bulk walk - it poses no meaningful version of that contention risk regardless
of how many merchants use it concurrently (each one costs about as much as one
ordinary live-tick request already does). **`rescan_daemons` is removed
entirely** (see Part C) and this new endpoint simply uses `state.daemons`,
the same pool the live scanner itself uses - correct because the workload
shape that motivated the split is exactly what's being removed.

---

## Part C - remove the manual rescan feature

Exhaustive removal checklist (file:line references from a full-repo inventory
pass, not assumed):

### C.1 Database

- New migration (next in sequence after whatever's current) that `DROP TABLE
  order_rescans` (created `migrations/0007_order_rescans.sql`) - **never edit
  a shipped migration**, add a new one that undoes it, same rule this
  codebase already follows for every other schema change.
- `orders.first_scanned_height`/`last_scanned_height`
  (`migrations/0008_order_scanned_range.sql`) - **open question**: keep
  (still meaningful from live scanning alone - "has this order's address ever
  been checked, how recently") or drop for maximal simplification. Leaning
  keep (cheap, harmless, still real information without rescan; only the
  *rescan-side* writer to these columns goes away, the live-scanning writer
  stays exactly as it is today) - not asserted as final here.

### C.2 Engine: store layer (`store.rs`)

Remove: `OrderRescan`, `RescanMode`, `RescanStatus`, `TriggerRescanOutcome`,
`NewOrderRescan` (structs/enums); `row_to_rescan`, `rescan_mode_from_str`,
`rescan_status_from_str` (parsing helpers); `get_rescan`,
`get_running_rescan_for_tenant`, `get_latest_rescan_for_order`,
`list_running_rescans`, `trigger_rescan`, `update_rescan_progress`,
`complete_rescan`, `fail_rescan`, `bump_scanned_range_for_order` (public
methods) - **if** C.1 keeps the two columns, `bump_scanned_range_for_order`'s
sibling `bump_scanned_heights_for_tenant` (the live-scanning writer) stays,
only the rescan-specific one goes. `is_order_currently_scanning` simplifies to
drop its rescan-`OR` clause (just the existing in-scope predicate) rather than
being removed outright - it likely still backs `OrderView`'s
`currently_scanning` field, which remains meaningful for live scanning alone.
Remove all 8 rescan-named unit tests in this file.

### C.3 Engine: scanner core (`scanner.rs`)

Remove: `RESCAN_START_HEIGHT_CUSHION_BLOCKS`, `RESCAN_PROGRESS_PERSIST_
INTERVAL_BLOCKS`, `RESCAN_STEP_MAX_ATTEMPTS`, `RESCAN_STEP_RETRY_DELAY`,
`RESCAN_CHUNK_BLOCKS` (superseded by Part A's own constants anyway);
`rescan_start_height`, `rescan_order`, `run_rescan_job`, `spawn_rescan_job`,
`retry_rescan_step`. Remove all 11 rescan-named test functions. **Keep**
`scan_transaction_for_tenant` and `recompute_and_notify` unmodified - both are
reused by Part B, not rescan-specific in the first place.

### C.4 Engine: daemon layer

`get_blocks_range` (`daemon.rs:39-61`, `daemon_rpc.rs`'s real override,
`daemon_fallback.rs`'s failover override) **stays** - repurposed by Part A,
not removed. Its two rescan-specific-in-name live tests
(`real_node_get_blocks_range_matches_get_block_transactions_for_the_same_
range`, `real_node_get_blocks_range_handles_the_start_height_zero_special_
case`) stay too (they test the mechanism itself, not rescan) - maybe renamed
if their names read as rescan-specific after this change, a small polish
item.

### C.5 Engine: HTTP layer

Remove routes `GET /api/v1/admin/tenant/rescans`, `POST`/`GET
/api/v1/admin/tenant/orders/{payment_id}/rescan` (`http/mod.rs`); handlers
`trigger_rescan`, `get_rescan_status`, `list_rescans`; types
`RescanStatusView`, `TriggerRescanRequest`; helpers `resolve_rescan_window`,
`build_rescan_status_view`, `rescan_percent_complete`, `rescan_list_etag`,
`with_rescan_cache_headers` (all in `admin.rs`). Add route
`POST /api/v1/admin/tenant/payments/lookup` (Part B.2) in its place. Remove
all rescan-named tests in `http/tests.rs` (13 functions plus the
`rescan_test_app_state`/`trigger_rescan_request` helpers); add the new
lookup endpoint's own tests (Part B.2's test list) in their place.

### C.6 Engine: `main.rs` / `AppState`

Remove `resume_running_rescans` entirely (its whole reason to exist - resuming
a durable background job across a restart - has no equivalent in an O(1),
synchronous lookup). Remove the second `build_daemon_clients` call and
`rescan_daemons` construction - back to the single `daemons` map `main.rs` had
before the earlier contention fix, since B.4 established the new feature
doesn't need the split. Remove `AppState::rescan_daemons`,
`default_rescan_lookback_days`, `max_rescan_lookback_days` fields
(`http/mod.rs:108-136`) and their settings reads in `main.rs`.

### C.7 Engine: settings (`settings.rs`)

Remove `PAYMENT_DEFAULT_RESCAN_LOOKBACK_DAYS`, `PAYMENT_MAX_RESCAN_LOOKBACK_
DAYS`. **Keep** `PAYMENT_REORG_CHECK_DEPTH` and `PAYMENT_EXPIRED_ORDER_GRACE_
PERIOD_MINUTES` - the grace-period widening (`docs/order_rescan_wbs.md` phase
4) is an independent, automatic, no-merchant-action feature that was always
explicitly "fully independent of phases 0-3" in its own original design and
is not part of what's being replaced here. Add
`PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB` (Part A.1).

### C.8 Control-plane (monokulo)

- `settings.rs`: remove `RESCAN_DEFAULT_LOOKBACK_DAYS`, `RESCAN_MAX_LOOKBACK_
  DAYS` (`settings.rs:49-50`) and their tests.
- `engine_client.rs`: remove `trigger_rescan`, `get_rescan_status`,
  `list_active_rescans`, `TriggerRescanRequest`, `RescanStatusView`, and their
  4 tests; add `lookup_payment` (Part B.3) and its tests.
- `http/orders.rs`: remove `rescan_lookback_days`, `trigger_rescan` handler,
  `build_rescan_section`, `TriggerRescanForm`, `seed_running_rescan` test
  helper, and the 8 rescan-named order-detail/dashboard tests; adjust
  `order_detail_page_body` to drop the rescan section it currently builds.
  Add the new lookup handler/tests (Part B.3).
- `views/orders.rs`: remove `OrderRescanSectionViewModel`,
  `RescanTriggerFormViewModel`, `RescanProgressViewModel`,
  `OrderDetailViewModel.rescan`/`.rescan_error`; `.meta_refresh`'s
  rescan-driven 5s/15s branch collapses to always-15s (nothing left to poll
  faster for) unless something else already varies it.
- `views/dashboard.rs`: remove `DashboardRescanRow`,
  `DashboardViewModel.active_rescans`/`.active_rescans_count_label`, the
  syncing-banner template block, and its own test.
- `scanner-test-support/src/lib.rs`: remove `TestEngineConfig.
  admin_rescan_daemon`/`with_admin_rescan_daemon`; the `spawn()` `AppState`
  literal's `rescan_daemons` field goes with `AppState` itself losing that
  field (C.6); `default_rescan_lookback_days`/`max_rescan_lookback_days`
  fields go too.
- `tests/e2e_stagenet.rs`, `tests/e2e_dashboard_stagenet.rs`: drop their now
  gone `rescan_daemons`/lookback-day `AppState` fields (both only ever set
  them to satisfy the struct's shape, per the inventory - not exercised).

### C.9 Documentation

- `docs/DESIGN.md`: remove §7.8 ("Merchant-triggered order rescan"), §8.3
  (`order_rescans` DDL), the three rescan HTTP-API table rows, the two
  rescan-lookback config entries. Add a new short section for the txid-lookup
  endpoint and its one new setting, matching the level of detail neighboring
  sections already use. **Keep** `CONTROL_PLANE_HTTP_CACHE_MAX_MB`/the
  HTTP-cache-aware transport documentation - that became monokulo's general
  default transport (used by `EngineClient`'s other methods and
  `CoingeckoRateProvider` too), not rescan-specific infrastructure, even
  though rescan's own `list_active_rescans` was its original motivating case.
- `docs/order_rescan_wbs.md`: **not deleted** - add a short header note
  ("superseded - removed on `<date>`, replaced by direct txid lookup; see
  `docs/txid_lookup_and_scan_chunking_wbs.md`") and leave the rest as the real
  historical record it is, same treatment `work_notes.md` already gives
  finished, superseded work elsewhere.
- `work_notes.md`: a real entry once this lands, same convention every other
  multi-session change here already gets.

---

## Open questions (need a decision before implementation starts)

1. `PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB`'s default, and
   `SCAN_CHUNK_MIN_BLOCKS`/`SCAN_CHUNK_MAX_BLOCKS`/`SCAN_CHUNK_EWMA_ALPHA`/
   `SCAN_CHUNK_INITIAL_AVG_BYTES` - same "needs a real, deliberately-chosen
   value" treatment as `docs/rescan_batching_proposal.md`'s own open
   questions, now scoped to the live scanner instead of a rescan job.
2. Keep or drop `orders.first_scanned_height`/`last_scanned_height` (C.1) -
   this WBS leans "keep," not decided.
3. The new endpoint's exact route/JSON shape (`POST /api/v1/admin/tenant/
   payments/lookup` proposed, not final) and the UI's exact placement (B.3) -
   both real product/API-shape decisions worth a quick look before Part B
   lands, not blocking Part A or C.
4. Whether `is_order_currently_scanning`/`OrderView.currently_scanning`
   (C.2) still earns its keep once it only ever reflects live-scan-in-scope,
   never a running background job - it becomes a much less interesting
   boolean once "currently scanning" can never mean "and also somebody
   triggered a manual rescan on it."

## Suggested execution order

Independent enough to land as three separate, sequential changes (matching
how the original rescan WBS itself landed phase by phase):

1. **Part A** (chunk sizing) - self-contained, no removal risk, immediately
   valuable, easiest to get wrong in isolation and verify in isolation.
2. **Part B** (txid lookup, new) - additive, ships without touching rescan at
   all, so it can be verified working end-to-end (including a real merchant
   click-through) before anything old is torn out.
3. **Part C** (remove rescan) - only once B is confirmed to actually cover the
   real use case, since C is the hard-to-reverse half (dropping a table,
   deleting UI/tests/routes) and should be the last step, not the first.
