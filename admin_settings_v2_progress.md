# Admin settings v2: progress notes

Resume from here. The plan is `admin_settings_v2.md`. Work happens on the
local branch `admin-settings-v2`; each finished task is one or more local
commits. Nothing is pushed.

## How to resume

1. `git checkout admin-settings-v2 && git log --oneline main..` shows what is
   done.
2. Read "Current step" below, then the task in `admin_settings_v2.md`.
3. Before committing any task: `cargo test --workspace` must pass (see
   "Baseline" for failures that already existed on `main`).

## Order of work

1. 7.1 panics can't take the engine down
2. 7.12 load and chaos harness (the base, extended as later tasks land)
3. 5.0 per-store scan cursor, together with 7.4 fair and parallel scanning
4. 7.2 scanning off the async runtime, 7.3 no repeated work and scan window
5. Part 1 live-settings library, then parts 2, 3, 4
6. Rest of part 5 (per-store key custody), part 6, rest of part 7

## Status

| Task | Status | Commit | Notes |
|---|---|---|---|
| WBS written and reviewed (two passes) | done | 7b1e47a | |
| Part 7 review, folded into WBS | done | 6cd5432 | D10 redesigned: `closed_at`, index sets in the custody API, catch-up window |
| 7.1 panics can't take the engine down | done | 97817e1 | parking_lot locks in engine, test support and monokulo; 3 new tests |
| 5.0 per-store scan cursor | done, reviewed, fixes applied | 6ded980, 6f5e5cc | review found 2 blocking bugs (grace-period payment skipped by snap; disabled tenant orders never expired), fixed with tests |
| 7.12 load and chaos harness (base) | partly: the randomised outage/reorg test in 5.0 is its seed; scale runs still to do | | |
| 7.8 fair webhook delivery | done | a9dee72 | |
| 7.9 supervisor backoff + tick deadline | done | 8762803, 6f5e5cc | restart counts on /status |
| 7.4 fair concurrent scanning | done | 6f5e5cc | per-network loops; per-call 10s deadline; per-network isolation not unit-tested (loops live in main.rs; covered once 1.4 moves boot into the library) |
| 1.1 live-settings crate | done | efef6bf | built by a sub-agent (it stopped at a usage limit after finishing the code); reviewed, 34 tests, clippy clean, merged |
| 7.6 node failures | done | 130c432 | cooldown, 30s call budget, pinned node per tick, 64MB response cap |
| 7.10 HTTP limits | done | 242ffba | constants for now; become live settings with part 1.2 |
| 7.7 database failures | done | 719dc2e | transient store/custody errors are 503 |
| 7.11 crash safety, SIGTERM | done (in-process); real-process kill -9 variant waits for the 6.0 harness | 3c9e77f | |
| 7.2 scanning off the async runtime | done | 298b928 | blocking pool, one scan per core |
| 7.3 window (D10), closed_at, index-set API, no table copy | done | 298b928 | socket backend uses the trait's default (covering range) until its wire protocol gains index sets |
| 7.3 mempool memory | done | 02b0d0f | |
| 7.3 socket wire protocol for index sets | done | c51dc26 | old servers fall back to range scans |
| 7.5 socket connection pool, reconnect | done | c51dc26 | |
| 5.8 socket recovery (canary, re-register, start degraded) | done | c51dc26 | the per-backend parts of 5.8 (registry, CAS on backend name) come with part 5 |
| 7.12 scale harness | done (ignored test) | see log | 1000 stores: cold 6.0s, warm 0.11s, block 0.21s; 5000: cold 30s, warm 0.52s, block 1.05s (release, this machine) |
| 7.13 status and logging | done (partly) | 40dbbc5 | status fields + throttled logs; no move to structured logging |
| 1.2 engine on live-settings | done | 8830c92 | |
| 2.1 nodes live (the reported bug) | done | 8830c92 | verified on the real binary |
| 2.2 unserved networks | partly | 8830c92 | save reports networks with stores but no node; reachability probe and /status list still to do (with 3.7) |
| 2.3-2.6, 2.8, 2.9 | done | 8830c92 | |
| 2.7 bind restart-only | done | 8830c92 | |
| 1.5 remove settings | partly | 8830c92 | unknown keys refused; old settings.rs still used by the scan chunk budget and bootstrap CLI |
| 1.3 monokulo on live-settings + part 3 | next | | |

