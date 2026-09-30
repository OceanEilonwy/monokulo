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
- **`wallets/<name>.json`**: one file per wallet, holding *everything* about
  it - keys and seed, `monero-wallet-cli`-style settings (accounts,
  subaddress labels, address book, description), and its own record of every
  output it has received, every transaction it has sent, and anything still
  waiting to confirm. Loaded through `cli-wallet` (never parsed by hand):
  - `merchant.json` - moneropay's own tenant, bootstrapped into
    `moneropay-stagenet.toml` with its view key + spend *public* key only
    (never the private spend key). Its full spend key is recorded too, like
    every other fixture here (there's no reason to withhold it - worthless
    stagenet XMR, and full recoverability/CLI use is strictly more useful,
    e.g. sweeping funds back to `spender`) - but only the derived public key
    ever goes to moneropay's real connect API
    (`ResolvedWallet::spend_public_key_hex`), so the e2e tests still exercise
    it exactly as a genuinely watch-only tenant would be.
  - `spender.json` - an ordinary wallet that received faucet funds and is
    used to *send* test payments to orders. Never given to moneropay - it
    plays the role of "the person paying an invoice."

  `crates/cli-wallet` is deliberately *not* a chain-scanning wallet: it
  trusts each wallet's file as the source of truth for what it owns, rather
  than re-deriving it from the chain on every run, and writes back to it
  after each successful send (marking the spent outputs spent, recording the
  send, and adding its change as pending). That write-back is expected -
  commit it. Changes happen under a `<name>.json.lock` file lock, so
  parallel runs can't lose each other's updates (the lock files are
  gitignored).

  These files contain real (if worthless - stagenet has no exchange value)
  private keys. Treat them like any other credentials file.
- **`stagenet-decoy-distribution.json`**: a cached snapshot of the RingCT
  output distribution, refreshed periodically via `cli-wallet`'s own
  `refresh-decoy-pool` bin (see that crate's doc comment) rather than fetched
  live on every send - the main reason these tests are fast. Shared by every
  wallet: it's chain data, not wallet data.

## Inspecting/driving a wallet by hand

`stagenet-wallet-cli` speaks `monero-wallet-cli`'s commands over the same
wallet files the suites use. Open a wallet and type commands at its prompt,
as with the reference wallet:

```sh
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --wallet-file spender
[wallet 5648a3]: balance
[wallet 5648a3]: show_transfers
[wallet 5648a3]: transfer <address> 0.001
[wallet 5648a3]: exit
```

or run one command and exit:

```sh
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --wallet-file spender balance
```

Amounts are in XMR, as in the reference wallet (`set unit` changes that).
`help` lists every command, `help <command>` shows one; the full list of
supported reference commands, and why the rest aren't, is in
[`crates/cli-wallet/README.md`](../crates/cli-wallet/README.md). Two
commands aren't in the reference wallet:

```sh
# split the largest spendable output into 16 equal outputs - see "Keeping
# enough outputs" below
[wallet 5648a3]: pocketchange

# record a payment this wallet received but didn't send itself (e.g. a
# fresh faucet payout); it resolves once it confirms
[wallet 5648a3]: add_output <txid>
```

New wallets: `--generate-new-wallet <name>` (fresh keys), plus
`--restore-deterministic-wallet [--electrum-seed "<phrase>"]` to restore a
16-word Polyseed or 25-word seed, or `--generate-from-spend-key <name>`.
`--wallet-file` defaults to `spender`; `--daemon-address` picks another
node, `--do-not-relay` signs without broadcasting. Shell completions:
`stagenet-wallet-cli completions <bash|zsh|fish|...>`.

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
accounts for this (it filters outputs by age, not just spent-status) and
greedily picks only as many outputs as needed - so as long as *some* output
is old enough and unspent, a run succeeds without help. If every known
output is either too young or already spent, the send fails fast with a clear
`InsufficientFunds` error, rather than hanging or false-passing.

### Keeping enough outputs: `pocketchange`

Every test payment spends one output and leaves its change locked for the
next 10 blocks, so the suites run fast only while the spender has plenty of
separate mature outputs - one per payment a session makes, at least.
`pocketchange` makes them: it splits the wallet's largest unlocked output
into 16 equal outputs of its own, the most one transaction can hold (the
change output is one of the 16):

```sh
cargo run -p cli-wallet --bin stagenet-wallet-cli -- pocketchange
# Splitting 0.006250000000 from 1 output(s) into 16 outputs of 0.000382893750 each ...
```

- `pocketchange 8` splits into fewer, bigger pieces (2 to 16).
- `pocketchange inputs=3` merges the 3 largest outputs first, for bigger
  pieces from smaller outputs.
- Each piece has to cover one test payment plus its fee - about 0.00037 XMR
  for `e2e_stagenet.rs`'s 0.000335 XMR order - or it can't pay a test on its
  own. The confirmation line shows the piece size before anything is sent;
  merge more inputs, or split into fewer pieces, if it's too small.
- Run it ahead of a test session, not inline in CI: the new outputs need
  their own 10 confirmations (~20 minutes) before they're spendable.
- Check what's there with `unspent_outputs` (sizes and a height histogram)
  or `balance detail` (how many outputs).

## Reproducing from scratch (new faucet funds)

If `wallets/spender.json`'s outputs ever run dry (everything spent, and
change too small/young to help):

1. Open https://stagenet-faucet.xmr-tw.org/ and send funds to the spender's
   address (`stagenet-wallet-cli address`; no need to generate a new wallet -
   the same address can receive any number of faucet payouts).
2. Record the faucet's txid: `cargo run -p cli-wallet --bin
   stagenet-wallet-cli -- add_output <txid>`. It stays pending until it
   confirms, then resolves on the next `refresh` (or send).
3. Once it's spendable (10 confirmations), turn the one big faucet output
   into many test-sized ones: `pocketchange` (see above). Repeat on the
   resulting outputs if one round isn't enough.


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
