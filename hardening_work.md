# Engine hardening work pack

## Purpose and starting point

Finish the remaining engine progress and SQLite concurrency work identified in
[`docs/engine_cancellation_audit.md`](docs/engine_cancellation_audit.md) and
[`docs/engine_resumable_progress.md`](docs/engine_resumable_progress.md). This is
an implementation brief, not a claim that the tasks below are complete.

The current worktree already contains migration `0018_partial_block_scans.sql`
and related code. Live and lagging block scans checkpoint per tenant and stage
matches until the block hash, payment writes, and cursor commit together. Plain
custody builds key tables in 256-entry batches. Mempool, registration, status,
and vanished-payment passes have bounded rotating work. The production HTTP
state opens two independent read-only SQLite connections for authentication and
status reads; most other `Store` calls still use a shared `Arc<Mutex<Store>>`.
Review and preserve this work. Do not assume it has been committed or deployed.

### Non-negotiable behavior

- A network high-water mark may advance after its block data and selected
  caught-up tenants are handled. Every omitted or failed eligible tenant must
  retain its own cursor and catch up before *that tenant's* cursor advances.
  A new or newly active order must receive the same coverage.
- Keep payment changes, pending recomputation, status transitions, and webhook
  enqueueing recoverable across cancellation and process restart. Delivery is
  at least once; stable `event_id` identifies retries.
- A reorg must re-examine existing payments **and** make replacement blocks
  eligible for forward scanning. Never overwrite the stored losing-chain hash
  before reconciliation finishes. If rewinding empties `scanned_blocks`, retain
  a common-ancestor anchor so startup cannot reseed at the tip and skip blocks.
- Do not hold a database mutex or SQLite transaction across a daemon, custody,
  DNS, or HTTP await. Use short transactions for related writes. A timed-out
  caller does not stop a synchronous SQL command already running on a worker.
- Keep WAL with `synchronous=NORMAL`. The operator accepts possible loss of
  recent commits on OS crash or power loss; do not silently change this policy.
- Scope all cursors, reconciliation queries, and progress records by network.
  Preserve the current behavior for disabled tenants, closed-order grace
  windows, lagging tenants, and terminal orders with pending payment changes.

## Recommended implementation order

1. Establish instrumentation and a reproducible constrained-workload fixture
   (Task 6.1), so later changes have a baseline.
2. Build the database worker interface and migrate writes (Task 2). Keep the
   old API only as a short-lived compatibility layer during migration.
3. Move independent reads to the read pool (Task 4) and make whole-set scanner
   passes bounded (Task 3). These can be reviewed in small slices.
4. Make reorg reconciliation durable and resumable (Task 1). Its state machine
   should use the worker and paging primitives from Tasks 2 and 3.
5. Add retry-safe new-tenant creation (Task 5), then run the full contention
   and failure matrix (Task 6.2–6.4).

The sequence is a dependency guide. A change may land earlier if it preserves
the invariants above and is independently testable.

## Task 1 — Durable, bounded reorg reconciliation

### Context

`check_for_reorg_and_reconcile` in `crates/scanner/src/scanner.rs` detects the
first hash divergence, then loads all non-voided affected payments through
`Store::find_payments_at_or_after_height` and all previously voided payments
through `Store::find_voided_payments_at_or_after_height`. It performs sequential
daemon lookups, recomputes dirty orders, and only then forgets scanned blocks
and clamps tenant cursors. A large payment set can repeat this work every tick.
`revalidate_recent_double_spend_voids` separately loads all recent voids and
can monopolize its background loop. Existing regressions around reorgs,
anchoring, double spends, and webhooks live in `scanner.rs` and `store.rs`.

### Work items

- [ ] Define a persisted per-network reorg job: fork point, losing-chain
      fingerprint/hash, replacement-chain identity or validation marker,
      phase, bounded payment position, and timestamps. Add a migration with
      indexes for each paged payment query. Document what invalidates/restarts
      a job when the daemon changes fork again while work is in progress.
- [ ] Bound and resume *detection* as well as reconciliation. The current
      `get_block_hash` comparison makes one sequential RPC per height in the
      configured reorg window. Save a validation position and its chain
      identity; do not let an incompletely validated window authorize a new
      `paid` transition or settlement webhook. Reconsider tick ordering so
      forward/catch-up scanning cannot act on a fork known to be losing.
