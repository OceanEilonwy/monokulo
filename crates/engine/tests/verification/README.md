# Engine properties and fuzz support

Engine property tests, scale scenarios, independent fuzz oracles, and their
shared fixtures live here. The `src/` tree keeps shipping code and short module
registrations. Files are grouped by the engine area they exercise; cross-domain support lives under `support/`.

These files remain **in-crate tests**, loaded through `#[path]` from their owning
modules. That preserves access to private implementation details without adding
public testing APIs. The module hierarchy and test names are unchanged, so
nextest filters, crash subprocesses, mutation checks and coverage exclusions
continue to work. The large scanner, work, HTTP and proof scenario modules live
here too because the properties share their fixtures.

- `work/portfolio/`: versioned scenarios, runner, independent model, effects, fixtures and backend controls.
- `work/history/`: single-wallet histories, daemon scripts, fixtures and expansions.
- `work/scheduler/`, `work/blocks/`, `work/mempool/`: policy/model properties and scale checks.
- `work/money/`, `work/nodes/`, `work/concurrency/`, `work/lifecycle/`: composed effect scenarios.
- `store/`: migrations, worker queue, durable reorg work and scale scenarios.
- `http/`, `proof/`, `status/`: authorization, verifier histories and status derivation.
- `notifications/`, `lifecycle/`, `resources/`, `inputs/`, `webhooks/`: their domain checks.
- `daemon/`, `scanner/`: reusable daemon and scanner fixtures/scenarios.
- `support/`: shared runtime/config, temporary database ownership, backend replacement,
  rendezvous, RPC and fuzz adapters.

Within domains, `properties.rs` generates cases, `scenario.rs` defines commands,
`model.rs` derives independent expectations, `fixtures.rs` creates inputs,
`effects.rs` supplies controllable IO, and `scale.rs` holds expensive bounds.
Descriptive existing filenames remain where a domain has several distinct suites.
These roles are organizational: a model must remain independent of the production
algorithm even when property and fuzz drivers share that model. No suite is copied
into a second fuzz-only implementation.

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
scripts/engine-fuzz.sh history
```

See the [engine verification guide](../../../../docs/ENGINE_VERIFICATION.md) for
harness roles and assurance limits, and [`docs/TESTING.md`](../../../../docs/TESTING.md) for the coverage map, scenario
ranges, regression replay instructions and mutation runner.
