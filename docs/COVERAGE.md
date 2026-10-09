# Coverage and UI evidence

Run coverage from the repository root. The default `all` command runs
workspace Rust tests, deterministic browser tests against a local controlled
engine, and the WooCommerce PHPUnit suite. It does not contact stagenet or
spend wallet funds.

```sh
cargo xtask coverage rust
cargo xtask coverage browser
cargo xtask coverage woocommerce
cargo xtask coverage all
cargo xtask coverage report
cargo xtask coverage open
```

`--help` lists the commands. Each collector writes its own test log,
JUnit report, result (`<component>/result.json`) and machine-readable
manifest under `target/coverage/`; `all` runs the three collectors side by
side and also writes `index.html`, `run.json`, and a validated offline
artifact. `report` does the same from collector outputs already in
`target/coverage/`, which is how CI joins its separate collector jobs. A
collector only cleans its own output directory. Completed component reports
remain if another component fails. An unavailable metric is labeled
`unavailable`, not 0%.

## Local setup

- Rust: `rustup`, Cargo, a network connection for `rustup update nightly`
  and the latest unpinned `cargo-llvm-cov` and `cargo-nextest`
  installations. The Rust command refreshes them on every run and installs
  nightly LLVM tools (CI installs them prebuilt and sets
  `COVERAGE_TOOLS_PREINSTALLED=1` to skip this). Tests run under nextest
  (`.config/nextest.toml`, profile `ci`), which runs the test binaries side
  by side.
- Browser: Node 24, `npm ci --prefix e2e/browser`,
  `npm ci --prefix crates/monokulo/pos-ui` (Cargo builds
  the POS app from it, see `crates/monokulo/build.rs`), and Chromium from
  `cd e2e/browser && npx playwright install chromium`. The browser
  fixture builds with Cargo `--offline`; fetch Rust dependencies before
  starting if the Cargo cache is empty.
- WooCommerce: Docker, Composer, and wp-env. Run
  `composer install --working-dir=plugins/woocommerce`, then from
  `plugins/woocommerce` run
  `npx @wordpress/env start --config=.wp-env.coverage.json`. This isolated
  config mounts this plugin as `monokulo`, beside WooCommerce, and starts the
  test database on port 8899. `coverage woocommerce` derives a temporary
  Xdebug-capable image from that running test container. Stop it with
  `npx @wordpress/env stop --config=.wp-env.coverage.json` when finished.

If wp-env downloads time out locally, set
`NODE_OPTIONS='--dns-result-order=ipv4first --network-family-autoselection-attempt-timeout=5000'`
for its start/stop commands. `cargo xtask coverage` reports a missing tool,
dependencies, or test container explicitly and exits nonzero.

## Reading the artifact

Open `target/coverage/index.html` directly, or download the CI artifact,
extract it, and open its `index.html`. The landing page links to annotated
LLVM source, Istanbul's authored JS/TSX report, PHPUnit's branch and path
pages, test logs, and a static screenshot gallery. The gallery files its
stages by page or feature (checkout, challenge, POS, POS session timeline,
store settings, Logs), each card linked to its full PNGs and the Playwright
test result. A toggle at the top picks the size shown: Desktop, Tablet or
Mobile, and Portrait or Landscape within Tablet and Mobile; a stage captured
at fewer sizes (the Logs page is desktop only) says so. The toggle is radio
buttons and CSS, so the page works from disk without script. Playwright's
own HTML reports retain attachments and failure traces. The gallery and
native reports use relative links; no local server is needed.

The browser collector runs two suites as projects of one Playwright run
(`coverage-browser.config.js`): the deterministic browser tests against a
fixture server, and the real-binaries tests (the specs selected by
`real-binaries.config.js`: the real engine and monokulo binaries with a fake
monerod). They share one report, one gallery and, in specs using
`coverage-test.js`, the instrumented browser coverage, and workers (half
the machine's threads, from four to eight; `E2E_WORKERS` sets another number): the fixture tests spread test by test,
the real-binaries tests file by file, each spec file with processes of its own.

## In CI

On main, once every platform's tests pass, `.github/workflows/release.yml`'s
`coverage` job runs each collector in a job of its own (`tests.yml` with
`coverage: true`: Rust, browser, WooCommerce and the engine stress run),
then its Summary job joins their outputs with `cargo xtask coverage report`
and uploads the combined artifact (`coverage-<sha>`). Each job's summary
has a table of passed, failed and skipped tests (`cargo xtask test-summary`,
from the JUnit reports) with the failures listed; Summary's also has the
coverage table. A manual Checks run with coverage does the same for any
branch.

## On GitHub Pages

Main's results don't need downloading:
<https://oceaneilonwy.github.io/monokulo/quality/> shows every test, the
coverage of each crate and file (linked to its annotated source), the
nightly property and fuzz runs, the stress points and the screenshot
gallery. `.github/workflows/pages.yml` rebuilds it whenever CI, the engine
property, fuzz or scale workflows, or the OpenWrt package finish on main:
`cargo xtask pages fetch target/pages` downloads the newest artifact of
each and `cargo xtask pages build` (`xtask/src/pages/`) renders a page per
section from them, plus `badge.json` (the README's coverage badge), shipping
only the report pages the pages link and the files those need. The pages
are plain HTML that reads without JavaScript; `web/pages/quality/`
holds their stylesheet and the script that adds the filters and the
screenshot viewer. Only the ZMQ build of the property and fuzz runs is
shown (`--feature` on both commands picks the other).

To see a local run the same way, after `cargo xtask coverage all`:

```sh
cargo xtask pages build --coverage target/coverage --out target/quality
cargo xtask serve target/quality   # then open http://127.0.0.1:8000
```

Opening `target/quality/index.html` from disk works too; the server only
matches how Pages serves the site.

Rust metrics include production source in Cargo workspace crates, with a
separate `mock-woocommerce` row. Browser metrics include checked-in
`checkout.js`, `challenge.js`, `monokulo-client.js`, and POS `main.tsx` and
`timeline.ts`.
Generated `pos-app.js`, `jsQR.js`, vendor packages, tests, and the `xtask`
crate are excluded from product denominators. PHP metrics include only
`monokulo.php` and authored `includes/*.php`; branch and path counts come
from PHPUnit's native Xdebug coverage object, not Clover line totals.

The first complete local report and conservative line floors are recorded in
`docs/coverage-line-baseline.json`. `cargo xtask coverage validate` checks the
manifest schema, required source areas, branch denominators, report links,
screenshot paths, and line floors during `coverage all`. When any recorded
tool version differs from the baseline, it reports the drift and treats the
floor as trend data pending review. Branch counts remain trend data.

## Explicit paid stagenet profile

```sh
cargo xtask coverage stagenet
```

This separate command instruments the real POS stagenet suite and writes
`target/coverage/stagenet/` plus `stagenet.json`. It starts the network-bound
test harness and broadcasts real stagenet transactions from the repository's
public wallet fixture. It is excluded from `all` and CI. Use it when the
node and fixture funds are available; compare its paid happy path with the
deterministic browser report rather than adding the two percentages together.

See [the browser migration audit](COVERAGE_MIGRATION_AUDIT.md) for the before
and after path comparison and [the work notes](../coverage_work_notes.md) for
progress and exact measured results.
