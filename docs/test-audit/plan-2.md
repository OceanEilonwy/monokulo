# Plan 2: the risk guardian's cut (audited commit f7fb5c9be)

## 1. Summary

Do most of what research proposes, with 8 verdicts overturned, 1 test added that covers a real gap, and a rule that nothing leaves PR CI until a deterministic test replaces what it guarded.

| Measure | Expected |
|---|---|
| Tests removed | about 480 of 2828 (about 305 deletes; 238 merges become about 50 table tests) |
| Serial seconds saved | about 770 s (Rust about 170 s, e2e about 600 s measured; WebKit not measured) |
| Local wall time | Rust about 40 s (the floor is the 38.8 s property test). e2e drops from 189 s to roughly 70–90 s instrumented. **Under 60 s for the whole suite only with T5 + T6.** |
| Longest CI test step | 522 s now; about 200–250 s after C1–C3 (browser shards); scale tests about 165 s after the split |
| Coverage lost | about 0.33 pp, from 0.38 pp before the overturns win back key_entry and query_page lines. No auth, money or key lines are lost. |

## 2. Cruft-cut actions (DB suggestions 53–62, source plan-2)

**A1 (53). Delete the presentational Rust tests.** Covers research 36, 28 and 32 (most of them), plus 10, 13, 14 and 24. That is 190 tests, about 45 s.
- I re-read every measured production line lost, B's 171 and C's 117 (`review2/b-lost.txt`, `c-lost.txt`). They are render arms: status words, Tor panel text, wallet-origin wording, toasts, `PosViewModel` assembly.
- No line makes an auth, ownership, amount or key decision. The ones close to that are put back by A5:
  - `query_page::value`/`schema`, admin DB reads with a per-handler `switched_off` gate
  - `key_entry::downloads`, which builds the CLI and `.sha256` URLs
- Lines I'm accepting:
  - `pos.rs:76` (XMR has 12 decimals on the POS page). The server still refuses fiat amounts with more than 2 decimals (`exchange_rate` tests) and pos-terminal.spec drives the page.
  - `wallets.rs:1042` ("Backup skipped" wording). It's display only.

**A2 (54). Merge 188 tests into table tests.** Covers research 11, 17–23, 29, 33, 34 and 37. Conditions, each one verified in the code:
- The merged login test must keep 1715's `HttpOnly` and `SameSite=Lax` asserts. 1715 is the only pin on them, and `http::csrf` relies on SameSite. 1725 checks neither.
- Keep the derive_status overflow-saturation row (595) and the key-image disagreement rows (220, 229).
- Keep one row per route in the cross-user 404 tables (1541/1540/1583, POS 1649/1688). These are the IDOR checks.
- Keep the `Unknown` row in the retire-state table (2020). Its lines show up as "lost" only because the measurement removed the whole group.

**A3 (55). New deterministic test replacing 347.** One table over every tenant route × {no bearer, wrong, own `pk_`, rotated, disabled} → 401.
- Tenant auth is a per-handler `AuthedTenant` extractor, not a layer, so a new route written without it is caught only by a sweep like this.
- Today, only `authorization_properties.rs` checks PUT `tenant/key-custody` without a bearer. `tenant/key-custody/bundle` has no such check in either test.

**A4 (56). Keep 656.** It's an overturn, see section 4.

**A5 (57). Five more overturns plus the 1075 fold.** See section 4.

**A6 (58). Browser flow set.** Research D 1, 4, 5 and 6 (delete 62, the POS fit merge, fold 40, 3 rewrites), plus D 7.
- D 7 adds the missing flows on real binaries: register, log out, wrong password, log in.
- The 2485 rewrite has to really submit the no-JS refund Save.
- Fold "no fiat line on an XMR order" (taken from deleted 2499) into the 1459 rewrite.
- Optional: one browser plugin-connect consent flow. 2619 connects through the fixture control, and the consent step is where a merchant could be phished. In-process, 1469, 1474 and mock-woocommerce 1080 cover the protocol well.

