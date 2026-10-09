# Engine verification guide

The engine uses generated histories, independent expected ledgers, real scanner
cryptography and SQLite, controlled failure injection, and libFuzzer. Passing
checks provide bounded evidence; they do not prove every possible execution or
certify that the engine is bug-free.

## Structure and shared execution

| Location | Responsibility |
|---|---|
| `crates/engine/src/` | Shipping implementation and short conditional test-module registrations |
| `crates/engine/tests/verification/<domain>/` | Private in-crate properties, scenarios, models and scale checks, grouped by the engine component they exercise |
| `verification/work/portfolio/scenario.rs` | Typed commands, independently generated setup, and the versioned fuzz codec |
| `verification/work/portfolio/runner.rs` and `effects.rs` | Drive actual engine rounds, controlled daemon/custody/SQL failures, notifications and recovery |
| `verification/work/portfolio/model.rs` | Expected recipients, amounts, canonical evidence, durable identities and accepted statuses, derived independently of scanner results |
| `verification/support/` | Shared backend replacement, configuration, RPC controls, rendezvous and fuzz adapters |
| `crates/engine/proptest-regressions/` | Saved cases, one file per property under its module's explicitly pinned directory; moving a module does not move its replay files |
| `fuzz/fuzz_targets/`, `fuzz/seeds/` | Nine thin fuzz drivers and reviewed seed inputs; drivers reuse the property harness and oracle |
| `fuzz/corpus/`, `fuzz/artifacts/`, `target/engine-exploration/` | Ignored mutable corpus, failures and measured reports |

`tests/verification` is a shared verification source tree, not a collection of
Cargo integration-test crates. `#[path]` loads its files inside their owning
modules, retaining private access and existing test identities. Ordinary shipping
builds exclude it; shared fuzz code requires the `fuzzing` feature. The
[tree README](../crates/engine/tests/verification/README.md) describes domain roles;
[TESTING.md](TESTING.md) gives the broader inventory and scenario ranges.

## Review corrections and evidence

An independent GPT-6 Astra review sampled implementation and harness code at
`4a4fcf3`. The corrections below address that review; passing local checks does
not constitute a second independent review or safety certification.

| Review concern | Implemented correction | Falsifiable check |
|---|---|---|
| Retry expiry cleared implementation and model together | Exercise production retry-map upkeep; model maintains independent counts/deadlines | Before/at/after one-hour expiry, key-local resets, and a never-forget mutant |
| Direct restart retained the original active store | Replace both observer and executor through one shared backend helper | Previous observer released, TEMP state absent, every persisted table equal, subsequent operations use replacement |
| Fault counters were satisfied by isolated component calls | Dedicated outstanding payment, separate component/engine controls, actual round cancellation and post-component custody delta | No credit on failed custody; actual SQL denials; exactly one expected payment after recovery |
| SQL failure always struck access zero | Typed selection of all accesses versus writes and positions 0–3 | Fixed sweep exercises all eight choices through both direct and worker backends; records denied operation names |
| Byte coupling obscured histories and shrinking | Independently generated setup plus typed command vectors and semantic failure traces | Codec round-trip, composed transition histories, and reviewed seeds checked against their scenarios |
| Verification code distracted from production | Domain grouping with shared roles and stable module identities | Before/after inventory comparison; unchanged private access and runner filters |
| Harness duplication and many-argument effects | Shared backend controls and borrowed effects context; separate independent portfolio model | Both property and fuzz entry points execute the same runner and oracle |
| Floating tooling and unmeasured budgets | Pin exploration toolchain/fuzzer, record versions, calibrate seeds and report actual growth | Reporter tests; campaigns fail if only corpus initialization executes |
| Selected mutations and serialized Loom implied broader assurance | Add deterministic critical decision grids and explicit exhaustive policy-call permutations | 27 selected mutants in both configurations; unrelated panics/timeouts/build errors are invalid results |

Typed portfolio properties generate 0–16 commands; the codec supports up to 32
and decodes any bytes. Commands cover late arrival, mining, ordinary extension of
1–4 blocks, reorgs of up to four heights, branch rebuilds that keep or move mined
transactions, disappearance, spent votes, proof lag of 0–7 blocks or mismatches,
clock advances of 1–8 × 301 seconds, backend restart, fast passes, custody
replacement, SQL operation/position faults, notification delivery and rounds.
Individual invoice goals, confirmation thresholds of 0–3, expiry and a pool-first
start are generated separately. Invalid arrivals, removals and spent evidence
are counted as skipped. Mining an already-mined transaction creates an empty block
instead: the report records skipped mining and the separate extension effect.

Mutation evidence consists of 54 detected runs (27 defects × default/ZMQ), with
healthy baselines passing and no surviving or invalid mutants. This is an
acceptance set, not a statistical mutation score. It includes funding,
confirmation, expiry and overpayment equality, retry expiry/deadline/free attempts,
scan ownership and the domain defect examples listed in TESTING.md. The status
boundary grid checks 720 combinations independently of production decisions.

Measured campaign examples from the review work:

| Campaign | Executions | After initialization | Unique corpus growth | Instrumentation growth |
|---|---:|---:|---:|---|
| Status, default, seed 432, 10 seconds | 275,097 | 274,526 | 32 | No additional coverage/features in the already explored corpus |
| Portfolio, default, seed 432, 180 seconds | 163 | 26 | 13 | +64 coverage counters, +2,763 features |
| Portfolio, final ZMQ implementation, seed 434, 180 seconds | 173 | 10 | 5 | +9 coverage counters, +1,495 features |

