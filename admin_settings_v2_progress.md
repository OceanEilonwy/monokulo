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
| 1.5 remove settings | done | 8830c92, 266f19a | unknown keys refused; old read path deleted |
| 1.3 monokulo on live-settings | done | 66f32ea | |
| 3.1-3.4, 3.6 | done | 66f32ea | engine retarget ends old streams; onion listener live; bind note in the restart banner |
| 3.5 pin already-live settings | partly | | existing signup/public_url tests cover it; no new ones |
| 3.7 merchant alerts | done | d7e7883 | |
| 2.2 unserved networks | done | d7e7883 | |
| Part 4 admin page | done except Playwright | 66f32ea | view tests; Playwright page tests wait for part 6 |
| independent review of 8830c92 + 66f32ea + d7e7883 | all items applied or decided | 5b980b5, e990f7f, see git log | item 1 (--help touching the DB) fixed in part 5 main.rs rework |
| Part 5 engine side (5.1 router, 5.2 live settings, 5.3 choose/switch API, 5.5 status) | done | 95f5991 | |
| Part 5 monokulo side (5.4 backend choice, 5.6 Key storage section, 5.7 alerts) and bootstrap CLI flag | done, reviewed, fixes applied | see git log | review found 1 blocking bug (socket path change stranded socket stores), fixed with tests |
| Review items 2 (chunked body limit) and 3 (alerts don't flap) of 8830c92/66f32ea/d7e7883 | done | 5b980b5 | |

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
- 1.3: monokulo readers of per-request settings (signup mode, public URL)
  use a typed `settings::get(db, &SETTING)` rather than a Live section:
  they were already read per request, and the registry writes the same
  table. Everything with runtime state goes through the registry.
- Part 4: parse errors show the library's messages ("Enter a whole number,
  0 or more." rather than the range) - accurate, slightly less specific.
- 3.7: the alert covers both "no reachable node" and "catching up" (the
  latter phrased gently); custody reasons join in part 5.
- D10 design details (after review): orders get `closed_at_utc`; the
  window is "non-terminal, or closed within the grace period"; the custody
  API gains an index-set scan call plus a protocol version, falling back
  to a min..max range for older sidecars.

- Part 5: the old single `key_custody.backend` setting is replaced by
  `key_custody.enabled_backends` (list) and `key_custody.default_backend`,
  both applied live. A one-time migration (marker row
  `migration.key_custody_per_store`) carries the old effective value over
  and labels every existing store with it, so nothing changes for an
  instance that doesn't touch the new settings.
- Part 5: `CustodyRouter` is itself a `KeyCustody`: every existing caller
  keeps working, and it routes each handle to the backend that issued it.
  Disabling a backend drops its stores' handles; they're left unserved
  (reported as `custody_disabled`) and come back from their sealed keys
  when it's enabled again. Nothing is ever deleted by disabling.
- Part 5: a store moves with `PUT /api/v1/admin/tenant/key-custody` and
  its keys entered again. The keys must match the store's own wallet
  (spend key, view key and network compared against the primary address);
  the new registration is made and the row updated before the old one is
  removed, so there is always a valid handle. A request that finds its
  handle gone mid-flight re-resolves it up to 3 times (order creation,
  payment lookup); the scan loop's per-store cursor already covers a
  skipped tick.
- Part 5: `/status` gains `key_custody` (each enabled backend and whether
  it answers, with a 2s deadline) and the unserved reasons
  `custody_disabled`/`custody_unavailable`; monokulo alerts the owner.
- Part 5: `GET /api/v1/admin/key-custody` lists enabled backends with a
  plain description and the default. It's on the private engine API, so
  monokulo proxies it; no auth needed beyond reaching the engine.

- Part 5 (monokulo): the backend choice appears on the add-store forms only
  when the engine offers more than one; the list comes from the cached
  engine status (`/status` now carries `key_custody` and
  `key_custody_default`), so forms stay synchronous and work with no JS.
  Labels describe where keys live rather than naming backends.
- Part 5 (monokulo): "Key storage" on a store's settings page shows where
  the keys are and a plain form to move them, with the keys typed again.
  The fields are always empty on render, including after an error. It is
  shown whenever there is somewhere else to move to, including when the
  store's own backend was turned off (then with a red notice).
- Part 5: `--bootstrap-wallet` takes `--key-custody-backend` and now
  checks that `--primary-address` is the wallet of the given keys on the
  given network. Verified on the real binary with dev-run.sh's stagenet
  wallet (accepted) and the same keys on mainnet (refused, nothing made).

- Review of part 5 (independent agent): one blocking finding, fixed.
  Changing `key_custody.socket_path` built a new socket client but kept
  the old handles, which the new server never knew, so socket stores were
  never scanned again until a restart. Now the router drops (and frees, in
  the background) the handles of any backend instance that is replaced or
  removed, and any `UnknownWallet` from a backend makes the router forget
  that handle, so the scan loop registers the store again on its next tick
  whatever made the backend lose it. A mutation check confirmed the new
  scanner test fails without the fix.
- Review of part 5, should-fix items applied: switches are serialised (one
  lock, since switches are rare) so the row and the live handle always
  name the same backend; a disabled store can't be moved (the UPDATE
  checks `disabled_at_utc`); handles dropped after a backend restart are
  freed there too; a failed removal from the old backend is logged; the
  engine logs a loud warning at boot for stores whose backend isn't
  enabled.
