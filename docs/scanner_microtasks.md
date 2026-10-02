# Scanner work units and scheduler

Status: implemented. This is the design and implementation record.

This document replaces the scanner's monolithic tick with small, durable,
idempotent **work units** chosen by one **scheduler** per network. It borrows
concepts from an experimental hardening branch (a durable reorg job, a
frozen candidate set, bounded pages, database write classes, the stress
fixture), but not its code.

## Goals

- **Always make progress.** A failing node, custody backend, tenant or
  payment delays only its own work. Nodes can go down, the app can restart,
  and the CPU can be throttled or memory tight. Healthy work keeps advancing
  every round.
- **Predictable cost.** Every unit reads and writes a bounded number of rows
  and makes a bounded number of daemon and custody calls. Nothing loads a
  whole table.
- **Correct by construction.** Invariants are enforced by types and by
  transaction boundaries, not by comments about call order.
- **Restartable.** All progress that is expensive to redo is in SQLite.
  Memory holds only what can be rebuilt cheaply (mempool bodies, retry
  timers).

## Invariants

These held before and must keep holding. Each lists how it is enforced.

1. **A tenant's cursor moves only past a block that was scanned for that
   tenant.** `Store::advance_scanned_cursors` takes `ScannedBlock`s. Only the
   block scan (`work::blocks`) can construct one: its fields are private and
   it has no public constructor. Every cursor update is conditional on the
   cursor still being at the parent height (`UPDATE … RETURNING`), so a
   concurrent rewind wins, and only the tenants that actually moved get
   their matches recorded.
   Tenants with nothing that could be paid (no order in scope) move by
   `Store::advance_idle_cursors`, in the same transaction that records the
   block. That is a predicate evaluated inside SQLite, not a list built
   earlier. The old "advance everyone except the failures" call is gone.
2. **Matches from a block become payments only after the whole block was
   scanned and its hash rechecked.** Staged rows (`partial_block_matches`)
   are promoted in the same transaction as the cursor advance.
