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
- 1.1 in progress: nightly 1.100.0 and cargo-llvm-cov 0.9.1 installed. The
  first Rust collection is compiling the workspace. An initial concurrent
  tool installation briefly conflicted on `llvm-tools`; the later run started
  after installation completed.

## Resume next

Inspect the Rust collector result and JSON, then add validated component
manifests. Continue 0.2 and 1.1 before 1.2.