- [ ] Replace whole-set payment loads with deterministic keyset pages. Include
      both non-voided and previously voided payments, including rows whose
      `block_height` is `NULL`. Avoid offset paging because writes can move rows
      between pages. Freeze a stable candidate worklist or immutable upper
      watermark for the job: updating `block_height`/`voided_at_utc` changes
      query membership, and new rows can appear behind a saved position.
      Persist each outcome and position atomically where possible, track
      deferred failures explicitly, and perform a final completeness check
      before deleting losing-chain hashes.
- [ ] Process a bounded number of payment outcomes and daemon calls per turn.
      Keep the phase allowance independent of block and mempool phases. Make a
      failed payment retryable without permanently starving later payments.
- [ ] Preserve the webhook/status ordering rule: do not announce `paid` based
      on confirmations from the losing chain while a detected reorg is pending.
      Payment mutations must create durable recompute obligations; status and
      webhook effects must be transactional. Define how the ordinary status
      sweep behaves while a reorg job is incomplete.
- [ ] Complete the final forget/ancestor-anchor/cursor-clamp operation as one
      database transaction after the job is verified complete. Fetch and verify
      the ancestor hash before that transaction; on failure retain the losing
      hashes and job for retry. Keep normal forward/catch-up scans and pruning
      from mutating or deleting the job's required hashes while it is pending.
      Ensure replacement blocks are forward scanned. Bound or stage the final
      rewind if its transaction would otherwise monopolize the writer.
- [ ] Page `find_payments_voided_since` for the slower false-positive recheck
      loop as well. Rotate a bounded cursor, revisit inconclusive rows, and
      avoid retaining an ever-growing candidate vector.
- [ ] Add progress and error metrics/logs: job age, fork point, phase,
      processed/remaining estimate, retry count, and last successful step.

### Acceptance evidence

- [ ] Kill or cancel after each phase boundary and after individual payment
      writes; restart the store and finish without lost/duplicate payments,
      status transitions, or webhook obligations.
- [ ] Exercise a second reorg during an unfinished job, a missing ancestor
      hash, a previously voided payment returning to chain, a `NULL` height,
      cross-network isolation, a replacement-only payment, and a payment
      inserted or changed while the candidate list is being processed.
- [ ] Test a reorg window larger than one hash-validation allowance: restart
      mid-validation and verify that no losing-chain `paid` event escapes.
- [ ] On one CPU with more affected rows than one work allowance, observe
      monotonically advancing persisted progress across turns while ordinary
      requests and other network loops remain responsive.

## Task 2 — Dedicated SQLite writer and bounded database commands

### Context

`SharedStore = Arc<Mutex<Store>>` in `crates/scanner/src/store.rs`. Scanner,
webhook, background-loop, settings, and HTTP write calls commonly execute
`rusqlite` synchronously after `store.lock()`. A slow query or disk stall blocks
the Tokio worker, and Tokio timeouts cannot preempt that operation. The
`ReadStorePool` already shows a bounded-channel, dedicated-thread pattern for
reads. `Store::in_transaction` defers order-change broadcasts until commit;
that property must survive the refactor.

### Work items

- [ ] Inventory production `store.lock()` call sites in `scanner.rs`,
      `loops.rs`, `webhook_delivery.rs`, `main.rs`, and `http/`. Classify each
      as read, write, or a required atomic read-modify-write operation. Record
      expected result size and whether it is currently inside an async path.
- [ ] Introduce one writer-owned SQLite connection on a dedicated thread with
      a bounded request queue and typed async command interface. Set connection
      PRAGMAs on that connection, run migrations before serving work, propagate
      SQL and worker failures, and shut down cleanly. Avoid an unbounded generic
      closure queue in the final public API when typed commands make ownership
      and transaction boundaries clearer.
- [ ] Specify cancellation semantics explicitly: dropping a caller may leave
      an enqueued or executing command running. Use idempotent commands,
      conditional updates, or durable intent for operations whose result can
      otherwise become ambiguous. Keep every required multi-write invariant in
      one transaction. Never hold a worker command open across external I/O.
