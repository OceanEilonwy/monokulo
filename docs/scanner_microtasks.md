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
   tenant.** `Store::advance_scanned_cursor` takes a `ScannedBlock`. Only the
   block scan (`work::blocks`) can construct one: its fields are private and
   it has no public constructor. Every cursor update is conditional on the
   cursor still being at the parent height, so a concurrent rewind wins.
   Tenants with nothing that could be paid (no order in scope) move by
   `Store::advance_idle_cursors`, in the same transaction that records the
   block. That is a predicate evaluated inside SQLite, not a list built
   earlier. The old "advance everyone except the failures" call is gone.
2. **Matches from a block become payments only after the whole block was
   scanned and its hash rechecked.** Staged rows (`partial_block_matches`)
   are promoted in the same transaction as the cursor advance.
3. **No settlement is announced from a chain that is being discarded.**
   While a reorg is pending (detected but not yet rewound), recompute runs
   with `SettlementGate::Frozen`. A transition *into* `paid` or `overpaid`
   is deferred, and its obligation stays queued. Other transitions still
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
    Advanced,          // did something; more may be due
    Idle,              // nothing due
    Blocked(&'static str), // waiting on another unit (e.g. a reorg)
    Failed(ScannerError),  // retried on a later round, with backoff
}
```

The executor can't mistake "nothing to do" for "failed". A `Failed` unit
stops its source for the rest of the round; other sources still run.

### Tiers

Sources are grouped in tiers. The order is the priority within a round:

| Tier | Sources | Durable state |
| --- | --- | --- |
| Chain | reorg detection, reorg job (collect, process, rewind) | `reorg_jobs`, `reorg_work` |
| Blocks | frontier and catch-up block scans, grouped by tenant cursor | tenant cursors, `scanned_blocks`, `partial_block_*` |
| Mempool | pool poll, body fetch, rotating scan | memory only (rebuilt from the pool) |
| Settlement | vanished-mempool check, recompute (obligations, then due orders) | `pending_payment_recomputes`, `orders.next_due_*`, `scheduler_positions` |
| Upkeep | scanned-range bookkeeping, void revalidation, pruning | `scheduler_positions` |

### Round budget

Each round has a deadline. Every tier has a **reserved share** of it
(Chain 20 %, Blocks 40 %, Mempool 15 %, Settlement 20 %, Upkeep 5 %).

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
  served round-robin from a persisted rotation position. While the frontier
  (the group at the network high-water mark) is behind the node, turns
  alternate between it and catch-up. The turn is kept across rounds, so
  even one-unit rounds alternate.
- **Mempool:** a rotating window of transactions and of tenants per
  transaction (the existing policy).
- **Recompute:** obligations first, then orders by `next_due_*`, earliest
  first. After a recompute an order's next due point moves forward, so a
  large backlog is served oldest-first and nothing is starved.
- **Failures:** a tenant or payment that fails gets an in-memory backoff. It
  is skipped until the backoff expires; everything else continues.

## Reorgs

**Detection is O(1) per round.** Block hashes chain, so the scanner compares
only the stored hash at `min(high-water, node tip)` with the node. If they
match, nothing below can differ. If they don't, a binary search over the
stored window finds the first divergent height in O(log depth) calls. Every
call is stateless, so a failure is simply retried.

**Reconciliation is a durable job** (`reorg_jobs` + `reorg_work`, one per
network):

1. **Collect.** Freeze `candidate_max_id = MAX(order_payments.id)`. Page
   every payment at or above the fork (voided or not) with
   `id <= candidate_max_id` into `reorg_work`. Paging is by id, so rows
   updated during the job can't shift between pages. Payments added later
   are either unconfirmed (mempool) or found by the forward scan after the
   rewind, because block scanning is suspended while a job exists.
2. **Process.** For each work row, locate its transaction and move, void
   or unvoid it (the existing rules), then delete the work row, all in one
   transaction. A failed row gets `attempts + 1` and a `next_attempt_at`
   backoff; later rows still proceed.
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
  write on the blocking pool (`AppState::write_store`), on the main
  connection.
- **Two connections write:** the worker and the API. SQLite's write lock
  arbitrates between them. Every write transaction starts with
  `BEGIN IMMEDIATE`, so its decision reads and its writes see one snapshot;
  a deferred transaction that read first could otherwise fail its first
  write with `SQLITE_BUSY_SNAPSHOT`, which no busy timeout retries. Scanner
  jobs are short (bounded pages), so an API write waits milliseconds at most.
- Scanner units never hold a database job across a daemon, custody or
  network await: they read, await the node, then write.
- `Db::over_shared` runs jobs inline on the shared store, for tests and
  in-memory databases.

## Failures

- **A node failure** (an error or no answer within 15 s) stops only the
  work that needs the node, leaves durable state where it was, and is
  retried next round. It is logged, not reported as a failed round. If the
  chain height itself can't be read, only the mempool is scanned and the
  round reports the error.
- **A storage failure** stops that tier for the round and is reported.
- **A custody failure** for one tenant leaves that tenant at its cursor
  with a retry delay (two immediate retries, then doubling to a minute).
  Other tenants carry on.
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
| `crates/scanner/src/work/mod.rs` | tiers, budget, progress floor, backoff, `run_round` |
| `crates/scanner/src/work/chain.rs` | reorg detection and the reorg job |
| `crates/scanner/src/work/blocks.rs` | block scanning, `ScannedBlock`, checkpoints |
| `crates/scanner/src/work/mempool.rs` | mempool rotation |
| `crates/scanner/src/work/settlement.rs` | vanished payments, recompute |
| `crates/scanner/src/work/upkeep.rs` | pruning, scanned ranges, void recheck |
| `crates/scanner/src/work/tests.rs` | the scheduler's guarantees |
| `crates/scanner/src/store/work.rs` | the durable state (migration 0019) |
| `crates/scanner/src/store/db.rs` | the database worker |

`scanner::run_scan_tick*` and `check_for_reorg_and_reconcile` remain as
entry points that run a round, or the reorg job to completion, on a shared
store. The existing scanner tests run through them unchanged.

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
- **The hash before the contents.** A block's hash is read before its
  transactions and rechecked after. With per-call failover, the contents
  then come from the node whose hash is recorded.

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

These are observations for this workload and machine, not capacity limits.

## Follow-ups

- The API still writes through the main connection's mutex, now on the
  blocking pool. Moving those writes onto the worker's `Admin` class would
  make the fairness between API and scanner writes a queue property rather
  than SQLite's lock. It would also allow removing `SharedStore` from
  `AppState`, which many tests and harnesses construct directly.
- The stress fixture doesn't yet inject reorgs, process kills or slow disk
  commands. The engine's tests cover the first two (restart mid-job,
  mid-block, the kill-anywhere test); slow disk is untested.
