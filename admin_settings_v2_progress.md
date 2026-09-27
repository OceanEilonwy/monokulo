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
| 7.12 load and chaos harness (base) | next | | |

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
- D10 design details (after review): orders get `closed_at_utc`; the
  window is "non-terminal, or closed within the grace period"; the custody
  API gains an index-set scan call plus a protocol version, falling back
  to a min..max range for older sidecars.

## Baseline

`cargo test --workspace` on `main` (43d7c53 + dc2d976): 913 passed,
0 failed, 20 ignored. Needs `npm ci` in `crates/monokulo/pos-ui` first
(the monokulo build script requires the POS app's node_modules).

After 7.1: 916 passed, 0 failed, 20 ignored.

## Current step

7.12 base harness: a load and chaos test helper in `scanner-test-support`
(payment generator for many stores, fake custody with latency and
failures), then 5.0 + 7.4 together.
