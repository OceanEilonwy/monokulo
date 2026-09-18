# Real stagenet end-to-end test

The actual test lives in Rust: [`../crates/scanner/tests/e2e_stagenet.rs`](../crates/scanner/tests/e2e_stagenet.rs),
run via `cargo test` like any other test in this crate. It drives the real
`scanner` library - config, store, key custody, scanner, router; the same
pieces `main.rs` wires together - against a real public Monero **stagenet** node,
and pays the order it creates with a real (tiny) transaction constructed, signed,
and broadcast entirely in Rust by [`crates/stagenet-test-wallet`](../crates/stagenet-test-wallet),
from a wallet that was funded by a public stagenet faucet. Nothing here is mocked,
and nothing here needs an external wallet process - the point is to prove the
actual scanning, detection, and status-update logic works against a real chain,
not just a simulated one. This directory holds the fixtures every real e2e
suite in the repo needs (this test, `e2e_dashboard_stagenet.rs`, both
`mock-woocommerce` real-stagenet tests, and the POS screen's own
`e2e/pos-playwright/` suite), plus a demo shop for manually eyeballing the same
flow through the actual embedded widget in a browser.

**The only external dependency is the public stagenet node itself** - no
wallet-rpc, no `monero-wallet-cli`, no other process. Sending a test payment
happens by directly signing a real CLSAG + Bulletproofs+ transaction from the
customer wallet's own private keys against outputs already known to be ours,
selecting decoys from a cached distribution snapshot, and broadcasting it over
the node's plain RPC - see `crates/stagenet-test-wallet/src/lib.rs`'s own
module doc comment for the full explanation (why no chain scanning, why decoys
come from a cache, why it's safe to trust that path's randomness/cryptography).

Following the same pattern as `daemon_rpc::live_node_tests` (`src/daemon_rpc.rs`),
the test is `#[ignore]`d so the default `cargo test` run stays hermetic and fast -
run it explicitly, from the repository root:

```bash
cargo test --test e2e_stagenet -- --ignored --nocapture
```

Decoy selection is served from the committed cache below rather than fetched live,
so a full run is fast (seconds, not minutes) and doesn't depend on a large
`get_output_distribution` fetch succeeding against a possibly-slow public node.

## Infrastructure used

- **Node**: `node.monerodevs.org:38089` (public stagenet node) - configured in
  `moneropay-stagenet.toml`'s `[monero_node.stagenet]` and in
  `crates/scanner/tests/support/mod.rs`'s `e2e_fixture` constants.
- **Faucet**: https://stagenet-faucet.xmr-tw.org/ - funded the customer wallet
  below.
- **`stagenet-wallets.json`**: persists the keys for two wallets so the whole
  setup is reproducible without re-funding from the faucet every time:
  - `merchant` - the tenant's watch-only wallet, bootstrapped into
    `moneropay-stagenet.toml`. moneropay only ever needs its view key + spend
    public key (never the spend key), so that's all that's configured there.
  - `customer` - an ordinary wallet that received faucet funds and is used to
    *send* test payments to orders, via `private_spend_key`/`private_view_key`
    read directly by the e2e tests. Never given to moneropay - it plays the
    role of "the person paying an invoice."

  This file contains real (if worthless - stagenet has no exchange value)
  private keys. Treat it like any other credentials file.
- **`stagenet-known-outputs.json`**: the customer wallet's ledger - every
  output it's ever known to control (original faucet payouts, plus every test
  run's own change output), each with its spent/unspent status and, once
  resolved, its height and raw serialized bytes. `crates/stagenet-test-wallet`
  is deliberately *not* a chain-scanning wallet: it trusts this file as the
  source of truth for what it owns, rather than re-deriving it from the chain
  on every run, and writes back to it after each successful send (marking the
  spent output spent, and adding a new pending entry for the change output).
  That write-back is expected - commit it.
- **`stagenet-decoy-distribution.json`**: a cached snapshot of the RingCT
  output distribution, refreshed periodically via `stagenet-test-wallet`'s own
  `refresh-decoy-pool` bin (see that crate's doc comment) rather than fetched
  live on every send - the main reason these tests are fast.

## One-time setup

Just `scanner` built:

```bash
cargo build --manifest-path ../Cargo.toml
```

## Running the test

```bash
cargo test --test e2e_stagenet -- --ignored --nocapture
```

This builds the scanner router in-process straight from `moneropay-stagenet.toml`
(via `tower::ServiceExt::oneshot` - no bound port, no separate `scanner`
process needed), creates a real order against it, pays that order with a real
transaction sent from the customer wallet, then drives the real scanner
(`run_scan_tick`, the same function `main.rs`'s production loop calls on a timer)
and polls the order's status until it reports `paid` (or a terminal
confirming/overpaid state) - failing the test otherwise.

**(Optional) Watch it happen in a browser too.** The test above only proves the
library logic; to see the same flow through the actual server process and
embedded widget, start the server and demo shop separately:

```bash
# from e2e/, in one terminal:
../target/debug/scanner moneropay-stagenet.toml
# prints a pk_... on first boot - note it

# in another terminal:
cd demo-shop && python3 -m http.server 8190
# then open http://127.0.0.1:8190/?endpoint=http://127.0.0.1:8180&pk=pk_...
# and click "Buy with Monero" - paying that order needs a separate real transfer,
# e.g. by calling crates/stagenet-test-wallet::send_payment directly
```

`moneropay.db*` (created alongside the config) is that server's tenant/order
database; delete it to start over with a fresh bootstrap.

## Why the config is tuned the way it is

- `confirmations_required = 1` and `zero_conf_max_xmr = "0.01"`: stagenet
  blocks land roughly every ~2 minutes, so requiring several confirmations
  would make the test spend most of its wall-clock time waiting on the chain
  instead of exercising moneropay's own detection logic. Every test payment
  here is well under 0.01 XMR, so it's covered entirely by 0-conf detection and
  resolves in seconds once broadcast. (This field is XMR-denominated, not
  fiat, despite the name it used to have - a real bug caught in the production
  config, see `docs/DESIGN.md` §13.)
- The test order's `xmr_amount_piconero` (335_000_000, i.e. 0.000335 XMR) is
  chosen to be a genuinely tiny real payment, per the project's own constraint
  of only moving trivial amounts on this shared faucet-funded wallet. The
  engine itself has no concept of fiat at all (`docs/fx_refactor.md`) - the
  monokulo owns fiat pricing in production; this test talks to the
  engine's own XMR-only API directly.

## A note on running the test repeatedly

Monero requires 10 confirmations (~20 minutes on stagenet) before a received or
change output becomes spendable. `stagenet-test-wallet`'s own `send` already
accounts for this (it filters the ledger by age, not just spent-status) and
greedily picks only as many outputs as needed - so as long as *some* ledger
entry is old enough and unspent, a run succeeds without help. If every known
output is either too young or already spent, the send fails fast with a clear
`InsufficientFunds` error, rather than hanging or false-passing.

## Reproducing from scratch (new faucet funds)

If `stagenet-known-outputs.json`'s tracked outputs ever run dry (everything
spent, and change too small/young to help):

1. Open https://stagenet-faucet.xmr-tw.org/ and send funds to
   `stagenet-wallets.json`'s existing `customer.address` (no need to generate a
   new wallet - the same address can receive any number of faucet payouts).
2. Add a new entry for the faucet's txid to `stagenet-known-outputs.json`
   (`txid`, `amount_piconero`, `spent: false`, `height`/`serialized_output_hex`
   left `null` until the next run resolves them - see `Ledger`'s own doc
   comment in `crates/stagenet-test-wallet/src/lib.rs`).
