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

## Resume next

Implement 0.2 `cargo xtask` commands and manifest validation.
