# Chunk B (browser): progress log

Branch `worktree-agent-ab85c284c5dc7a950`, from origin/main `6bac4c3b1`.
Scope: e2e, crates/ui component specs, apps/engine node tests, the coverage
fixture, and monokulo's status-cache setting.

## Done

Counts are tests as Playwright or node lists them.

| Suite | Before | After |
|---|---|---|
| e2e, `coverage-browser.config.js` | 285 at the audit (229 Chromium + 56 Firefox; WebKit didn't launch here) | 82 Chromium, plus 1 `@gallery` test in the coverage run |
| crates/ui component specs | 30 | 13 |
| apps/engine node tests | 36 | 10 (the 8 G4 keepers, 2822, and the new #126 test) |

1. **Status cache setting (Q6 for 2657, 2683 and 2615): `5cb34a31a`.**
   - `engine.status_cache_ms` (settings.rs): default 10 000, `range(1_000, 300_000)`, `applies: Restart`,
     read into `ServerConfig::status_cache`. The const `CACHE_TTL` in http/status_page.rs is gone. The TTL
     lives in `StatusCacheShared` (an `AtomicU64`, defaulting to the setting's default) and is set with
     `status_page::set_status_cache_ttl`.
   - Unit test `settings::tests::the_engine_status_is_trusted_for_ten_seconds_unless_set_and_never_under_one`
     pins the default and the 1 s floor.
   - e2e/browser/real-stack.js writes `status_cache_ms = 1000` under `[engine]`. The coverage fixture calls
     `set_status_cache_ttl(.., 1 s)` in code. 2615 went from 14.9 s to 1.5 s, and is now a step of 2609.
   - 2045 rewritten: `monokulos_own_calls_to_an_embedded_engine_keep_the_status_dot_green` makes one call
     more than the engine's default instance limit (601) instead of 2 400: 2.0 s to 0.35 s.
   - `every_monokulo_setting_on_the_admin_page_saves_correctly` saves the new setting too.
   - This commit also deleted live-view-hidden-tab.spec.js by mistake (a staged deletion went in with
     it); its replacement node test landed two commits later in `5d0b026cf`.
2. **Chromium only (F5, Q3) and T-f: `ebe7fd945`.** The Firefox and WebKit projects and the launch probe
   are gone, and so are their dead branches: `CAMERAS`, `holdUpCode`, `CLIPBOARD_PERMISSIONS`,
   `RECONNECTS_FAILED_STREAM`, and `photographs()` in the gallery. The `real-binaries` project comes first.
   The project names `fixture` and `real-binaries` are unchanged (xtask/src/suites.rs uses them).
   refund-camera.spec.js is one test: 2601 with 2602 folded in. 2603 and 2604 are deleted under G1.
3. **apps/engine (G4, G2/2524): `5d0b026cf`.** 27 drawing-math tests are deleted. The 2524 hidden-tab
   regression is a node test in marks.test.ts: a mark in a frame a hidden tab never plays is still listed
   once the playback passes it.
4. **Components (F6 browser part, research 31 and 32, G3 2761 and 2777, T-h): `3fe694437`.** The pixel,
   colour and layout specs are deleted (2760, 2761, 2762, 2763, 2764, 2768, 2769, 2770, 2775 to 2780,
   2785), and so is the dev server (port 4180) that only 2760 used. The copy block is folded into the copy
   test and the tabs with logos into the keyboard tabs test. `fullyParallel: true`.
5. **N1 account-access.spec.js (real binaries): `941aadb26`.**
   - A merchant signs up (the admin opens sign-up and sets `public_uri`), logs out, is refused a wrong
     password (no session cookie set), and logs back in.
   - A plugin's connect request then waits behind the login page, which names the shop. The merchant
     connects their store, and Monokulo sends them back to the shop with the token and nonce.
   - The plugin's `/finish` redeems the token for `sk_`/`pk_` keys, and only once.
6. **Checkout (F4, G1, G2, N4): `8d7e8d428`.** 28 tests down to 11.
   - The flows absorb these steps: 2480 into 2478; 2494, 2495 and 2498 into 2477; 2484, 2490 and 2499
     (the XMR and AUD amounts) into 2496; 2502 (double spend, expiry) into 2497; 2501 into 2486; 2487
     into 2491.
   - N4: 2485 now types an address without JavaScript, presses Save, and sees it saved on a fresh load.
   - Deleted: 2481, 2482, 2483, 2492, 2503 and 2504.
   - 2500 (theme precedence) moved into 2692 (site-themes.spec.js). 2692 was rewritten without its
     computed colours or its 320px fit checks. It keeps the theme flow and its gallery captures (`site`,
     `hosted-payment`).
7. **Challenge: `f12cc3458`.** 2508 clicks "continue" (11 s to 0.7 s). 2509 runs on its page clock beside
   2507's real 10 s wait, which is kept.
8. **Merchant client: `4f5c0f796`.** 2543 folded into 2547.
9. **POS (F4, G1, G2, Q6 2592): `d9bba5fb2`.** pos-terminal goes from 36 tests to 16, and
   pos-responsive-layout.spec.js (15 sizes) is deleted.
   - 2566 absorbs 2583, 2585, 2593, 2595 and 2599, plus one tablet fit check carried over from
     2567/2600 (plan 1: "one fit assertion moves into 2566 at tablet size").
   - 2569 folded into 2576, and 2589 into 2577.
   - 2582 and 2586 folded into 2581, on a second page that shares the context's paused clock.
   - 2571, 2572 and 2594 folded into 2587, and 2598 into 2596.
   - 2592 uses `page.clock.fastForward` (32 s to 0.4 s).
   - Deleted: 2568, 2570, 2573, 2574 and 2575.
10. **Live view: `dd7fa52cb`.** One flow, 2526, absorbs 2525 (the stress run), 2527, 2530 (its JavaScript
    half) and 2531. Geometry and label tests are deleted, and 2539, 2540 and live-view-performance.spec.js
    go. The fixture's story plays an event every 30 ms instead of 120. 20 s to 9 s; 3 of 3 repeats passed.
11. **Settings, wallets, store and status (fixture): `8818d7378`.**
    - 2609 absorbs 2611 and 2615 (following, then lagging, then held, all live). 2617 absorbs 2616.
    - 2623 absorbs 2625, 2626 and 2627, without the CSS, column or phone checks.
    - Deleted: 2610, 2612, 2613, 2614, 2621, 2624, 2628, 2629, 2630, 2631, 2632, 2633, 2634, 2637, 2638,
      2639, 2640 to 2644, 2472, 2474, 2475, 2476, 2515 and 2516.
    - Whole spec files deleted: disclosures, dropdowns, javascript-disabled, short-values, wallet-details
      and pages-gallery-themes.
    - The fixture loses the test-networks-only merchant and the webhooks' `?empty`, which only deleted
      tests used.
12. **E2E_PREBUILT taken back out: `84d06b4dc`.** Chunk A builds with `buildBinaries()` before the browser
    step, so it isn't needed.
13. **Admin, logs, settings sections and wallet setup (real binaries): `648ad4a04`.**
    - admin-settings-and-nodes: 2650 is now JavaScript-only. 2656 absorbs 2662's "Save anyway". 2658 is
      tagged `@gallery`. Deleted: 2649, 2651, 2652, 2653, 2654, 2655 and 2657.
    - admin-settings-layout.spec.js is deleted (2659 to 2661, 2663 to 2670), and so is
      admin-query.spec.js (2646 to 2648).
    - Logs: 2672 absorbs 2676 and 2678. Deleted: 2673, 2674, 2675 and 2682.
    - Settings sections: 2686 absorbs 2687, and 2688 absorbs 2689. 2691 moved into 2617 (fixture).
    - Wallet setup: 2694 absorbs 2695 (on the page clock). Deleted: 2698 and 2699.
14. **Gallery (T-k, Q4): `acf04aa36`.**
    - `@gallery` tests run only when `COVERAGE_SCREENSHOTS=1`: `grepInvert` in both configs.
    - Shapes go from 6 to 3 (`desktop`, `tablet-landscape`, `mobile-portrait`) × light/dark, in
      coverage-screenshot.js and the reporter's `SHAPES` together. The gallery's device toggle is Desktop,
      Tablet and Mobile, with no orientation toggle.
    - The `asIs` capture option is gone: nothing used it after the cuts.

## Decisions

- **2657 deleted (G1) rather than sped up (Q6).** Tier 2 deletes it, and views test 1831 pins the marker.
  The status-cache setting still serves 2683 (node-settings) and 2615.
- **2524 is a node test.** So the fixture-pacing caveat no longer applies: the story's pacing went to
  30 ms for 2526. The 200 ms coalesce gap isn't needed, since only 2526 plays the story now.
- **The status TTL lives in `StatusCacheShared` and is set at start**, not passed to
  `get_status_cached`. That function has twelve callers in chunk C's files; this way only main.rs needs a
  line (see Needs).
- **2692 rewrite:** I kept its behaviour (the merchant's theme on the site, the POS and the share frame,
  the `?theme=` precedence and the embed option from 2500) and its captures. I dropped its computed
  colours and fit checks. It isn't tagged `@gallery`, because it tests behaviour.
- **2530:** only its JavaScript half was folded. Its no-JS "Go" half was dropped under the
  one-no-JS-e2e-per-mechanism rule.
- **No-JS e2e kept, one per mechanism:** settings save bar 2618; dialogs as pages 2620; wallet setup 2697;
  logs Refresh 2680; the no-JS checkout N4 (2485). The new "cancelling on the POS reaches the
  customer's checkout" test, which postdates the audit, keeps its one no-JS check.
- **Auth spot checks before deleting:** 2613/2540 are pinned by `http::engine_page::tests::only_admins_see_the_engine_page`
  (anonymous 401, merchant 403) and `proof_of_work_checking_is_shown_to_admins_only`. 2646's write refusal
  is pinned by `http::query_page::tests::a_write_is_refused_and_changes_nothing`, and its download by
  `the_rows_download_as_csv_and_json`.
- **I edited docs/ui-components.md, which no chunk owns.** It pointed at the deleted
  pos-responsive-layout spec.

## Deviations from the plan

- The flows were merged in their natural order, not mechanically. For example, 2566 takes a single
  tablet fit check where plan 1 said "one fit assertion", and the old 2585 "busy store" 44 orders stay so
  that "no paging" is still proven.
- Q6 2526/2524: as above, 2524 left the browser.
- 2491 (checkout stream retry) took 2487 (the library's polling fallback) on a second page, as the DB
  says. They test different libraries, but share the paused clock and the fixture.

## Tests already failing on main

None seen in my scope. Under heavy machine load (load average 140 to 150 from the other chunks' builds),
fixture tests time out at 40 s, and screenshot captures hang. On a quiet machine they pass.

## Needs from other chunks

- **Chunk C, crates/monokulo/src/main.rs:** apply the status-cache setting at start. Right after
  `app_state` is built (before `build_router`), add
  `monokulo::http::status_page::set_status_cache_ttl(&app_state.engine, monokulo_settings.server.load().status_cache);`.
  Until then the real binaries accept `engine.status_cache_ms` (and stop on a bad value) but keep the
  10 s default, so 2683 (node-settings) stays at about 13 s on this branch.
- **Chunk C, crates/monokulo/src/views/admin.rs (the test `PLACEMENTS` table, after the
  `("engine.url", M, General, "engine"),` row near line 2230):** add
  `("engine.status_cache_ms", M, General, "engine"),`.
  `views::admin::tests::the_tab_list_covers_both_registries` fails on this branch without it.
- **Chunk C (if it removes Rust tests that N1 replaces):** plan 4 says account-access.spec.js replaces
  1695, 1696 and 1858 to 1870.
- **Chunk A:** the e2e project names are unchanged. `@gallery` tests need `COVERAGE_SCREENSHOTS=1`,
  which the coverage run already sets.

## Timings

Measured on this machine, uninstrumented, with no screenshots, at 8 workers (32 cores). The other chunks
were building, so the load average was 15 to 20 during the runs below.

| Suite | Plan's measurement (before) | Plan's target | This branch |
|---|---|---|---|
| e2e, `coverage-browser.config.js`, Chromium | 131 s (285 tests) | about 30 s | **40.6 s, 82 passed** |
| crates/ui component specs | 36 s | about 11 s | **11.7 s, 13 passed** (stylebook build included) |
| apps/engine node | about 2 s (with ui lint) | | 0.12 s, 10 passed; ui lint 6 passed |
| Rust, `-E 'test(status_page) \| test(settings::)'` | 2045 took 2.0 s | | 125 passed in 2.7 s; 2045 now 0.35 s |

- The e2e run's long pole is node-settings.spec.js (2683, about 13 s). It still runs with the 10 s status
  cache, because main.rs doesn't apply `engine.status_cache_ms` yet (see Needs). With that line it should
  be a few seconds, and the run should come close to the 30 s target.
- I made two earlier full runs while the load average was 140 to 180. Both timed out widely (21 and 5
  failures), so I ran it a third time on a quieter machine. The figures above come from that third run.
- Gallery check, with `COVERAGE_SCREENSHOTS=1` on the status, POS and admin specs: 20 passed, including
  the `@gallery` test. Captures are 25 desktop, 25 mobile-portrait and 14 tablet-landscape, each light
  and dark. The "missing required groups" message is expected for a partial run.
