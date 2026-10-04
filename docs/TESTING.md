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

The scanner suite (`crates/engine/src/work/properties.rs`, registered under
`work::tests::properties`) has forty-three generated properties. The first five cover:

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

Fifteen additional properties in `work/money_properties.rs` exercise money guarantees:

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

Seven further properties in `work/expansion_properties.rs` target storage and
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

Sixteen properties in `work/node_properties.rs` now use the production
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
- Reorg models retain two bootstrap blocks and stay within the configured window.
  Bootstrap/genesis and deeper-than-window forks have example tests, but are not
  generated histories yet; hashes outside the retained window cannot establish
  a fork without additional evidence.
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
| The engine serves no public (`/api/v1/t/{pk}/...`) routes and grants CORS to no origin on any route | The engine is private (DESIGN.md §4, §10.3); a public route or a CORS grant coming back would reopen a surface nothing but monokulo should reach | `the_engine_serves_no_public_order_routes_and_no_cors` (`crates/engine/src/http/tests.rs`): the old routes answer 404 and preflights get no `Access-Control-Allow-Origin` |
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
| The engine's per-token rate limit trips after the configured request count within the window (keyed on the `sk_`, or on the address for the token-less tenant-creation and `/status` routes), and resets after the window elapses | Core mechanism correctness | `admin_rate_limit_middleware_*` and `unauthenticated_routes_are_limited_per_address_by_the_admin_limiter` (`crates/engine/src/http/tests.rs`) drive requests past a small limit with a fabricated `ConnectInfo` |
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

Twelve generated properties in `key_custody::{plain,router}::properties` use 64
cases by default, with real Monero keys and a real paying RingCT transaction.
They run under the existing CI/daily property filter. Run `cargo nextest run
-p engine --lib --locked -E 'test(key_custody::plain::properties::) |
test(key_custody::router::properties::)'`.

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
