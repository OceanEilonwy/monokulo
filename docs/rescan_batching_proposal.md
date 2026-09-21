# Rescan chunk sizing + cross-job block caching — proposal

**Nothing below is implemented yet. This is a proposal for review, written so it
can be turned into a WBS the same way `docs/order_rescan_wbs.md` started as a
plan before its own phases 0-6 landed** (see that file's own header and
`work_notes.md` for the convention this follows).

Follow-up to the two changes already shipped on this branch:
1. `AppState::rescan_daemons` — a separate daemon-client connection pool for
   rescans, so a rescan's own request volume can't starve the live scanner's
   latency against the same node (real, measured ~18x slowdown before the fix).
2. `MoneroDaemonClient::get_blocks_range` + `RpcDaemonClient`'s real
   `get_blocks.bin` override — batches a rescan's historical block walk into
   `RESCAN_CHUNK_BLOCKS = 100`-block chunks (`scanner.rs:808`), one HTTP round
   trip per chunk instead of one per block.

This proposal covers two further questions raised after that: (1) `100` is a
fixed guess, not sized against anything real — should chunk size instead track
a memory/bandwidth budget, adapting to real block sizes the way Monero's own
long-term block weight tracking adapts limits to real network conditions? (2)
what actually happens today when multiple tenants rescan at the same time, and
is there a real opportunity to stop re-downloading the same blocks?

---

## Part 0 — current state, confirmed by reading the code

### Chunk sizing today

