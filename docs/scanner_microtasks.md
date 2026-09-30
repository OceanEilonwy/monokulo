# Scanner work units and scheduler

Status: design and implementation record.

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
   tenant.** `Store::commit_tenant_block` takes a `ScannedBlock`. Only the
   block-scan unit can construct one (its fields are private and it has no
   public constructor). Every cursor update is conditional on the cursor
   still being at the parent height, so a concurrent rewind wins. Tenants
   with nothing that could be paid (no order in scope) are moved along in
   the same transaction that records the block for the network. That is a
   predicate evaluated inside SQLite, not a list built earlier.
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
tier; it doesn't starve one. If the round ends with work left over, the
loop starts the next round immediately instead of sleeping for the poll
interval (work-conserving catch-up).

### Fairness inside a tier

- **Blocks:** tenants are grouped by cursor. Each group scans its next block
  once: one fetch, then one view-key scan per tenant. Groups are served
  round-robin from a persisted rotation position. The frontier group (at
  the network high-water mark) goes first, so fresh payments stay fast.
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
- **Expiry held because the tenant is behind:** due again 30 s later.

Two indexed keyset pages (`next_due_at <= now`, `next_due_height <= tip`)
replace the rotating scan over every non-terminal order.

## Database access

- Scanner units never hold a database lock across a daemon, custody or
  network await.
- Every write transaction starts with `BEGIN IMMEDIATE`, so its decision
  reads and its writes see the same snapshot. A deferred transaction that
  read first could otherwise fail its first write with `SQLITE_BUSY` when
  another connection had written in between.
- A dedicated writer thread per engine serves write classes (`Scanner`,
  `Webhook`, `Admin`) round-robin from bounded queues, so SQLite work never
  blocks a Tokio worker and no class starves another.

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
