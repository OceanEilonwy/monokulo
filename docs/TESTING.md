# Monokulo — Test Suite Document

Companion to [`DESIGN.md`](DESIGN.md). For each area: what to test, why it matters
enough to test deliberately (rather than relying on incidental coverage), and how to
implement it. Organized by component, in roughly the order a component would be built.

## 0. Testing Philosophy

- **Prefer real cryptographic primitives over mocks wherever feasible.** The
  `key_custody` tests already do this — they scan a real transaction, lifted
  byte-for-byte from `monero-rs`'s own test suite, against a real view pair, rather
  than asserting against a hand-rolled stub of what scanning "should" do. A mocked
  crypto layer can pass while the real math is wrong; a real fixture can't.
- **Mock only the one boundary that can't run in CI: `monerod` itself.** Everything
  behind `MoneroDaemonClient` (§DESIGN.md 7.1) gets a scripted fake for unit-level
  scanner/reorg tests. A separate, smaller integration tier runs against a real
  `monerod --regtest` for wiring-level confidence (§10).
- **Pure functions get exhaustive, table-driven tests.** The status-derivation
  function (§DESIGN.md 7.6) is the highest-value target for this: it's a pure function
  of a handful of inputs, every branch is enumerable, and it is the single source of
  truth for what a merchant and customer see.
- **Concurrency-sensitive components get stress tests, not just unit tests.** The
  writer actor's single-writer property and minor-index uniqueness are *assumptions*
  the rest of the system depends on — the risk category here is races, so the test
  has to actually create concurrency, not just call methods sequentially.
- **Security-relevant negative tests are not optional.** IDOR prevention, SSRF
  mitigation, and webhook signature verification each get dedicated adversarial tests,
  not incidental coverage from happy-path tests.

### Generated engine tests

Property tests, scale scenarios, fuzz oracles and shared fixtures are centralized
under [`crates/engine/tests/verification/`](../crates/engine/tests/verification/README.md),
grouped by engine area. Production modules contain short `#[path]` registrations;
the tests retain their private access and existing module names. Cargo-fuzz drivers
and seeds remain in the root `fuzz/` package. Saved regression files have explicitly
pinned paths, so reorganizing the sources preserves replay and runner commands.
The [engine verification guide](ENGINE_VERIFICATION.md) summarizes harness roles,
review corrections, evidence and the limits of these checks.