The portfolio run recorded 163 successful-case semantic observations; reviewed
seed median cost was 1.84 seconds and maximum was 2.55 seconds. These observations
explain its 900-second default campaign budget and 120-second per-input deadline.
The final ZMQ smoke campaign recorded 173 successful cases and only ten new-input
executions after initialization, again showing why sustained runs need the larger
default budget. They are measurements of particular runs, not portable performance guarantees or
source-line coverage percentages. Raw logs, corpus checksums, revision/dirty state,
compiler/fuzzer versions, calibration and semantic JSONL are retained in a unique
`<target>/<features>/<seed>/<revision>-<run-id>/` directory per invocation, alongside
`report.json`. Early failures produce terminal reports; invalid or absent completion
metrics fail validation. Reviewed corpus seeds are refreshed by content hash;
calibration rejects missing or empty seed sets.
Semantic counts distinguish recorded-plus-synthetic fixtures from
synthetic-only fixtures, and selected commands from applied transitions and skipped
commands. Applied counts identify accepted domain actions, not necessarily money
changes: an idempotent proof update or a round still performs real engine effects.
Counts on other targets remain unavailable rather than
being invented from instrumentation counters.

## Running and reproducing checks

Run from the repository root. Saved regressions replay before fresh property cases.

```sh
# Ordinary properties and scenarios; repeat with --features engine/zmq,key-custody/snp.
PROPTEST_CASES=128 ENGINE_PROOF_CASES=16 PROPTEST_RNG_SEED=431 \
  cargo nextest run -p engine -p key-custody --lib --locked --profile ci

# Expensive scale fixtures include explicitly ignored tests.
cargo xtask engine scale default
cargo xtask engine scale zmq

# Every selected mutation and its healthy baseline, both configurations.
cargo test -p xtask mutations
cargo xtask engine mutations --features both --cases 32 --seed 241

# Exhaustive serialized policy event orders, both configurations.
cargo test --manifest-path fuzz/Cargo.toml --locked --test interleavings
cargo test --manifest-path fuzz/Cargo.toml --locked --features zmq --test interleavings

# Calibrated, instrumented campaign; omit seconds to use the target default.
RUSTUP_TOOLCHAIN=nightly-2026-10-02 ENGINE_FUZZ_SEED=432 cargo xtask engine fuzz portfolio
RUSTUP_TOOLCHAIN=nightly-2026-10-02 ENGINE_FUZZ_SEED=433 cargo xtask engine fuzz portfolio 900 zmq
cargo test -p xtask exploration
```

Exploration CI pins `nightly-2026-10-02` and cargo-fuzz `0.13.2`; ordinary repository
builds retain the project's nightly policy. The fuzz runner explicitly selects
rustc's native host triple for build, calibration and execution: a musl-built
cargo-fuzz installer otherwise defaults to musl, whose static libc is incompatible
with AddressSanitizer. Use the versions recorded in a run's
`replay.json` for reproduction. Run `cargo fuzz build --fuzz-dir fuzz` (and repeat
with `--features zmq`) to build all nine AddressSanitizer targets. Their binaries
can replay reviewed seeds with `-runs=0`; this is a replay check, not sustained
exploration. Scheduled/manual property and fuzz workflows upload regressions,
logs and reports. PR CI runs ordinary tests and both scale configurations;
mutation and sustained fuzz campaigns have separate workflows. Evidence-runner
regressions run in a small PR workflow and once before scheduled/manual fuzz and
property matrices. Shared temporary database ownership cleans SQLite sidecars and
crash markers for both test and fuzz builds; legacy fixture aliases remain for
existing test callers.

The final engine inventories contain 863 tests in the default build and 869 in
the ZMQ build, including 21 explicitly ignored tests in each. The ordinary engine
and key-custody run therefore executes 913 default / 919 ZMQ tests when SNP custody
is enabled with ZMQ. These totals include examples and scenario tests as well as
properties. Final local smoke validation used eight generated cases, four proof
cases and RNG seed 431, in addition to persisted regressions and deterministic
sweeps; routine and scheduled CI retain their larger budgets. The separate scale runner executes all 15 scale tests in each build,
including its ignored large fixtures. Other ignored expensive fixtures retain
their separate documented runners.

## Assurance boundaries

- Status uses an independent wider aggregate specification. The scheduler's
  ancestral reference is differential regression protection, supplemented by
  independent progress and generation constraints; common ancestral mistakes
  remain possible.
- Portfolio expected funds come from planned recipients and amounts, not scanner
  output. Synthetic fixtures exercise real scanning but may have invalid network
  signatures. Recorded bodies supply a separate provenance category; neither
  category proves consensus validation or independently validates crypto primitives.
- Portfolio proof commands control trusted evidence and hash matching. Separate
  real-verifier fixtures test verifier integration. Daemon response parsing and
  PoW arithmetic remain outside this expansion, as agreed.
- Connection replacement and worker restart are distinct from killing an OS
  process. Dedicated subprocess crash suites cover process boundaries; portfolio
  reopen controls alone do not claim that assurance or that an old worker joined.
- Exhaustive permutations cover small serialized pure-policy call sequences.
  Real rendezvous, cancellation and database/worker tests cover selected actual
  overlaps; neither is exhaustive exploration of Tokio, parking_lot or SQLite
  interleavings or memory races.
- A larger green test count is not evidence of greater semantic diversity.
  Inspect actual command/boundary observations, minimized failures, mutation
  results and measured corpus/instrumentation growth. Retain a concrete named
  regression when a generated case discovers a bug.
