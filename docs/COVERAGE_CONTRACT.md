# Coverage run contract

The run root is `target/coverage/`, which is ignored through `/target` in
`.gitignore`. Each collector owns one directory (`rust/`, `browser/`, or
`woocommerce/`) and one root manifest (`rust.json`, `browser.json`, or
`woocommerce.json`). The common landing page is `index.html`. Paths in manifests
are relative to the run root so an extracted artifact works offline.

Every component manifest follows [the JSON schema](coverage-manifest.schema.json);
[the example](coverage-manifest.example.json) is a fixture for validation.
`revision` is the HEAD commit; `source_dirty` indicates that the working tree
contained changes during collection, so that commit alone does not identify
the exact measured source.
`covered` and `total` are integers only when measured. An unavailable metric
uses `null` for both numbers and names that metric in `unavailable`; zero means
that the collector measured zero. A failed test retains its exit code and log
path. Versions are resolved during the run, never inferred from a pin.

## Source boundaries

| Collector | Included | Excluded |
| --- | --- | --- |
| Rust | Production `src/` of Cargo workspace members | `xtask`, tests, examples, benches, build outputs, dependencies |
| Browser | `crates/monokulo/static/{checkout,challenge,monokulo-client}.js`, `crates/monokulo/pos-ui/src/*.tsx` | Generated `pos-app.js`, `jsQR.js`, vendors, Playwright tests |
| PHP | `plugins/woocommerce/monokulo.php`, `plugins/woocommerce/includes/` | WordPress, WooCommerce, vendor, tests |

The default run excludes ignored live stagenet, Tor, and `live-monokulo` tests.
Rust line and branch totals come from a single instrumented workspace test
execution. Browser and PHP collectors retain their native definitions of a
branch; counts from different languages must not be summed.

The Rust collector refreshes the newest available nightly compiler and
`cargo-llvm-cov` release on each run with `rustup update nightly`,
`rustup component add llvm-tools-preview --toolchain nightly`, and
`cargo +stable install cargo-llvm-cov --locked`. These commands need network
access. Exact `rustc`, Cargo, and collector versions go into each manifest.

## Commands

`cargo xtask coverage --help` lists the entry points. `rust`, `browser`, and
`woocommerce` run only their collector. `all` runs all three sequentially and
writes `target/coverage/run.json` after each one, retaining completed reports
when a later collector fails. Each collector's old output directory is removed
at its own start; ordinary Cargo build artifacts are untouched. Test output is
saved in `<collector>/test.log`. `open` opens the last landing page with the
system default browser and fails when that page does not exist.

The Rust collector also writes `rust-crates.json` and
`rust/crates/index.html`. Its crate rows are derived from `cargo metadata`,
including a separate `mock-woocommerce` row. Each crate page lists measured
production files with links to LLVM's annotated source. A source file that
contributes no executable code to the default build, or requires a feature
that is not enabled, is listed as unavailable without inventing a zero-line
denominator. The crate totals are checked against LLVM's workspace totals.

The browser collector builds instrumented assets under `browser/assets/`, runs
the deterministic Playwright suite with two workers, stores per-test frame
snapshots under `browser/raw/`, and merges them into `browser/index.html`,
`browser/lcov.info`, `browser/coverage-final.json`, and `browser.json`.
The collector checks that each of the four authored source areas has executed
lines and branches. Checkout code inside the real POS iframe contributes to
the same report. Source maps attribute the coverage-only POS bundle to
`pos-ui/src/main.tsx`.