`rescan_order` (`scanner.rs:840-892`) walks `from_height..=to_height` in fixed
`RESCAN_CHUNK_BLOCKS` (100)-block chunks, computing `chunk_size =
remaining.min(RESCAN_CHUNK_BLOCKS)` each iteration and calling
`daemon.get_blocks_range(height, chunk_size)`. `100` was chosen as "a
conservative middle ground" (its own doc comment) - never measured against a
real byte budget. Real Monero blocks vary enormously: a near-empty block is a
few hundred bytes: a block full of large RingCT transactions under load can be
several hundred KB. A fixed block-count chunk therefore has no fixed *memory or
bandwidth* cost - the live comparison test added with the batching change
(`daemon_rpc.rs`'s `real_node_get_blocks_range_matches_get_block_transactions_
for_the_same_range`) happened to pull back a ~360KB response for 3 blocks
around a busy real mainnet block; 100 blocks of that density would be ~12MB in
one response, while 100 blocks of near-empty ones would be a few KB.

### Concurrency today

- **Per tenant**: `order_rescans_one_running_per_tenant`
  (`migrations/0007_order_rescans.sql:35`), a partial unique index on
  `tenant_id WHERE status = 'running'`. A second trigger for the same tenant
  returns the existing job's status rather than starting a new one
  (`TriggerRescanOutcome::AlreadyRunning`, `http/admin.rs`). This is a real,
  enforced guardrail.
- **Across tenants**: **no limit at all.** Every trigger (`http/admin.rs`'s
  `trigger_rescan`) and every boot-time resume (`main.rs`'s
  `resume_running_rescans`) calls `scanner::spawn_rescan_job`
  (`scanner.rs:1008`), which is a bare `tokio::spawn` (wrapped once more for
  panic-catching) with no semaphore, no queue, no cap. If ten different
  tenants each have an expired order and ten merchants happen to trigger a
  rescan within the same minute, ten `rescan_order` calls run **fully
  concurrently** - real parallel tasks on tokio's multi-thread runtime, all
  sharing the one `AppState::rescan_daemons` connection pool per network
  (deliberately separated from the live scanner's own `daemons`, but *not*
  separated from each other). This is the exact same contention class as the
  bug just fixed (§`AppState::rescan_daemons`'s own doc comment) - just
  rescan-vs-rescan instead of rescan-vs-live-scanner, and currently
  unaddressed.
- **Block sharing**: none. Two rescan jobs covering overlapping historical
  ranges (plausible - "last week's late payments" is a common shape, so
  several tenants' rescans converging on a similar recent window isn't a
  contrived scenario) each independently re-fetch and re-decode the exact same
  blocks from the exact same node. No cache, in-memory or on-disk, exists
  anywhere in this codebase for block data. The one real cache that does exist
  (`shared::http_cache`, `moka`-backed, WBS Phase 3.1 of
  `order_rescan_wbs.md`) is real *HTTP* caching keyed by URL - it cannot apply
  here regardless, since `get_blocks.bin` is a `POST` whose meaningful key is
  its *body* (`start_height`/`max_block_count`), not its URL; standard HTTP
  caching semantics have nothing to say about that. Any block cache here has
  to be a bespoke, application-level one.

---

## Part 1 — dynamic, memory-budget-based chunk sizing

### Goal

Replace the fixed `RESCAN_CHUNK_BLOCKS` constant with a chunk size computed so
that the *response* for one `get_blocks.bin` call stays close to a configured
memory budget, adapting to whatever block sizes this rescan is actually
walking through - fewer, bigger-block chunks on a busy stretch of chain, more,
smaller-block chunks on a quiet one - the same spirit as Monero's own
long-term block weight tracking adapting limits to real observed network
conditions, deliberately simplified for what this actually needs (see "why not
literally Monero's algorithm" below).

### Where the size signal comes from

`get_blocks.bin`'s response byte size isn't known until after the request -
classic chicken-and-egg. Two components close that loop:

1. **A byte-size proxy that doesn't require touching `MoneroDaemonClient` at
   all**: `rescan_order` already receives fully-decoded `Vec<Vec<Transaction>>`
   per chunk. Re-serializing each `Transaction` with the same
   `monero::consensus::encode` this codebase already uses everywhere
   (`monero::consensus::encode::serialize`, the mirror of the `deserialize`
   already used in `daemon_rpc.rs`) gives back its consensus-encoded byte
   length - a close proxy for the real wire size (the real response adds a
   fixed, small epee-framing overhead per field, negligible next to real
   transaction payloads). This keeps the estimator entirely inside the
   already-well-tested, non-RPC-specific `rescan_order` function - no trait
   change, no plumbing raw response bytes up through `MoneroDaemonClient`,
   `FallbackDaemonClient`, and every test double. `get_blocks_range`'s
   signature is untouched by this proposal.
2. **An exponentially-weighted moving average (EWMA) of bytes-per-block**,
   maintained as a plain local (`let mut avg_bytes_per_block: f64`) inside
   `rescan_order`'s own loop - **not** persisted anywhere, **not** shared
   across jobs (see "why per-job, not global" below). After each chunk:
   ```
   let chunk_bytes: usize = chunk.iter().flatten()
       .map(|tx| monero::consensus::encode::serialize(tx).len())
       .sum();
   let observed_avg = chunk_bytes as f64 / chunk.len() as f64;
   avg_bytes_per_block = ewma_alpha * observed_avg
       + (1.0 - ewma_alpha) * avg_bytes_per_block;
   ```
   `ewma_alpha` (a fixed constant, e.g. `0.3`) controls how fast the estimate
   reacts to a real shift in block size versus how much a single anomalous
   block (one giant consolidation tx, or a run of empty blocks) can swing it.
   This needs a real value picked with the same rigor `RESCAN_STEP_MAX_ATTEMPTS`/
   `RESCAN_START_HEIGHT_CUSHION_BLOCKS` were - not asserted here, called out as
   an open question below.

### Sizing the next chunk

```
let budget_bytes = rescan_chunk_memory_budget_mb * 1024 * 1024;   // new setting
let by_budget = (budget_bytes as f64 / avg_bytes_per_block).floor() as u64;
let chunk_size = by_budget.clamp(RESCAN_CHUNK_MIN_BLOCKS, RESCAN_CHUNK_MAX_BLOCKS)
    .min(remaining);
```

- **Cold start**: no observation exists for the very first chunk of a job.
  Start `avg_bytes_per_block` at a fixed, conservative constant (e.g. an
  estimate of "typical" block size safely on the large side, so the first
  request undershoots the budget rather than overshoots it) rather than
  guessing a block count directly - keeps exactly one code path for "compute
  chunk size from an average," cold-start or not.
- **Clamps, and why they're still needed even with a real budget**:
  `RESCAN_CHUNK_MIN_BLOCKS` (e.g. `1`) stops a pathological giant-block
  estimate from computing a chunk size of `0` and stalling forever;
  `RESCAN_CHUNK_MAX_BLOCKS` (e.g. `1000`, well above today's fixed `100`)
  stops a pathological near-zero estimate (a long run of empty blocks, or a
  bootstrap value picked too low) from computing an absurdly large single
  request even though the *byte* budget alone would technically allow it - a
  monerod on the other end ignoring `max_block_count` (older versions; see
  `get_blocks_range`'s own doc comment) means a request that *asks* for 1000
  is still safely bounded, but asking for a request in the millions has no
  such backstop.
- **New setting**: `payment.rescan_chunk_memory_budget_mb` (`u32`), same
  `PaymentConfig` scalar-setting pattern every other rescan knob already uses
  (`settings.rs:113-114`'s neighbors) - default worth picking deliberately
  (not asserted here): large enough that a typical chunk is still
  multi-block (amortizing round-trip overhead, the whole point of batching in
  the first place), small enough that one request can't balloon memory on a
  constrained self-hosted box. `8` MB is a reasonable starting point to
  validate against, not a final answer.

### Why per-job, not a global/persisted average

A `Store`-persisted or process-global average block size would let one job's
observations inform another's cold start, and would survive a restart - but it
also couples jobs that have no reason to be coupled (a rescan walking a quiet
stretch of chain three years ago and one walking last week's busy chain
shouldn't share an estimate), and it's real complexity (a new table or
settings write path, contention on a shared mutable value across concurrent
jobs) for a benefit that's genuinely marginal: the EWMA converges within a
handful of chunks regardless, and a job spans enough chunks that a few
sub-optimally-sized ones at the very start cost little. Keep this simple
unless real operation shows the cold-start cost matters.

### Why not literally Monero's own long-term-median-weight algorithm

Named directly since the request asked for it: Monero's long-term median block
weight exists to resist a sustained *attacker*-driven spike gaming the block
size limit over ~100,000 blocks, a consensus-critical security property. This
proposal has no adversary and no consensus stakes - it only ever decides "how
many blocks should I ask for next," where a wrong guess costs at most one
oversized-or-undersized request, self-correcting the very next chunk. An EWMA
over the current job's own already-fetched chunks is the right-sized tool;
reimplementing a 100,000-block long-term median for this would be real,
unjustified complexity solving a problem this doesn't have.

---

## Part 2 — bounding concurrent rescans, and not re-fetching the same blocks

### 2.1 A global concurrency cap

Add `payment.max_concurrent_rescans` (`u32`, e.g. default `4`) and a single
process-wide `Arc<tokio::sync::Semaphore>` (constructed once in `main.rs`
alongside `rescan_daemons`, threaded into `AppState` and into
`resume_running_rescans` the same way `rescan_daemons` was). `spawn_rescan_job`
acquires a permit *inside* the spawned task, before calling `run_rescan_job` -
a job whose permit isn't yet available simply waits (its row stays `running`
with `current_height` unchanged; nothing merchant-visible needs to change,
though `RescanStatusView`'s existing `stalled` signal, WBS's own post-launch
resilience follow-up, needs a look - see open questions). This bounds how many
rescans can hammer `rescan_daemons`'s shared connection pool at once,
regardless of how many tenants happen to trigger one in the same window - the
same principle as the `rescan_daemons` split itself, just applied
rescan-to-rescan instead of rescan-to-live-scanner.

### 2.2 In-memory, cross-job block cache (the real fix for redundant fetches)

A single process-wide cache, keyed by `(Network, height)` → `Arc<Vec<Transaction>>`
(decoded, not raw bytes - avoids re-parsing on a hit, and `Transaction: Clone`
already, confirmed against `monero` 0.22.0's own definition), consulted by
`rescan_order` before each chunk fetch and populated after. Concurrent rescan
jobs covering overlapping ranges then genuinely stop re-downloading and
re-decoding the same blocks - the second job's request for an already-cached
height is served from memory, zero daemon round trips for that portion.

- **Backing store**: `moka::future::Cache` with a byte-based `weigher`, the
  exact pattern `shared::http_cache` already established for
  `CONTROL_PLANE_HTTP_CACHE_MAX_MB` (WBS Phase 3.1) - not a new idea in this
  codebase, a second real use of one already proven here. A new setting,
  `payment.rescan_block_cache_max_mb` (default e.g. `64`), the same
  "megabytes, not entry count" reasoning that setting's own history in
  `order_rescan_wbs.md` already argued for.
- **Reorg safety - the one real correctness constraint**: a block within
  `payment.reorg_check_depth` (`settings.rs:111`, default `20`) blocks of the
  current chain tip can still be orphaned and replaced. Never cache (and never
  serve from cache) a height `> tip - reorg_check_depth` - the exact same
  safety boundary `check_for_reorg_and_reconcile` already draws for a
  completely different purpose (re-examining recorded payments), reused here
  rather than invented fresh. A cache entry below that depth is, for every
  practical purpose already assumed elsewhere in this codebase, immutable -
  matches the existing reasoning in `rescan_order`'s own doc comment about why
  the *tip* side of a rescan needs no held-back buffer (`scanner.rs`, the
  paragraph starting "Deliberately scans all the way to the literal tip").
  Near-tip heights (rare for a rescan, whose whole premise is a stale window,
  but real for its final approach to `to_height`) simply bypass the cache
  entirely, same as before this proposal.
- **Who populates it**: primarily rescan jobs (the real redundant-work case).
  The live scanner's own per-tick walk could populate it too at no real cost,
  but rarely benefits from it (each block is normally scanned exactly once
  under normal operation) - worth doing for symmetry/simplicity, not because
  it's expected to matter.
- **Lifetime/scope**: process-local, in-memory only, gone on restart - exactly
  the property that makes this phase safe to ship first: no persistence
  format to design, no migration, no corruption mode, bounded blast radius if
  something about the sizing is wrong (worst case, cache evictions happen more
  often than ideal; never a correctness issue given the reorg-depth
  exclusion above).

### 2.3 Optional on-disk cache (explicitly the higher-risk, later phase)

The request specifically named an optional on-disk flag, so naming it here as
its own phase rather than folding it into 2.2: `payment.rescan_block_cache_
persist_to_disk` (`bool`, default `false`) plus a directory setting, backing
the same `(Network, height)` key with blob storage that survives a restart and
accumulates across days/weeks - genuinely useful for a self-hosted operator
where "last week's late payments" rescans from *different* tenants land on
different days, past what an in-memory-only cache (gone on every restart)
could ever help with.

This is deliberately scoped as **later, and more carefully**, because it adds
real new failure modes the in-memory tier doesn't have: a storage format to
pick (a flat keyed file store, or a small dedicated SQLite table - *not* the
main `scanner.db`, to keep an operationally-optional cache from ever being on
the critical path for the merchant-facing database), a real bounded-size
eviction policy enforced across restarts (not just moka's in-memory LRU), and
a residual correctness question worth stating plainly rather than glossing
over: an extremely deep reorg *beyond* `reorg_check_depth` is already a
documented, accepted residual risk everywhere else in this codebase (the live
scanner's own reorg reconciliation only ever looks `reorg_check_depth` deep)
- a persistent on-disk cache doesn't make that risk worse, but it does make a
stale cached block survive longer (across restarts, potentially days) than an
in-memory cache's own natural churn would, so it deserves being named
explicitly here rather than assumed away.

### 2.4 What this deliberately does not attempt

Coalescing two *simultaneously in-flight* requests for the exact same
not-yet-cached height (a "single-flight" pattern, e.g. via a
`Mutex<HashMap<Key, Shared<Future>>>` so a second concurrent request for a
height already being fetched awaits the first's result instead of issuing a
duplicate call) is a real, further refinement on top of 2.2 - but only matters
when two jobs' chunk boundaries happen to overlap in the same narrow time
window, a much narrower case than "two jobs eventually visit the same already-
resolved height," which 2.2's plain cache already fully covers. Worth naming
as a real Phase 3+ candidate if operational data shows it matters, not
worth building speculatively now.

---

## Open questions (need a decision before this becomes a WBS)

1. `ewma_alpha`'s value, and the cold-start byte-size estimate - both need to
   be picked deliberately (real reasoning, ideally checked against real chunk
   data the way `RESCAN_START_HEIGHT_CUSHION_BLOCKS` was), not asserted in
   this proposal.
2. `payment.rescan_chunk_memory_budget_mb`'s default, `RESCAN_CHUNK_MIN_BLOCKS`/
   `RESCAN_CHUNK_MAX_BLOCKS`'s values - same "needs a real default, not a
   guess" treatment.
3. `payment.max_concurrent_rescans`'s default, and whether a queued-on-a-
   permit job should surface differently on `/status`/the order-detail page
   (today's `stalled` signal means "running but not progressing" - a job
   queued behind the semaphore *is* that, technically, and might need its own
   distinct label so an operator doesn't mistake "waiting for a permit" for
   "actually stuck").
4. `payment.rescan_block_cache_max_mb`'s default.
5. Whether Part 2's in-memory cache should also be consulted by the *live*
   scanner's own per-tick walk (proposed above as "harmless, do it anyway") or
   left rescan-only for a smaller initial blast radius.
6. Phase 2.3 (on-disk cache) is deliberately left unscoped on storage format
   pending a decision on whether it's wanted at all before design time is
   spent on it - the in-memory tier (2.2) may already be enough in practice
   for most self-hosted deployments, and is a much smaller thing to build,
   test, and reason about first.

## Suggested phase order for the eventual WBS

1. Dynamic chunk sizing (Part 1) - self-contained, no new shared state, lowest
   risk, directly replaces `RESCAN_CHUNK_BLOCKS`.
2. Global concurrency cap (2.1) - self-contained, small (one semaphore).
3. In-memory cross-job block cache (2.2) - the real fix for redundant
   downloads, real but bounded new complexity (reuses `moka`, already proven
   in this codebase for exactly this "byte-bounded cache" shape).
4. On-disk cache (2.3) - only after 1-3 are real and operating, and only if
   real usage shows restart-surviving reuse is worth its own added complexity.
