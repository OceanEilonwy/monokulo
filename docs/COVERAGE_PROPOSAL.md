# Test coverage proposal

Implementation steps and completion checks are in
[the coverage work breakdown](COVERAGE_WBS.md).

## Decision

Use the current nightly `cargo-llvm-cov --branch` run to measure **both lines and
branches** in the Rust workspace. Add a small Rust `xtask` as the single entry
point for collecting and browsing reports from all six product areas. The `xtask`
invokes each language's own test runner; Rust instrumentation cannot measure
JavaScript, TypeScript, or PHP code.

Keep one report per language and component. Publish line and branch counts with
their denominators and tool versions, rather than a single blended percentage.
The different coverage engines do not define a branch identically.

## Coverage map

| Product area | Measured source | Test run | Collector |
| --- | --- | --- | --- |
| Engine | `crates/scanner`, `shared`, key custody crates | `cargo test --workspace` | `cargo-llvm-cov` |
| Monokulo API | `crates/monokulo` Rust | `cargo test --workspace` | `cargo-llvm-cov` |
| Payment embed UI | `static/checkout.js`, `challenge.js` | deterministic `surface.spec.js` browser suite | Istanbul instrumentation and Playwright |
| POS UI | `pos-ui/src/*.tsx` | deterministic `surface.spec.js` browser suite | Istanbul instrumentation in a Vite transform, then Playwright |
| Monokulo client library | `static/monokulo-client.js` | deterministic `surface.spec.js` browser suite | Istanbul instrumentation and Playwright |
| WooCommerce plugin | `plugins/woocommerce/monokulo.php`, `includes/*.php` | PHPUnit in `wp-env` | Xdebug coverage |

The `mock-woocommerce` Rust crate belongs in the Rust report as test support and
production-like mock code, but should have its own row. Third-party `jsQR.js`,
generated `pos-app.js`/CSS, authored CSS (which has no executable branches),
vendored dependencies, Rust test source, and PHP test
source are outside the denominator. Include all authored production source files,
including untouched files, so a report cannot improve just because a file was
never loaded.

## Rust collection

Refresh the `nightly` channel and install the newest available
`cargo-llvm-cov` release before each coverage run. Install
`llvm-tools-preview` for that nightly. These are separate tools:
`cargo-llvm-cov` is a Cargo-installed subcommand, not part of the nightly
toolchain. The same instrumented test run supplies line and branch counts:

```sh
rustup update nightly
rustup component add llvm-tools-preview --toolchain nightly
cargo +stable install cargo-llvm-cov --locked
mkdir -p target/coverage
cargo +nightly llvm-cov --workspace --locked --branch --html
cargo +nightly llvm-cov report --branch --json \
  --output-path target/coverage/rust.json
```

The first command runs the tests once and writes a browsable report at
`target/llvm-cov/html/index.html`. The second command renders machine-readable
data from those **same** profiles without rerunning tests. The `xtask` copies the
HTML into the common coverage site described below and prints its entry point.
It should create a fresh coverage directory, run the default workspace tests,
fail on a failed test, and emit a per-crate line/branch summary.
Doctests are excluded by `cargo-llvm-cov` unless explicitly enabled. Do not use
`--all-features` as the default: scanner's `e2e` feature changes the test tier and
brings in network-bound stagenet targets. Tests marked `#[ignore]` stay out of the
repeatable baseline.

The `xtask` runs those update checks before collection and records the exact
`rustc +nightly --version`, `cargo +nightly --version`, and
`cargo llvm-cov --version` output in the report manifest. It checks nonzero
branch denominators for representative `if`/`match` code. `--branch` is
unstable, so a new nightly or collector release can change results or fail;
the run must fail visibly rather than silently using an older tool, substituting
region coverage, or reporting missing branches as 0%. Comparisons across runs
must show the tool versions and flag a version change. The ordinary stable
toolchain tests remain independent of coverage collection.

If later browser tests launch a Rust backend and that execution needs to count,
use `cargo llvm-cov show-env --sh`, then build and start the instrumented server
under those settings, run Playwright, stop the server cleanly, and finally run
`cargo llvm-cov report`. Use a separate profile directory and merge only profiles
from the same source revision, toolchain, and feature set. This is an extended tier,
not part of the default hermetic baseline.

## Browser and PHP collection

