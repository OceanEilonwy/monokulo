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
| 1.1 live-settings crate | delegated to a sub-agent in a worktree | | verify its work before merging |
| 7.6 node failures | next | | |

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

## Current step

7.6 (node cooldown, per-call deadline, one node per tick, response size
limit) in `daemon_fallback.rs`/`daemon_rpc.rs`. The live-settings crate
(1.1) is being built by a sub-agent in a separate worktree; when it
reports, review its diff and tests, then merge into this branch.
