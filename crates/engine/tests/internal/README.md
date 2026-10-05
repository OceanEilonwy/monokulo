# Engine properties and fuzz support

Engine property tests, scale scenarios, independent fuzz oracles, and their
shared fixtures live here. The `src/` tree keeps shipping code and short module
registrations. Files are grouped by the engine area they exercise; shared
oracles and fixtures live at this directory's root.

These files remain **in-crate tests**, loaded through `#[path]` from their owning
modules. That preserves access to private implementation details without adding
public testing APIs. The module hierarchy and test names are unchanged, so
nextest filters, crash subprocesses, mutation checks and coverage exclusions
continue to work. The large scanner, work, HTTP and proof scenario modules live
here too because the properties share their fixtures.

- `work/`: scheduler, block effects, mempool, engine histories, portfolio models,
  concurrency, lifecycle and scale scenarios.
- `store/`: migration, worker queue, reorg staging and queue scale scenarios.
- `http/`, `proof/`, `status/`: authorization, order boundaries, verifier histories
  and pure status derivation.
- `daemon/`, `scanner/`: scripted daemon and scanner scenarios used by the suites.
- Root files: shared support, notification/lifecycle, resource and webhook checks.

`#[cfg(test)]` includes generated tests only in test builds. Shared fuzz oracles
and fixtures use `#[cfg(any(test, feature = "fuzzing"))]`; ordinary engine builds
exclude them. The cargo-fuzz drivers, reviewed seed inputs and disposable corpus
remain in the repository's central `fuzz/` package.

Each property module pins its regression file through
`property_support::persist`. Existing counterexamples remain in
`crates/engine/proptest-regressions/` and replay before fresh cases. Do not rely
on Proptest's source-location-derived persistence here: moving a test file must
not lose its saved cases. Explicit case counts, RNG seeds, shrinking options and
the option to disable persistence retain their existing behavior.

Run the same commands as before, from the repository root:

```sh
cargo nextest run -p engine --lib --locked -E 'test(::properties::)'
scripts/engine-scale.sh default
scripts/engine-scale.sh zmq
scripts/engine-fuzz.sh history 60
```

See [`docs/TESTING.md`](../../../../docs/TESTING.md) for the coverage map, scenario
ranges, regression replay instructions and mutation runner.
