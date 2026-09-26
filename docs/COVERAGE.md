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
cargo xtask coverage open
```

`--help` lists the commands. Each collector writes its own test log and
machine-readable manifest under `target/coverage/`; `all` also writes
`index.html`, `run.json`, and a validated offline artifact. A collector only
cleans its own output directory. Completed component reports remain if a
later component fails. An unavailable metric is labeled `unavailable`, not
0%.

## Local setup

- Rust: `rustup`, Cargo, a network connection for `rustup update nightly`
  and the latest unpinned `cargo-llvm-cov` installation. The Rust command
  refreshes both on every run and installs nightly LLVM tools.
- Browser: Node 24, `npm ci --prefix e2e/pos-playwright`,
  `npm install --prefix crates/monokulo/pos-ui`, and Chromium from
  `cd e2e/pos-playwright && npx playwright install chromium`. The browser
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
for its start/stop commands. The coverage script reports a missing tool,
dependencies, or test container explicitly and exits nonzero.

## Reading the artifact

Open `target/coverage/index.html` directly, or download the CI artifact,
extract it, and open its `index.html`. The landing page links to annotated
LLVM source, Istanbul's authored JS/TSX report, PHPUnit's branch and path
pages, test logs, and a static screenshot gallery. The gallery has stable
checkout, POS, and challenge stages, each linked to its full PNG and the
Playwright test result. Playwright's own HTML report retains attachments and
failure traces. The gallery and native reports use relative links; no local
server is needed.

Rust metrics include production source in Cargo workspace crates, with a
separate `mock-woocommerce` row. Browser metrics include checked-in
`checkout.js`, `challenge.js`, `monokulo-client.js`, and POS `main.tsx`.
Generated `pos-app.js`, `jsQR.js`, vendor packages, tests, and the `xtask`
crate are excluded from product denominators. PHP metrics include only
`monokulo.php` and authored `includes/*.php`; branch and path counts come
from PHPUnit's native Xdebug coverage object, not Clover line totals.

The first complete local report and conservative line floors are recorded in
`docs/coverage-line-baseline.json`. `scripts/validate-coverage.py` checks the
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