Instrument authored JS at the served source boundary and the POS TSX through a
coverage-only Vite transform. Check compatibility with the repo's Vite 8 version
before selecting a ready-made plugin; a small transform using
`istanbul-lib-instrument` is the fallback. In a dedicated coverage build, collect
Istanbul counters from every Playwright page and frame after each `surface.spec.js`
test, merge them across
workers, and save LCOV plus HTML under `target/coverage/browser/`. Enable source
maps so `pos-ui/src/main.tsx`
receives the coverage, rather than the checked-in `static/pos-app.js`. The browser
suite currently serves checked-in JS bundles, so the coverage runner must serve
the instrumented equivalents in its fixtures. Add a small smoke assertion that
each of the three browser source areas has nonzero executable lines **and** branches;
otherwise an apparently successful browser run may have missed an entire asset.
Do not count third-party `jsQR.js` or tests. Use the deterministic browser
tier by default (`surface.spec.js` plus the real-rendered replacement tests
described below). The real stagenet POS suite stays an explicit extended run
because it spends funds and depends on a public node.

Run the PHP suite in its existing `wp-env` tests container with Xdebug's coverage
mode. Provision a coverage-capable test image if the current container lacks
Xdebug. Add a coverage-only PHPUnit configuration with a filter for the plugin's
authored PHP files, including uncovered files. With the checked-in PHPUnit 9.6
version, `--path-coverage` enables the branch/path analysis; it needs Xdebug, as
PCOV supplies lines only. Generate PHPUnit XML or HTML under
`target/coverage/woocommerce/` for branch inspection and Clover for line exchange.
Keep the existing `live-monokulo` group excluded from
the default run. The `xtask` should fail clearly if the container, PHPUnit, or
Xdebug is missing instead of recording zero coverage.

## Entry point and outputs

Proposed commands, implemented as an `xtask` workspace crate and a Cargo alias:

```sh
cargo xtask coverage rust          # one nightly run; lines + branches
cargo xtask coverage all           # Rust + deterministic browser + PHP suites
cargo xtask coverage open          # open the most recent local HTML index
```

### How to inspect a run