**A7 (59). Firefox down to D's 3-test smoke.** WebKit: see T3.

**A8 (62). Research C's handler gap is not real.**
- `website_submit` refuses while a plugin is active, and `disconnect_submit` refuses while orders are open. 1469 pins both in-process: POST → 422 and the row is unchanged.
- The retire gate lives in the engine (`retire_wallet` → 409) and is pinned by engine 304 (a store uses the wallet) and 314 (open orders).
- So views 1990 and 1994 can go too.
- Real gaps to add (cheap, in-process):
  - `disconnect_submit`'s fail-closed branch (engine unreachable → refuse) has no test.
  - The bundle route's auth (handled by A3).

## 3. Time-target options, with the risk of each

| # | Option | Facts | Local wall | Longest CI step | Risk |
|---|---|---|---|---|---|
| T1 | A1–A7 cuts | −770 s serial | e2e 189 → about 80 s | browser 522 → about 330 s (setup build still inside) | Low. 0.33 pp, render lines only. |
| T2 | Proof fixture (research 26) | 24 tests, 207 s, about 5 s of setup each | −74 s serial | rust step −20 s | Low–medium. A committed chain can go stale or hide a PoW regression. The test must **re-verify** the fixture (PoW, key change) on load, and xtask regenerates it. Keep the key-change tests on the crossing chain. |
| T3 | Cut WebKit | not measured, est. 150 s | none (it doesn't launch locally) | release browser 764 → about 600 s | **Medium.** WebKit is iPad/iPhone Safari, the likely counter tablet and the Monokular iOS engine. Clipboard, camera, EventSource and flex/grid all differ there. Keep a wider smoke: POS new/background/resume, camera scan, counter pay, checkout pays, fit loop. Cut only after a CI run measures it. |
| T4 | Paused clocks (A 27, D 2592 countdown) | 5 s + 25 s | −30 s serial | small | Low if the production interval stays (memory rule). A paused tokio clock auto-advances while `spawn_blocking` SQLite work is pending, which can hide timing races. Keep one real-time sibling per retry family. |
| T5 | Exclude properties from the **local** default (`--profile quick`); CI keeps them | 212 tests, 645 s serial, holds the 38.8 s floor | Rust floor 38.8 → 23 s (scale test 850) | none | Low locally, because PR CI still runs them at 64 cases. **Touches an excluded suite.** |
| T6 | Split the 4 longest e2e tests (2623 51 s, 2619 46 s, 2631 39 s, 2617 36 s); raise workers to 16 on 32 cores | Playwright floor = longest test | e2e about 45–55 s | browser shards even out | Low. Same assertions, split at their natural steps. |
| T7 | Move properties out of PR CI to the scheduled `engine-properties.yml` | PR rust step is 198 s with them, already under 300 | none | none on Linux | **High. Not recommended.** `authorization_properties` is the only PR-time check on key-custody auth histories, and the money properties pin exactly-once credit across forks. Only after A3 lands, with a named list of what stays. **Touches an excluded suite.** |
| T8 | `every_sql_failure_*` snapshot/restore (A 25) | 77 s | −38 s | small | **Medium.** A DB snapshot skips recovery from in-process state, which is what these tests are for. Instead, **split each sweep by fault-index range into several tests**: wall time drops, semantics stay the same. |
| C1 | Browser binaries built in their own step; browser tests in 2 shards (per project on release) | step 522 includes the build | none | about 200–250 s | None in meaning. Costs about 2 min of CI minutes per PR. |
| C2 | Scale scenarios over 2 jobs | 328 s | none | about 165 s | None. Still excluded from the cut. |
| C3 | Windows rust tests `--partition count:3` | 625 s (release) | none | about 210 s | None. |

**Reaching the targets:**
- **CI, each test step under 300 s:** T1 + C1 + C2 + C3. No test leaves CI. Reachable.
- **CI read as a job including its build:** not reachable on Windows (build 2416 s) or macOS (1118 s). Linux needs a warm sccache to get the 521 s build under 300 s. Say so to the user rather than cut tests to hit it.
- **Local whole suite under 60 s:** T1 + T5 + T6, running Rust (about 25–30 s) beside uninstrumented e2e (about 45–55 s) on 32 cores. That is plausible but not proven: plan 3 has to measure it. Without T5, the 38.8 s property test plus CPU contention probably lands at 60–70 s. Never use T7 to get there.

## 4. Verdicts I overturned (42 spot-checks, 38 upheld)

| Test | Research | Mine | Why |
|---|---|---|---|
| 656 `list_active_tenants_excludes_disabled_ones` | delete | **keep** | It's the only pin that a deleted store's keys are not re-registered in key custody at boot (`run.rs:412 register_all_tenants`; no test drives that path). |
| 347 `lookup_payment_requires_authentication` | delete | **rewrite → A3** | Auth is per handler, and 320/340 don't reach lookup. Widen it to every tenant route. |
| 1600 client library | delete | **rewrite** | Its negative asserts enforce the user's boundary rules: no engine routes in the embed, no `postMessage`, and checkout.js never touches `window.parent`/`top`. A positive spec can't prove those absences. |
| 1653 query long value | delete | **rewrite** | It's the only test of `/admin/query/value`, which runs the SQL again with its own `switched_off` check. Make it a gate table over the 4 query routes: 403 non-admin, 404 when switched off, writes refused. |
| 1573 order detail | delete | **rewrite** | Keep the one money assert: a missing fiat quote shows "—", never a made-up amount. |
| 1511 CLI download links | delete | **keep** | A pure function that runs in 0 ms. It pins the URLs and `.sha256` checksum URLs that merchants are told to trust for the key tool. |
| 1910 `never_shows_a_meta_refresh` | delete | **rewrite** | The rule still stands (meta refresh only on checkout and the challenge page). Make it a source guard over `views/`. |
| 1075 mock receiver rejects bad signatures | delete | **merge into 1081** | Without it, a mock that accepts anything would quietly make "delivered and verified" mean nothing. |

**Upheld after reading the code:**
- Engine: 648, 650, 675 (354 and 375 cover them over HTTP, plus scanner 8648); 804; the derive_status, void-recheck, grace-period and corroboration merges.
- Auth: 1695, 1696; 1705 and 1709 as rows; 1715 conditionally (A2); 1854 (connect.rs 1480 covers it); 1866 (http 1583 pins the server gate); 1418 (1436 covers re-running setup by GET and POST); 1503, 1510.
- Other monokulo: 1544, 1599, 1666, 1727, 1869, 1876, 1989 (webhooks 809 pins "shown once"); the retire merges; the store_site view deletes.
- Shared and mock: 2214, 2227, 2229, 1072–1074, 2125.
- e2e: 2499 (with the fold), 2633, 2634, the Firefox cut.

Plainly cruft that research marked keep: 2640 (19.8 s of wallet-fold layout). Keep one mainnet/test separation assert from it and fold that into 2694.

## 5. Sequencing

1. **A3 + A8's fail-closed test + A4** (half a day). They add protection before anything is removed.
2. **A1 + A5** (1 day). Re-run `measure.sh without` and `prodlost.py` and check that no new auth or money file shows up.
3. **A2 table tests** (1 day). Review each table against its row list.
4. **T2, T4, T8-as-sharding, B 39, E 9** (1.5 days).
5. **A6 + A7 browser** (2 days). Keep the gallery feeders.
6. **C1–C3** (1 day). Then measure WebKit in CI and decide on T3.
7. **T5 + T6, measure local** (half a day).

## 6. Open questions only the user can answer

1. Which browser does the counter tablet run: iPad Safari or Android Chrome? This decides how much WebKit stays (T3).
2. Is "segment" a CI step or a job? If it's a job, do you accept sccache/warm caches instead of cutting tests?
3. May the local default profile skip property tests (T5) while CI keeps them?
4. Do you want a browser plugin-connect consent flow, or are the in-process tests 1469/1474/1080 enough?
5. Once A3 exists, would you ever accept moving properties out of PR CI (T7)?