- [ ] Migrate write call sites in small groups: payment/staging/cursors,
      recompute and webhook enqueue, webhook delivery outcomes, tenant/order
      administration, and settings. Preserve post-commit order-change
      broadcasts and existing unique constraints/triggers.
- [ ] Set bounds for queue length, query result rows, and transaction size.
      Where one SQL statement can affect many rows, add keyset pages or another
      bounded write strategy rather than merely moving a large stall to the
      writer thread. A bounded FIFO queue alone can starve scanner commits
      under sustained HTTP/admin/webhook writes: define per-source admission,
      reserved capacity, or another fair scheduling rule, with bounded enqueue
      waits and retry behavior. Configure SQLite busy handling so a contended
      command eventually yields an error or retry signal.
- [ ] Remove the production shared writer mutex after migration. Audit CLI
      paths separately; `local_admin::bootstrap_wallet` currently receives a
      `&Store` across a custody await and should be split into preflight and
      final database steps.

### Acceptance evidence

- [ ] A deliberately stalled writer leaves the one-thread Tokio runtime able
      to run timers, daemon I/O, and unrelated read requests. Queue capacity
      applies backpressure without unbounded memory growth. Under sustained
      competing writes, payment/cursor commits continue to make progress.
- [ ] Cancellation before enqueue, while queued, during execution, and after
      commit is tested for payment, cursor, order, and webhook operations.
- [ ] Existing scanner, HTTP, settings, webhook, and migration tests pass;
      no database guard is held across an external await.

## Task 3 — Bound remaining whole-set scanner work

### Context

`run_scan_tick_with` calls `Store::active_tenant_ids`, fetches each tenant and
its full `scan_window`, then later calls
`bump_scanned_heights_for_tenant` for every range. `catch_up_lagging_tenants`
loads every lagging tenant and may fetch a full historical scan window per
tenant. `register_missing_wallets_reporting` also lists all active tenants
before selecting 16; the scan loop clones the entire `wallet_handles` map.
The bounded mempool and status pages do not bound these earlier
SQL/materialization steps. See `store.rs`, `scanner.rs`, and `loops.rs`.

### Work items

- [ ] Add indexed, deterministic keyset pages for active and lagging tenants
      and registration candidates. Check query plans against the real schema;
      add only indexes that improve the measured workload.
- [ ] Define a per-network fair scheduler with durable or restart-safe cursors
      for tenant eligibility and catch-up. Bound tenants and scan-window rows
      materialized per turn. Respect live versus lagging status, network scope,
      registration state, and disabled tenants.
- [ ] Specify exactly how omitted active tenants are marked left behind. A
      network high-water mark may advance for other tenants, but an omitted
      tenant's own cursor must remain behind and catch-up must cover every
      missed block before its cursor advances. Include tenants/orders created
      while a page is being processed. In particular,
      `Store::advance_caught_up_cursors` currently advances *all* tenants at
      `height - 1` except an explicit `left_behind` list. Change this to an
      explicit scanned-tenant allowlist or equivalent transactionally derived
      eligibility before paging the active list; otherwise omitted tenants
      silently skip blocks. Avoid materializing a huge omitted-tenant list.
- [ ] Bound very large single-tenant scan windows without losing a match.
      Design a stable window generation/snapshot and resumable index pages;
      resetting progress on a changed window is acceptable only if repeated
      changes cannot cause permanent starvation. Keep completed key-table
      batches reusable.
- [ ] Page scanned-range updates and other bulk writes. Preserve
      `first_scanned_height` set-once behavior and ensure
      `last_scanned_height` never falsely claims coverage for an omitted
      tenant or order. Measure and bound `anchor_unset_cursors`,
      `clamp_cursors`, `snap_disabled_cursors`, `prune_scanned_blocks_below`,
      and any network-wide reorg rewind; moving these whole-network statements
      onto a worker does not by itself bound writer occupancy.
- [ ] Account for work by operations and wall time, not only an outer Tokio
      timeout. Expose per-phase deferred counts and oldest lag so an operator
      can distinguish slow progress from stalled progress.

### Acceptance evidence

- [ ] A tenant population larger than one page, a tenant window larger than
      one page, and many lagging tenants all make measurable forward progress
      across ticks on one CPU.
