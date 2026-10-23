# Real stagenet end-to-end tests

These tests run Monokulo against a real public Monero **stagenet** node and
pay real (tiny, worthless) orders with transactions constructed, signed and
broadcast entirely in Rust by [`crates/cli-wallet`](../crates/cli-wallet),
from a faucet-funded wallet. Nothing is mocked but the shop, and no external
wallet process is needed: the only external dependency is the node.

| Test | What it proves |
|---|---|
| [`crates/e2e-harness/tests/e2e_stagenet.rs`](../crates/e2e-harness/tests/e2e_stagenet.rs) | The engine, driven in-process, detects a real payment to an order it made. |
| [`crates/e2e-harness/tests/e2e_dashboard_stagenet.rs`](../crates/e2e-harness/tests/e2e_dashboard_stagenet.rs) | A merchant who signs up and sets up a store sees the payment on their home page with the right total. |
| [`crates/mock-woocommerce/tests/e2e_stagenet_connect_flow.rs`](../crates/mock-woocommerce/tests/e2e_stagenet_connect_flow.rs) | A shop plugin connects, makes an order, and gets a signed `order.paid` webhook. |
| [`crates/mock-woocommerce/tests/e2e_stagenet_confirmation_threshold.rs`](../crates/mock-woocommerce/tests/e2e_stagenet_confirmation_threshold.rs) | An order is held to its store's confirmation threshold. |

This directory holds the fixtures they share: the wallets and the decoy
cache below. Sending a payment signs a CLSAG + Bulletproofs+ transaction from
the spender's own keys against outputs it already knows, with decoys from the
cached distribution, and broadcasts it over the node's plain RPC; see
`crates/cli-wallet/src/lib.rs`'s module doc for why.

Every test is `#[ignore]`d, so the default run stays hermetic, and named
`live_`, so the daily live-network run (`cargo xtask live`,
`.github/workflows/live-network.yml`) runs them with every other test that
needs the network. Run one by hand from the repository root:

```bash
cargo test -p e2e-harness --features e2e --test e2e_stagenet -- --ignored --nocapture
cargo test -p mock-woocommerce --features e2e --test e2e_stagenet_connect_flow -- --ignored --nocapture
```

## Infrastructure used

- **Node**: `node.monerodevs.org:38089` (public stagenet node), set in
  `crates/e2e-harness/tests/support/mod.rs`'s `e2e_fixture` constants (the
  mock-woocommerce tests have their own copy in `tests/support/mod.rs`).
- **Faucet**: https://stagenet-faucet.xmr-tw.org/ - funded the spender wallet
  below.
- **`wallets/<name>.db`**: one SQLite file per wallet, holding *everything* about
  it - keys and seed, `monero-wallet-cli`-style settings (accounts,
  subaddress labels, address book, description), and its own record of every
  output it has received, every transaction it has sent, and anything still
  waiting to confirm. Loaded through `cli-wallet` (never parsed by hand):
  - `merchant.db` - the store's wallet. Its full spend key is recorded
    (worthless stagenet XMR, and it lets `wallet-cli` sweep funds back to
    `spender`), but the tests only ever give Monokulo its view key and spend
    *public* key, so the store is watch-only as in production.
  - `spender.db` - an ordinary wallet that received faucet funds and is
    used to *send* test payments to orders. Never given to Monokulo - it
    plays the role of the customer.

  `crates/cli-wallet` is deliberately *not* a chain-scanning wallet: it
  trusts each wallet's file as the source of truth for what it owns, rather
  than re-deriving it from the chain on every run, and writes back to it
  after each successful send (marking the spent outputs spent, recording the
  send, and adding its change as pending). That write-back is expected -
  commit it. Changes happen under a `<name>.db.lock` file lock, so
  parallel runs can't lose each other's updates (the lock files are
  gitignored). The files are binary; see `crates/cli-wallet/README.md` for
  reading them and for readable `git diff`s.

  These files contain real (if worthless - stagenet has no exchange value)
  private keys. Treat them like any other credentials file.
- **`wallets/ci/`**: the daily live-network run's own `spender.db` and
  `merchant.db`, which nothing else spends from - see "The daily run's
  wallets" below.
