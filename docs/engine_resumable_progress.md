# Engine progress under constrained resources

This is the implementation follow-up to `engine_cancellation_audit.md` (2026-09-28).

## Implemented

- Live block scans and lagging-tenant catch-up keep a per-tenant, per-block
  transaction position in SQLite. Matching outputs remain staged until the
  complete block, its hash recheck, and the cursor update commit together.
  A changed block hash or scan window resets the position and staged matches.
  The block phases have time allowances; a slow block resumes on later ticks.
- Plain custody builds live and one-off key tables in 256-entry CPU batches.
  Completed batches survive cancellation. Mempool work rotates through at most
  64 transactions and 32 tenants per transaction per tick. Registration attempts
  rotate through at most 16 tenants per pass. Routine status recomputation uses
  bounded, rotating pages. The vanished-mempool sweep uses a 64-payment page
  and moves its cursor after each attempted payment, including failed lookups.
- Webhook delivery's DNS check (monokulo's since the engine stopped sending
  webhooks), socket slot acquisition/connection/exchange,
  and wallet registration checks now have end-to-end time bounds. The custody
  socket protocol accepts a stable registration ID when re-registering an
  existing tenant, making retries after a lost response return the same handle.
- Authentication and status reads use separate read-only SQLite connections
  on two bounded worker threads. WAL allows those reads to overlap with a
  writer. The read pool does not hold the shared writer mutex.

## Remaining work before claiming a hard progress guarantee

1. Move the shared write connection and all remaining handler/scanner SQL to a
   dedicated worker with a bounded queue. Split large queries and writes into
   pages or short transactions. A Tokio timeout cannot interrupt a synchronous
   SQLite call already running on an async worker.
2. Make reorg reconciliation durable and resumable. It still loads all affected
   payments and performs an unbounded series of daemon calls before it rewinds
   the scan window. The recent double-spend recheck likewise needs a bounded
   cursor. Until these are addressed, a large reorg or many recent voids can
   repeatedly consume a loop's time allowance.
3. Bound the number of active tenants and scan-window rows loaded at the start
   of a tick, and cap per-tick scanned-range updates. Current block CPU and
   mempool scans yield between units, but those SQL passes are still whole-set.
4. Give new-tenant creation a caller-provided idempotency key and persist its
   registration intent before asking a remote custody service to create the
   wallet. Existing-tenant registration is idempotent; a cancelled *new*
   tenant request can still strand a remote wallet.
5. Measure progress with production-size blocks, tenants, mempool snapshots,
   and injected SQLite lock/disk delays on a one-CPU runtime. The current
   regression suite checks correctness and contention paths but does not
   establish a production capacity threshold.

Webhook delivery is at least once. Receivers should apply `event_id` only once;
the bundled WooCommerce receiver records seen IDs, but its check-and-save is not
atomic across simultaneous deliveries. WAL with `synchronous=NORMAL` stays as
the accepted durability policy: recent commits may be lost on power failure.