- [ ] With more active tenants than one page, verify that tenants omitted
      from this turn's scan keep their old cursor and subsequently catch up;
      repeat while a new order is created during the turn.
- [ ] Inject continually failing and continually changing tenants; healthy
      tenants and other networks still advance. An order created during a
      partial pass receives catch-up and cannot be skipped.
- [ ] Verify memory/result-size bounds and scan-range fields against the
      existing grace-window and cursor tests.

## Task 4 — Finish migration of reads away from the writer lock

### Context

SQLite in WAL mode permits concurrent reads on separate connections. The old
`store.lock()` serialized reads because it guarded a *single* `rusqlite`
connection, not because SQLite requires every read to serialize. The current
`ReadStorePool` in `store.rs` has two independent read-only connections with
bounded queues. `AppState::read_store` in `http/mod.rs` is used for bearer-token
authentication and status-page queries, but many other handlers and scanner
reads still call `state.store.lock()` or `store.lock()`.

### Work items

- [ ] Use the Task 2 inventory to migrate independent HTTP GET/auth/detail,
      scanner selection, and background-loop reads to read workers. Keep
      transactionally coupled read-modify-write decisions on the writer;
      splitting those onto a reader could introduce a race.
- [ ] Replace generic read closures where useful with named queries and bounded
      result types. Ensure a cancelled request cannot strand an owned connection
      or grow the queue. Apply read-only/query-only PRAGMAs on each connection.
- [ ] Define consistency expectations: a reader sees a committed snapshot;
      critical post-write responses should use the writer's returned row or a
      follow-up read after commit. Preserve tenant/network authorization checks
      as data changes concurrently.
- [ ] Exercise WAL checkpoint behavior under sustained readers and writer
      traffic. Keep individual read transactions short; expose queue wait and
      query duration. Tune pool size using measurements rather than increasing
      it merely to mask long queries.
- [ ] Remove stale comments claiming all `Store` access uses one lock; keep
      tests for simultaneous readers, reader/writer overlap, and isolation.

### Acceptance evidence

- [ ] All read-only production call sites are documented as migrated or as
      deliberately atomic parts of a writer command. No ordinary read takes
      the writer mutex.
- [ ] Under a slow read and an active writer, another reader can proceed;
      authorization and order responses never use uncommitted state.

## Task 5 — Retry-safe new-tenant registration

### Context

Existing tenants re-register through
`KeyCustody::unseal_and_register_in_idempotent` using their stable tenant ID.
`http::admin::create_tenant` still calls unscoped `register_wallet_in`, derives
the address, seals material, then calls `Store::create_tenant`. Cancellation
after a remote register but before the response or database insert can leave a
wallet with no tenant row. Also, `create_tenant` currently mints an admin
secret and stores only its hash: replaying a successful HTTP response after
the client loses it requires an explicit secret-response strategy. The actual
caller, `crates/monokulo/src/http/connections.rs`, creates an engine tenant
before inserting its local `store_connections` row; a failed insert or lost
engine response can orphan a tenant even after the engine endpoint is made
idempotent. `crates/monokulo/src/engine_client.rs` currently sends the request
without a retry key.

### Work items

- [ ] Define the API idempotency contract: required caller-supplied key
      (header or request field), scope, validation, retention, and response to
      reuse with different key material or settings. The engine tenant-creation
      POST currently has no auth header; if a retry key can replay the original
      `secret_token`, treat the key as a bearer secret or add authenticated
      provisioning. Require high entropy, reject short/predictable keys,
      constrain replay lifetime, and never log keys or replayed credentials.
      Update the engine caller that provisions tenants, not just the endpoint.
- [ ] Before coding, choose and document a secure exact-response replay
      strategy for `secret_token`. Do not persist it as plaintext. A retry
      must either return the original usable credential or follow an explicit
      safe recovery protocol; returning an inaccessible tenant is insufficient.
- [ ] Add a durable registration intent with stable tenant/registration ID,
      request fingerprint, backend, phase, and cleanup metadata. Commit it
      before contacting custody. Use the existing idempotent custody protocol
      with that ID; concurrent retries must converge on one intent and wallet.
