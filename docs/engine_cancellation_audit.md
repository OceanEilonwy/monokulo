# Engine cancellation and single-CPU audit

Date: 2026-09-28

This audit covers the chain scanner, mempool cache, per-tenant cursors, reorg
reconciliation, custody backends and socket protocol, registration paths,
background-loop supervision, payment/status persistence, and webhook delivery.
It is not a proof of correctness, a cryptographic review, or a capacity benchmark.
The earlier plain-custody cache cancellation fix remains part of this worktree.

## Additional defects reproduced and fixed

### Socket responses could cross request boundaries

`SocketKeyCustody::call` previously borrowed a stream left inside its pooled
slot. An outer cancellation dropped the request future without discarding that
stream. A subsequent caller could read the abandoned request's response; the
protocol does not carry request IDs, and responses for the same method would
not be distinguished by their envelope type. For scans, this could associate
another request's matches with the current tenant or transaction.

The request now owns the stream while in flight. Only a complete response
returns it to the pool. Cancellation, partial frames, and transport errors
close it. A socket-pair regression cancels after the peer receives the request
but before the reply and verifies that the slot cannot reuse the connection.

### Payment writes could outlive their status/webhook obligation

A payment write commits before the end-of-tick recompute. The `touched` set was
volatile, while routine recomputation excludes terminal orders. Cancelling a
tick could therefore leave a closed order's stored amount/status and webhook
stale, even though its new payment was recorded. Reorg height changes have the
same need for durable follow-up.

Migration 0017 adds a durable pending-recompute table. SQLite triggers insert
an obligation with payment inserts and actual amount, height, or void changes.
The scanner includes those orders regardless of their scan-window eligibility.
Only the transaction that updates status and enqueues any resulting webhook
clears the obligation. The migration also schedules existing payment-bearing
orders once to repair historical gaps.

Regression coverage cancels after a paid order's second payment is written,
lets the order leave the scan window, and checks that retry produces
`overpaid` and exactly one corresponding queued webhook. Further checks cover
reopening the database, migration backfill, network isolation, height/void
changes, duplicate sightings, and preservation after webhook-enqueue failure.

### Completed webhook outcomes were discarded with a slow batch

The delivery worker collected the entire batch before persisting any outcome.
Cancellation while one request remained pending lost the outcomes of completed
attempts, causing avoidable duplicate deliveries or forgotten retry counts.
It now persists each outcome before awaiting the next one. A regression holds
one HTTP request pending and cancels the batch after another attempt completes.

This does not provide exactly-once HTTP delivery. A merchant may accept an
HTTP request just before the engine loses its connection or process. Retries
retain the stable `X-Monokulo-Event-Id`; receivers must deduplicate that ID.

### A failed tenant could repeatedly prevent block scanning

The mempool pass retried a failed tenant for every transaction in the same
tick. Thirteen transactions against one unresponsive backend could consume
120 seconds before the block pass, over and over. A paused-time regression
reproduced this without CPU timing assumptions.

The mempool pass now defers a tenant after its first scan failure until the
next tick. Its unscanned transactions are not marked complete. Tests check
block progress across consecutive ticks and payment detection after recovery.

## Remaining limits and follow-up priorities

1. **Explicit work budgets and resumable progress remain necessary for large
   workloads.** Per-call and whole-tick deadlines bound waiting but do not
   guarantee progress if a single key-table build or block takes longer than
   its budget. Cancellation discards unfinished table work; block cursors
   advance only after the whole block. Repeating the same oversized work can
   therefore repeatedly hit a deadline. A large number of failing tenants can
   also exhaust a tick even after the per-tenant mempool fix. This is a
   structural finding, not a measured production capacity threshold. Next
   steps should include phase fairness, bounded/incremental CPU work, and
   resumable block processing, tested with one CPU and production-size data.
2. **SQLite work runs synchronously on async workers.** The reviewed hot paths
   release store locks before network awaits, but large synchronous queries,
   status sweeps, and disk stalls can still delay all tasks on a one-worker
   runtime. Tokio deadlines cannot preempt synchronous work. A dedicated store
   worker and bounded batches would provide a stronger responsiveness bound.
3. **Some waits sit outside their advertised deadlines.** Webhook DNS
   validation runs before the HTTP request timeout. Wallet registration/state
   checks run before the scanner tick timeout; socket-slot acquisition and
   connection establishment precede the socket exchange timeout. These paths
   need end-to-end bounds in addition to normal scheduling/progress mechanisms.
4. **Remote registration has an ambiguous cancellation outcome.** A custody
   server can create a wallet and then lose the response when the client is
   cancelled. Closing the connection prevents response confusion but cannot
   undo an already-executed registration. Idempotent registration IDs or
   lifecycle/lease cleanup are needed to bound abandoned remote wallets.
5. **Crash recovery is not power-loss testing.** SQLite uses WAL with
   `synchronous=NORMAL`. The new pending records are atomic with payment writes,
   but this audit does not establish durability against OS crashes, storage
   faults, or power loss. That existing durability policy warrants a separate
   operational decision and fault-injection tests.

## Validation

Each of the four new regression tests failed before its corresponding fix.
The cancellation regressions use explicit poll/I/O boundaries; only the
unresponsive-backend test advances virtual time to exercise deadlines.

The affected test suites are run with one CPU and four concurrent test threads:

```sh
taskset -c 0 cargo test -p scanner -p key-custody-service \
  -p key-custody-server --tests --locked -- --test-threads=4
```

Result: **424 passed, 0 failed, 18 ignored** across the scanner and custody
client/server suites (including 375 passing scanner library tests). The ignored
tests retain their existing external-service/manual-run requirements.

Clippy completed for the same three crates with `--tests --locked`; warnings
were confined to unchanged code. `git diff --check` also passed. No deployment,
production database migration, or external service changes are performed by
this audit.