3. **No settlement is announced from a chain that is being discarded.**
   While a reorg is pending (detected but not yet rewound), the status plan
   (`store::plan_status`, a pure function over the order's facts) is frozen.
   A transition *into* `paid` or `overpaid` is deferred, and its obligation
   stays queued. Other transitions still
   happen: expiry, confirmations counting, partial, and `paid` walked back.
   Mempool detection keeps running.
4. **A status change and its webhook commit together**, and a payment
   change leaves a durable recompute obligation. This is unchanged:
   `recompute_and_notify` and the `pending_payment_recomputes` triggers.
5. **Replacement blocks are always forward-scanned after a reorg.** Rewind
   deletes the losing hashes, re-anchors the common ancestor if the window
   would otherwise be empty, and clamps cursors, all in one transaction. It
   does this only after every candidate payment was re-examined and the
   ancestor hash was read.
6. **Never void a payment on absence alone.** Unchanged:
   `void_if_double_spend_proven`.
7. **An order does not expire while its tenant is behind.** Unchanged. An
   expiry held this way is rescheduled with a short delay, so it doesn't
   hold the front of the due queue.

## Work units

A unit is one bounded step of one kind of work. It returns `Progress`:

```rust
enum Progress {
    Advanced,             // did something; more may be due
    Idle,                 // nothing due
    Blocked(Wait),        // waiting, for a reason the type names
    Failed(ScannerError), // retried on a later round, with backoff
}
```

The executor can't mistake "nothing to do" for "failed". A `Failed` unit
stops its source for the rest of the round; other sources still run. `Wait`
is a closed set (the chain height is unknown, a reorg is being reconciled,
a rewind happened this round, the node failed, the node can't serve its tip,
the mempool is unreadable, reorg candidates are retrying, the node's chain
diverged from the recorded one). A round reports each tier's steps and
outcome in a `PerTier`, indexed by `Tier`.

### Tiers

Sources are grouped in tiers. The order is the priority within a round:

| Tier | Sources | Durable state |
| --- | --- | --- |
| Chain | reorg detection, reorg job (collect, process, rewind) | `reorg_jobs`, `reorg_work` |
| Blocks | frontier and catch-up block scans, grouped by tenant cursor | tenant cursors, `scanned_blocks`, `partial_block_*` |
| Mempool | pool poll, body fetch, rotating scan (and the fast path, below) | memory only (rebuilt from the pool) |
| Settlement | vanished-mempool check, recompute (obligations, then due orders) | `pending_payment_recomputes`, `orders.next_due_*`, `scheduler_positions` |
| Upkeep | scanned-range bookkeeping, void recheck, pruning, WAL checkpoint | `scheduler_positions` |

### Round budget

Each round has a deadline. Every tier has a **reserved share** of it
(Chain 20 %, Blocks 40 %, Mempool 15 %, Settlement 20 %, Upkeep 5 %).

The round is 10 s (`ROUND_BUDGET`), measured by the round length sweep
(docs/engine_stress.md); its comment gives the reasons. The shares are the
only other times written down. Everything a tier does in one
call follows from its share (`Tier::reserved()`, worked out at build time
from `ROUND_BUDGET`), so a change to the round or the shares carries
through:

| Time | From | Today |
| --- | --- | --- |
| A block request's target (`SCAN_CHUNK_TARGET_CALL_SECS`) | Blocks share | 4 s |
| One store's key-custody scan of a run of a block's transactions | Blocks share | 4 s |
| One store's scan of a pool transaction (round and fast path) | Mempool share | 1.5 s |
| One vanished payment's lookups | Settlement share | 2 s |

A round raised for a large block (`work::blocks::round_budget_for`) splits
its own length by the same shares (`Tier::share_of`). The per-call times
above stay at the base round's, so a large block's pages stay small.
Node timeouts don't follow the round: they come from each node's link
(docs/engine_scaling.md section 2).

- **Pass 1** runs each tier in priority order until its share is used or
  it goes idle.
- **Pass 2** gives the remaining time to the tiers that still have work, in
  priority order.

A unit that has not started is not started past its deadline. A running
unit's daemon and custody calls are bounded by per-call timeouts. SQLite
work is bounded by page sizes, because a timeout cannot interrupt it.

**Progress floor:** every tier with work completes at least one unit per
round, even when its share is already spent. So a throttled CPU slows every
tier; it doesn't starve one. One unit of a tier does a bounded slice of
*every* queue the tier owns: detection and a step of the reorg job;
the vanished-payment page and a page of recomputes; pruning, a void-recheck
page and a scanned-range page. So a round with time for a single unit per
tier still advances everything.

If the round ends with work left over, the loop starts the next round
immediately instead of sleeping for the poll interval (work-conserving
catch-up).

### Fairness inside a tier

- **Blocks:** tenants are grouped by cursor. Each group scans its next block
  once: one fetch, then one view-key scan per tenant. Catch-up groups are
  served round-robin from a persisted rotation position (`Rotation`, keyed
  by the cursor a group reached, so a group that moved isn't served twice).
  While the frontier (the group at the network high-water mark) is behind
  the node, turns alternate between it and catch-up. The turn is kept
  across rounds, so even one-unit rounds alternate.
- **Big groups:** one block scan covers at most 256 tenants of a group (a
  page, in id order) and one commit moves them. With more at one cursor,
  the unit scans the same block (held in the cache) for the next page
  before the group moves on, and a block it has started it finishes for
  every page whatever the time, so the group moves together. Only past the
  unit's 8 scans does the rest stay behind as a catch-up group. (When the
  first page moved on alone, 1000 tenants at one cursor split into groups
  that fetched the same blocks again: 2.5 blocks a second against 4.9 now,
  in the round length sweep.)
- **Idle stores:** a store with nothing that could have been paid from a
  block on (every order closed before the block's time) moves straight to
  the high-water mark, whether it is at the frontier or catching up, even
  when no one else in its group can be scanned.
- **Mempool:** a rotating window of transactions and of tenants per
  transaction (the existing policy).
- **Recompute:** obligations first, then orders by `next_due_*`, earliest
  first. After a recompute an order's next due point moves forward, so a
  large backlog is served oldest-first and nothing is starved.
- **Failures:** a tenant or payment that fails gets an in-memory backoff. It
  is skipped until the backoff expires; everything else continues.

## Reorgs

**Block identity.** Blocks come from the node as `ChainBlock`s: height,
hash, parent hash and transactions, from one call (`get_chain_blocks`; the
RPC client decodes the block blob and computes its id). A block is scanned
only if it extends the recorded chain (its parent hash is the recorded
parent's) and matches any hash recorded at its height; the commit checks
both again inside its transaction. A block that doesn't fit is `Diverged`:
nothing is recorded, and the frontier waits (`Wait::ChainDiverged`) for the
chain tier to open a reorg job, instead of asking for the block again.

**Detection is O(1) per round.** Block hashes chain, so the scanner compares
only the stored hash at `min(high-water, node tip)` with the node. If they
match, nothing below can differ. If they don't, a binary search over the
stored window finds the first divergent height in O(log depth) calls. Every
call is stateless, so a failure is simply retried. The node gives its tip's
id along with its height, so while the recorded chain ends at the tip the
comparison costs no call at all (`docs/node_rpc_efficiency.md`).

**Reconciliation is a durable job** (`reorg_jobs` + `reorg_work`, one per
network):

1. **Collect.** Freeze `candidate_max_id = MAX(order_payments.id)`. Page
   every payment at or above the fork (voided or not) with
   `id <= candidate_max_id` into `reorg_work`. Paging is by id, so rows
   updated during the job can't shift between pages. Payments added later
   are either unconfirmed (mempool) or found by the forward scan after the
   rewind, because block scanning is suspended while a job exists.
2. **Process.** For each work row, locate its transaction and apply
   `chain::decide` (a pure table from voided × location × proof to Keep,
   Move, Restore or Void), then delete the work row, all in one transaction.
   A payment found nowhere and not proven double-spent goes back to
   unconfirmed; a voided one found in a block is restored and given that
   block's height (even if the void recheck restored it meanwhile). A
   failed row gets `attempts + 1` and a `next_attempt_at` backoff; later
   rows still proceed, and a node failure ends the page.
3. **Rewind.** Once `reorg_work` is empty, read the ancestor hash. Then, in
   one transaction: delete the scanned blocks at and above the fork,
   re-anchor the ancestor if the window is empty, clamp cursors, and delete
   the job. If the ancestor can't be read, the losing hashes and the job
   stay, and rewind retries.

A deeper fork found while a job is open lowers the job's fork and re-enters
Collect (idempotent `INSERT OR IGNORE`). A fork above the job's fork needs
nothing: the rewind covers it.

While a job exists:

- Blocks are suspended.
- Recompute runs frozen (invariant 3).
- Mempool and upkeep run normally.

## Recompute scheduling

`orders.next_due_at` (unix time) and `orders.next_due_height` are written by
`recompute_order_status` in the same `UPDATE` as the status:

- **Open (not terminal):** due at `expires_at` (expiry) and at `tip + 1`
  while it has a payment with fewer confirmations than required (the
  confirmation count shown to customers changes every block).
- **Terminal:** both are `NULL`. A later payment change creates an
  obligation through the existing triggers.
- **A transition held back** (expiry while the tenant is behind, or a
  settlement during a reorg): due again at `now`, so it is retried next round
  and queues behind anything due earlier.
- **A changed deadline** (a trigger on `expires_at_utc`): due at once.

Two indexed keyset pages (`next_due_at <= now`, `next_due_height <= tip`)
replace the rotating scan over every non-terminal order.

## Database access

No SQLite call runs on a Tokio worker thread in production:

- **The database worker** (`store::Db`) is one thread with its own
  connection. The scanner's units and webhook delivery send it jobs in
  classes (`Scanner`, `Webhook`, `Admin`), each with a bounded queue (64),
  served round-robin. A backlog in one class delays another by at most one
  job, and a full queue makes its callers wait instead of growing memory.
  A job runs to completion even if its caller stops waiting, so every job
  is a whole, idempotent step. The worker shares the main store's
  order-change notifications, so live updates see its commits.
- **API requests** read through the read pool (`AppState::read_store`) and
  write on the worker's `Admin` class (`AppState::write_store`), so API and
  scanner writes share one queue discipline. The read pool is
  `shared::sqlite::Pool`: read-only connections (`database.read_connections`,
  default 4, read at start), each on its own thread, all taking reads from
  one queue, so a read waits only for a free connection, never behind a slow
  read on a particular one. Monokulo reaches its own database the same way
  (`db::Database`: `read` on its pool, `write` on one writing connection).
- **Key registration** lists stores through the worker too; the loops no
  longer hold the shared store at all.
- **Handlers reach the database only through `AppState::db`**
  (`store::Database`: the read pool, the worker and order-change
  subscriptions). There is no shared store in `AppState`; tests run the same
  handle inline on one in-memory store (reads still read-only), with a
  test-only `Database::lock` for setting up and checking state.
- **Connections** are tuned once: a 5 s busy timeout, a 128-statement cache
  (every hot query is `prepare_cached`), and a journal size limit. Upkeep
  runs a passive WAL checkpoint every ten minutes. Migration 0020 adds the
  indexes the hot queries need, and a test checks each query's plan uses
  them.
- **Transactions:** every write transaction starts with
  `BEGIN IMMEDIATE`, so its decision reads and its writes see one snapshot;
  a deferred transaction that read first could otherwise fail its first
  write with `SQLITE_BUSY_SNAPSHOT`, which no busy timeout retries. Scanner
  jobs are short (bounded pages), so an API write waits milliseconds at most.
- Scanner units never hold a database job across a daemon, custody or
  network await: they read, await the node, then write.
- `Db::over_shared` runs jobs inline on the shared store, for tests and
  in-memory databases.

## Failures

- **A node failure** (an error, a missing block, or no answer within 15 s)
  stops only the work that needs the node, leaves durable state where it
  was, and is retried next round. It is logged (at most once a minute per
  kind), not reported as a failed round. If the chain height itself can't be
  read, only the mempool is scanned and the round reports the error.
- **A storage failure** stops that tier for the round and is reported.
- **A custody failure** for one tenant leaves that tenant at its cursor
  with a retry delay (two immediate retries, then doubling to a minute).
  Other tenants carry on. Retry delays are typed by what they key
  (`Backoff<TenantKey>`, `Backoff<OrderKey>`) and forgotten after an hour
  without failures.
- **A diverged chain** the chain tier couldn't yet open a job for (its own
  lookups failed) stops the frontier for the round with
  `Wait::ChainDiverged`; the job opens once the node answers.
- **A reorg candidate the node can't answer about** is retried with backoff
  and, after 12 attempts, left as recorded (the rule for ambiguous evidence:
  never void on absence alone). So one payment can't hold a network's block
  scanning forever.
- **A panic** in a database job fails only its caller. A panic in a round
  restarts the network's loop (the existing supervisor), which costs only
  in-memory state.

## Where it lives

| Path | What |
| --- | --- |
| `crates/engine/src/work/mod.rs` | tiers, budget, progress floor, backoff, `run_round` |
| `crates/engine/src/work/chain.rs` | reorg detection and the reorg job |
| `crates/engine/src/work/blocks.rs` | block scanning, `ScannedBlock`, checkpoints |
| `crates/engine/src/work/mempool.rs` | mempool rotation and the fast path |
| `crates/engine/src/work/settlement.rs` | vanished payments, recompute |
| `crates/engine/src/work/upkeep.rs` | pruning, scanned ranges, void recheck |
| `crates/engine/src/work/tests.rs` | the scheduler's guarantees |
| `crates/engine/src/store/work.rs` | the durable state (migrations 0019, 0020) |
| `crates/engine/src/store/db.rs` | the database worker |
| `crates/engine/src/loops.rs` | the per-network round loop and fast mempool loop |

`scanner::run_scan_tick*` and `check_for_reorg_and_reconcile` remain as
entry points that run a round, or the reorg job to completion, on a shared
store: for tests, the e2e harness and tools. The engine's own loop keeps a
`ScanState` across rounds and shares it with the fast path.

## Fast mempool path

Zero-conf detection shouldn't wait for a round. A second loop per network
(`loops::run_fast_mempool_loop`, every 250 ms or the poll interval if
shorter) runs `work::mempool::fast_pass`:

- It lists the pool's txids and keeps only transactions no store has been
  scanned for yet.
- It scans them against every store with something in scope (windows
  reloaded at most once a second), up to 4096 scans a pass. Past that, the
  rest are left to the next pass and the round's rotation, never dropped.
- A match is recorded and its order recomputed (status and webhook) in the
  same database job, using the last round's chain height, and webhook
  delivery is woken at once (`ScanState::waking`).
- It shares the round's mempool state, so the rotation skips what the fast
  path already scanned.

A pool that can't be read is reported as unreadable, never as empty.

## Testing

- **Fault sweeps.** `Store::fail_nth_access` denies the nth SQL statement
  through SQLite's authorizer. The sweeps run a story (a payment seen in the
  pool, mined, reorged out, mined again and confirmed; a payment
  double-spent out of the pool; a void recheck; a checkpointed block's
  commit; an open reorg job), fail every statement of a round in turn, and
  require the fault-free outcome, and that no tier runs away. A store-level
  sweep fails every statement of every scheduler operation and requires the
  database to be exactly as before each failure. The sweeps found a block
  tier spinning on a fork the chain tier hadn't opened yet.
- **Coverage.** `cargo +nightly llvm-cov -p engine --lib --branch` covers
  every branch of `work/*`, `store/work.rs`, `store/db.rs` and `scanner.rs`,
  and llvm's per-line view has no unexecuted line in them. Test modules are
  left out of the report (`coverage_nightly`). A few error closures on lines
  that run (conversions that can't fail in practice) stay unexecuted; the
  summary table counts those. Closing the gaps found a store stuck behind
  forever at a block older than its orders, and a restored void keeping a
  reorged-away height.
- **Logs.** `test_log::capture` records the capturing thread's events, so a
  test asserts a failure was reported (and throttled) rather than only that
  nothing broke.

## Decisions

- **No per-order work items.** Scanning costs one key exchange per
  transaction per *tenant* (the view key); the per-order part is a table
  lookup. Units are per tenant group and per block, and per order only for
  status recomputes.
- **No event log.** New blocks are "a cursor below the high-water mark";
  a reorg is its job row. Work is derived from state, so there is nothing to
  keep in step.
- **No large-tenant index paging.** Plain custody caches each wallet's
  subaddress table and builds it in bounded batches, so scanning a
  transaction costs the same for a store with 2 orders or 300.
- **Reorg detection is stateless.** It costs one lookup when the chain
  agrees, and O(log depth) otherwise, so it needs no durable progress.
- **Block bodies are only trusted within a round.** They are cached per
  round (one pinned node), never across rounds.
- **After a rewind, replacement blocks are scanned from the next round,**
  against a freshly read chain.
- **One call for a block's identity and contents.** `get_chain_blocks`
  returns hash, parent hash and transactions together, so a block's
  contents can't come from a different node or fork than its hash.
- **Polling, not ZMQ, for the pool.** A 250 ms poll (of what changed in
  the pool since the last one: `docs/node_rpc_efficiency.md`) gives
  near-instant detection with the existing RPC client. ZMQ push through
  `libzmq` would be a C dependency the design avoids (`docs/DESIGN.md`);
  an opt-in pure-Rust subscriber that wakes these polls early is
  behind the `zmq` feature (`docs/monero_zmq.md`).
- **A checkpoint is matched by block hash alone.** The hash names the
  height too.

## Stress results

`cargo xtask stress ci` on an AMD Ryzen 9 5950X pinned to one CPU. The
workload is the same `scenario_v3` for both engines.

### Legacy tick, `d57ff06` (baseline)

| Tenants | Status | 4 measured ticks | Final lag | Max timer delay | Max HTTP |
| --- | --- | --- | --- | --- | --- |
| 32 | sustainable | 3.18 s | 0 | 3.6 ms | 5.1 ms |
| 64 | sustainable | 5.93 s | 0 | 3.9 ms | 5.1 ms |
| 128 | sustainable | 11.11 s | 0 | 4.5 ms | 5.9 ms |

All three fault points recovered. The RPC fault point showed a 74 ms timer
spike. Raw data: `docs/stress/baseline-legacy-run.json`.

### Scheduler with the database worker (this branch)

| Tenants | Status | 4 measured ticks | Final lag | Max timer delay | Max HTTP | Max admin write |
| --- | --- | --- | --- | --- | --- | --- |
| 32 | sustainable | 2.81 s (−11 %) | 0 | 3.3 ms | 4.1 ms | 35 ms (legacy 46) |
| 64 | sustainable | 5.42 s (−9 %) | 0 | 3.6 ms | 5.1 ms | 34 ms (legacy 54) |
| 128 | sustainable | 10.73 s (−3 %) | 0 | 3.5 ms | 5.5 ms | 56 ms (legacy 60) |

All three fault points recovered. Under the RPC fault point the worst timer
delay fell from 74 ms (legacy) to 2.9 ms. SQLite no longer runs on the
runtime's workers. The worker's longest job (34 ms) and longest queue wait
(1.7 ms) are dominated by the fixture's deliberate 25 ms write lock per tick.
The fixture's admin writes still go through the main connection, as the API
does. Raw data: `docs/stress/scheduler-db-worker-run.json`; the scheduler
before the worker: `docs/stress/scheduler-run-1.json`.

### After the review fixes (this branch, final)

Same build profile as the runs above (crypto crates unoptimised):

| Tenants | Status | 4 measured ticks | Final lag | Max timer delay | Max HTTP | Max admin write |
| --- | --- | --- | --- | --- | --- | --- |
| 32 | sustainable | 2.78 s (−13 %) | 0 | 4.0 ms | 3.9 ms | 35 ms (legacy 46) |
| 64 | sustainable | 5.31 s (−11 %) | 0 | 3.4 ms | 4.7 ms | 35 ms (legacy 54) |
| 128 | sustainable | 10.33 s (−7 %) | 0 | 3.9 ms | 4.5 ms | 35 ms (legacy 60) |

All three fault points recovered; the worst timer delay under any fault was
3.3 ms. Admin writes now run on the worker's `Admin` class, so a scanner job
can queue behind one of them: the longest queue wait (33 ms) is exactly the
fixture's deliberate 25 ms-plus write per tick, and the longest admin write
no longer grows with the tenant count. Raw data:
`docs/stress/scheduler-final-run.json`.

With the Monero crypto crates optimised in dev builds (now the default, for
the tests' sake) the same workload's measured ticks take 0.22 / 0.27 /
0.38 s: scanning, not scheduling, dominated the numbers above. Raw data:
`docs/stress/scheduler-final-optimised-crypto-run.json`.

These are observations for this workload and machine, not capacity limits.

## Follow-ups

- The stress fixture doesn't yet inject reorgs, process kills or slow disk
  commands. The engine's tests cover the first two (restart mid-job,
  mid-block, the kill-anywhere test); slow disk is untested.