- [ ] Change the current register-then-seal sequence to seal first and call
      `unseal_and_register_in_idempotent`, or extend the custody protocol with
      an equivalently idempotent raw-material registration operation. Merely
      adding an HTTP idempotency key around `register_wallet_in` leaves the
      ambiguous remote-wallet outcome in place.
- [ ] Make the address derivation, sealing, tenant insertion, intent completion,
      and wallet-handle cache updates retryable. Define the compensation/reaper
      path for failed or abandoned intents and for backend restarts. Do not
      remove a handle that another completed tenant now owns.
- [ ] Add a durable, user-scoped provisioning intent in monokulo before the
      engine request. Persist the stable engine idempotency key and enough
      request/response state to retry the same request, recover the encrypted
      credential, and finish the local `store_connections` insert exactly once.
      A new random local connection ID on each retry is insufficient. Cover
      all three callers of `create_connection_for_user`: `POST /connections`,
      the dashboard connect path, and `http/connect.rs`'s connect flow.
- [ ] Bound every remote step end to end. Persist enough state that a process
      restart can resume or clean up without relying on an in-memory map.

### Acceptance evidence

- [ ] Cancel or drop the response at every remote and database boundary;
      retries return one tenant and a usable credential, with no permanent
      orphan wallet. Concurrent same-key retries converge; conflicting reuse
      is rejected. Restart between phases and repeat on both custody backends.
      Inject failure after the engine response but before monokulo's local
      insert; retry must finish one local row for that same engine tenant.
- [ ] Existing tenant creation, custody switching, bootstrap, authentication,
      and offboarding tests still pass.

## Task 6 — Constrained-load measurement and failure validation

### 6.1 Fixture and baseline

- [ ] Add a versioned scenario file and fixed seed for a reproducible
      benchmark/soak fixture with configurable tenant, order, scan-window,
      block-transaction, mempool, reorg-payment, and recent void counts.
      Include at least one dataset larger than every configured page/work
      allowance. Keep generated payment/key material deterministic. Record the
      fixture schema version, seed, scenario checksum, and workload parameters
      in every result so the run can be reproduced exactly.
- [ ] Use a fresh file-backed SQLite database per scenario with the engine's
      real migrations and WAL/`synchronous=NORMAL` settings. Run real scanner,
      store, status, and custody code against deterministic daemon/custody
      fixtures with specified latency and failure schedules. Preserve the
      actual key-scanning and SQL costs; label any mocked or omitted subsystem
      prominently. Do not use `:memory:` for disk/lock capacity claims.
- [ ] Record the host, one-CPU pinning method, SQLite journal/PRAGMAs, dataset
      sizes, daemon/custody latency model, and baseline measurements before
      setting numeric service objectives. Do not call an arbitrary toy fixture
      “production-size.”
- [ ] Give every scenario a stated warm-up interval, measured interval, and
      bounded drain interval. Keep setup/compilation and data generation out
      of throughput timings. Use the same seed and input schedule at each load
      point; retain failed or interrupted points in the report.

### 6.2 Progress and responsiveness

- [ ] Measure per-phase work completed, oldest backlog age, ticks to catch up,
      async timer delay, database queue wait/query time, and HTTP read latency.
      Include WAL file size and checkpoint lag under long and sustained reader
      traffic. Run with one CPU and concurrent reads, writes, and daemon/custody
      work.
- [ ] Establish explicit acceptable budgets from the baseline and intended
      hardware. Assert no starvation: every healthy eligible tenant, payment
      page, reorg job, and network makes progress across bounded turns even
      while another item repeatedly fails or times out.
- [ ] Sweep tenant load geometrically to bracket capacity within the CI time
      budget. Specify active fraction, open orders per active tenant, payment
      and block arrival rates, and read/write contention for each point.
      Classify a point as sustainable only when backlogs do not grow during
      measurement, all eligible work advances, and stated latency/lag budgets
      hold. Report the highest observed sustainable point and the first failing
      point as a bracket; if no point fails, say “at least the largest tested.”
      Never present total tenant count alone as a universal server limit.

### 6.3 Fault matrix

- [ ] Inject SQLite busy/lock contention and slow disk commands, daemon RPC
      delays/failures, custody slot saturation, cancellation at await points,
      process termination, changing block hashes, and concurrent order creation.
      Reopen the file-backed database and verify cursors, payment/status rows,
      pending obligations, and webhook queue invariants.