The engine uses [Proptest](https://proptest-rs.github.io/proptest/) as a dev-only
dependency. The first suite, `status::properties`, checks status derivation against
an independent aggregate specification and checks payment ordering, confirmation
growth, additional payments, expiry, and splitting payments with identical evidence.
Generators mix small values with full-width integers and funding boundaries,
including saturated totals. They respect the order API's positive expected amount
and the rule that mempool payments have zero confirmations. Named example tests
remain alongside these generated checks.

These are ordinary, always-enabled Rust tests. `cargo test`, nextest, the existing
Linux/macOS CI test jobs, and Rust coverage all run them. Status properties use **256
generated cases per property**; scanner properties use **64 cases per property**, and the four verifier
history properties plus the real verifier/scanner integration use **16 cases per property** because they use real proof
fixtures and verifier workers,
plus any persisted regressions. `PROPTEST_CASES` overrides these budgets; no live daemon or
extra test service is needed. Run from the repository root:

```sh
# Status properties and existing status examples.
cargo test -p engine --lib --locked status::

# Properties alone through the same runner used by CI.
cargo nextest run -p engine --lib --locked -E 'test(/^status::properties::/)'

# Extended local exploration: 10,000 cases per property.
PROPTEST_CASES=10000 cargo test -p engine --lib --locked status::properties::

# Repeat an exploration with a chosen decimal u64 RNG seed.
PROPTEST_CASES=10000 PROPTEST_RNG_SEED=42 cargo test -p engine --lib --locked status::properties::

# Scanner histories and the named regressions they discovered.
cargo nextest run -p engine --lib --locked -E 'test(/^work::tests::properties::/)'

# Larger reproducible scanner exploration (128 cases for each property).
PROPTEST_CASES=128 PROPTEST_RNG_SEED=42 cargo test -p engine --lib --locked work::tests::properties::
```

Normal runs use fresh randomness. For a failure, Proptest shrinks the input and
prints the counterexample; it also saves a replay seed under
`crates/engine/proptest-regressions/` (`status/properties.txt`, `work/properties.txt`, `work/money_properties.txt`, `work/expansion_properties.txt`,
`work/node_properties.txt` and `proof/node_properties.txt`). Subsequent
runs replay persisted seeds before new cases. CI uploads these directories on
test/coverage failure. Download the artifact and restore the directory under the
corresponding crate, then rerun the failing test. Keep regression files in Git after
a fix, and add a named example for a discovered bug: seeds depend on the strategy,
so a concrete example protects the case when generators change. Do not ignore the
regression directory. See [Proptest's persistence documentation](https://proptest-rs.github.io/proptest/proptest/failure-persistence.html).

The scanner suite (`crates/engine/tests/verification/work/properties.rs`, registered under
`work::tests::properties`) includes generated histories and focused properties. The original five history families cover:

- Histories of up to 24 optional events: mining, pool arrival/removal, forks,
  double-spend evidence, daemon/custody outages, failures of individual daemon calls,
  SQLite statement faults, rounds, recovery checks, and restarts. Every case also includes initial payment observation/mining, a final
  fork and restart. A separate model tracks the canonical chain, transaction
  location and evidence, without calling production status/reorg decision helpers.
- Interrupted block scans with a real payment staged before the block finishes,
  followed by a restart and a fork whose replacement may or may not pay the order.
  Uncommitted matches must never become payments, and old checkpoints must clear.
- Reorg jobs interrupted by daemon and custody outages, restarted while still
  open, then recovered. Double-spend evidence may void a missing transaction;
  mining the original transaction again must restore the same payment row.
- Replacement branches of different lengths, including shorter tips, with a
  payment moved, removed, or unchanged below the fork. Confirmation counts and
  settlement must converge to the new depth after restart. These replacements
  also appear in the general histories.
- Two to five tenants sharing the fixture wallet through distinct custody handles,
  with one backend failing. Tenant pages range from one to three entries; healthy
  tenants must reach the tip without duplicate payments, while the failed tenant
  stays put, then catches up after recovery and restart.

Fifteen additional properties in `work/money/properties.rs` exercise money guarantees:

| Generated scenario | Guarantee checked |
|---|---|
| Up to eight independent payments across three orders; partial, exact and excess funding; different mining depths and replacement survivors | Each output credits only its destination order once; settlement requires enough funds at the configured depth; affirmative double spends remove only affected funds; re-mining restores credit |
| Late additional payment after an order is settled; disappearance with or without spent-input evidence | Confirmed funds continue covering the order; no false unconfirmed/confirming downgrade or webhook; only affected extra money loses credit |
| Multiple outputs in one transaction, including unassigned subaddresses | Every matching output retains its own index and amount; no output credits the wrong order; tenant defaults and order confirmation overrides apply independently |
| Two to five transactions reusing one output key | Credit only one spendable output; never settle unresolved copies, even with a zero-confirmation policy; the credited winner changes correctly through forks and disappearance |
| Payment mined before expiry while custody is down | Expiry waits for catch-up; delayed scanning finds money rather than prematurely expiring the order |
| Late pool payments just inside, at, or outside a closed order's grace boundary | The configured grace boundary controls rescanning, including expired orders reopening when paid |
| Tenants registering keys after the network has scanned their payment blocks; small catch-up pages | Unregistered keys cannot hide healthy tenants behind the page limit; every tenant catches up and credits once when registered |
| New order added while its transaction is already cached | A changed subaddress scan window finds the new order without crediting a different order |
| Concurrent fast passes, rounds and API order creation through the production SQLite worker; stale pool sightings during mining | Exactly-once credit and settlement notifications survive task interleavings, changed scan windows and restarts; pool rediscovery cannot erase a committed block height |
| Arbitrary sequences of fast mempool passes, ordinary rounds and restarts | Both paths share exactly-once credit; mining preserves one row and settles at the required depth |
| Missing transaction with unspent, pool-spent, disputed, failed or empty key-image answers | Ambiguous evidence cannot void or erase observed money |
| Distinct fixture wallets sharing a daemon and scheduler | Each wallet credits only its own outputs, regardless of arrival order and restart |
| Trusted verifier results lag the node tip or disagree with the payment attestation | Settlement waits for both verified depth and the correct payment block; advancing the verified chain releases the obligation without PoW arithmetic |
| Mainnet and stagenet tenants registered together | One network's rounds do not credit or advance another network's orders; each scheduler progresses independently |
| Large blocks scanned in pages, fetch failures, interruptions, restarts and replacement blocks | Incomplete blocks do not publish staged payments; recovery or fork handling converges to the correct payment state |

Seven further properties in `work/history/properties.rs` target storage and
recovery boundaries. All ranges in this table are inclusive:

| Property | Generated range / assertion |
|---|---|
| Large amounts survive real output scans, mining and reopen | Total 1–`i64::MAX`, biased toward the storage maximum and exact integer boundaries around 2^53; two outputs, thresholds 0–8; invoices just below, equal to or just above the received total, clamped to the supported positive range |
| Unsupported amounts and aggregates fail atomically | Individual amounts above `i64::MAX`, or two individually supported payments whose sum exceeds it; the transaction rolls back payments and status without queuing notifications |
| Unsupported invoices preserve address allocation and idempotency | Rejected invoice above `i64::MAX`; retry with a valid amount reuses the same address index/key exactly once |
| Reorg collection/processing crosses queue limits | 15/16/17, 255/256/257 and 511/512/513 outputs; initially mined or pooled; interruption after 1–24 zero-budget rounds, SQLite fault position 0–199, restart, reconciliation and re-mining preserve output identity |
| Settlement queue rotation reaches every order | 63/64/65 and 127/128/129 orders, thresholds 1–4, optional restart; pending recomputes drain and exactly one paid notification is queued per order |
| Repeated multi-order histories preserve money | Three orders, 3–9 independent payments of 1–999 piconero, thresholds 0–5; 2–8 replacement phases of 1–6 blocks; varied mined/pool destinations, custody outages, nine RPC-failure bits, SQLite faults and restarts; an independent model checks every recovered phase |
| Actual process death during SQLite execution recovers | Pool credit, interrupted block scanning or reorg reconciliation; kill after 0–2999 SQLite VM progress callbacks; three outputs of 1–999 piconero each; reopen, integrity check, no leaked staged block credit, exactly-once recovery and one paid notification |

SQLite stores nonnegative amounts up to `i64::MAX`; these integration properties
exercise that supported range and reject unrepresentable values. Pure status
properties still explore all of `u64`. The aggregate boundary regression found
an unchecked signed cast: recomputing a total above the storage maximum could
write a negative received amount. A checked SQLite binding now fails that write,
so transactional callers roll it back. A named regression fixes the concrete
`i64::MAX + 1` case independently of generator changes. A second recovered failure
showed stale confirmations when the tip shrank above the scanner's recorded
height: no recorded hash diverged, so no reorg job opened. Settlement now
persists its observed tip and atomically queues mined-payment recomputes on a
decrease. A named restart regression covers confirming and previously settled
orders; SQL fault sweeps verify the position/obligations commit together and
remain scoped to their network. First use also queues recomputes for legacy
state that has no recorded settlement tip.

The subprocess crash test launches the current Rust test binary with only its
child helper selected. The child reconstructs the fixture wallet/daemon, opens
the real database file, and stops inside a SQLite progress callback. The parent
kills it and waits for termination before reopening the database. Rust destructors
and rollback cleanup cannot run in the killed process. Each rendezvous has a
ten-second deadline and temporary database/marker cleanup; no daemon service or
new dependency is required. This models process death, not power loss, an OS
crash or a filesystem failure. Candidate recovery uses a positive work budget:
zero-budget rounds intentionally process one candidate and cannot drain 513
candidates within the smaller scenarios' 120-round recovery bound.

Sixteen properties in `work/nodes/properties.rs` now use the production
`FallbackDaemonClient` and a newly pinned client for each actual money round.
Each node has its own chain, pool, per-RPC errors/hangs, response delays and
adversarial answers. The tests use real SQLite and wallet output scanning.
They do not substitute a single logical daemon for multi-node failover.

| Generated property | Inclusive ranges / checks |
|---|---|
| Transport failover, exclusion, cooldown and recovery | 1–5 nodes, healthy/error/hanging/delayed responses; delays 0–99 ms; generated excluded sets, including refusal to exclude every node; bounded aggregate calls; recovery of the highest-priority eligible node |
| Spent-input corroboration against independent vote model | 1–5 nodes, 1–5 key images, four evidence values per image; errors, hangs, truncated and oversized vectors, exclusions; disagreement is disputed, absent valid answers is an error, a single valid vote follows the existing trust policy |
| Location corroboration against independent evidence model | 1–5 nodes; NotFound/pool/block-2/block-3/block-4 answers; errors, hangs and exclusions; positive location evidence takes precedence; no answers returns an error; one eligible configured node uses the existing no-second-opinion path |
| Total outage and recovery through one returning node | 2–5 nodes, each failing or hanging; 1–5 outage rounds; restart optional; amount 1–999, required depth 1–4; observed money does not disappear, later mining preserves identity and queues one paid event |
| False spent votes and sole-reachable-node policy | 1–5 nodes, arbitrary liar position; honest peers reachable or unavailable; 1–7 rounds, restart optional; conflicting votes preserve money, the accepted sole-vote policy can void it, re-mining restores the same payment |
| Inconsistent block/outline and malformed body responses | Eleven modes: truncated, duplicated, reordered or unsolicited bodies; no block; wrong block parent/height; wrong outline hash/height/parent/timestamp; 1–4 rounds, restart optional, amount 1–999; invalid responses cannot advance the block cursor or publish staged money; valid duplicate/reorder responses do not duplicate credit |
| Inflated tips and false attestation with checking enabled | 2–5 nodes, inflation 1–99 blocks, required depth 1–5, partial/full verified ceiling, wrong/correct attestation, restart optional; only the correct verified depth/attestation releases settlement |
| Explicit zero-confirmation and unchecked-height policies | 1–5 nodes, amount 1–999, thresholds 0–5, inflation 6–99; checking on/off; zero-confirmation pool acceptance and unchecked-height trust remain explicit policy limits |
| Combined multi-node money histories | 2–5 nodes, three orders, 3–6 payments of 1–999; 2–12 phases; independent per-node RPC error/hang masks 0–511; divergent branches, pool omission, inflated heights, false absence, SQLite faults 0–199 and restarts; no void without affirmative evidence or settlement before verification, then exactly-once recovery for all orders |
| Cancellation at every pinned RPC | All nine scanner RPC kinds; a 1-ms caller deadline cancels a hanging request; failure enters cooldown and the next pin selects a healthy peer without mixing answers within the call |
| Timeout boundary behaviour | Responses at 14,999 / 15,000 / 15,001 ms around the per-node deadline; bounded failover and later primary recovery; an exact-deadline tie may validly resolve either way |
| Pool omission and unrelated transactions | 2–5 nodes, 1–5 rounds, restart optional; all nodes omit the payment from their pool view; an honest later block still detects it and unrelated transactions cannot credit it |
| False mining location without block membership | 2–5 nodes; fabricated block location 3–12, required depth 1–5, restart optional; no matching transaction in the proven block means no settlement; actual subsequent mining restores one correct payment identity |
| Secondary hangs under the actual settlement deadline | 2–5 nodes; location/spent/both secondary RPCs hang; healthy peers delayed 0–900 ms, including explicit 800 ms cases and an all-healthy control; amounts 1–999; restart optional; a formerly paid zero-confirmation transaction proven double-spent loses credit and queues one detection event; counters prove the hang was reached and cancelled |
| Bounded tenant-page service and recovery | Tenant page 1–3, pages per unit 1–3, group size `2 * page * units + extra` with extra 1–4; one unit advances at most `page * units` tenants, the pool tier also runs, then all tenants recover both payments across optional restart |
| Real verifier/scanner fork integration | 3–5 nodes, amount 1–999, required depth 1–3; one shared store/client; initially verified Paid payment removed on a heavier valid fork, optional forged primary is excluded, actual branch validation/reconciliation must be reached; SQL access fault 0–49, restarts and paced intermediate proof progress; re-mining preserves payment IDs and exactly two legitimate Paid transitions |


Four generated properties in `proof/node_properties.rs` run the real `Follower`
with existing mined/forged proof fixtures, not a substitute verifier. They test
scheduling and settlement trust rather than fuzzing PoW arithmetic:

| Generated property | Inclusive ranges / checks |
|---|---|
| Verifier histories through dishonest branches and outages | 2–5 nodes, 1–6 phases with healthy/error/hanging/forged states and optional repeated follower restarts; every case also forces all-down and all-hanging phases; a forged branch never becomes proven; recovery converges to the honest chain and restores node eligibility |
| Configured-majority anchoring | 2–5 configured nodes with exactly one answering; remaining nodes fail or hang; anchoring stays held until a configured majority returns, including follower restart |
| Deep verifier RPC failures | 3–5 nodes; hash, difficulty-header or sampled-block-blob request fails/hangs on one peer; healthy peers supply the anchor; transport failure is not a dishonesty exclusion; restart optional |
| Malformed and wrong-ID sample retry | 3–5 agreeing nodes; arbitrary liar supplies undecodable bytes or another valid block's bytes; optional follower restart; faulty sample and healthy responses must be completed, and healthy peers establish the honest anchor |


Four fixed scanner examples and a fixed verifier example complement the random
cases: outer-timeout failover, hanging corroborating peers, all eleven malformed
response shapes, sole-configured/sole-reachable/disagreeing spent votes, and an
anchor member withholding sampled blocks.

These tests found four production failures: cancellation before a pinned
client's own deadline did not record node failure; corroboration had no independent
bound for hanging peers; a paged outline could disagree with its header's height;
and one agreeing anchor member withholding a sampled blob blocked a healthy
majority. Cancellation now records failure, each corroborating call has a bound,
outline height/parent/timestamp must match its header, and missing, malformed or wrong-ID sampled blobs
are retried against other members of the agreed window. All existing proof checks
still validate a returned sampled blob.

Review follow-up also bounds each block unit by tenant pages, preserving durable
catch-up cursors for the remaining tenants. A durable tenant-page rotation
includes failed attempts, so an unavailable wallet cannot repeatedly hide healthy
tenants after a restart. Its fairness assertion requires a new
pool payment to be recorded in the same round. Settlement divides the caller's
deadline between its remaining evidence operations and reserves time for durable
writes: the final operation uses the remaining budget, so two healthy 800 ms
responses can complete even while another peer hangs.

The tests explicitly preserve three existing trust policies: a sole reachable
spent-input vote is accepted; checking disabled trusts reported height; a
zero-confirmation policy accepts pool evidence even with checking enabled.
Testing these policies does not remove their trust assumptions. Anchor creation,
in contrast, still requires a majority of the configured nodes, not merely a
majority of the nodes that happened to answer.

Run the node-focused suites with the same runners:

```sh
PROPTEST_CASES=128 PROPTEST_RNG_SEED=47 cargo test -p engine --lib --locked money::nodes::
PROPTEST_CASES=16 PROPTEST_RNG_SEED=47 cargo test -p engine --lib --locked proof::tests::properties::
cargo nextest run -p engine --lib --locked -E 'test(::money::nodes::) | test(proof::tests::properties::)'
```

The daily workflow includes these properties in both feature configurations and
retains their regression files. Daily runs use 512 cases for ordinary properties
and 32 for real verifier and integration properties. Manual runs select these
budgets independently; `ENGINE_PROOF_CASES` overrides `PROPTEST_CASES` for the
proof and integration families. Each job has a 90-minute limit.

Both default and CI nextest profiles kill an ordinary test process after ten
120-second slow periods (20 minutes), and a proof process after twelve 300-second
periods (60 minutes). The `proof-workers` group admits at most two proof test
processes, including the integration property, to limit CPU/memory contention.
These are outer watchdogs for synchronous hangs; scenario-level deadlines remain
much shorter. Plain `cargo test` does not provide these process watchdogs, so use
nextest for sustained exploration. Failed tests are never retried automatically.

RPC faults use the shared `Rpc` operation enum and named mask bits. Each adversarial
node records attempted, completed and cancelled calls, including cancellation of
an unfinished future. SQL fault traces record checks, whether the requested denial
fired and its authorization action. Generated out-of-range SQL positions remain
valid no-fault cases; targeted SQL sweeps and the fixed initial denial ensure actual
failure paths are also reached.

Proof fixtures use a fixed clock (`TEST_NOW = 1_800_000_000`). Test-only anchor
sampling uses `PROPTEST_RNG_SEED` (47 when unset), prints its seed and selected
heights in captured output, and leaves production sampling unpredictable. Preserve
the seed, generated input, regression files and failure output together for replay.
The seed controls generated inputs and test samples, but real worker scheduling,
process timing and UUIDs remain nondeterministic; it does not reproduce every task
interleaving.

The process-death runner retains random SQLite opcode interruption and adds eight
named checkpoints: before/after commit for staged block matches, payment publication,
status recompute plus webhook creation, and reorg completion. The parent verifies
the exact rendezvous, kills the child without Rust cleanup, reopens the database,
checks integrity and converges to the expected money/identity/event state. Hooks are
compiled only into tests and activated only in the crash subprocess.

The money oracle independently totals funds that have reached the required depth;
it does not call production status or conflict functions. After convergence, another
round must preserve money state and webhook event counts. Amounts and thresholds,
transaction ordering, work budgets, batch sizes and fault positions vary. The
existing RingCT fixture is complemented by clear-amount transactions with real
one-time-key derivation and distinct inputs. These test output scanning; they do
not claim consensus validity or validate transaction signatures.

Normal CI runs all properties on Linux and macOS. The separate
`.github/workflows/engine-properties.yml` workflow runs **512 cases per property
every day**, both with default features and with `zmq`, using the GitHub run ID as
a recorded replay seed. Manual runs offer 128–1024 cases. It retains regression
seeds and the JUnit report. This workflow is configured locally; it starts once
published to GitHub. Larger local exploration uses the same commands above.

Assertions run both during histories (no duplicates, no new void without evidence,
no new settlement while reconciliation is open) and after bounded recovery
(correct payment height, credited amount, status, cursor and canonical hashes).
Stored confirmations on settled orders are snapshots; the model requires them to
meet the threshold rather than keep increasing on every block. Stable extra rounds
must preserve payment/order state and webhook event counts. Named regressions also
cover a second fork after a candidate was processed, and after rewind but before
replacement blocks were scanned. The discovered fix remembers the replacement
branch durably, recollects candidates when it changes, and retains a hash anchor
across the gap before rescanning. It uses the existing scheduler position table,
so no schema migration is needed.
Further named regressions cover stale confirmations after a shorter replacement,
reopening a settled order whose payment is below the fork, and tenant starvation
with a one-page work limit and multiple tenant pages. Rewind queues durable
confirmation recomputes. The block work limit counts tenant pages, and durable
page rotation prevents failed wallets from blocking later tenants. A zero-confirmation settlement also reopens when a reorg removes
the confirmed winner of an output-key conflict; unresolved copies cannot remain
settled merely because the order was paid before. A separate named regression
covers a healthy catch-up tenant hidden behind an unregistered tenant; catch-up
queries now filter registered keys before applying the page limit.

Each case and shrink attempt gets a fresh SQLite file, real output scanner and
Monero transaction fixture, and scripted `FakeDaemonClient`. Most properties use
a paused current-thread Tokio runtime. Restarts close all SQLite handles, reopen the file, and replace
`ScanState`; the independent key-custody backend stays alive. The harness advances
Tokio time and passes explicit Unix time to the same round executor used by
`run_round`, so both in-memory backoff and persisted retry deadlines advance
without sleeps. History rounds use a zero work budget (one unit per tier) and
one transaction per scan batch to expose interruptions. Money scenarios also vary
budgets and batch sizes and control exact expiry/grace boundaries. Every round has
a five-second virtual async timeout. SQLite runs through `Db::over_shared` for
deterministic sequential execution. The concurrent property switches to the
production `Db::open` worker and a real two-thread Tokio runtime, runs independently
spawned fast-loop, round-loop and API tasks with generated yield schedules, and
bounds each concurrent phase by a ten-second real timeout. It checks stable
recovery and exactly one paid notification per order, as well as payment identity
and amount. It adds no wall-clock sleeps.

The generated models still have explicit limits:

- Generated concurrent scenarios sample real task interleavings; they do not
  exhaustively control the executor or explore every weak-memory ordering. The new subprocess harness samples abrupt
  process death during SQLite execution. It does not model OS crashes/power loss or exhaust every crash point.
  Existing concurrency and SQL fault-sweep tests complement these cases.
- The original reorg model retains two bootstrap blocks and stays within the
  configured window. Separate lifecycle properties now generate bootstrap,
  genesis replacement and deeper-than-window forks with failures and database
  reopens. Hashes outside the retained window cannot establish a fork without
  additional evidence.
- Wallet isolation uses two deterministic key pairs and valid output derivations;
  it does not fuzz arbitrary cryptographic keys, encodings or signatures.
- The settlement-ceiling property supplies trusted verifier-result fixtures.
  It does not run PoW arithmetic or explore proof-verifier task scheduling.

Daemon response parsing and PoW arithmetic remain deliberately deferred.
Proptest shrinks vector length and individual event fields; [its state-machine companion](https://proptest-rs.github.io/proptest/proptest/state-machine.html)
remains an option when future models need state-dependent transition shrinking.

## 1. `KeyCustody` Boundary

Already implemented in `src/key_custody/plain.rs` (4 tests passing as of this
writing); listed here for completeness and to flag gaps.

| Test | Why | How | Status |
|---|---|---|---|
| Register → derive subaddress 0/0 → matches root view pair keys | Baseline correctness of the register/derive path | Compare `public_spend`/`public_view` against independently-derived values | **Done** |
| Register → remove → derive fails `UnknownWallet` | Handles must not outlive their wallet | Call `derive_subaddress` after `remove_wallet`, assert error variant | **Done** |
| Scan a real fixture transaction, known view pair → finds the one owned output with the right subaddress index and a nonzero decrypted amount | Proves the real RingCT amount-decryption + key-matching path works, not just that the trait compiles | `monero-rs`'s own `code_coverage_owned_tx_out` test vector, copied via script (never hand-retyped, to avoid transcription bugs) | **Done** |
| Repeated scans over an unchanged range rebuild the lookup table exactly once; a range change triggers exactly one more rebuild | Proves the caching optimization (§DESIGN.md 6.2.4) actually engages, not just that results stay correct | `#[cfg(test)]` atomic rebuild counter on `WalletEntry`, asserted after 3 same-range calls and 1 range-widening call | **Done** |
| Seal → unseal_and_register on a fresh backend instance ("simulated restart") derives the same address as before sealing | Proves the at-rest round trip actually survives a process restart, which is the entire reason `seal`/`unseal_and_register` exist | Two separate `PlainKeyCustody` instances; assert derived addresses match despite different `WalletHandle`s | **Done** |
| `WalletMaterial::from_hex`/`from_raw_bytes` reject malformed input (wrong length, non-hex) | Admin API will feed this directly from user-typed hex; must fail with `InvalidKeyMaterial`, not panic | Table of bad inputs (empty string, odd-length hex, 31/33 bytes, non-hex chars) | **Gap — add** |
| `scan_tx_outputs`/`derive_subaddress` on an unknown handle → `UnknownWallet`, not a panic | Defensive boundary — a caller bug (stale handle after restart) must degrade to an error, not a crash | Call each method with a handle from a *different* `PlainKeyCustody` instance | **Gap — add** |
| Concurrent scans across many wallets/handles complete without deadlock | The `RwLock`-per-registry + `Mutex`-per-wallet-cache combination is new; worth a direct concurrency smoke test | Spawn N tokio tasks calling `scan_tx_outputs` against a mix of shared and distinct handles; assert all complete within a timeout | **Gap — add** |
| `seal()` output is versioned by `key_custody_backend`, and `unseal_and_register` on mismatched bytes fails loudly | Prevents a future TEE backend from silently misinterpreting `PlainKeyCustody`-sealed bytes (or vice versa) after a backend migration | Feed `PlainKeyCustody::unseal_and_register` a byte string of the wrong length; assert `InvalidKeyMaterial`, not a garbage successful registration | **Gap — add** |

Testing note: `ZeroizeOnDrop` correctness (does the view key actually get scrubbed from
memory) is not practically assertable from safe Rust — trust the `zeroize` crate's own
test suite for that guarantee rather than trying to inspect freed memory. It's enough
to assert, once, that `WalletMaterial` implements `ZeroizeOnDrop` at compile time (a
trait-bound smoke test), not to try to prove it at runtime.

## 2. Order Status Derivation (§DESIGN.md 7.6)

**Why this gets disproportionate attention**: it is the one function every other
component (webhooks, the widget, the admin API) trusts without re-deriving, and it has
the most edge cases per line of any component in the system — multi-payment
interactions, native 0-conf thresholds, and expiry all intersect here.

Implement as table-driven (or `rstest`/`proptest`-parametrized) tests over
`(valid_payments, xmr_amount_piconero, confirmations_required,
now, expires_at) -> expected_status`:

| Scenario | Expected | Why it's worth a named case |
|---|---|---|
| No payments, before expiry | `pending` | Baseline |
| No payments, after expiry | `expired` | Time-boundary branch |
| One payment, amount < expected, before expiry, regardless of its confirmation depth | `partial` | Confirms "partial" is about *amount*, not confirmation depth — a fully-confirmed half-payment is still `partial`, not `confirming` |
| One payment, amount < expected, **after** expiry | `expired` | Deliberate: funds can sit at the address unresolved past the deadline; this is intentional given no auto-refund path exists, and needs to be locked in as a named case, not an accident |
| Total == expected, all contributing payments 0-conf, `confirmations_required > 0` | `unconfirmed` | The "seen but not trusted" branch — must not be conflated with `confirming` |
| Total == expected, all 0-conf, `confirmations_required = 0` | `paid` | Native 0-conf trust path |
| Total == expected, mixed 0-conf + on-chain payments, `min_conf < confirmations_required` | `confirming`, not `unconfirmed` | `all_zero_conf` must be false the instant *any* contributing row has a `block_height` |
| Total == expected, `min_conf == confirmations_required` exactly | `paid` | Off-by-one boundary — `>=`, not `>` |
| Total == expected, `min_conf == confirmations_required - 1` | `confirming` | The other side of the same boundary |
| Total > expected, confirmed | `overpaid` | |
| Total > expected, 0-conf, `confirmations_required = 0` | `overpaid` | Combines the overpaid and native 0-conf branches |
| Two payments summing to exactly `expected`, one later voided, remainder < expected | `partial` (recomputed from the survivor alone) | **Direct regression test for the multi-transaction/double-spend scenario this design was corrected for** — see below |
| Two/three payments where voiding one still leaves the total ≥ expected | `paid` (unchanged) | Proves a double-spend that turns out to be financially irrelevant does not incorrectly downgrade the order |
| A voided payment's row is fully excluded from `total`/`min_conf`, not just zeroed | Guards against an implementation that filters late or double-counts | Assert directly against the SQL-level aggregate, not just the Rust-level derivation, if the recompute is partially expressed as a query |

**Independence test (critical, given this was the exact bug caught in design review)**:
construct a case where voiding a payment changes `status` and one where it does not,
and assert `double_spend_detected_at` is set **in both**, while `status` differs
appropriately. This is the direct executable version of "these are two axes, not one,"
and should be the canonical regression test protecting that decision going forward.

## 3. Chain Scanner & Reorg Detection

**Why this deserves more test investment than its size suggests**: reorgs are rare in
production, which means a bug here can go unnoticed for a long time and then surface
exactly when a merchant's money is on the line. This is the inverse of "test what
breaks often" — test this because it's high-consequence and low-frequency, which means
production usage will not shake the bugs out for you.

Requires a `FakeDaemonClient` (implements `MoneroDaemonClient`, §DESIGN.md 7.1) driven
by a small scripted timeline: `push_block(height, hash, txs)`,
`reorg_from(height, new_blocks)`, `set_mempool(txs)`, `set_key_image_status(image,
status)`. This makes every scenario below deterministic and fast, with no live node.

| Scenario | Why | How |
|---|---|---|
| Happy path: tx seen in mempool → included in a block → confirmations increase as height advances → reaches `paid` | Baseline wiring test for the scanner's own orchestration (as distinct from `KeyCustody`'s crypto correctness, already covered in §1) | `FakeDaemonClient` timeline; scanner-level test can stub the `KeyCustody` call itself if convenient, since this test is about orchestration, not crypto |
| Mempool poll idempotency: the same unconfirmed tx observed across many poll ticks produces exactly one `order_payments` row | The scanner polls every ~1s and *will* see the same tx repeatedly; this must be a no-op, not a growing pile of duplicate rows or a surfaced error | Feed the same tx to the scan-and-record path N times; assert row count stays 1 (relies on, and should explicitly exercise, `UNIQUE(txid, output_index)`) |
| Reorg where the tx reappears at a different height | Most common real-world reorg outcome; must not misreport as a problem | Script a `reorg_from` that includes the same tx in a different block; assert `block_height` updates, no incorrect status regression, no spurious double-spend event |
| Reorg where the tx falls back into the mempool | Second most common outcome | Script a reorg whose replacement blocks omit the tx but the mempool still has it; assert `block_height → NULL`, confirmations → 0, status recomputes (e.g. `paid → confirming`/`unconfirmed`) |
| Reorg where the tx vanishes and `is_key_image_spent` proves a different, confirmed transaction consumed the same inputs | The actual double-spend case | Script the vanish + a `set_key_image_status(image, SpentInBlockchain)` with a different txid; assert `voided_at` set, status recomputed, `double_spend_detected_at` stamped, `order.double_spend_detected` webhook enqueued |
| Reorg where the tx vanishes but `is_key_image_spent` reports unspent (still propagating) | **Safety property**: must never void on ambiguous evidence | Same vanish, but `set_key_image_status` reports unspent; assert the payment row is left alone (not voided), pending a later re-check |
| The exact two-transaction scenario from design review: one payment voided, the other intact | Direct regression test tying the scanner-level behavior to the status-function test in §2 | Two matched payments on one order; void one via the daemon-proof path above; assert the *order's* recomputed status and `double_spend_detected_at` match §2's expectations, exercised through the full scanner path this time, not just the pure function in isolation |
| A reorg is reported at its true fork point at every depth the window covers; one deeper is reported at the window's edge instead | This limitation is a deliberate design choice (§DESIGN.md 3, 7.5) — the test exists to keep it an intentional, documented boundary rather than something that silently regresses. Note the original phrasing here ("one deeper than the window is *not caught*") turned out to be wrong when actually exercised: a deeper reorg **is** detected, just at the wrong (too high) height, which is a materially different failure mode — payments below the window keep counting at heights that no longer exist | Loop one case per depth from 1 to the full window; then one case one block deeper, asserting the reported point is the window edge and that a payment below it is left untouched. **Done** (`a_reorg_is_detected_at_its_true_fork_point_at_every_depth_the_window_covers`, `a_reorg_deeper_than_the_window_is_reported_at_the_window_edge_and_leaves_older_payments_alone`) |
| `scanned_blocks` stays bounded in size over many simulated blocks | Resource/leak concern, not correctness — but the pruning that bounds it must never empty the window (an empty window reads as "never scanned" and re-seeds at the tip) | Advance the fake chain far past the configured window; assert the row count stays capped, then assert a reorg at the far edge of the configured depth is still both detectable and rewindable. **Done** (`the_scanned_block_window_stays_bounded_as_the_chain_grows`) |
| A zero-conf payment whose transaction leaves the mempool without being mined, because a conflicting transaction won | The one double-spend shape reorg detection structurally cannot see (no recorded block hash ever changes), and the one a merchant using a native 0-conf threshold is exposed to | Record a mempool match, drop the transaction from the fake's pool, mine a conflicting transaction, mark the shared key images `SpentInBlockchain`; assert the void, the status retraction and the `order.double_spend_detected` event. Then the same script with the key images left unspent, asserting *no* void — a dropped or evicted transaction is not a double-spend. **Done** (`a_zero_conf_order_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved`, `a_mempool_payment_that_merely_disappears_is_never_voided_on_that_evidence_alone`) |
| The tip trades places repeatedly (a block-withholding pool publishing in bursts), with one transaction moving in and out of the chain | The realistic on-the-wire shape of selfish mining, as opposed to one clean reorg — the property at risk is bookkeeping (one row, counted once), not detection | Several rounds of `reorg_from` alternating between two chains, two ticks each; assert one payment row, the latest agreed height, no double-spend flag, and the amount counted exactly once. **Done** (`rapidly_alternating_chain_tips_never_lose_or_double_count_a_payment`) |
| A different daemon, serving a divergent history, is swapped in mid-run | Answers "do we need per-daemon sync state?" — no: the stored `(height, hash)` window is re-validated against whoever answers now, so a swapped, rolled-back, eclipsed or lying node is the same case as a reorg and takes the same code path (§DESIGN.md 7.7) | Drive ticks against one `FakeDaemonClient`, then against a second with a different chain, then back; assert reconciliation and rewind each time. **Done** (`swapping_to_a_daemon_serving_a_different_chain_reconciles_exactly_like_a_reorg`) |
| A node that fails partway through a reconciliation pass, *after* a void has already committed | The one mid-tick failure whose damage is not self-healing: a voided row leaves both sweeps' input sets by construction and a terminal order is skipped by the per-tick recompute, so a deferred status update is lost permanently rather than retried | Two mempool payments, both proven double-spent, with the daemon failing on the second lookup; assert the first void's status change, total, and both webhook events all landed anyway. **Done** (`a_void_that_lands_before_the_node_fails_still_updates_the_order_it_belongs_to`) |
| A node lying about height, omitting or inventing mempool/block transactions, or reporting a height far below the recorded high-water mark | These are the trust-boundary cases (§DESIGN.md 7.7); the tests exist to state which are closed by construction and which are accepted, so a future change can't quietly move one across the line | One test per case, asserting the *actual* consequence rather than an aspiration: inflated height inflates confirmations (accepted), an invented transaction cannot become a payment (closed), an omitted one only delays detection (bounded), a lagging node changes nothing at all (closed). **Done** |

## 4. Chain Scanner Throughput / Active Watchlist

| Test | Why | How |
|---|---|---|
| `KeyCustody.scan_tx_outputs` is never invoked for a tenant with zero pending orders | This is the entire justification for the active-watchlist design (§DESIGN.md 7.3) — worth proving directly rather than trusting it by inspection | Wrap `KeyCustody` in a call-counting spy; register many tenants, give only one a pending order; assert the spy's call count attributes only to that tenant across several scan ticks |
| Randomized sequence of range changes causes a table rebuild exactly on each distinct range and never on a repeat | Generalizes the fixed rebuild-count test already in `plain.rs` (§1) beyond the two hand-picked cases | `proptest`-generated sequence of `(major_range, minor_range)` pairs with repeats interspersed; assert rebuild count equals the number of *distinct adjacent* range changes |

## 5. Multi-Tenancy & Auth (IDOR focus)

**Why dedicated adversarial tests, not just happy-path auth tests**: this exact bug
class (a path parameter usable for authorization alongside a separate token) was
caught during design review, not incidentally. The fix is structural (no `{id}` in
tenant-scoped routes), but the *row-level* version of the same rule
(`WHERE id = ? AND tenant_id = ?`) is a per-handler discipline that regressions can
reintroduce one handler at a time — this needs standing test coverage, not a one-time
audit.

| Test | Why | How |
|---|---|---|
| Tenant A's valid `sk_` + tenant B's real `payment_id` → 404, not tenant B's order | The one place a client-supplied identifier still exists within a tenant-scoped route; must never be sufficient on its own | Create orders under two tenants; request B's `payment_id` using A's token; assert 404 (not 403 — don't confirm the id even exists) |
| `pk_` alone, on any `/api/v1/admin/*` route | `pk_` must never be usable as authorization anywhere | Every admin route, called with a bearer value that's actually a `pk_`; assert rejected |
| A rotated `sk_` stops working immediately | Rotation must invalidate the old token, not just issue a new one alongside it | Rotate; retry a request with the pre-rotation token; assert rejected |
| A single-bit-flipped valid token is rejected | Confirms the real SHA-256 hash-compare path is being used rather than e.g. a prefix or length check | Flip one character of a known-valid `sk_`; assert rejected |
| The engine serves no public (`/api/v1/t/{pk}/...`) routes and grants CORS to no origin on any route | The engine is private (DESIGN.md §4, §10.3); a public route or a CORS grant coming back would reopen a surface nothing but monokulo should reach | `the_engine_serves_no_public_order_routes_and_no_cors` (`crates/engine/tests/verification/http/tests.rs`): the old routes answer 404 and preflights get no `Access-Control-Allow-Origin` |
| Monokulo only ever calls the engine's admin API and `/status` | Keeps the boundary from eroding one call at a time | `every_engine_call_uses_the_admin_api_or_status` (`crates/monokulo/src/engine_client.rs`) checks every URL the engine client builds |
| Two tenants issued back-to-back never receive the same `pk_`/`sk_`, and a tenant's `sk_` is never recoverable via any `GET` | Basic credential hygiene | Direct assertions on creation responses and subsequent `GET`s |

## 6. Writer Actor & Concurrency Correctness

**Why stress tests specifically**: the single-writer design is a correctness
*assumption* everything else (minor-index uniqueness, idempotent payment recording)
relies on. The risk category is races, which sequential unit tests cannot exercise by
construction — these tests must actually generate concurrency.

| Test | Why | How |
|---|---|---|
| N concurrent order-creation requests for one tenant never allocate the same `minor_index` | Directly protects the "two customers never watch the same address" property (§DESIGN.md 8.2) | Spawn N (e.g. 100+) concurrent create-order calls against a real writer actor backed by a temp-file or in-memory-with-shared-cache SQLite; collect all issued `minor_index` values; assert no duplicates. This should hold even if it's also backstopped by `UNIQUE(tenant_id, minor_index)` — the test should assert zero constraint-violation retries under normal load, not just eventual correctness through a retry loop |
| N concurrent duplicate match-events (simulating the scanner double-reporting one output across ticks) resolve to exactly one `order_payments` row | Mirrors §3's idempotency test but specifically stresses the writer's handling of the `UNIQUE(txid, output_index)` conflict under real concurrency, not sequential calls | Fire the same match event from multiple concurrent tasks; assert one row, and that the "losing" writes fail closed (no error surfaced to a client) rather than corrupting `amount_received_piconero` via a lost update |
| A long-running write transaction never blocks a concurrent read | The entire justification for splitting the read pool from the writer (§DESIGN.md 9) | Hold a writer-actor transaction open deliberately (e.g. via a test hook); issue a concurrent read-pool query; assert it returns promptly rather than waiting on the writer |
| An interrupted process (simulated by killing mid-write and restarting against the same DB file) leaves a consistent schema | WAL crash-recovery is a SQLite guarantee, but this system depends on it heavily enough to deserve a direct regression test rather than trusting it by reputation | Kill the process (or the connection) mid-transaction in a controlled way; reopen the same database file; assert the schema and existing rows are intact and queryable |

## 7. Webhook Delivery

| Test | Why | How |
|---|---|---|
| HMAC signature matches an independently-computed value for a fixed `(secret, payload)` test vector | Merchants implement verification against the documented scheme; a fixed vector lets them (and this test suite) cross-check the exact same computation | Hardcode a `(secret, payload, expected_signature)` triple in the test; recompute and compare bit-for-bit — this triple should also appear in end-user documentation |
| A failing endpoint (mock server returning 500) increments `attempt_count` and pushes `next_attempt_at` out on a backoff schedule; a later 2xx sets `delivered_at` and stops retries | Core reliability contract | Local mock HTTP server (e.g. `wiremock`) scripted to fail N times then succeed; drive the delivery worker's claim-and-attempt loop directly rather than through a real clock/sleep |
| A webhook URL resolving to a loopback/private/link-local address is rejected at **both** registration and delivery time | SSRF is a real vector here (§DESIGN.md 11) — the "at both times" phrasing matters because DNS can change between registration and delivery, so a registration-time-only check is insufficient | Register a webhook with a public-looking hostname whose DNS is then changed (or stub the resolver) to a private IP before delivery; assert the delivery is refused, not just the registration |
| A webhook target that issues an HTTP redirect to a private address is not followed | Same SSRF concern, redirect-based bypass specifically | Mock server responding with a 3xx to a private IP; assert the delivery worker does not follow it |
| A status transition fires exactly one `order.<status>` event; an independent double-spend void fires exactly one `order.double_spend_detected` event; a void that also changes status fires both | Direct test of the two-event-family design (§DESIGN.md 11) — this is the same distinction §2's "independence test" checks at the status-function level, checked here at the delivery-enqueueing level | Drive each scenario through the writer actor; assert the exact multiset of `webhook_deliveries.event_type` rows created |
| A duplicate delivery (simulated lost-ack) is accepted as an expected, documented characteristic, not silently deduped by hidden logic | The at-least-once contract must actually hold, not just be claimed in docs | Simulate an ack loss (client receives 2xx, worker doesn't observe it before a retry fires); assert two deliveries occur, i.e. that this isn't secretly exactly-once |

## 8. DDoS Protections

| Test | Why | How |
|---|---|---|
| The engine's per-token rate limit trips after the configured request count within the window (keyed on the `sk_`, or on the address for the token-less tenant-creation and `/status` routes), and resets after the window elapses | Core mechanism correctness | `admin_rate_limit_middleware_*` and `unauthenticated_routes_are_limited_per_address_by_the_admin_limiter` (`crates/engine/tests/verification/http/tests.rs`) drive requests past a small limit with a fabricated `ConnectInfo` |
| An oversized request body is rejected before JSON parsing | Defends against a cheap way to waste CPU on parsing before validation | Send a body larger than `max_body_bytes`; assert rejection and that no parse work occurred (e.g. via a spy/log assertion, if feasible) |
| Monokulo's challenge: a correct proof is accepted; an insufficient one, an expired one, one signed with another key, one for another client, a replayed one and a wait token presented before its 10 seconds are refused; the replay store is capped and fails closed | The challenge is what stands between "past soft" and the checkout; each refusal path is a way to skip it | `abuse::challenge` unit tests (`crates/monokulo/src/abuse/challenge.rs`) |
| Client identity: `X-Forwarded-For` is only believed from a trusted proxy (last untrusted hop wins), IPv6 is grouped by `/64`, PROXY v1 headers parse (circuit id from `fc00::/16`) and malformed ones are refused, and the ordinary listener never honours a PROXY header | One identity drives every limit; a spoofable identity would make them all worthless | `abuse::identity` / `abuse::proxy_protocol` unit tests; `clients_behind_a_trusted_proxy_...` (`http/abuse.rs`); `crates/monokulo/tests/onion_listener.rs` (real sockets, synthetic PROXY headers: per-circuit budgets, header-less connections dropped, the public listener refuses PROXY) - runs by default |
| Tiers: under soft allowed, past soft challenged (page interstitial / JSON `429` + challenge), a solved proof continues, past hard `429` + `Retry-After` everywhere, signed-in merchants and store keys never challenged, under-attack challenges anonymous pages and API but not streams or static files, CORS allows `Monokulo-Proof` and exposes `Monokulo-Challenge`/`Retry-After`, only admins see challenge counts; the limiter's memory is capped | The agreed matrix (`docs/ABUSE_PROTECTION.md`) | `abuse::limiter` unit tests; `http::abuse::tests` (in-process router with a fabricated `ConnectInfo`) |
| Monokulo embed policy and key auth: a restricted store takes orders only from verified pages or the store's secret key; wrong/other store's keys get `401`; key requests have their own budget; browser-created orders of a restricted store only render in a frame (`Sec-Fetch-Dest`) | Step 4/7 of the engine-boundary work pack | `http::embed_domains::tests` (`secret_key_orders_...`, `a_restricted_store_only_works_on_its_verified_domains`, `a_restricted_stores_browser_created_orders_only_open_inside_a_frame`) |
| Real Tor: a real `tor` running `deploy/tor/torrc.snippet` in front of monokulo's onion listener; visitors on separate SOCKS-isolated circuits get distinct identities; only the abusive circuit is challenged and then blocked; the stream cap is per (circuit, store); tor accepted the PoW/export/intro-DoS/stream settings (control port `GETCONF`) | Proves the Tor path end to end, not just the parser | `crates/monokulo/tests/e2e_tor.rs`, `#[ignore]`d (needs tor >= 0.4.8 with `pow: yes` and the live Tor network, ~5 minutes): `cargo test -p monokulo --test e2e_tor -- --ignored --nocapture`; see `e2e/README.md` |
| Global concurrency cap: requests beyond the configured limit queue or reject rather than spawning unbounded tasks | Directly tests the semaphore's presence, not just that requests eventually succeed | Drive many concurrent slow-handler requests; assert the number of simultaneously in-flight requests never exceeds the configured cap |

## 9. Schema / Migration

Automates what was manually verified live against `sqlite3` during design (§DESIGN.md
8) — that verification should not be the only time these constraints get exercised.

| Test | Why | How |
|---|---|---|
| `CHECK (status IN (...))` rejects any value outside the seven valid ones | Regression protection — this constraint was specifically tightened once already (removing `'double_spent'`) during design | Attempt an insert/update with an invalid status string; assert the DB rejects it |
| `UNIQUE(tenant_id, minor_index)` rejects double-issuance | Backstop for the writer-actor allocation logic in §6 | Attempt to insert two orders with the same `(tenant_id, minor_index)` |
| `UNIQUE(txid, output_index)` makes duplicate payment recording a constraint violation the application layer turns into a no-op | Backstop for §3/§6's idempotency tests | Attempt duplicate insert directly against the table, independent of any application code, to isolate the schema-level guarantee from the app-level handling of it |
| Foreign keys reject an order/payment/webhook referencing a nonexistent tenant/order | Prevents orphaned rows | Attempt inserts with a bogus parent id with `PRAGMA foreign_keys = ON` |
| The migration applies cleanly to a fresh database | Baseline — this exact check was run manually multiple times during design and should not regress to a manual step | Apply `migrations/0001_init.sql` to a fresh temp file in a test, assert success |

## 10. Integration / End-to-End

**Logs from a test.** Set `MONOKULO_TEST_LOG` to a `tracing` filter to see what the
engine, monokulo and the harness log while a test runs, on standard error, even if the
test hangs: `MONOKULO_TEST_LOG=debug cargo test -p mock-woocommerce <test>` (or
`cargo nextest run --no-capture ...`). Every test engine turns it on as it starts
(`engine_test_support::init_test_logging`); unset, nothing is logged.

**Why a separate tier from §3's mocked-daemon tests**: unit-level correctness against
a scripted fake doesn't prove the pieces compose correctly against a *real* Monero
node's actual RPC responses (real serialization quirks, real field-naming, real error
shapes). Conversely, a real node can't practically simulate a reorg on demand, so this
tier is for wiring confidence, not for the reorg scenarios in §3.

- Run against Monero's own `--regtest` mode in CI: fast, free, deterministic block
  timing (blocks are mined on demand rather than waited for).
- At least one full-lifecycle smoke test: create an order, fund it from a regtest
  wallet, mine blocks, observe `status` transitions through both the polling API and
  the SSE stream, confirm the real `MoneroDaemonClient` implementation's RPC calls
  parse correctly against a real node's real responses.
- Explicitly out of scope for this tier: reorg/double-spend scenarios (regtest doesn't
  produce realistic reorg conditions since the test controls the chain directly) —
  those stay in §3 against the scripted fake.
- Implemented today against a real public **stagenet** node rather than a local
  `--regtest` instance (simpler to stand up, no local `monerod` build required):
  [`crates/e2e-harness/tests/e2e_stagenet.rs`](../crates/e2e-harness/tests/e2e_stagenet.rs) drives the real
  config → store → key custody → scanner → router pipeline in-process and pays the
  order it creates with a genuine transaction constructed, signed, and broadcast
  entirely in Rust (`tests/support/mod.rs`, no external wallet process). `#[ignore]`d
  like `daemon_rpc::live_node_tests`, run explicitly with
  `cargo test --test e2e_stagenet -- --ignored --nocapture`; see
  [`e2e/README.md`](../e2e/README.md) for detail - the public stagenet node is the
  only external dependency.

- The WooCommerce checkout runs end to end by default, without stagenet:
  `mock-woocommerce`'s `a_full_woocommerce_checkout_is_created_with_the_key_opened_and_paid`
  connects through monokulo, creates the order with the store's secret key, opens
  monokulo's checkout page and receives the engine's signed `order.paid` webhook
  (the test engine settles the order with `TestEngineHandle::mark_order_paid`).

## 11. Client Library / Widget (browser-level)

**Why this tier exists separately**: it's the one surface with no compiler/type
system backing it (plain JS on an arbitrary third-party static site) and the one place
cross-origin behavior needs to be proven end-to-end rather than assumed from the
server-side tests in §5.

| Test | Why | How |
|---|---|---|
| A restricted store refuses order creation from a page on an unverified domain | Confirms monokulo's *server-side* embed policy is what's protecting this, not just browser CORS, which is a client-side courtesy an attacker's own script simply doesn't have to honor | `a_restricted_store_only_works_on_its_verified_domains` (`crates/monokulo/src/http/embed_domains.rs`) sends the request with forged and unverified `Origin` headers, no browser involved; the Playwright surface test `a restricted store's framing header keeps its checkout off other websites` covers the browser side of `frame-ancestors` |
| The "Checking your connection" interstitial solves itself with JavaScript, waits 10 s and continues without it, works in a cross-site frame either way, and `monokulo-client.js` solves an order-creation challenge on its own | Customers must get through a challenge with no action and no cookies, in Tor Browser (no JS) too | Playwright surface tests in `e2e/pos-playwright/tests/surface.spec.js`, against a small Node server implementing the challenge protocol with the real `challenge.js`/`monokulo-client.js` |
| `postMessage` payload shape is stable across a status change | The merchant's own page depends on this contract | Drive a checkout through a status transition in a test harness; assert the received message matches the documented shape |
| SSE reconnects after a dropped connection without duplicating already-seen status updates | Real-world flakiness (router Wi-Fi, mobile customers) makes this a realistic scenario, not a hypothetical one | Simulate a connection drop mid-stream; assert reconnection occurs and no duplicate/out-of-order UI state results |
| The double-spend explanation banner renders independently of the current status label | Direct UI-level regression test for the two-axis design (§2, §DESIGN.md 7.6) — this is the customer-facing consequence of that decision and deserves its own check, not just the data-layer test | Render the widget against a fixture order with `double_spend_detected_at` set and `status = 'paid'`; assert both the "paid" state and the warning banner are visible simultaneously |

## 12. Non-Functional / Resource

These are advisory, not hard gates — resource measurements are inherently noisier on
shared CI hardware than correctness assertions, so treat a failure as a signal to
investigate rather than an automatic build break.

| Test | Why | How |
|---|---|---|
| Binary size stays within a tracked budget | Directly protects the "as small as possible" goal (§DESIGN.md 2) from silent dependency creep | CI step comparing the built binary's size against a checked-in baseline, failing (or warning) past a configured delta |
| Idle CPU/memory footprint stays within a rough budget | Directly protects "minimal resource use, doesn't compete with the router's other duties" | Run the server idle for a fixed period under a resource-measuring wrapper; assert RSS and CPU time stay under a generous threshold — treat as advisory given shared-CI noise |

### Order creation and allocation properties

`http::tests::properties` drives the real authenticated router and custody implementation.
Ten generated properties use 64 cases by default and run in the existing default/ZMQ
CI matrix and daily exploration filter. `PROPTEST_CASES` and persisted regressions
work as for scanner properties. Run `cargo nextest run -p engine --lib --locked
-E 'test(http::tests::properties::)'`.

| Property family | Inclusive ranges and guarantees |
|---|---|
| Tenant-scoped creation histories | 2–4 tenants, 1–24 events, eight reusable keys; base amount 1–999,999,999,999 plus key offset; repeats, metadata changes, invalid/conflicting requests, custody failures, stale handles and database reopen. Independent wallet/index address derivation, exact purchase fields, stable replies and contiguous allocation counters. |
| Request boundaries | Full-width u64 amounts and confirmation counts, explicit zero/max/max+1 amounts and confirmation boundaries; keys of 1–128 visible ASCII bytes, empty/129-byte/space/control/non-ASCII keys. Invalid requests cannot change orders or counters. |
| Concurrent production-worker creation | 2–32 callers across 1–3 tenants; shared or distinct keys, amounts 1–9,999. Bounded contention errors may require client retries; every purchase eventually gets exactly one order and unique address. |
| SQL failure recovery | Denial positions 0–99, optional reopen; observable denial traces, no burned indices, idempotent recovery. A fixed sweep covers every reached denial position through creation. |
| Cancellation and tenant disable during derivation | Amounts 1–9,999; explicit rendezvous proves the derivation was reached. Cancellation preserves allocation for retry; disabling a tenant before its claim cannot create an order. |
| Allocation exhaustion and consistency | Last four u32 counter values, matching/mismatched claimed indices. MAX is the terminal exclusive scan bound; allocation cannot overflow it. Fixed replay checks preserve old idempotent orders even after exhaustion and reject corrupt out-of-range counters. |
| Process death around allocation commit | Amounts 1–9,999; named before/after-commit rendezvous, hard kill, integrity check and reopen. Both the order and counter commit together, and retry creates or recovers exactly one order. |
| Keyless purchases and conflicting keyed purchases | 2–16 identical keyless requests create distinct orders; amounts 1–9,999 and confirmation counts 0–19; changing amount, merchant reference or confirmation override under an existing key returns conflict without writes. |

This suite exposed exhaustion of the u32 allocation counter and inconsistent
caller-supplied indices. Both are rejected before committing. Allocation now also
checks tenant enablement inside the claim. The SQL sweep exposed a one-shot failure
restoring the inline read-only test connection; restoration now retries once so
subsequent valid writes can recover.

### Key-custody properties

Twelve generated properties in the `key-custody` crate's
`{plain,router}::properties` use 64 cases by default, with real Monero keys and
a real paying RingCT transaction. They run under the existing CI/daily property
filter. Run `cargo nextest run -p key-custody --lib --locked -E
'test(plain::properties::) | test(router::properties::)'`.

| Property family | Inclusive ranges and guarantees |
|---|---|
| Sparse scan-window histories | 1–12 generated windows of 0–12 input indices, biased toward 0–15 with full-width u32 and MAX; forced payment inclusion/removal/reinclusion; 1–8 paying/unrelated transactions. Cached and fresh scans match an independent known-output oracle, preserve batch positions and amounts, derive only newly added indices and never rebuild unchanged windows. |
| Lookup/live cache independence | 1–12 major/minor range changes, small and near-MAX endpoints, including empty/reversed ranges. Lookups agree with fresh scans, reuse unchanged ranges, and leave the live scan window intact. |
| Scan cancellation | Lookup/live paths, cold/warm caches, 0–11 extra indices. A blocked CPU worker proves cancellation reached queued work; both caches subsequently recover the real payment. |
| Registration, sealing, retries and removal | 1–16 concurrent same-ID callers, 1–128-character IDs; full-width major/minor indices, all three networks; same material returns one handle, conflicting material is refused, removal and restart preserve address identity while invalidating old handles. |
| Malformed inputs and resource bounds | 0–96 arbitrary sealed bytes, independent material decoding; oversized ranges of 1,000,001–u32::MAX entries fail without corrupting an existing wallet. Arbitrary registration IDs, explicit 0/128/129-byte boundaries, with no allocation on invalid IDs. |
| Concurrent wallet scans | 2–16 generated calls plus forced paying/unrelated/empty/recovery calls, windows of 0–11 indices drawn from 0–11; wallets retain independent caches and matches under interleaving. |
| Router ownership histories | Two real backends; 1–24 register/idempotent-register/replace/disable/remove/backend-loss/retained-reload/unknown-handle events; four generated wallet identities, full-width minor indices. Both backends start populated; every live handle derives its own wallet's address and only affected handles are invalidated or freed. |
| Outages and epochs | 1–8 failed derivations, all networks, full-width minor indices; failures retain handle ownership. Either backend advances through 1–u64::MAX epochs; only that backend's handles are invalidated, stable epochs preserve new handles, and combined epochs cannot overflow. |
| Interrupted registration | 1–8 registrations held after backend allocation, then replacement; late replies are rejected and old keys freed. Cancellation after allocation followed by an idempotent retry recovers one handle with no duplicate wallet. |

A fixed scan-table test crosses 255/256/257 and 511/512/513 entries and changes
windows during partial construction. The epoch property exposed an unchecked sum
of backend epochs; status aggregation now saturates while per-backend invalidation
continues to compare each actual epoch independently.

### Webhook delivery and recovery properties

Twenty-one generated properties in `webhook_delivery::properties` use **64 cases per
property** by default. They use real local HTTP endpoints, file-backed SQLite,
and the production database worker. A separate queue oracle calculates eligible
heads and tenant shares without calling the production selector. A stalled DNS
resolver is injected only to prove the timeout covers name resolution. No external
service, new dependency, or live daemon is needed. Existing Linux/macOS CI and the
daily default/ZMQ exploration job select these tests automatically.

| Property family | Inclusive ranges and guarantees |
|---|---|
| Retry and restart histories | 0–9 failures, ceilings 1–8, success statuses 200/201/204/299, arbitrary Unicode payload text. Every request preserves payload bytes and event ID, has one valid fresh signature, carries merchant headers, and uses the exact 60/120/240/480/960/1920/3840-second backoff from the recorded attempt time. Reopening preserves all row fields; success clears failures and terminal outcomes stop retries. |
| Stored-header defenses | Nine case-varied reserved names and values of 1–32 ASCII characters, plus a fixed simultaneous override attempt. Even legacy stored headers cannot replace or duplicate signing, event identity, content type, host, framing, or connection headers. Safe merchant headers still arrive. Admission and delivery share the reserved-name rule. |
| Independent fairness/FIFO model | 1–20 tenants, 1–8 orders each, 1–4 events per order; 1–24 retry/success/give-up/no-op actions; tenant shares 0–7 and total limits 0–64; clocks 999/1000/1100. The earliest pending event blocks later events even while waiting for retry, terminal events unblock them, and tenant shares apply after order-head selection. |
| Late outcome histories and overlapping ticks | 1–29 reordered bookkeeping outcomes, timestamps 1–99,999. A real overlapping-request property uses late HTTP failures 400–599 and ceilings 1–8. Acknowledged success remains authoritative, duplicate/late failures cannot rewrite it, and terminal failures cannot be reopened by retry scheduling. |
| Private-destination policy reload | IP literals and `localhost`; forced allow→deny followed by 1–15 generated flips. Tightening policy prevents new requests even after a permitted request warmed a connection. Clients retain separate pools with fixed resolver policies. |
| Environment proxy bypass | Four HTTP/ALL proxy environment names, isolated child processes and a real local proxy. Guarded requests must not reach the proxy or bypass private-destination classification. Guarded delivery ignores system proxy settings; the explicit private-URL policy retains proxy support. |
| Redirects and HTTP status classes | Redirects 301/302/303/307/308 never reach their target or forward a signed event. Statuses 200–599 are recorded exactly; only 2xx succeeds, and all other responses follow the failure/give-up contract. |
| Timeouts, disconnections and DNS stalls | Zero timeout, hanging HTTP, connection refusal and accepted-connection reset; 1–15 ms deadlines, ceilings 1–4. Stalled DNS is bounded by the same deadline. Failures are persisted, URL tokens are excluded from errors, reopen preserves the row, and a healthy endpoint recovers eligible retries. |
| Cancellation while a batch is running | 1–6 fast and 1–6 held requests. Rendezvous proves fast outcomes committed and slow requests entered before cancellation. Committed successes survive, unfinished rows remain retryable, and subsequent delivery converges. |
| Actual batch/concurrency bounds | 1–20 tenants, 1–6 orders each, 1–3 events per order; full drain verifies exact request count and FIFO. A separate 16–64-tenant test fills and holds all 16 worker slots, checks no seventeenth request starts, then drains; batch size never exceeds 50 or four eligible heads per tenant. |
| Legacy payload identity | Arbitrary Unicode raw payloads and full-width u64 non-string event IDs. Two real sends preserve exact body bytes and the stable delivery-ID fallback, with valid signatures. |
| Subscription lifecycle | Two subscriptions on the same order, 2–6 events each. A disabled subscription does not block the other; reopening and reenabling preserves its own FIFO. Deletion cancels only its queue and allows the documented delete-and-recreate rotation. |
| Lowered budgets and integer limits | Recorded attempts 1–15 and new ceilings 0–15; an already exhausted row retires without another network request or fabricated attempt. Small/full-width u32 attempt counts and ordinary/near-MAX i64 timestamps cannot panic or overflow; retry timestamps saturate. |
| Observable SQLite failures | Denial positions 0–31 across success, retry, give-up and already-exhausted retirement. Errors are surfaced, later completed outcomes still commit, and unaffected/pending rows recover after reopen. A fixed sweep covers every reached SQL boundary for all four outcomes. Deletion additionally tests positions 0–23 and a complete reached-boundary sweep, with both correct and incorrect tenant IDs; failed deletion rolls back parent and children together. |
| Process death at durability boundaries | Generated success/retry/give-up/exhausted-retirement outcomes, before/after the atomic write. A fixed sweep always kills a child at **all eight** named rendezvous points, checks SQLite integrity after reopening, and verifies terminal/pending state. A delivered request whose acknowledgement was not persisted is sent again with the same body/event ID and a valid newly timed signature. |

Fixed regressions also protect success/error cleanup, late-outcome immunity,
reserved stored headers, a warmed `localhost` connection after policy tightening,
and full-width counters/timestamps. Replay seeds are committed in
`crates/engine/proptest-regressions/webhook_properties.txt`.

These tests enforce **at-least-once**, so an interrupted acknowledgement or an
overlapping worker may deliver a duplicate. The durable attempt count tracks
recorded outcomes rather than every possible request received by the merchant.
Consumers must deduplicate by event ID. Explicit subscription deletion atomically
removes its delivery rows, including history; a request already in flight can still
finish. Ordinary delivery/give-up retains rows for inspection. The signing/SSRF
primitive tests remain in `shared`; these properties exercise their delivery wiring.

```sh
# Local default budget, including fixed regressions and complete fault/crash sweeps.
cargo nextest run -p engine --lib --locked -E 'test(/^webhook_delivery::/)'

# Larger, reproducible exploration with the optional ZMQ engine configuration.
PROPTEST_CASES=128 PROPTEST_RNG_SEED=83 cargo nextest run -p engine --lib --locked \
  --features zmq -E 'test(/^webhook_delivery::/)'

# All three new target suites together.
cargo nextest run -p engine --lib --locked -E \
  'test(http::tests::properties::) | test(key_custody::plain::properties::) | test(key_custody::router::properties::) | test(webhook_delivery::properties::)'
```

## Deterministic scheduler, queue and boundary exploration

The production round runner now delegates its two-pass scheduling decisions to
`work::scheduler::Scheduler`. It accepts explicit elapsed monotonic time, issues
one `RunUnit`, and accepts only that outstanding unit's completion. Tokens include
an explicit round generation; duplicates and completions from another generation
cannot alter progress. The runner still executes the real daemon/custody/SQLite
operations, records activity, and preserves the first actual failure.
`work::retry::Retry` similarly separates retry policy from the clock and map.
`store::dispatch::Dispatch` supplies the real database worker's class rotation;
Tokio channels and the worker thread still own admission, capacity and execution.

This split supplements the existing integration suites. SQLite remains the source
of durable truth: issuing an effect does not mean it committed. In particular,
block publication rechecks the parent, block identity, tenant cursor and pending
reorg inside its transaction. A cancelled caller's accepted database job can still
commit, and must leave the same durable recompute obligation as an observed reply.

| Suite | Generated surface and bounds | Required assertions |
|---|---|---|
| `work::scheduler::properties` | 0–99,999 ns budgets, 0–149,999 ns opening costs; 0–255 scripted units, each 0–999 ns; generated positive tier shares; every progress outcome; full `u32` retry counts, `u64` times/generations and full-width `Duration` budgets/opening costs | Full trace matches a separate interpreter of the pre-extraction two-pass contract; each tier gets its progress floor; terminal tiers stop; duplicate, outstanding and other-generation effects cannot change counts; retries saturate, reset and expire per key. A fixed sweep covers all 1,024 combinations of five tier outcomes. |
| `store::db::properties` | Three classes, 1–66 submissions per class around capacity 64; cancellation before admission and after admission; injected job panics; 1–64 accepted jobs per class when all senders close | FIFO within each class, exact round-robin drain, no execution of cancelled unaccepted work, execution and durable writes of accepted abandoned work, isolated panic failure. A fixed sweep explores all 8^6 readiness histories for each continuously ready class. Fairness is measured in service turns, not wall-clock time. |
| `work::blocks::properties` | Six late-commit situations × cancelled/observed caller × staged/direct real cryptographic scan; 1–255 cache actions, heights 0–31 and sizes/budgets 0–999,999 | Stale parents/hashes, rewound or advanced cursors and pending reorgs cannot publish money; valid abandoned writes survive reopen with a recompute obligation. Every late-commit combination also runs in a fixed sweep. Cache contents, byte accounting, protection, victim selection and discarded bytes agree with an independent eviction model. |
| `loops::properties` | 1–39 generation actions over three networks; panic/return recovery; 1–11 real admin configuration saves with network masks 0–7, changing fallbacks and a local endpoint that holds RPC responses; 1–7 restart failures before stopping | Previous supervised futures are gone before replacement; other networks retain their generations; dropping ownership stops children; the actual manager follows saved settings; stopping during factory or loop restart backoff is immediate. |
| Scanner lifecycle properties | Bootstrap heights 0–4; 1–11 outage/fault/reopen actions; tips 12–49, retention depths 1–9, forks 0–4 blocks below the edge, up to four full database reopens; genesis divergence histories | No bootstrap creates money; a newly arriving real payment is found once after recovery; retained-window evidence is reconciled, while payments below it retain the documented limitation; genesis replacement finishes without a stranded reorg. |
| `scaling::properties` | Full-width counters, durations and timestamps; arbitrary floats plus fixed NaN/infinity/zero/extreme cases; 1–255 telemetry events; changing valid link costs and memory budgets | Requests remain bounded, increasing usable resources cannot reduce their size, timeouts stay within their floor/ceiling, timestamp/counter arithmetic cannot wrap or panic, telemetry stays bounded even with a stopped/backwards clock. |
| `store::properties` | Every current schema version (1–27) with real historical migrations and existing money/queues; amounts 1–`i64::MAX`, Unicode payloads, SQL authorizer fault positions 0–399; generated process kills from every historic prefix (1–26) | Upgrade preserves payment identity/amounts, exact queued payloads, partial scans, recompute obligations and reorg work; migration versions remain contiguous after failure; reopening finishes the upgrade; integrity and foreign keys hold. Fixed sweeps cover all historical schemas, every reached boundary of the final migration, and kills before/after commits from every historic prefix. |
| `exploration::properties` | Byte histories 0–4,095 bytes; all float bit patterns; CPU range/set histories and full-width endpoints | Shared fuzz oracles check scheduler/dispatch invariants, sizing, bounded CPU parsing, setting and identifier serialization, malformed input handling. |

New properties default to **64 cases**, overridden by `PROPTEST_CASES`; regression
seeds are replayed first. The existing ordinary and daily property jobs discover
these suites through `::properties::`. Run the new surfaces or the complete suite:

```sh
PROPTEST_CASES=128 PROPTEST_RNG_SEED=47 ENGINE_PROOF_CASES=16 \
  cargo nextest run -p engine --lib --locked -E 'test(::properties::)'
cargo nextest run -p engine -p shared --lib --locked --features zmq
```

Coverage-guided fuzzing lives in the isolated `fuzz/` Cargo workspace. Its lockfile
uses the application's locked versions for shared dependencies; `libfuzzer-sys`
and exhaustive policy event orders are additional test-only checks. `engine/fuzzing` exposes only exploration
entry points and is absent from ordinary shipping builds. All eight targets invoke
actual production policy or boundary code through the same oracles used in normal
properties:

- `scheduler`: time, progress outcomes and completion sequencing.
- `status`: independent aggregate status oracle, full-width inputs and metamorphic checks.
- `history`: real scanner, database and custody histories with independent money/recovery checks.
- `notifications`: actual notification waits, coalescing, cancellation, network isolation and topic decoding.
- `queue`: bounded arrivals, closed classes, class selection and draining.
- `mempool`: batched scan reservations, partial completions, cancellation, eviction, changed windows, failed-tenant filtering, snapshots and cache budgets.
- `resources`: arbitrary numeric bit patterns in request sizing, retries and timeouts.
- `inputs`: CPU lists, node settings, scalar settings, identifiers, headers, URLs and signature header rejection.

Install `cargo-fuzz` once, then use the bounded runner. The final parameter is an
optional `zmq` feature selection. The budget excludes compilation.

```sh
cargo install cargo-fuzz --locked
ENGINE_FUZZ_SEED=47 scripts/engine-fuzz.sh scheduler 60
ENGINE_FUZZ_SEED=47 scripts/engine-fuzz.sh queue 60 zmq
ENGINE_FUZZ_SEED=47 scripts/engine-fuzz.sh resources 60
ENGINE_FUZZ_SEED=47 scripts/engine-fuzz.sh inputs 60
ENGINE_FUZZ_SEED=47 scripts/engine-fuzz.sh mempool 60 zmq

# Replay a saved failing input directly; replace the artifact filename.
cargo fuzz run --fuzz-dir fuzz inputs fuzz/artifacts/inputs/crash-HASH

# Enumerate every serialized policy event ordering.
cargo test --manifest-path fuzz/Cargo.toml --locked --test interleavings
cargo test --manifest-path fuzz/Cargo.toml --locked --features zmq --test interleavings
```

The fuzz runner copies reviewed `fuzz/seeds/` into ignored mutable corpora, supplies
an explicit seed and per-target input/deadline settings,
and uses AddressSanitizer. The daily/manual `engine-fuzz.yml` workflow exercises
all nine targets under both feature configurations, caches evolving corpora and
uploads corpora, reports and failure artifacts. Separate jobs enumerate all
serialized event permutations against the actual scheduler/reservation policies
(two events: two orders; three events: six orders). These replace the former Loom
models whose mutex serialized every entire policy operation. They cover the same
policy event order space directly; they do not model memory races or instrument
Tokio, parking_lot, SQLite or network effects. Real worker/custody rendezvous,
notification, HTTP and subprocess-crash scenarios test those effects.


Fuzzing explores paths rather than proving every execution. The retained-window
trust limitation remains explicit; daemon response decoding and PoW arithmetic
remain deferred. Every discovered product bug should get a named regression in
addition to its minimized input or persisted Proptest seed.

## Concurrent mempool scan reservations and cache locks

The fast mempool loop and round mempool tier now atomically reserve each
transaction/tenant before calling custody. The reservation rechecks the completed
scan window, so two callers that previously selected the same work cannot both
scan it. Different transactions and tenants can still scan concurrently; changed
windows for one transaction/tenant wait until its current reservation ends, then
are rediscovered by the round rotation. This prevents simultaneous old/new-window owners
from completing out of order. Cached older windows can still require a subsequent
rescan; window generations identify content rather than chronological order.

Reservations are ownership guards, not held mutex guards. Success records the
completed window and releases its reservation atomically. Failure, cancellation
and unwinding release unfinished reservations without marking them successful.
Pool eviction and cache clearing preserve live reservations. A cancelled caller's
accepted custody/blocking computation or database job can still finish; retrying
that work may be necessary. Database idempotency remains the protection against
repeated payment publication. Body fetching is not coalesced by these scan claims.
The fast-path scanned count excludes transactions for which no scan was reserved.

Body serialization, remembered-ID sorting/shortening, and full-cache destruction
on `forget` now happen outside the mempool mutex. Remaining critical sections
include cache insertion, ID snapshot copying, pool pruning, and reservation
bookkeeping; none awaits network, custody, or database work while locked.

`work::mempool::properties` includes the following surfaces. Cases default to 64;
`PROPTEST_CASES` and `PROPTEST_RNG_SEED` control expansion and replay. The existing
property jobs discover them automatically, and the daily fuzz matrix runs `mempool`
with both default and ZMQ features.

| Property/scenario | Generated range | Assertions |
| --- | --- | --- |
| Single-tenant ownership histories | 1–255 events; 8 callers, 4 transactions, 4 tenants, 4 windows | Cancellation, completion and eviction agree with independent ownership model. |
| Batched ownership/cache histories (`mempool` fuzz oracle) | 0–4,096 bytes; up to 682 complete events; 8 caller slots, transactions and tenants; tenant masks 0–255; window indices 0–255; cache count cap 0–16, byte cap 0–65,535 | Exact admitted tenants, owner keys and completed generations; partial/repeated/foreign completions; owner replacement; duplicate batch inputs; eviction preserves live ownership; due filtering; sorted/limited snapshots; duplicate bodies and byte/count caps. Every step compares complete maps/sets with independent ordered models. Four reviewed histories also run as normal regression tests. |
| Concurrent overlapping batches on real OS threads | 2–8 callers; 4 transactions; nonempty masks across 8 tenants; full-width `u32` window indices | Every requested transaction/tenant has exactly one live owner; unrelated work is admitted; guards release all claims. Positive barriers keep owners alive while the main thread inspects actual state. |
| Full-width body-cache accounting | 1–127 insert/remove events; 16 transaction IDs; count cap 0–16; all `usize` sizes and byte budgets | Accepted entries and retained bytes match a `u128` reference sum; duplicate insertions, removal and overflow cannot corrupt accounting or exceed caps. Sizes are supplied accounting inputs; this does not fuzz transaction decoding. |
| Real fast-pass/round contention | Either entry point wins; success or cancellation; 1–8 competing calls | Losers do not call custody; abandoned owner releases work; retry publishes payment once; repeated calls preserve payment identity. Uses actual production entry points, cryptographic fixture and file-backed SQLite worker. A fixed sweep also covers all four winner/cancellation combinations. |
| Mixed tenant outcomes | Custody failure, payment write failure, recompute-obligation write failure, inline status write failure, or empty scan then expanded window; both tenant orders; 1–8 repeat calls | A successful tenant remains complete while the affected tenant alone rescans; failed transactions leave neither payment nor credited amount; repair preserves stable payment identities. A fixed sweep runs all 10 outcome/order combinations regardless of random draws. |
| Held-owner boundary sweep | Success, custody error, custody timeout, cancellation during custody or after accepted DB admission, failed payment write, cache clearing/eviction during custody | Competitors do not scan; no cache mutex remains held during awaits; unsuccessful claims are retryable; accepted abandoned database writes remain safe to repeat. Timeout uses Tokio's paused clock after a positive custody rendezvous. |

Fixed tests additionally cover changed windows, unrelated work and panic after
partial batch completion. Existing two-thread fast/round/API money properties
continue to cover restarts and reorgs. The byte fuzzer runs real synchronized cache
and reservation code sequentially; it does not model weak-memory thread schedules,
network calls or SQLite. Separate thread and integration properties exercise concurrent callers, custody
boundaries and SQLite publication. These checks do not assert that accepted backend work stops when its
caller is cancelled, or that window generations impose chronological ordering.

```sh
PROPTEST_CASES=256 PROPTEST_RNG_SEED=113 cargo test -p engine --lib --locked \
  work::mempool::properties
ENGINE_FUZZ_SEED=113 scripts/engine-fuzz.sh mempool 60
ENGINE_FUZZ_SEED=113 scripts/engine-fuzz.sh mempool 60 zmq
```

A manual diagnostic compares identical cache operations behind `parking_lot` and
Tokio mutexes on two Tokio worker threads, at 256 and 20,000 cache entries. It
reports acquisition wait and critical-section hold p50/p99/max; sorting occurs
outside both locks. Run it separately from correctness tests:

```sh
cargo test -p engine --lib --locked compare_blocking_and_yielding_cache_mutexes \
  -- --ignored --nocapture
# Use --release for deployment-oriented measurements; debug timings are diagnostic.
```

The initial debug run found typical holds around 6 microseconds at 256 entries,
but full-cache operations at 20,000 entries reached millisecond holds. Tokio's
mutex did not shorten those CPU operations and had greater p99 acquisition delay
in that workload. These synthetic timings are machine/load dependent and are not
production latency guarantees, and this diagnostic does not measure HTTP latency
or unrelated-task responsiveness. Keep `parking_lot` for the current synchronous
ownership guards; reduce or shard large cache operations before treating an async
mutex as a general cure. An async mutex yields waiting tasks but still executes
critical-section CPU work on its holder's runtime thread. Switching the reservation
mutex also requires redesigning synchronous cancellation cleanup, which cannot
await a lock in `Drop`.

[Tokio’s mutex guidance](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html#which-kind-of-mutex-should-you-use) likewise recommends a blocking mutex for ordinary shared data when the critical section does not span an await.


## Status coverage-guided fuzzing

`status` shares the independent aggregate specification with the existing status
properties. Inputs are capped at 4 KiB, with full-width expected amounts, required
confirmations and signed timestamps, and up to 128 payment records. Expected amounts
remain positive; pool payments have zero confirmations. Assertions check the exact
status, ordering independence, equivalent payment splitting and settlement under
confirmation growth. Reviewed seeds force expiry, saturation, mixed evidence and
zero-confirmation boundaries. Normal properties also generate byte histories.

Run `ENGINE_FUZZ_SEED=149 scripts/engine-fuzz.sh status 60` (append `zmq` for that
configuration), or `cargo test -p engine --lib status::properties`. Daily fuzz jobs
include both configurations and preserve corpus/replay artifacts.


## Full scanner-history fuzzing

`history` shares `work::history::Harness` with the original scanner properties,
including the independent canonical-chain/money oracle and real wallet transaction.
Each input runs up to 32 four-byte commands: mining, pool changes, same/shorter/longer
forks, spent evidence, daemon/custody outages, nine RPC failures plus malformed spent
answers, SQL denial positions 0–255, rounds, fast passes, cancellation attempts,
reopens and recovery checks. Each run forces initial money observation, a final
fork/reopen/recovery and re-mining with stable payment IDs. Rounds retain the existing
five-second virtual deadline; SQL fault traces verify the requested denial was
actually reached when applicable. Inputs above 128 bytes have an ignored tail.

This exercises real tier executors, file-backed SQLite and crypto, using the same
inline DB mode as sequential properties. Cancellation attempts use yielding inline
admission; real worker/custody cancellation rendezvous remain in the dedicated
integration properties. It does not fuzz response decoding, PoW, arbitrary valid
transactions, every thread ordering or power loss. Each execution cleans its own DB.

Run `ENGINE_FUZZ_SEED=157 scripts/engine-fuzz.sh history 60` and append `zmq` for
that configuration. `coverage_guided_histories_recover_with_real_scanner` generates
the same byte commands as a normal property; reviewed histories replay as tests.
Daily default/ZMQ fuzz jobs retain evolving corpora and failure artifacts.


## Notification and lifecycle exploration

`notifications` checks real Tokio `NodeWakes` against an independent three-network,
three-kind pending-permit model. Up to 128 commands generate bursts of 1–256 signals,
wait intervals 0–255 ms, cancelled waits before the minimum gap or after registration,
replacement wake state and arbitrary topics. Pool/chain/proof isolation, exact wake
counters, polling deadlines and minimum-gap throttling are checked with virtual time.
The topic decoder is shared with the production ZMQ subscriber in both feature builds.

Normal properties also run the actual fast mempool loop with a real paying wallet
under missing, wrong-network, burst and outage/recovery notifications, then positively
join shutdown and prove stale signals cannot restart work. All four modes run in a
fixed sweep. Under ZMQ, real TCP publishers disconnect/rebind, switch endpoints,
duplicate endpoints, disable/reenable settings and send invalid/stale topics. Counters
prove receipt and reconnect; the fixed lifecycle history complements generated 1–7
configuration changes. Existing manager/supervisor histories cover loop replacement,
panics, stop signals and configuration saves. Notifications never substitute for RPC
money evidence. Transport histories use bounded real deadlines; they do not enumerate
all OS/socket interleavings.

Run `cargo test -p engine --lib node_events::` and add `--features zmq` for transport
properties; `ENGINE_FUZZ_SEED=167 scripts/engine-fuzz.sh notifications 60` runs the
shared wait oracle (append `zmq` for that build). Both daily matrices discover it.


## Engine authorization properties

`http::tests::properties::authorization` sends requests through the production
`build_router`, without the test token-injection layer. Its explicit matrix covers
all 25 registered method/route combinations: tenant metadata/lifecycle, orders,
refund addresses, payment lookup, webhooks and SSE, plus engine status, tenant
creation, settings, logs, activity and proof-anchor administration. A fixed sweep
runs every route with absent/wrong/tenant-as-engine credentials, and all 14 tenant
routes with eight invalid tenant credential classes (missing, public key, revoked,
disabled, wrong scheme, unknown, tampered and engine token as bearer).

Generated tests use 2–4 real tenants, inline or production file-backed DB workers,
1–32 ownership/rotation/disable/read/refund/list events, ASCII credential noise up
to 128 bytes, order amounts 1–9,999, and 2–32 concurrent rejected writes or SSE
requests, duplicate capability headers, eight malformed Bearer forms, filtered
order/webhook lists and 1–12 foreign writes preceding a positive owner SSE event.
Independent principal state checks current/revoked/disabled credentials;
cross-tenant identifiers cannot expose or mutate another tenant, body identities
cannot redirect an authenticated purchase, and rejected requests preserve a full
ordered dump of every SQLite table. Positive owner controls prove rejection tests
have not merely broken all access; repeated valid SSE opens check permit cleanup.
The named history forces wrong-owner access, rotation, old-token rejection and
terminal disablement even when random histories shrink. A real daemon/transaction
fixture also exercises 1–4 repeated foreign payment lookups before and after the
owner records its payment: forged body identity cannot expose its order or credit
any wallet, and the full database stays unchanged for every foreign lookup.

The engine token is the instance-wide administrative capability; tenant routes
additionally require a tenant secret. These tests preserve that existing policy,
not a separate operator-role scheme. Rotation/disablement assertions apply to new
requests, not retroactive cancellation of an already authenticated stream/job.
Primitive token hashing remains tested in `shared`, outside this engine package.

Run `PROPTEST_CASES=128 PROPTEST_RNG_SEED=181 cargo test -p engine --lib
http::tests::properties::authorization` (append `--features zmq` before the filter).
The existing daily default/ZMQ property filter includes all new families.

## Combined production-worker concurrency histories

`work::tests::properties::concurrency` holds an actual block scan, fast mempool
pass and production-router payment lookup at positive custody rendezvous. All
three then reach admitted writes on the file-backed database worker before it is
released. Six admission permutations, four caller-abandonment choices, secret
rotation/refund/configuration changes and a persisted reorg guard run with 1–4
replays. A fixed sweep forces all six permutations and all four cancellation
choices. Recovery recreates volatile scanner state, drains the actual tiers and
checks exact amount, stable payment identity, mined location, no pending reorg or
recompute work, and exactly one paid webhook. Reopening preserves the payment.
The scan gate covers both batched engine scans and single HTTP lookup scans.

Run `PROPTEST_CASES=128 cargo test -p engine --lib
work::tests::properties::concurrency` with default and `--features zmq` builds.
These properties use explicit barriers, not sleeps to guess whether work started;
they cover these controlled interleavings rather than every OS thread schedule.

## Production-worker and repeated scanner crash recovery

The scanner durability suite now runs all eight staging/publication/status/reorg
before/after-commit checkpoints with the actual file-backed worker as well as the
existing inline harness. Generated amounts are 1–9,999 piconero per output.
A separate 1–5-process pipeline kills successive recovery workers on the SAME
SQLite file: publication after commit, recompute before/after commit, then reorg
completion before/after commit. Every intermediate reopen checks integrity,
foreign keys, all three output identities and amounts, status/event atomicity,
persisted recompute obligations and the reorg settlement freeze. Final recovery
must drain both queues and retain exactly one paid event and the original IDs.
The fixed sweep forces all eight checkpoints and the complete five-kill pipeline.

These are process-death tests, not simulations of power loss or torn storage.
Paused-clock worker tests explicitly hold virtual time until OS replies arrive;
subprocess rendezvous uses a wall-clock deadline and wall-clock polling.
Run `PROPTEST_CASES=64 cargo nextest run -p engine --lib --locked -E
\'test(work::tests::properties::money::expansions::)\'` in both feature builds.


## Mixed-wallet transaction history exploration

The shared `portfolio` property/fuzz harness generates 2–4 distinct wallets,
2 orders per wallet, and 2–4 distinct transactions. One transaction always pays
multiple wallets. Each transaction has 1–4 additional outputs (up to 20 total
across the scenario), with amounts 1–65,536, paying minor indices 1/2 or an
unassigned 99. Invoices deliberately request the exact total, half the total or
one more than the total, with confirmation thresholds 0–3. The independent ledger
comes from recipient instructions, not scanner results or the database.

In legacy byte histories, all outputs are first observed in the pool. Up to sixteen commands then independently
mine, return or drop transactions, replace branches, reopen a worker/reset scanner
state, repeat the fast path, and change chain length. Absence without positive
spent evidence must preserve the funds. At each quiescent point, exact recipient,
transaction/output identity, amount, height, non-void/non-superseded state, status,
and foreign-tenant rejection agree with the ledger. IDs remain stable. Final
forced mining with cold scanner state and a SQLite reopen provide positive controls.
Both inline and production file-backed worker modes are generated. Property byte
vectors contain 0–192 bytes; the decoder bounds work and ignores any unused tail.

These are scanner-valid transparent crypto fixtures with derived one-time keys
and per-output transaction keys, not fully signed transactions accepted by a live
Monero network. Consensus proof/signature arithmetic and daemon decoding remain
outside this package. Three reviewed seeds force mixed forks, worker restarts and
partial payments. `portfolio` is the ninth default/ZMQ daily fuzz target; its corpus
and failures use the existing runner/artifact workflow.

Run `PROPTEST_CASES=64 cargo test -p engine --lib mixed_wallet`, adding
`--features zmq` for that build. Run `ENGINE_FUZZ_SEED=229 scripts/engine-fuzz.sh
portfolio 900` and append `zmq` for fuzzing that configuration.

## Named mutation checks: testing the tests

`scripts/engine-mutations.py` deliberately introduces 27 defects, one at a time,
in a disposable detached worktree. The caller's engine sources are never edited.
Each selected test must first pass on the healthy snapshot, then fail by an
assertion bearing the defect’s specified `BOUNDARY:` marker on the mutant. All defects are checked in default and ZMQ builds:

| Intentional defect | Required detecting test |
|---|---|
| Never expire retry state | Production retry-map upkeep at expiry boundaries |
| Change funding/confirmation/expiry/overpayment comparison at equality (four mutations) | Independent 720-case status decision grid |
| Move retry expiry early/late, delay the second free attempt or change deadline equality (four mutations) | Production retry expiry and retry policy decision grids |
| Bypass completed-window or in-flight ownership guards (two mutations) | Production reservation decision grid |
| Double the persisted amount received | Mixed-wallet ledger seed replays |
| Accept a scanned block despite a changed parent | Complete late-commit prerequisite sweep |
| Read an order without checking its tenant | Named authorization/revocation history |
| Drop payment insert/update recompute obligations | Complete late-commit prerequisite sweep |
| Commit paid status without its webhook | Combined real-worker concurrency sweep |
| Accept another round's completion | Generated scheduler generation property |
| Trust one node’s spent vote despite disagreement | Combined portfolio independent void ledger |
| Publish completion for a different scan window | Held-owner wrong-window regression |
| Bypass matching-proof settlement requirements | Combined portfolio independent status ledger |
| Reverse the earliest-mined conflict winner | Durable conflict winner/reorg regression |
| Send a later webhook event before an earlier retry | Production FIFO/backoff regression |
| Retain an obsolete reorg staging checkpoint | Reopen/fork/network staging sweep |
| Retain obsolete staged matches | Same sweep, independent staging-row assertion |
| Keep custody handles live across backend epoch changes | Generated backend epoch isolation property |
| Let new pool arrivals displace old-window rescans | Sustained-arrival payment regression |
| Share the tenant-page cursor across transactions | Three-transaction scheduling regression |

Compiler/linker errors, zero selected tests, unrelated panics, wall timeouts and
failed rendezvous/virtual deadlines are invalid runs, never successful detections.
A surviving mutant or any invalid result fails the command. Sixteen runner checks
use real tiny Cargo test programs to verify those outcome classes, including
process-group cleanup on POSIX timeout. The isolated worktree is removed even
when a mutant fails; output contains the revision, tracked local patch hash,
cases/seed, baseline results, exact mutations/commands and full failure logs.
Tracked local edits and new engine modules are snapshotted for pre-commit checks.

```sh
python3 scripts/test_engine_mutations.py
python3 scripts/engine-mutations.py --cases 32 --seed 241
# Optional single configuration:
python3 scripts/engine-mutations.py --features zmq --cases 64
```

JSON and logs live in ignored `target/engine-mutations/`, with compiled artifacts
in its `build/` directory. A weekly/manual `engine-mutations.yml` job runs both
configurations and retains JSON plus logs. These checks demonstrate detection of
these 27 selected defects; they are not a percentage score for every possible bug.
The money, crash, concurrency, fuzz and authorization suites remain complementary.

## Combined money, proof, node, custody and delivery histories

The shared `portfolio` property/fuzz harness now composes three real adversarial
nodes through production fallback/corroboration, independent multi-wallet money
accounting, trusted verifier results, wallet handle replacement, SQL denial,
restart/reorg recovery and the production webhook executor against local HTTP.
Generated histories have up to 16 commands (previously eight), 2–4 wallets,
2–4 transactions, amounts 1–65536 and confirmation thresholds 0–3. The first
three branch changes exercise missing/mismatching proof before proof catches up;
zero-confirmation acceptance remains the explicitly configured trust boundary.
Proof fixtures exercise integration; they do not replace real-verifier properties.
Unanimous spent evidence voids absent transactions; conflicting evidence cannot.
Final canonical mining restores every output and retains its original payment ID.

Every input forces a real pending RPC timeout, a reached custody fault, a reached
SQL denial, and an all-node outage with unchanged money/cursors. Fixed inline and
worker histories force spent disagreement, void/restoration, proof holds, handle
replacement and reopen. Recovery requires all tenant cursors to reach the model
chain tip before comparing settlement depth, rather than stopping when payment
rows alone match. Proof holds deliberately retain settlement obligations; healthy
final recovery drains them. Status commits must have their corresponding durable
webhook, delivery fails once through actual HTTP, retries preserve exact event
bytes, and every expected event eventually reaches its tenant's destination.

Run `cargo test -p engine --lib mixed_wallet` and `cargo test -p engine --lib
combined_portfolio`; append `--features zmq` for that build. The existing portfolio
fuzzer and daily matrix automatically run this same expanded harness; reviewed
`combined-worker-0`/`combined-worker-1` seeds force the combined interactions.

## Proof/configuration/shutdown schedules and scan ownership

The mempool executor now delegates admission/completion/release to the pure
`Reservations` policy. Each admitted scan owns a non-cloneable lease; its existing
RAII guard releases unfinished leases on cancellation/panic. Completed generations
and live ownership remain separate from body-cache eviction. A completion carrying
a different window generation cannot mark success or release the current owner.
Existing independent property/fuzz ownership models run through this production
adapter unchanged. Exhaustive event permutations explore completion/rescan and
cancellation/cache-reset ordering using the production policy.
This explores atomic policy-call order, not parking_lot internals or the whole
Tokio/SQLite runtime; the existing real-thread/real-caller tests remain necessary.

Actual file-backed worker schedules additionally compose a block scan, proof
publication/anchor forgetting/mismatching proof, and tenant confirmation changes.
All six admission orders × four cancellation boundaries × three proof states ×
two custody replacement choices run in a fixed 144-case sweep, plus generated
properties. Gates prove entry into custody and worker admission. Accepted effects
must drain after callers abandon them, and a fresh scanner generation with the
replacement custody handle must preserve one exact payment/stable ID. Confirmation
and proof holds must produce no paid webhook; subsequent canonical proof catch-up
must settle once, enqueue exactly one paid event, drain recomputes, and survive a
DB reopen. This is controlled generation replacement, not an OS scheduling proof.

Run the `concurrency` and `mempool` engine tests (both feature builds), and
`cargo test --manifest-path fuzz/Cargo.toml --locked --test interleavings` (append
`--features zmq`). Existing daily property and event-order jobs discover the expansions.

## Recorded and generated paying RingCT histories

The shared portfolio harness selects a recorded-corpus mode with input bit 7;
bit 6 selects whole/pruned daemon body presentation. This mode has 2–4 wallets
and 3–4 transactions. It combines the frozen paying Bulletproof2 transaction
(output 1/minor 1/exactly 7,000,000,000 piconero), untouched recorded foreign
Bulletproof2/CLSAG/tagged Bulletproof+ transactions, and generated mixed-wallet
additional-key payments. The independent ledger now carries explicit output
indexes, including noncontiguous known recipients. Positive amount decryption
for CLSAG/tagged Bulletproof+ additionally uses clearly labelled synthetic pruned
bases; these preserve the known ciphertext/commitment but have no network-valid
signatures. Untouched recorded signatures remain in the foreign corpus.

A fixed 36-history sweep forces inline/worker × whole/pruned × three foreign
variants × exact/partial/insufficient goals, with thresholds 1–3. Histories include
pool observation, mining, multiple forks, spent disagreement, void/restoration,
custody replacement, DB reopen and delivery. Exact amounts, output indexes,
wallet isolation, statuses, stable whole-transaction IDs and recovery obligations
are checked throughout. The fixture-curation test checks frozen IDs, input/output
counts, RingCT type, tagged/untagged shape, signatures and known recipient results
using the crypto library directly. Recorded provenance and upstream licensing
are in `tests/fixtures/RECORDED_TRANSACTIONS.md` and its JSON manifest.

Run `cargo test -p engine --lib recorded_ringct` and the existing `mixed_wallet`
properties in both feature builds. Reviewed recorded/CLSAG portfolio seeds are
shared with the existing sanitizer fuzzer and daily CI; no external node is needed
for any test or fuzz execution.


## Boundary observations and strict mutation evidence

The mutation runner also executes four complete healthy boundary suites in each
feature build. Its schema-v2 report retains counters and fails a baseline if a
required observation is missing or malformed. Counters are emitted only after
scenario assertions pass. They measure observed bounded scenarios, not source
branch coverage or an exhaustive probability of catching bugs.

| Healthy boundary suite | Observations per feature build |
|---|---|
| Combined portfolio, inline and worker | Each mode independently reaches cancelled real RPC deadlines, a custody scan error, actual SQL denial, all-node outage with unchanged money/cursors, disputed and unanimous spent evidence, missing and mismatching proof holds, proof release, handle replacement, mid-history/final reopen, canonical void restoration, real HTTP 503 and a retry with identical bytes followed by full drain. Counts come from observed successful checks. |
| Proof/config/shutdown worker schedules | 144 completed schedules; 72 replace custody; 48 lose the anchor and 48 use a mismatching anchor. Six admission orders × four caller-abandonment positions × three proof states × two custody-generation choices. |
| Reorg staging cleanup | 18 reopen schedules across all three networks, forks 1/3/1,000, with/without reopening before completion. 36 obsolete checkpoints/match sets removed; 36 below-fork/other-network sets preserved. The accompanying generated property explores fork heights 1–2,000 and both reopen choices. |
| Recorded transaction histories | 36 complete histories, including 18 whole and 18 pruned presentations; both DB modes, all three frozen foreign RingCT types and three invoice goals/confirmation thresholds. Paying type-5/6 bases are explicitly synthetic; fixture provenance is documented separately. |

Detection requires the expected marker inside an assertion panic. Printing the
marker before an unrelated assertion cannot earn a detection. Runner tests cover
wrong assertions, printed-marker false positives, malformed/missing counters and
aggregation, in addition to compilation, zero-test, unrelated-panic and timeout
rejection. Reports preserve baseline logs, each exact patch/command and the local
snapshot hash; the weekly/manual workflow uploads the report and logs separately
for default and ZMQ.

```sh
python3 scripts/test_engine_mutations.py
python3 scripts/engine-mutations.py --features both --cases 32 --seed 241
# Human-readable evidence, including per-suite boundary observations:
python3 -m json.tool target/engine-mutations/report.json
```

Custody policies and their generated properties live in the extracted `key-custody` crate. The mutation runner selects that package for epoch checks; ZMQ build choices apply to engine tests.


## Scale correctness and measurements

`./scripts/engine-scale.sh default` and `./scripts/engine-scale.sh zmq` run the
complete package, including the deliberately ignored large fixtures. The normal
engine suite runs the smaller generated properties and fixed regressions. The
large fixtures are separate to keep ordinary edit/test cycles short; they are
required in `engine-scale.yml` on relevant pull requests, weekly, and manually.
The runner uses one test process at a time, no retries, a fixed/replayable seed,
and the CI process watchdog. `PROPTEST_CASES` and `PROPTEST_RNG_SEED` remain
available for replay/shrinking. Logs, JUnit and replay settings are saved under
`target/engine-scale/<default|zmq>/`; counterexamples keep the usual Proptest files.

| Surface | Generated range / fixed scale points | Checked through production boundaries |
|---|---|---|
| Distinct merchants and catch-up groups | 1–64 tenants, four orders each, 1–8 groups, 2–8 custody outage rounds; fixed 257/513/1025/2049 tenants, 16 groups, eight custody outage rounds | Real distinct wallet material, recorded RingCT money, file-backed worker, one observed node outage and one reached SQL denial, prefix custody failures, scheduler/worker reopening, every cursor reaching the tip, exactly one correct credit and zero foreign credits, persisted final ledger |
| Large pool and competing fast passes | 2–128 transactions; fixed 257/1025/4097/8193 | Reached custody failure, two live fast callers, exactly one successful scan per transaction/window, independently known payment amount/output/ID, no duplicate publication, accounted body bytes, release after pool departure |
| Transaction × merchant scheduling | 2–65 tenants × 2–6 transactions; fixed 257×2, 513×3, 1025×5; named 96×3 regression | Previously completed transactions paying addresses allocated later; every pair must be revisited within a work-count bound. These tenants intentionally share the paying wallet to make every missing pair observable. The distinct-wallet family above checks isolation. |
| Sustained new pool traffic | 320 new transactions per round for eight consecutive rounds | A previously scanned transaction paying a newly allocated address must be rediscovered while arrivals continue; the first address receives nothing |
| Pure pool rotation | 1–257 initially queued IDs, 1–64 completed per turn, 1–32 new arrivals per turn; fuzz ticket histories up to 1024 operations over eight IDs | New admissions cannot extend an existing item's wait; ticket oracle checks admission order, partial progress, missing bodies/deferred IDs, departures and membership uniqueness |
| Worker admission and dispatch | 1–8 waves; fixed 64 waves = 12,288 accepted jobs | All three queues filled to 64 each, abandoned accepted callers still commit exactly once, cancelled waiting callers never commit, exact per-class FIFO, one turn per three continuously ready jobs, complete drain |
| Webhook backlogs | 1–16 healthy / 1–8 failing tenants, 1–4 orders each, 1–3 events per order; fixed 1025 healthy / 128 failing, four orders, three events = 13,836 deliveries | Real HTTP/SQLite, reached 503 responses, healthy tail progress, persisted retry deadlines and reopen, eventual failed-merchant recovery, per-order FIFO, stable retry bytes, valid signatures and unique protocol headers, at most 16 live deliveries |
| Production cache limits | 20,065 attempted small bodies against the 20,000-body cap; serialized 8 MiB bodies against 128 MiB; 8193 block headers against a 1 MiB budget | Duplicate admission, integer overflow rejection, exact byte accounting, reclaim/refill, scanned-first eviction, preserved anchor, replacement, oversized pinned-anchor exception, branch-range removal and complete clear |

The round safety net now uses stable FIFO transaction membership and a separate
tenant bookmark for each transaction. New arrivals join behind existing work.
Only attempted bodies and unavailable bodies move to the back; bodies left by a
time deadline retain their position. Failed tenant scans remain due for later
laps. A shared tenant bookmark can phase-lock the two rotations and leave some
pairs unvisited, even when both counters keep advancing. The fast pass retains
its bounded new-transaction optimization. The mempool fuzzer also runs the pure
rotation against an independent monotonic-ticket oracle. Both named defects are
checked by the mutation runner in default and ZMQ builds.

No correctness assertion depends on a universal latency or throughput number.
`cargo xtask stress scale` separately measures 512/1024/2048 tenants, four orders
per tenant plus one 1024-order window, real SQLite readers/writers and status HTTP,
and three 256-tenant recovery points (RPC failures, a held writer, and one custody
slot). This versioned scenario is `xtask/stress/scenario_scale_v1.json`; existing
CI/full workloads are unchanged. Hardware/affinity, revision, scenario checksum,
commands, queue/query/HTTP/timer measurements, Linux process RSS/high-water RSS,
and recovery observations are stored in `target/coverage/stress-scale/`, alongside
an HTML report. Slow latency/capacity is reported rather than made a correctness
failure in this profile; actual fixture errors, missing tenant/background progress
or missing fault/recovery evidence still fail. Scheduled/manual scale CI uploads
these measurements; pull requests run the correctness matrix.

Cache limits account serialized bytes, not total process memory. IDs, windows,
completed transaction/tenant generations, orders and active leases grow with
current workload cardinality; this package checks accounted caps and reclamation,
not a constant whole-engine RSS guarantee. RSS measurements include the process's
allocator, SQLite, HTTP and test fixture, and unsupported hosts report null.
The capacity fixture repeats a recorded transaction across synthetic blocks as a
scanner workload; its throughput is not a consensus-valid-chain or financial
oracle. The scale properties use separate expected ledgers. Nonce-varied foreign
transactions and transparent scheduling payments are scanner fixtures, not newly
signed Monero network transactions. High-cardinality fixtures are deliberately
split to test each bound without requiring an 8193 × 2049 crypto cross product.

Portfolio restart controls replace both the active observer and executor, compare every persisted table, and check that connection-local TEMP state disappears. `connection-reopened-*` means connection replacement; `worker-restarted-mid-history` additionally means a new worker executor. These controls do not claim a process crash; subprocess crash tests cover that boundary separately.

Combined portfolio fault controls use an outstanding synthetic payment on the
same database/executor. Daemon deadlines run through `run_round_at`; the custody
attempt delta is captured after the separately named component scan; failed
custody creates no payment. Four SQL access positions run through real rounds,
with denied operation names recorded. Recovery must persist exactly one output
with the planned amount. SQL denial may be handled by a fallback/retry; an
additional counter distinguishes round-level errors. Isolated component entry
is not counted as engine-path coverage.

Portfolio semantic histories use `MKP\x01`, 128 fixed setup bytes, then four-byte
command records (up to 32). Properties generate setup and typed commands
independently (0–16 commands), shrink command lists structurally, and round-trip
the codec. Fuzzing uses the same runner/oracle. Inputs without the v1 marker retain
legacy decoding, so reviewed seeds and persisted byte regressions still replay.
Typed commands independently control arrivals, mining, extension (1–4 blocks),
reorg (up to four heights), disappearance, spent evidence, proof lag (0–7 blocks)
and mismatch, elapsed time (1–8 × 301 seconds), restart, faults and delivery.
Invoice goals, thresholds (0–3) and expiry are independently selected. Planned
outputs enter the expected ledger only after observation; disappearance retains
observed funds. The oracle tracks proof hashes independently across reorgs and
accepted settlements across proof changes. Failures print decoded semantic traces.

`work/portfolio/model.rs` owns the independent planned-output/status oracle and
its durable identity/accepted-status memory. `runner.rs` converges real effects
using a borrowed `Effects` context and a `Ledger` view, replacing the former
15-argument function. Typed properties and the fuzz adapter call the same runner;
there is no fuzz-only scanner or copied oracle. Both single-wallet and portfolio
histories use `support/backend.rs` for connection replacement and persisted-table
comparison, while retaining their separate domain expectations. Shared fixtures
continue to supply real custody and temporary databases.


Exploration workflows pin `nightly-2026-10-02` and cargo-fuzz `0.13.2`; ordinary
project builds retain the repository's nightly policy. The fuzz runner writes
`target/engine-exploration/fuzz/<target>/<features>/<seed>/<revision>-<run-id>/`: `replay.json` records
revision, dirty state, compiler/Cargo/fuzzer versions and limits; `calibration.json`
records measured reviewed-seed costs; raw logs and `report.json` retain executions,
rate, coverage/feature growth and unique corpus growth. Calibration failure is fatal.
Every invocation has an independent directory. Build and calibration failures
produce terminal reports; a remaining running report identifies interrupted work. Reviewed seeds are copied by content hash, so restored
corpora always include their current contents.
Semantic portfolio counts distinguish fixture category/backend, selected commands,
applied transitions and skipped commands;
raw successful-case observations remain available. Other targets report instrumentation
and corpus metrics; absent semantic counts are not silently presented as coverage.
Pure targets default to 300 seconds/10-second deadlines; queue and notification
scenarios to 600/60; history to 900/60; portfolio to 900/120 with a 260-byte limit.
These conservative deadlines are checked against measured seeds on every campaign;
review seed costs and actual execution rates before increasing sustained budgets.
Manual budgets override defaults. Exact tool-version artifacts allow replay using
the recorded toolchain even when local builds use another nightly.

Successful campaigns require parsed initialization and completion records with a
positive execution delta. Missing/malformed evidence is invalid; a campaign that
completes only seed initialization is rejected as insufficient exploration; `executions_after_initialization` makes this explicit. Increase its budget rather than treating replay-only work as a successful campaign.


Mutation checks include a systematic operator/guard grid around funding,
confirmation, expiry, overpayment, retry deadlines/expiry/free attempts and scan
ownership, alongside the domain defect examples. Deterministic boundary grids
make equality mutations falsifiable without depending on random sampling.
Healthy baselines and exact intended assertions remain mandatory. Any survivor
or invalid run fails the campaign and retains its source patch and execution log
for investigation. These 27 selected mutations are acceptance checks, not a
statistical mutation score or evidence of complete decision coverage.

Typed fault commands independently select SQL writes versus all statement accesses and positions 0–3. A fixed operation/position sweep requires actual denial and exact payment recovery for every choice, with denied operation names retained in semantic reports.
