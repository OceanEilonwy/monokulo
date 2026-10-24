# Test audit: results

This is what the audit's plan (`docs/test-audit-plan.md`) delivered, measured on the integration branch `test-audit-integration`. The decisions are in `DECISIONS.md`, and each chunk's log is in `progress/`.

## Both targets are met

| Target | Before | After |
|---|---|---|
| **North star:** the whole local suite in under a minute, excluding compilation (`cargo xtask test`, 32 cores) | about 110 s | **57.0 s**: Rust 28.1 s, e2e (Chromium) 28.8 s, components 15.9 s, node 0.7 s |
| **Hard limit:** no CI test step over 5 minutes | 764 s (release browser), 626 s (Windows Rust) | **194 s** at most (CI run 40105517984) |

The CI test steps, before and after:

| Step | main (release run 40084876632) | After (run 40105517984) |
|---|---|---|
| Linux, Rust tests | 208 s | 163 s |
| Linux, doctests | 110 s | 182 s (it varies with the cache: 110 to 464 s on main) |
| Linux, browser tests | 617 s (764 s on another run) | 81 s |
| Linux, scale scenarios | 322 s | 66 s |
| macOS, Rust tests | 212 s | 131 s |
| Windows, Rust tests | 626 s | three partitions: 161, 194 and 188 s |
| Coverage, Rust tests | (no recent run on main) | 146 s to build, 153 s to run (run 40105520240) |
| Coverage, browser tests | 579 s (last green run) | 201 s |

Build steps are outside the limit, as the owner decided (Q2). Each test step now has a build step of its own, so its time is the tests' alone.

## The suite

| | At the audit (f7fb5c9be) | After |
|---|---|---|
| Rust tests, ci profile (the CI suite) | about 2,470 | 2,183 (27 skipped) |
| Rust tests, the local default | the same | the ci profile without generated properties, `::scale::` tests and real RandomX |
| e2e | 285 (Chromium and Firefox) | 83, Chromium only (one of them runs only in the coverage run) |
| Component specs | 30 | 13 |
| Node tests | 36 + 6 | 10 + 6 |

Upstream added tests while this work ran, so the counts are approximate.

Chunk by chunk:
- **CI and tooling (A).** Build steps for browser, scale and Windows, which runs in three partitions. Chromium only. `cargo xtask test`. Guard rails R1-R4. xtask's tests went from 122 in 20.9 s to 90 in 2.0 s.
- **Browser (B).** Tiers 1 and 2. N1 (sign up, log out, wrong password, log back in, plugin consent). N4 (a no-JS refund save that really submits). The clock-wait fixes. The `@gallery` tag. 3 screenshot shapes. The status cache became a setting, `engine.status_cache_ms`.
- **Monokulo and the other crates (C).** 1,396 tests became 1,092. The presentational tests went, families were folded into tables, and dead code went with its tests. N3. Rewrites that pin a rule: 1459, 1653 and 1910. An explicit engine-page timing.
- **Engine (D).**
  - F9: a stand-in hasher; 5 real-RandomX tests remain.
  - F8: the SQL sweeps split by fault range; a paused clock for the registration retry.
  - T-i: properties out of the local default.
  - L2: 36 named fixed-input tests.
  - L3: the ringct split.
  - N2: the store-route auth table, read from the router.
  - The ci profile went from 66.9 s to 32.8 s locally.

## Coverage

See "Coverage, measured" below.

## What CI showed, and the fixes made at integration

- The store-route auth table (N2) looked for `"\n}\n"` in a source file. A Windows checkout writes those lines with `\r\n`, so the table now normalises line endings first.
- The machine-figures test failed whenever the clock read 999 (`now_unix` 1792839992). It now checks only the node's own figures.
- The live-view test failed under coverage instrumentation, because a request interception aborts the cross-document view transition. The fix is in the coverage harness (`serveInstrumentedAssets` turns the transition off on intercepted pages); no product code changed.
- Doctests went back to `--exclude xtask`. A coverage run builds its instrumented tests first (`cargo xtask coverage build-rust`), and its doctests are left to the plain Linux job.
- Two base-path walks added upstream took 19 s and 13 s. They now take about 4 s and 3 s, because a stream stops being read once it goes quiet.
- Two Windows-only failures (`one_resource_per_file`, `no_handler_answers_with_an_error_object`) were already failing on main. A later main commit fixed them, and the merged branch passes on Windows.