## Decisions made while working

(Each with the reason. The user asked for no further questions; these are
reported at the end.)

- D10 (from the user): scan window per store is orders open or closed
  within the grace period. Late payments go through payment lookup.
- 7.1: also converted monokulo's locks to parking_lot, not only the
  engine's. Same poisoning failure mode, and monokulo's tests lock the
  engine's store anyway. Cheap and mechanical.
- 7.1: kept `expect` only where a failure is impossible or at boot (a
  listener that can't bind ends the process, by design). Each is allowed
  with the reason written next to it; the scanner crate now denies
  unwrap/expect outside tests.
- 5.0: catch-up checks each block against the hash the live scan stored
  (where still stored). `get_blocks_range` doesn't return block hashes, so
  this checks the node's current view, not the fetched data itself; full
  protection against mixed forks comes with 7.6 (pin one node per tick).
- 5.0: the `/status` report of lagging tenants is folded into 3.7's
  `unserved_tenants` list rather than built twice.
- 5.0: order of work changed slightly: 7.12's full scale harness comes
  after 7.2/7.3/7.4, which it measures. The randomised correctness test
  landed with 5.0.
- Review of 5.0 (independent agent): applied all blocking and should-fix
  items. Not applied: genesis-reorg backlog loss (impossible on a real
  chain); deep stale hash between the reorg window and the prune window
  pausing catch-up (recovers by itself once pruned).
- 7.4: within a tick, tenants are scanned concurrently in one task. That
  helps slow I/O backends (socket) but gives no CPU parallelism for the
  in-process backend; that is 7.2.
- Transactions with no tx public key or with script outputs are treated as
  "no match" by `PlainKeyCustody` (they can't pay any wallet); other
  output-check errors still fail the scan for that tenant.
- 7.3: closed_at for an expired order is its deadline (not when expiry was
  noticed), so held expiry during a store's gap doesn't stretch its window.
- 7.3: catch-up updates the same per-wallet live table to its (as-of-cursor)
  window rather than building a separate one; the difference is usually
  a few indices, so it's cheap, and it keeps one code path.
- 7.3: a paid order now stays in scan scope for the grace period after it
  closes (overpayments within it are seen). Three existing tests encoded
  the old "paid leaves scope at once" rule and were updated.
- 1.2: "configured network" now means "has a daemon client" (one source
  of truth). Two monokulo tests encoded the old "configured, no node"
  state and were updated; test-support gives configured networks an inert
  client.
- 1.2: monero_node.<network> gained environment variables
  (SCANNER_MONERO_NODE_<NETWORK>), because the library requires every
  setting to have one. Harmless and occasionally useful.
- 1.2: the body limit is checked on declared/exact length each request;
  a fixed 16MiB outer ceiling still applies to bodies of unknown length.
- D10 design details (after review): orders get `closed_at_utc`; the
  window is "non-terminal, or closed within the grace period"; the custody
  API gains an index-set scan call plus a protocol version, falling back
  to a min..max range for older sidecars.

## Baseline

`cargo test --workspace` on `main` (43d7c53 + dc2d976): 913 passed,
0 failed, 20 ignored. Needs `npm ci` in `crates/monokulo/pos-ui` first
(the monokulo build script requires the POS app's node_modules).

After 7.1: 916 passed, 0 failed, 20 ignored.
After 6f5e5cc: 940 passed, 0 failed, 20 ignored.
After 719dc2e: 986 passed, 0 failed, 20 ignored.
After 02b0d0f: 994 passed, 0 failed, 20 ignored.
After c51dc26: 1000 passed, 0 failed, 20 ignored.
After 8830c92: 1011 passed, 0 failed, 21 ignored.

## Current step

1.3 (monokulo onto live-settings) with part 3 (engine URL + HTTP cache,
exchange rates, onion listener live; engine-unreachable warning; bind
note), then part 4 (admin page), part 5 (per-store key custody), part 6.