- Review of part 5, not changed: the unauthenticated
  `GET /api/v1/admin/key-custody` (backend names and fixed descriptions
  only, on the private engine), agreed acceptable by the reviewer.

- Review item 4: the loop manager and the loops moved to
  `scanner::loops`, tested (loops start and stop with saved node
  settings; a store is registered again within seconds after its backend
  is replaced). The manager is now supervised: if it panics, dropping it
  stops the loops it started (their stop senders go with it), and its
  restart starts them again.
- Minor review items: the scanner status re-insert race is fixed (a loop
  doesn't record a tick for a network whose node was just cleared);
  save_monokulo errors are logged; the engine admin token (the only secret
  setting) gets a "Clear it" box, since an empty field has to mean "keep";
  retarget does nothing when the URL and cache size are what they already
  are; the status cache remembers which engine URL it came from, so an old
  engine's status (and its alerts) are never shown for a new one; the
  unused live handle for the per-request section is removed (the section
  stays, because every setting must belong to one).
- Onion listener A-B-A: a real bug, not just a race. The acceptor task
  kept the socket until the next connection arrived after the listener was
  dropped, so moving back to an address it had left failed. It now exits
  as soon as the listener is dropped; binding also retries briefly while
  the address is still in use.
- Not changed: retrying NodesReloadable after a boot failure. Its prepare
  can only fail if the TLS backend can't initialise, which affects every
  network the same way and needs a fix to the machine, not a retry.
- Not changed: the test harness's own rate limiter is still separate from
  the settings' one, on purpose, so tests keep their generous limit
  (test-only; no behaviour in the product depends on it).

- 1.5: every engine setting is now read through `EngineSettings`. The
  scan tick gets the chunk memory budget from the live scan settings
  (passed in, so a saved change applies to the next tick); bootstrap reads
  the tenant defaults through a new `engine_settings::read_section`, the
  same validation and fallback as boot. The old scalar machinery in
  `scanner::settings` (and `shared::settings`'s env/db/default resolver)
  is deleted; what's left there is the saved node's shape, the private
  bind check and the environment access with its test overrides. The test
  count went down by 12 with it: those tests covered the deleted code.

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
After d7e7883: 1018 passed, 0 failed, 21 ignored.
After part 5 engine side: 1031 passed, 0 failed, 21 ignored.
After part 5 complete: 1036 passed, 0 failed, 21 ignored.
After part 5 review fixes and review items 2-3: 1044 passed, 0 failed.
After review item 4 and the minor items: 1050 passed, 0 failed.
After 1.5 (old settings code deleted with its 12 tests): 1038 passed, 0 failed.

## Current step

Part 6: dev-run.sh (key_custody.enabled_backends, comments), docs
(DESIGN.md 8.1, README, KeyCustody module docs), 6.0 real-binary harness,
6.3/6.4 end-to-end tests, Playwright admin page tests on the
coverage_fixture, real-process kill -9 test.
