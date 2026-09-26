# Real stagenet end-to-end test

The actual test lives in Rust: [`../crates/scanner/tests/e2e_stagenet.rs`](../crates/scanner/tests/e2e_stagenet.rs),
run via `cargo test` like any other test in this crate. It drives the real
`scanner` library - config, store, key custody, scanner, router; the same
pieces `main.rs` wires together - against a real public Monero **stagenet** node,
and pays the order it creates with a real (tiny) transaction constructed, signed,
and broadcast entirely in Rust by [`crates/cli-wallet`](../crates/cli-wallet),
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
spender wallet's own private keys against outputs already known to be ours,
selecting decoys from a cached distribution snapshot, and broadcasting it over
the node's plain RPC - see `crates/cli-wallet/src/lib.rs`'s own
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
- **Faucet**: https://stagenet-faucet.xmr-tw.org/ - funded the spender wallet
  below.
- **`stagenet-wallets.json`**: persists the keys for every named wallet the
  suites need, loaded through `cli-wallet::WalletStore` (never
  parsed by hand anymore - see below):
  - `merchant` - moneropay's own tenant, bootstrapped into
    `moneropay-stagenet.toml` with its view key + spend *public* key only
    (never the private spend key). Its full spend key is recorded too, like
    every other fixture here (there's no reason to withhold it - worthless
    stagenet XMR, and full recoverability/CLI use is strictly more useful,
    e.g. sweeping funds back to `spender`) - but only the derived public key
    ever goes to moneropay's real connect API
    (`ResolvedWallet::spend_public_key_hex`), so the e2e tests still exercise
    it exactly as a genuinely watch-only tenant would be.
  - `spender` - an ordinary wallet that received faucet funds and is used to
    *send* test payments to orders, via its private spend/view keys. Never
    given to moneropay - it plays the role of "the person paying an invoice."
    (Renamed from `customer` once `cli-wallet` grew a
    general-purpose wallet store/CLI rather than remaining this one suite's
    private fixture.)

  This file contains real (if worthless - stagenet has no exchange value)
  private keys. Treat it like any other credentials file.
- **`stagenet-known-outputs.json`**: the spender wallet's ledger - every
  output it's ever known to control (original faucet payouts, plus every test
  run's own change output), each with its spent/unspent status and, once
  resolved, its height and raw serialized bytes. `crates/cli-wallet`
  is deliberately *not* a chain-scanning wallet: it trusts this file as the
  source of truth for what it owns, rather than re-deriving it from the chain
  on every run, and writes back to it after each successful send (marking the
  spent output spent, and adding a new pending entry for the change output).
  That write-back is expected - commit it.