- [ ] Distinguish process-crash recovery from power-loss durability. Power-loss
      of recent WAL `NORMAL` commits is accepted; tests must not claim otherwise.

### 6.4 Reproducible `xtask` run and visual CI artifact

- [ ] Extend `xtask/src/main.rs` with documented entry points such as
      `cargo xtask stress ci`, `cargo xtask stress full`, and
      `cargo xtask stress open`. The `ci` profile is a bounded one-CPU capacity
      sweep plus contention/failure checks; `full` runs longer local sweeps.
      Select the first CPU allowed by the process affinity/cgroup instead of
      assuming CPU 0 exists. Limit concurrency explicitly; record the actual
      CPU affinity and cgroup quota used.
- [ ] At run start, capture a machine profile into
      `target/coverage/stress/hardware.json`: OS/kernel, CPU model, logical and
      effective cores, affinity and quota, available and cgroup-limited RAM,
      storage/filesystem information relevant to the test database, Rust and
      SQLite versions, and the engine revision/dirty state. Use the limits
      visible to the process as well as host totals. Omit hostnames, serial
      numbers, cloud instance IDs, and other unnecessary identifiers. Put a
      readable **“Hardware used for this run”** panel at the very top of
      `target/coverage/stress/index.html`, before any charts or capacity claim.
- [ ] Write machine-readable per-scenario results and time series under
      `target/coverage/stress/` (JSON plus CSV where useful). Include commands,
      scenario version/seed/checksum, start time, duration, pass/fail reason,
      hardware profile, raw metrics, and paths to bounded logs. Write results
      atomically after each point so a later failure leaves completed evidence.
- [ ] Generate a self-contained offline HTML report from those files with
      escaped data and relative links only. Include a visual load-versus-lag
      and throughput chart, p50/p95/p99 latency charts or tables, progress and
      starvation timeline, DB queue/WAL panels, the capacity bracket and its
      exact workload definition, and visible failed/skipped/incomplete points.
      Inline SVG/CSS is sufficient; viewing the report must not require a
      server, CDN, network access, or benchmark tool installation.
- [ ] Integrate with the existing coverage asset rather than creating a
      disconnected report: run `cargo xtask stress ci` after
      `cargo xtask coverage all` in `.github/workflows/coverage.yml`, add a
      “Engine stress” link/status to `target/coverage/index.html` and
      `target/coverage/run.json`, and keep `target/coverage/stress/` in the
      existing `actions/upload-artifact` upload. Keep both collectors' partial
      output when one fails; use `if: always()`/`continue-on-error` as needed
      to upload diagnostics, then fail the job for correctness or missing
      report files. Increase or split the workflow timeout only based on a
      measured CI run; do not silently drop scenarios to fit it.
- [ ] Document local reproduction, CI profile bounds, report interpretation,
      and hardware comparability in `docs/`. Hosted CI runner hardware and
      contention can change, so show each run's machine profile and do not
      compare throughput as a regression across unlike profiles. Gate CI on
      correctness, required progress, valid output, and a maximum runtime;
      keep a numeric tenant-capacity threshold informational until repeated
      measurements on controlled hardware justify one.

### 6.5 Gates and report

- [ ] Run `taskset -c 0 cargo test -p scanner -p key-custody-service
      -p key-custody-server --tests --locked -- --test-threads=4`, relevant
      integration tests, `cargo check`/Clippy for touched crates, and
      `git diff --check`. Note pre-existing Clippy warnings separately.
- [ ] Publish measured results and remaining limits in `docs/`; state the
      hardware and workload for every claimed bound. Update the original
      audit/follow-up docs only after the corresponding acceptance checks pass.
- [ ] Validate the generated stress HTML by opening the downloaded CI artifact
      locally with networking disabled. Check that hardware appears first,
      charts and links render, raw data agree with plotted values, and a
      deliberately failed scenario remains visible instead of vanishing.

## Scope boundary

The bundled WooCommerce receiver's non-atomic check-and-save of `event_id`
under simultaneous deliveries is a separate receiver-side task. It does not
change the engine's at-least-once delivery contract. Keep it visible in the
handoff, but do not treat it as a SQLite engine-lock task.