- **`stagenet-decoy-distribution.json`**: a cached snapshot of the RingCT
  output distribution, refreshed periodically via `cli-wallet`'s own
  `refresh-decoy-pool` bin (see that crate's doc comment) rather than fetched
  live on every send - the main reason these tests are fast. Shared by every
  wallet: it's chain data, not wallet data.

## The daily run's wallets

The live-network workflow pays its orders from `wallets/ci/`, not from the
wallets above, so a developer's run and the daily one never pick the same
outputs at once (each takes the file lock, but they hold different copies of
the file). The suites and `wallet-cli` take the wallet directory from
`E2E_WALLET_DIR` (relative to the repository root), which the workflow sets:

```sh
E2E_WALLET_DIR=e2e/wallets/ci cargo run -p cli-wallet --bin wallet-cli -- balance
```

The merchant has 32 subaddresses made ahead, so `rescan` finds the payments
the tests' engines send to each order's subaddress (a fresh engine starts at
index 1).

The workflow can't commit the files it changes, so it keeps their newest
copy in the Actions cache and rescans the last 1500 blocks before each run
(`cargo xtask live wallets check`); with no cached copy it rescans from the
height the pair was made. After the tests, `cargo xtask live wallets keep`
sweeps what the merchant received back to the spender, splits the spender's
outputs when fewer than twelve can pay an order, and merges the ones too
small to. The pair only loses the fees, a few hundredths of a stagenet XMR a
month. Each run also starts by asking both stagenet faucets to pay the
spender (`cargo xtask live wallets faucet`, which runs `wallet-cli faucet`
for each); a faucet that won't pay doesn't fail the run. When the spender
can't pay a run all the same, the check fails with its address: send it
stagenet XMR from the faucet, and the next run's rescan finds it.

## Inspecting/driving a wallet by hand

`wallet-cli` speaks `monero-wallet-cli`'s commands over the same
wallet files the suites use. Open a wallet and type commands at its prompt,
as with the reference wallet:

```sh
cargo run -p cli-wallet --bin wallet-cli -- --wallet-file spender
[wallet 5648a3]: balance
[wallet 5648a3]: show_transfers
[wallet 5648a3]: transfer <address> 0.001
[wallet 5648a3]: exit
```

or run one command and exit:

```sh
cargo run -p cli-wallet --bin wallet-cli -- --wallet-file spender balance
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
`wallet-cli completions <bash|zsh|fish|...>`.

## Why the fixtures are tuned the way they are

- `PAYMENT_CONFIRMATIONS_REQUIRED = 0`: stagenet blocks land roughly every
  two minutes, so requiring confirmations would spend most of a run waiting
  on the chain instead of exercising detection. 0-conf settles the payment
  as soon as it is seen in the mempool.
- `e2e_stagenet.rs`'s order is 0.000335 XMR, a genuinely tiny payment, since
  these tests only move trivial amounts from the shared faucet-funded wallet.

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
cargo run -p cli-wallet --bin wallet-cli -- pocketchange
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

If `wallets/spender.db`'s outputs ever run dry (everything spent, and
change too small/young to help):

1. Ask a faucet to pay the spender: `cargo run -p cli-wallet --bin
   wallet-cli -- faucet` (or `faucet --provider cypherfaucet`). No need
   to generate a new wallet - the same address can receive any number of
   faucet payouts.
2. The command records the payout's txid itself. It stays pending until
   it confirms, then resolves on the next `refresh` (or send). By hand
   instead: open https://stagenet-faucet.xmr-tw.org/, send to the
   spender's address (`wallet-cli address`), then `wallet-cli --
   add_output <txid>`.
3. Once it's spendable (10 confirmations), turn the one big faucet output
   into many test-sized ones: `pocketchange` (see above). Repeat on the
   resulting outputs if one round isn't enough.


# Real Tor end-to-end tests

`crates/monokulo/tests/e2e_tor.rs` checks monokulo's onion service behind an
external `tor` process on the live Tor network, and
`crates/monokulo/tests/e2e_arti.rs` checks Tor inside monokulo (its module doc
says how to run it). Like the stagenet tests they are `#[ignore]`d by default.
The rest of this section is about `e2e_tor.rs`.

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
