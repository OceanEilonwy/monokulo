# Chunk A, ci-tooling: progress

Branch `worktree-agent-a706d731e32925c73`, from origin/main `6bac4c3b1`.
Owns `.github/workflows/**`, `xtask/**` and `docs/coverage-line-baseline.json`.

## Done

| Item | Commit | What changed |
|---|---|---|
| T-a | ce963770c | The browser job has a **Build the binaries** step before Run tests: `node -e "require('./e2e/browser/real-stack').buildBinaries()"` (real-stack.js's `BUILD_ARGS` stays the one place the arguments are listed) and `cargo build -p xtask`. The global setup's `cargo build` then finds nothing to do. |
| T-b | ce963770c | The doctests name the same packages as the Build step: Linux no longer passes `--exclude xtask`, which had changed feature unification and rebuilt every library. `cargo test --doc --no-run` is refused by Cargo, so the libraries can't be compiled on their own. With `--unit-graph`, the doctests' graph compared with the build's showed 3 tiny proc-macro units differing, against 733 with `--exclude xtask`. |
| T-c | ce963770c | The scale job has a **Build the scale tests** step: `cargo build -p xtask` and `cargo nextest run --no-run -p engine --lib --locked --profile ci`, the build `cargo xtask engine scale` runs. |
| T-d | ce963770c | On Windows the rust tests job runs `cargo nextest archive` once, builds xtask, and uploads both. A new `rust tests partition` job (a matrix of 1, 2 and 3) runs `--partition count:N/3`. The archive is extracted into the checkout's `target/`, the path the tests were built at, so paths compiled in (`CARGO_BIN_EXE_*`, `CARGO_MANIFEST_DIR`) still hold. Each part summarizes with the shipped `xtask.exe`, so none spends 284 s building xtask. `summary` needs the new job. xtask is built in the Build step on every platform. |
| Q3 | ce963770c, 99e288e90 | The workflows install only Chromium. The Playwright cache key is now `playwright-chromium-…` in platform.yml and live-network.yml. xtask no longer names Firefox or WebKit runs (`support::project_browser` and its uses in summary, timings and the pages report are removed, with their 2 tests). |
| T-e | 0e0fc11b6 | `a_hanging_program_is_invalid_and_reaped` runs `sh -c 'sleep 60 & echo $! > …; wait'` with a 2 s deadline and checks that the background child was reaped (20.1 s → 2.0 s). |
| F6 r9 | 0e0fc11b6 | `mutations::run()` hands its verdict to a pure `judge()`. The 12 throwaway-crate tests are now 15 rows of one table test over canned Cargo output (`only_a_healthy_pass_is_a_baseline_and_only_the_intended_assertion_a_detection`). One real Cargo run is kept (`the_intended_assertion_detects_the_named_defect`), so the table stays true to what an unpinned nightly prints. Everything stays under `mutations::tests`, which engine-mutations.yml filters on. |
| F6 r16 | ac34f6835 | The two `#[ignore]` lints are one test, `every_ignored_test_says_why_and_runs_daily_or_says_why_not` in live.rs, with one parse of the workspace. nextest runs each test in its own process, so two tests can't share a parse. |
| F6 r13 | ac34f6835 | Deleted: 2356, 2357 (logo), 2387, 2454 (docs), 2405 (site header), 2450, 2464, 2466 (report markup). The 5 format tests became rows of one table test (2397, renamed `report_figures_read_as_people_write_them`). 2449's annotated-source link and 2436's escape assert moved into 2453 `every_local_link_of_every_page_resolves`. |
| G4 (xtask) | ac34f6835 | 2388–2391 folded into 2393 (fetch). 2400 and 2403 folded into 2401 (model). 2402 folded into 2461 (landing; see deviations). 2409, 2423, 2444, 2457, 2465 and 2467 became rows of 2445, renamed `each_input_builds_its_sections_and_a_missing_one_leaves_them_out`, which does 6 site builds where 8 tests did 8. |
| T-j | 1a583c29d | `cargo xtask test` (xtask/src/suites.rs). Before the clock it builds the Rust tests (`nextest --no-run`), the browser binaries and `npm run build:ui`. Then it runs Rust (`cargo nextest run --workspace --locked --features monokulo/snp`, nextest's default profile), then e2e (`coverage-browser.config.js --project fixture --project real-binaries`). `test:ui`, `test:ui:unit` and `test:engine` run alongside, logging to `target/xtask-test/`. It prints each runner's wall time and the total. The `COVERAGE_*` variables are removed from every child, so nothing is instrumented or photographed. |
| R1, R2, R4 | 73ea578e2 | xtask/src/ratchets.rs, run as xtask tests (the Linux PR job runs them) and by `cargo xtask ratchets [--prune]`. R1 (syn) flags a `#[test]` whose every assertion is a `contains` with at least one HTML needle (a known tag, a custom element, or `attr="`). Escaping, secret and no-JS tests are allowed by a word in their name. R2 counts `getComputedStyle`, `boundingBox`, `toHaveCSS` and colour literals per e2e file, with 2 reasoned allowances. Baselines: `xtask/ratchets/markup-only-tests.json` (123 tests), `xtask/ratchets/e2e-style-reads.json` (32 file:rule keys, 111 reads). A new finding fails, and so does a fixed one still in a baseline; `--prune` only removes. R4 reads the `required` list in coverage-browser.config.js and checks that each group is photographed by a spec the coverage run runs. It resolves `group: 'x'`, `group: CONST` and options constants, and otherwise uses `guessGroup`'s rule. |
| R3 | 90199639e | `cargo xtask test-summary --fail-slow`: a non-property Rust test over 5 s, or a browser test over 15 s, that `xtask/src/slow.rs` doesn't list (by name prefix, each with its reason) is listed under "Too slow" and exits 1. platform.yml passes it only on `pull_request` events without coverage, in the rust and browser Summarize steps. The rust step still writes the doctest line before failing. |
| Q4 | none | xtask's gallery `Shape` enum reads the manifest and must still read older 6-shape artifacts that Pages may fetch, so it is unchanged. Nothing in xtask lists the shapes to capture. |

**Counts (xtask tests):** 122 → 88. Removed or merged: −10 mutations (12 cases became table rows), −29 pages/logo/live, −2 browser naming. Added: +1 `cargo xtask test` source check, +5 ratchets, +1 slow gate.

## Timings (xtask, `nice -n 10 cargo nextest run -p xtask`, warm)

| | Tests | Wall |
|---|---|---|
| Before (origin/main 6bac4c3b1; DB at f7fb5c9be: 20.1 s) | 122 | 20.9 s |
| After | 88 | 2.0 s (4.6 s on a run where the pages islands build was cold) |

The longest test is now the 2 s hanging-program test. The pages tests' first run builds the site's ui islands with Vite, and the other pages tests wait on its lock.

## Decisions

1. T-b: rather than a separate doctest build, the doctest step names the same packages as the Build step. The coverage job's doctest step is unchanged: its Build step is skipped, because coverage builds instrumented tests. The step takes 105 s, under the limit.
2. T-d only on Windows. macOS takes 230 s, under the limit. `count:` partitions, as the plan says. The partition jobs run even when the rust tests job failed only on doctests (`!cancelled()`). Without an archive, the download fails the job.
3. T-a reuses `buildBinaries()`, so the workflow can't drift from the global setup's arguments. No skip flag was needed in e2e/.
4. Q3 in xtask: Firefox and WebKit naming was removed as dead (nothing produces those project names once the suite is Chromium-only).
5. judge(): one real Cargo run is kept, since the nightly is unpinned and libtest's output format is what the judge parses.
6. R1 is a heuristic: a test can call `.unwrap()` without counting as an assertion, and only `assert!`-family macros count. Its HTML test needs a known tag name, because a CLI usage string (`^<blocks back>`) had been a false positive.
7. R3's SLOW list was seeded from local times over about 0.4 of each limit (Rust over 2 s, e2e over 6 s), which allows for slower CI runners. CI per-test times aren't known, because CI uploads no JUnit. A stale entry fails nothing.
8. T-j: e2e is limited to the Chromium projects with `--project`, so it holds before chunk B drops the Firefox/WebKit projects. On Windows, Cargo runs exclude xtask (a build can't replace the running xtask.exe).

## Deviations from the plan

- 2402 `a_site_without_the_apk_repository_is_an_error` (landing): the DB says merge into 2401, a model test in another module. It was merged into 2461, the landing page test, instead, using that test's checkout so the error comes from the missing repository and not from a missing key.
- 2397 and 2445 were renamed, because each now covers more than its old name says.
- T-j was not run end to end: this chunk may not run the engine, monokulo or browser suites. Its source check passes.

## Tests already failing on main

None in xtask. Without `npm ci` at the repository root, 24 pages tests fail ("the islands build failed"); that is the environment, not the code.

## Needs from other chunks

- **B (browser), must land with this branch:** CI now installs only Chromium. `coverage-browser.config.js`'s `launches()` returns true on CI, so while its firefox/webkit projects remain, the CI browser step fails. Remove those projects (F5/Q3).
- **B:** if the Playwright project names `fixture` or `real-binaries` change, update `E2E` in `xtask/src/suites.rs`. `every_runner_names_a_script_and_config_that_exist` fails until then.
- **B:** crates/ui's Playwright `webServer` runs `npm run build:stylebook` inside the clock of `cargo xtask test`. Consider prebuilding or reusing it.
- **B:** after the e2e cuts, `no_new_browser_test_reads_styles_boxes_or_colours` reports the fixed reads. Run `cargo xtask ratchets --prune`. If a cut removes the last feeder of a required gallery group, `every_required_gallery_group_has_a_spec_that_photographs_it` fails, which is the point.
- **C (app-rust):** after the F1/F3 deletes, `no_new_test_only_finds_text_in_rendered_markup` lists the deleted tests as fixed. Run `cargo xtask ratchets --prune` on the integration branch.
- **C, D:** `xtask/src/slow.rs` names slow tests by name prefix. Once F8/F9/T-g land, drop the entries that are no longer slow (stale ones fail nothing). A renamed slow test needs its entry renamed, or the first PR it runs in fails.
- **D:** `cargo xtask test` uses nextest's default profile, so it follows the T-i `default-filter` without changes here.
- **Orchestrator:** re-baseline `docs/coverage-line-baseline.json` after the combined coverage run (plan step 11). That needs the integrated numbers, so it isn't done here.