The task creates **`target/coverage/index.html`** as a static landing page. It
shows a table of each component's `covered / executable` lines and `covered /
total` branches, test status, and direct links to the native source reports.
Rust component rows also link to crate-filtered file lists built from the JSON;
each file links to its LLVM annotated source page.

The native reports are:

- `rust/index.html`: LLVM's file tree and per-file annotated source, with line
  execution counts and taken/missed branch details.
- `browser/index.html`: Istanbul's file tree and annotated authored JS/TSX source.
- `woocommerce/index.html`: PHPUnit's annotated plugin PHP source.

The `open` command opens that landing page in the default browser. Someone using
the underlying tool directly can open `target/llvm-cov/html/index.html` after the
Rust command above, or run the following to test and open LLVM's report at once:

```sh
cargo +nightly llvm-cov --workspace --locked --branch --open
```

The task also records a manifest with commit, toolchain, collector versions,
executed commands, test result, exclusions, and whether branch collection was
enabled. Store native HTML and JSON/LCOV/XML beside the landing page for
diagnosis. Show `unavailable` rather than `0%` when a collector cannot provide a
metric. Do not merge branch numerators from different languages or set a global
branch threshold.

### Browser test value audit

Review the existing 24 `surface.spec.js` tests before adding gallery-only tests.
The three real stagenet `pos.spec.js` tests cover login, a real order/checkout,
QR refund entry, 0-conf success, and a confirming order that is backgrounded and
later finishes. They **do not** exercise most failure and unusual UI states.
Moreover, they are intentionally excluded from default runs because they need
public stagenet access and real transactions. A deterministic browser test using
Monokulo's *actual rendered UI* and a controlled test engine is the way to get
repeatable coverage of those states. The existing `scanner-test-support` crate
already starts a real network-bound test engine and offers `mark_order_paid`;
extend that test support only for state transitions that the browser suite
cannot otherwise drive. Mock network responses, clock, or camera permission at
those boundaries as needed; keep the HTML, CSS, and shipped JavaScript real.

| Existing surface tests | Unique reason to keep or action to take |
| --- | --- |
| Refund QR, saved/editable, cleared/restored, live update during editing, validation, server/network errors, camera failure, no-JS form (tests 1-4 and 9-14) | Keep the behaviors: stagenet covers only QR upload and saved state. Move assertions onto the real checkout view with controlled responses; consolidate overlapping saved/editable cases after comparing their distinct assertions. |
| Checkout address copy/selection and compact refund layout (tests 6 and 8) | These check interactions and narrow-width geometry missing from stagenet. Move to the real rendered checkout before removing the hand-built DOM versions. |
| POS background/reload/reopen/cancel/search and all status badges (tests 16-17) | Keep the behaviors: stagenet covers background and paid, but not reload, cancel, search, or the other statuses. Run against the real POS app with a controlled engine; the current tests already use the shipped POS bundle but stub its checkout frame. |
| Client embed refund flag/status subscription and challenge proof (tests 18 and 24) | Keep: the real POS suite does not exercise the public client library API. Their mocked HTTP responses are useful protocol inputs; replace only the hand-built page shell where a rendered UI claim is made. |
| Challenge in JS, no-JS, and cross-site frame (tests 21-23) | Keep the browser outcomes: stagenet does not trigger rate limiting. Prefer a real Monokulo challenge response in the deterministic harness; until then, these tests still exercise shipped browser scripts and browser frame behavior. |
| Health indicator polling (test 15) | Keep the healthy/error/unknown transitions; stagenet does not force them. Use the real page for the indicator and a controlled `/status/summary` response. |
| Hand-built layout checks (tests 5 and 7) | Replace with geometry assertions on real checkout/POS DOM, then delete these hand-built versions. The existing `pos-fit.spec.js` checks no scrolling over many real viewport sizes, but does not itself assert header/iframe overlap. |
| Hard-coded CSP framing policy and synthetic `Sec-Fetch-Dest` server (tests 19-20) | Remove once a browser test exercises the **actual** Monokulo response for an allowed and blocked frame. Current tests mainly prove browser behavior against headers/HTML invented in the test; Rust handler tests already assert the real policy and emitted CSP. |

For every test being replaced, record its regression claim, the replacement
test and fixture, and the proof that the replacement fails when that behavior is
broken. Delete the old test in the same change only after the replacement passes
in the default deterministic suite. If no replacement is needed because the
test proves no project behavior beyond an existing Rust or browser test, record
that overlap and delete it. Do not use coverage percentage alone to decide:
line/branch hits cannot show whether an assertion checks the right behavior.
As an additional check, collect browser coverage separately for the current
surface suite and for an explicit stagenet run (serving instrumented browser
assets in both), then compare covered source
locations and branches. Repeat after migration to confirm that moving a test
has not left an exercised path behind. Record the comparison alongside the
assertion inventory; a coverage match alone is insufficient.
The real stagenet tests remain as a separate wiring check of live node,
payment, and browser composition; screenshots do not make them a substitute for
deterministic edge-case tests.

### UI screenshots in the report

Add a **Screenshots** link to the landing page and a small static gallery at
`target/coverage/screenshots/index.html`. Group images by component, test, and
named stage. Each card shows a thumbnail, a plain-language caption, the test
result, and a link to the full-size PNG. The gallery is a visual record of the
tested state; the coverage percentages still come from instrumentation. The
gallery and PNG links must work from the downloaded CI artifact without a server.

Capture a small, deliberate set of checkpoints in the deterministic browser
tier. Add `tests/coverage-visual.spec.js` for the states that need real rendered
checkout markup, and include it alongside `surface.spec.js` in a coverage-only
Playwright config. The visual tests should assert the same key state changes
before capturing them. Move an existing assertion into that spec and delete its
old hand-built-UI version in the same change; do not add screenshot-only or
duplicate tests. These are candidates; start with about ten images and
keep only stages that show a distinct UI state:

| Area | Checkpoints to add after state assertions |
| --- | --- |
| Checkout embed | Initial waiting screen; refund address saved after QR upload; validation error; partial-payment update while the refund field is being edited |
| POS | Empty keypad; order open with checkout frame; order in background stack; reopened order; finished order in the list |
| Challenge | Interstitial in the no-JS test before its timed continuation; checkout after continuation |
| Layout | One narrow POS viewport in addition to the normal desktop capture |

The current `routeCheckout` fixture is minimal hand-written HTML, and the POS
route serves only `<h1>Shared payment view</h1>` inside its checkout iframe.
Before putting checkout or order-open screenshots in the gallery, add a
deterministic local fixture server that renders Monokulo's real checkout view
and styles with fake order/engine data. Point the screenshot subset at that
server and assert the real visual landmarks before capture. Have the coverage
harness start and stop this server; the coverage-only Playwright config should
select both specs and write its HTML report under `target/coverage/`. It must
serve the same instrumented browser assets used by the coverage collector so
these visual tests contribute to the browser line and branch data. Retain the
small hand-written fixtures only for narrow protocol tests that need them,
but do not present those fixtures as product screenshots. The current
`monokulo-client.js` test has no rendered merchant interface, so it should be
linked from the report as a tested library flow without an illustrative
screenshot. If a visual merchant demo is added later, capture it then. The
JavaScript-enabled interstitial can solve too quickly for a stable screenshot;
use the existing no-JS test for the visible challenge checkpoint.

The real stagenet `pos.spec.js` suite can add optional checkpoints for order QR,
0-conf success, confirming, backgrounded, and finished states. It remains outside
the default coverage run. Capture those screenshots without masking: the wallet
data is public stagenet fixture data already in the repository. A failed test
also retains Playwright's diagnostic screenshot and trace in a separate
**Failures** area.
Diagnostic images do not replace the named checkpoints from passing tests.

Add a small helper in `e2e/pos-playwright/` such as
`captureCoverageStage(page, testInfo, stage, options)`. It should be a no-op in
ordinary test runs and activate when the coverage harness sets
`COVERAGE_SCREENSHOTS=1`. Call it **after** the locator assertions for each
checkpoint; do not use sleeps or a screenshot as an assertion. Use a fixed
viewport/device scale for the coverage project and disable animations and the
text caret during capture. Use a page screenshot for the whole POS, and a
locator screenshot inside the checkout iframe when its internal state is the
subject. For a test that creates its own browser context (such as the no-JS
case), pass that page explicitly. Keep stage names stable and unique within a
test, for example `pos-order-open` and `checkout-refund-saved`.

The helper attaches each PNG with `testInfo.attach()` and content type
`image/png`. Configure the coverage Playwright run with the
standard HTML reporter plus a small custom reporter. On each test result, the
custom reporter reads named PNG attachments, copies them into
`target/coverage/screenshots/images/`, and writes a manifest containing suite,
test ID/title, stage name/order, project/viewport, result, retry number, caption,
and relative PNG path. Include the test ID and retry in filenames so parallel
workers and retries cannot overwrite each other. The `xtask` builds the static
gallery from that manifest and validates that every listed image exists. It
should also verify that at least one checkpoint from checkout, POS, and
challenge flows was produced; a missing reporter or skipped fixture must fail
visibly.

Configure Playwright's HTML reporter in the coverage run as a second way to
inspect tests and their attachments. Keep its output and failure traces inside
the coverage artifact, but use the static gallery as the primary entry point:
Playwright's report is normally opened with `npx playwright show-report`, while
the gallery PNG links work directly from a downloaded directory. The coverage
landing page should show a compact strip of key screenshots and a link to the
full gallery alongside the numeric browser coverage row.

Do not generate screenshot baselines or pixel-difference gates as part of this
work. The existing assertions establish the expected state; screenshots make
that state easy to examine. Limit the default gallery to roughly a dozen named
images so that report size and browsing remain manageable.

In CI, print the same component table in the job summary and upload the entire
`target/coverage/` directory as one downloadable artifact. The downloaded
artifact opens locally at `index.html`, with every source link working without a
server. CI artifacts are downloads, not a directly browsable hosted site. If a
hosted URL is needed later, publish the same static directory on the project's
chosen access-controlled site; the collector and report layout need no change.
First add an artifact-producing job with no percentage gate. Once a baseline is
reviewed, gate on test success, nonempty coverage for every intended source area,
and a modest per-component line floor. Keep branch numbers visible as trend data
until the nightly Rust lane and browser source mapping are stable.

## Rollout checks

1. Run the Rust coverage lane twice from a clean coverage directory and confirm
   the same source files and line/branch denominators when the resolved tool
   versions match, with no ignored network tests executed. When versions
   change, display that change next to the coverage comparison.
2. Validate the branch JSON has real branch totals and inspect one known
   conditional in the HTML report. Open the generated landing page and follow
   each component link to annotated source.
3. For each browser asset, deliberately leave one branch untaken and confirm
   that its original source location is marked missing.
4. In PHP, confirm an untaken gateway branch appears in the report and no
   WordPress, WooCommerce, vendor, or test code enters the denominator.
5. Run the deterministic browser suite with screenshot capture enabled. Check
   that every expected named stage appears in the static gallery, its thumbnail
   and full PNG open offline, and its link points to the right test. Repeat with
   two workers and a retry to check filename collisions and ordering.
6. Make one browser test fail deliberately: confirm the named stages captured
   before failure remain visible, and that the separate failure screenshot and
   trace appear in diagnostics. Keep the CI artifact small enough to download.

This document is a proposal; no coverage collector is installed or run by it.

## Tool references

- [Rust compiler instrumentation](https://doc.rust-lang.org/rustc/instrument-coverage.html)
- [`cargo-llvm-cov` commands and limitations](https://github.com/taiki-e/cargo-llvm-cov/blob/main/README.md)
- [`cargo-llvm-cov` branch tracking](https://github.com/taiki-e/cargo-llvm-cov/issues/8)
- [PHPUnit 9.6 coverage options](https://docs.phpunit.de/en/9.6/textui.html)
- [Xdebug branch and path coverage](https://xdebug.org/docs/code_coverage)
- [Istanbul instrumentation API](https://github.com/istanbuljs/istanbuljs/blob/main/packages/istanbul-lib-instrument/api.md)
- [Istanbul browser coverage object and reporting](https://istanbul.js.org/docs/advanced/coverage-object-report/)
- [LLVM HTML source and branch views](https://llvm.org/docs/CommandGuide/llvm-cov.html)
- [Playwright test attachments](https://playwright.dev/docs/api/class-testinfo#test-info-attach)
- [Playwright custom reporters](https://playwright.dev/docs/api/class-reporter)
- [Playwright screenshots](https://playwright.dev/docs/screenshots)
- [Playwright HTML reporter](https://playwright.dev/docs/test-reporters#html-reporter)
