# Timing facts (audited commit f7fb5c9be, collected 2026-10-23)

## Targets set by the user

- North star: the whole local test suite (excluding compilation) completes in **under 1 minute**.
- Hard limit: the longest **test segment in CI** takes **at most 5 minutes**. Read here as: each CI
  test step (the step that runs tests) is at most 300 s. Job wall time including the build is reported
  beside it, since a reviewer could reasonably read "segment" as the job.

## Local, this machine (32 cores, 62 GB), instrumented coverage run, Rust and browser side by side

| Suite | Tests | Serial (sum of durations) | Wall | Longest test |
|---|---|---|---|---|
| Rust nextest, non-property | 2137 | 632 s | 47.6 s together with property (one nextest run) | 23.1 s |
| Rust nextest, property (`::properties::`) | 212 | 645 s | (same run) | 38.8 s |
| xtask (separate nextest run, not instrumented) | 122 | 47 s | 20.1 s | 20.1 s (mutations hanging-program test) |
| e2e Playwright (coverage-browser.config.js: fixture, real-binaries, firefox projects; WebKit could not launch here) | 285 | 1302 s | 189 s (8 workers: `min(8, max(4, cpus/2))`) | 51.2 s |
| crates/ui Playwright components | 30 | 52 s | 56 s (1 worker) | 4.6 s |
| node --test (apps/engine, ui lint) | 42 | 4 s | ~2 s | 1.9 s |

- Tests over 5 s: Rust non-property 36 tests = 320 s serial; property 36 = 475 s; e2e 57 = 776 s.
- The e2e numbers were taken with screenshots (1434 WebPs) and instrumentation on; PR CI runs neither,
  so research D estimates real e2e time is about half the measured.
- nextest runs each test in its own process, so `OnceLock`/static caching never helps across tests
  (the engine proof tests pay ~5 s of RandomX + mining setup each: 24 tests, 207 s).
- The Rust wall time floor is the longest single test (38.8 s property; 23.1 s non-property).
- The Playwright wall time floor is the longest test (51.2 s) plus the worker cap of 8.

## CI (GitHub-hosted runners, 4 vCPU), step durations in seconds

PR run 39948763905 (checks.yml, Linux only, no coverage):

| Job | Build step | Test step(s) | Over 300 s? |
|---|---|---|---|
| linux / rust tests | Build the tests 521 | Run the tests 198, doctests 104 | build yes; tests no |
| linux / browser tests | (binaries built inside the test step's global setup) | Run tests 522 | **yes** |
| linux / engine scale tests | – | Run the scale scenarios 328 | **yes** |
| linux / engine progress tests | – | Measure engine scanner progress on one CPU 198 | no |
| lint | – | Clippy 93 | no |

Release run 39939077652 (main, all platforms; failed on Windows):

| Job | Build | Tests | Other |
|---|---|---|---|
| linux / rust tests | 656 | 203 + doctests 135 | |
| macos / rust tests | 1118 | 230 | |
| windows / rust tests | 2416 | **625** | Summarize 284 |
| linux / browser tests | – | **764** | |
| linux / engine scale tests | – | 217 | |

Coverage job (release run 39115987324, last green one): rust tests 278 + doctests 105; browser tests
579; woocommerce start 101 + tests 60.

Raw step tables: target/test-audit/ci-*.tsv.