- **`stagenet-decoy-distribution.json`**: a cached snapshot of the RingCT
  output distribution, refreshed periodically via `cli-wallet`'s own
  `refresh-decoy-pool` bin (see that crate's doc comment) rather than fetched
  live on every send - the main reason these tests are fast.

## Inspecting/driving the spender wallet by hand

`cli-wallet` ships a general CLI over the same `WalletStore`/
`StagenetTestWallet` the suites use as a library - useful for checking on the
fixture between runs or topping up the pool of spendable outputs, without
writing a one-off script:

```sh
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --help

# check what's there
cargo run -p cli-wallet --bin stagenet-wallet-cli -- address
cargo run -p cli-wallet --bin stagenet-wallet-cli -- balance

# a real, tiny stagenet payment
cargo run -p cli-wallet --bin stagenet-wallet-cli -- send <address> <piconero>

# split the spendable balance into 4 smaller, independently-aged outputs -
# run this ahead of a test session (each piece still needs its own
# SPENDABLE_AGE confirmations, ~20 minutes, before it matures), not inline
# in CI
cargo run -p cli-wallet --bin stagenet-wallet-cli -- split 4

# a real payment that also splits its own change into pieces, so ordinary
# test traffic keeps the pool topped up for free
cargo run -p cli-wallet --bin stagenet-wallet-cli -- send <address> <piconero> --split 3

# record an output this wallet received but didn't send itself (e.g. a
# fresh faucet payout)
cargo run -p cli-wallet --bin stagenet-wallet-cli -- output add <txid>

# import a wallet from a real seed phrase (16-word Polyseed or 24/25-word
# legacy Electrum-style) under a new name
cargo run -p cli-wallet --bin stagenet-wallet-cli -- wallet add <name> --seed "<phrase>"
```

Every subcommand defaults to acting as the `spender` wallet against the
standard `e2e/*` fixture paths above (`--wallet`/`--wallets-path`/
`--ledger-path`/`--decoy-distribution-path`/`--node-url` override any of
that). Shell completions: `stagenet-wallet-cli completions <bash|zsh|fish|...>`.

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
transaction sent from the spender wallet, then drives the real scanner
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
# e.g. by calling crates/cli-wallet::send_payment directly
```

`moneropay.db*` (created alongside the config) is that server's tenant/order
database; delete it to start over with a fresh bootstrap.

## Why the config is tuned the way it is

- `confirmations_required = 0`: stagenet
  blocks land roughly every ~2 minutes, so requiring several confirmations
  would make the test spend most of its wall-clock time waiting on the chain
  instead of exercising moneropay's own detection logic. Native 0-conf
  resolves the payment as soon as it is seen in the mempool.
- The test order's `xmr_amount_piconero` (335_000_000, i.e. 0.000335 XMR) is
  chosen to be a genuinely tiny real payment, per the project's own constraint
  of only moving trivial amounts on this shared faucet-funded wallet. The
  engine itself has no concept of fiat at all (`docs/fx_refactor.md`) - the
  monokulo owns fiat pricing in production; this test talks to the
  engine's own XMR-only API directly.

## A note on running the test repeatedly

Monero requires 10 confirmations (~20 minutes on stagenet) before a received or
change output becomes spendable. `cli-wallet`'s own `send` already
accounts for this (it filters the ledger by age, not just spent-status) and
greedily picks only as many outputs as needed - so as long as *some* ledger
entry is old enough and unspent, a run succeeds without help. If every known
output is either too young or already spent, the send fails fast with a clear
`InsufficientFunds` error, rather than hanging or false-passing.

## Reproducing from scratch (new faucet funds)

If `stagenet-known-outputs.json`'s tracked outputs ever run dry (everything
spent, and change too small/young to help):

1. Open https://stagenet-faucet.xmr-tw.org/ and send funds to
   `stagenet-wallets.json`'s existing `spender.address` (no need to generate a
   new wallet - the same address can receive any number of faucet payouts).
2. Record the faucet's txid: `cargo run -p cli-wallet --bin
   stagenet-wallet-cli -- output add <txid>` (or add the entry by hand -
   `txid`, `amount_piconero`, `spent: false`, `height`/`serialized_output_hex`
   left `null` until the next run resolves them - see `Ledger`'s own doc
   comment in `crates/cli-wallet/src/lib.rs`).


# Real Tor end-to-end test

`crates/monokulo/tests/e2e_tor.rs` checks monokulo's Tor support against a real
`tor` process and the live Tor network. Like the stagenet tests it is
`#[ignore]`d by default.

## Requirements

- `tor` 0.4.8 or newer on `PATH`, built with the proof-of-work module:
  `tor --list-modules` must show `pow: yes`.
- Outbound network access to the Tor network. No root, no system tor, no
  torrc of your own: the test writes a temporary one.

## Running it

```sh
cargo test -p monokulo --test e2e_tor -- --ignored --nocapture
```

It takes about five minutes, mostly waiting for tor to bootstrap and for the
fresh onion service's descriptor to become reachable (each wait has a timeout
of several minutes and fails with a clear message).

tor's `DataDirectory` (its cached network consensus and relay descriptors) is
kept between runs in `target/tmp/e2e-tor-data`, the way a real tor client
keeps it, so later runs bootstrap in seconds. The very first run has to
download all of it, which on a slow day can take longer than the bootstrap
timeout. If that happens, run the test again. The onion service itself (keys
and address) is still new every run. A new circuit to the onion service can
also take a while to build, so each visitor retries its SOCKS connection for
up to five minutes. A failed attempt never reaches monokulo, so retries can't
change the counts the test checks.

## What it does

1. Starts a test engine and monokulo in-process, with one store and one order,
   and monokulo's onion listener on a loopback port. Small limits (soft 5,
   hard 12, stream cap 3) keep the number of requests over Tor low.
2. Starts `tor` with the cached `DataDirectory` above and a fresh onion
   service directory, using the service lines of
   `deploy/tor/torrc.snippet` verbatim (only the directory and target port
   are rewritten), plus a `SocksPort` and a cookie-authenticated
   `ControlPort`. The tor log is in the printed temporary directory.
3. Waits for `status/bootstrap-phase` `PROGRESS=100`, then checks
   `GETCONF HiddenServiceOptions` shows `HiddenServiceExportCircuitID=haproxy`,
   `HiddenServicePoWDefensesEnabled=1`, the intro-DoS defence and the stream
   limits.
4. Connects to the `.onion` through the `SocksPort` as four visitors, each with
   its own SOCKS username/password (so tor's `IsolateSOCKSAuth` gives each its
   own circuit), and checks: one distinct circuit identity per visitor; visitor
   A is challenged past the soft limit while B is not; A's solved proof is
   accepted; A alone gets `429` + `Retry-After` past the hard limit; visitor C
   can hold 3 live-update streams and the 4th gets `429` while D can still open
   one.

The fast, default-run counterpart with synthetic PROXY headers is
`crates/monokulo/tests/onion_listener.rs`.
