# Engine stress fixture

`cargo xtask stress ci` runs the bounded one-CPU fixture and `cargo xtask stress full`
runs the larger sweep. `cargo xtask stress open` opens the offline report at
`target/coverage/stress/index.html`. CI runs the `ci` profile in the
`coverage-stress` job and links the report from the coverage index.

An optional third argument names the scanner *driver* to measure, for example
`cargo xtask stress ci legacy`. A named driver writes its report to
`target/coverage/stress-<driver>/`, so two engines can be compared on the same
machine and scenario. Without one, the production driver is measured.

The fixture and report were first written for an experimental hardening branch.
They were ported here with the same scenario file, so their numbers stay
comparable. That branch's engine changes were not taken.

## What runs

The versioned scenario is `xtask/stress/scenario_v3.json`. The report records its
SHA-256 checksum, seed, run profile, driver, hardware, exact command, and raw JSON
for each tenant count.

Each point:

- starts with a new file-backed SQLite database, using the real engine migrations,
  WAL and `synchronous=NORMAL`;
- registers real plain-custody wallets;
- runs the scanner against a scripted daemon, on one allowed CPU with a two-worker
  Tokio runtime (the `server.worker_threads` default);
- excludes setup and compilation from the measured tick duration.

The workload is deterministic: wallet keys, tenant and order IDs, transaction data,
the block arrival schedule, and one transaction per block. The first tenant has 300
open orders, so every point also exercises a large scan window. The other tenants
have two orders each.

While the ticks run, other work competes with the scanner:

- two read-only SQLite workers;
- two in-process HTTP `/status` readers;
- one admin writer that saves settings through the engine's own write path;
- a separate connection that holds SQLite's write lock for 25 ms at the start of
  each tick.

Progress is measured over the fixture's own read-only connection, never through
engine APIs, so it stays valid while the engine changes: the network high-water
mark, lagging tenants, and the slowest tenant cursor.

## Fault points

Each fault point runs a short scheduled fault, then six drain ticks. Every tenant
must recover during the drain.

- **RPC:** 2 ms added to every scripted daemon RPC, and a transient failure on every
  third RPC through block height 2.
- **SQLite lock:** a 1.5-second write lock during the first two ticks. This models a
  blocked writer, not a slow storage device or a power failure.
- **Custody:** real plain-custody scans limited to one slot with a scripted 5 ms
  service delay.

## Reading the results

The hardware panel appears before the charts. It includes CPU affinity and the
cgroup limits visible to the process. Hosted runner results are comparable only
when these and the scenario agree.

Each point gets one of three statuses:

- **Sustainable:** the measured ticks fit the configured interval, the backlog does
  not grow, and the oldest lag stays within the scenario allowance.
- **Overloaded:** an informational capacity observation.
- **Failed:** a fixture error, a tenant that never advances, or an unresponsive
  process (HTTP latency or timer delay over the scenario limit). A failed point
  fails CI.

The report keeps every completed point even when a later one fails.

This fixture measures scanner, custody and SQLite CPU, and tick progress. It does
not inject slow disk commands, process kills, reorgs or custody failures, so it
must not be used as a universal tenant-count limit. The engine's own tests cover
those failures. Under the accepted WAL `NORMAL` policy, recent commits can be lost
on an OS crash or power loss. Process-crash recovery is a separate property.

## Results

Measured results for each driver are recorded in `docs/scanner_microtasks.md`.

## Round length sweep

`cargo xtask stress rounds` measures what the scheduler's round length
(`work::ROUND_BUDGET`) costs and buys. `round_sweep` runs the production
scheduler through a backlog of blocks from a scripted node, rounds back to
back as the engine's loop runs them, pinned to one CPU and built in release
(the sweep weighs scan time against link time). The scenarios are in
`xtask/stress/round_sweep_v2.json`:

- **The node's link** has a round trip, a time to first byte per block and a
  transfer rate, and reports the rate as a measured client does, so block
  requests are sized as in production. It answers the tip with its id and
  the pool in one request, as `RpcDaemonClient` asks them.
- **Blocks** are 13 kB (today's pruned average) and each pays tenant 0, so
  every block records a payment and recomputes an order.
- **Tenants** start in one group, or in several spaced through the backlog
  (catch-up groups sharing the block cache).

Each point reports:

- **Blocks a second**: each group's blocks, averaged over its tenants.
- **Sent per distinct**: blocks the node sent over distinct blocks sent.
  Above 1 is blocks fetched ahead and let go of before their scan.
- **Round p50 and longest round**: settlement and the mempool get a turn
  once a round, so the longest round is about the longest they wait.
- **Idle round**: a round with nothing left to scan, the fixed cost.
- **Discarded**: bytes the block cache let go of before a scan of them committed.

`--round-budget-ms` changes the round's length only. Per-call times are
shares of `ROUND_BUDGET` itself, fixed at build time (`Tier::reserved`). To
measure a different `ROUND_BUDGET` whole, set it in `work/mod.rs` and run
`ROUND_SWEEP_BUDGETS_MS=<the same, in ms> cargo xtask stress rounds`. Each
run writes to its own `target/coverage/stress-rounds-<ms>`.

### What it found

Recorded runs: `docs/stress/round-budget-sweep-baseline.json` (before the
block cache outlasted the round) and `docs/stress/round-budget-candidates.json`
(each round length with `ROUND_BUDGET` set to it).

- **The block cache was per round.** A run fetched ahead, sized to the
  link's 4 s target, was dropped when its round ended and fetched again. At
  2 s rounds the node sent 12 to 19 times the distinct blocks for one group
  over 2 Mbit/s and 256 kbit/s links; at 10 s, 1.3 to 1.4 times. The cache now outlasts
  the round within the scan memory budget (docs/engine_scaling.md, "As
  built"): 1.00 at every length for one group.
- **Catch-up groups evicted each other's runs.** With 16 groups, each
  group's 1 MB run (the response cap at the default 8 MB budget) didn't fit
  beside the others: 4.7 blocks a second and 9 MB discarded at 10 s. Each
  group's request is now capped at its share of the budget: 13.4 blocks a
  second and 130 kB discarded.
- **The round length** then barely matters over a nearby node, and matters
  over a high-latency one. That sets 10 s; the reasoning is on
  `ROUND_BUDGET`.

Not covered: the scripted node answers instantly apart from its link
model, so monerod's own time to build an answer is only the time to first
byte per block. A thousand tenants on one CPU are bound by scanning
(about 5,000 tenant-blocks a second).
