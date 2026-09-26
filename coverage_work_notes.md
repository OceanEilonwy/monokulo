# Coverage implementation notes

Plan: [docs/COVERAGE_WBS.md](docs/COVERAGE_WBS.md). Complete tasks in plan order,
except that browser instrumentation 3.1–3.2 must follow the 2.1 inventory and
precede deletion of old browser tests.

## Starting state

- Branch started at `3f2558a` with existing, uncommitted POS changes and two
  untracked planning documents. These belong to the previous work; preserve them
  and stage only coverage implementation files in each commit.
- `nightly` and `cargo-llvm-cov` were absent locally. Docker and Node are present.

## Progress

- 0.1 complete: manifest schema and example, source boundaries, and the
  unpinned nightly/collector refresh script. JSON fixture syntax and shell
  syntax checked. Full schema validation awaits the xtask validator; Python's
  `jsonschema` package is not installed in this environment.
- 0.2 in progress: `cargo xtask coverage` entry point compiles and its help
  works. It preserves a run manifest after each component and logs each child.
  Browser and WooCommerce collector scripts are still pending.
- 1.1 complete: nightly 1.100.0 and cargo-llvm-cov 0.9.1 installed. The
  workspace test run passed and produced `rust/index.html`, `rust/raw.json`,
  and `rust.json` from one profile set. Measured 24,451 / 27,070 lines and
  1,294 / 1,698 branches. `scanner` has 370 and `monokulo` 1,060 branch
  denominator. Ignored live-node and stagenet tests stayed ignored. The first
  attempt had concurrent tool installation and the second exposed LLVM's
  nested `html/` output; both issues are resolved. The source tree was dirty
  before coverage work began; reports now mark that condition.

## Resume next

Build crate-filtered Rust summaries and annotated-source links for 1.2. Then
continue 0.2 browser/PHP collectors and the 2.1 browser inventory.
