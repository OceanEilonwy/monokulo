# Contributing

Monokulo builds on the latest nightly Rust (`rust-toolchain.toml`), Node 24
for the POS app, and CMake with a C++ compiler for RandomX.
[docs/RUNNING.md](docs/RUNNING.md#from-release-binaries-or-source) has the
full build steps.

## Running in development

Install the POS app's dependencies once (`cd crates/monokulo/pos-ui &&
npm ci`); a debug build of monokulo then embeds the
POS with Solid's development diagnostics, a release build the minified one.

`scripts/dev-run.sh` builds and runs both processes locally the same way
they run in production, with all state kept under `.dev-run/` (gitignored)
and a real stagenet Monero node pre-configured so payments actually work
end-to-end without touching mainnet funds. Each process's options file
(`.dev-run/engine/engine.toml`, `.dev-run/monokulo/monokulo.toml`) is
written with the dev values the first time only; delete one to get them
back.

```sh
scripts/dev-run.sh start            # build (debug) + start both processes
scripts/dev-run.sh start --no-build # restart without rebuilding
scripts/dev-run.sh stop
scripts/dev-run.sh restart [--no-build]
scripts/dev-run.sh status
scripts/dev-run.sh logs [engine|monokulo]   # tails both by default
```

- engine: `http://127.0.0.1:8080`
- monokulo: `http://127.0.0.1:8081` (open this one - first visit redirects to
  its own first-run admin setup wizard)

The dev engine token and `MONOKULO_ENCRYPTION_KEY` are generated once
and persisted under `.dev-run/`, so they're reused across restarts.

## Using the CLI wallet during development

`crates/cli-wallet` is a fast, stagenet-only test wallet used to fund and pay
real dev/test orders without a full wallet-rpc process. It ships two `[[bin]]`
targets under the `cli-wallet` package:

```sh
# monero-wallet-cli's commands over one SQLite file per wallet (e2e/wallets/).
# With no command it opens the wallet and prompts, like the reference wallet:
cargo run -p cli-wallet --bin wallet-cli -- --wallet-file spender
#   [wallet 5648a3]: balance
#   [wallet 5648a3]: transfer <address> 0.001      (amounts in XMR)
#   [wallet 5648a3]: show_transfers
#   [wallet 5648a3]: help

# ...or runs one command and exits
cargo run -p cli-wallet --bin wallet-cli -- --wallet-file spender balance

# Split the largest output into 16 equal outputs (the most one transaction
# holds), so the e2e tests have enough mature outputs to run fast - see
# e2e/README.md "Keeping enough outputs"
cargo run -p cli-wallet --bin wallet-cli -- pocketchange

# Record a payment by hand (e.g. after a faucet payment)
cargo run -p cli-wallet --bin wallet-cli -- add_output <txid>

# New wallet file: fresh keys, or restored from a seed phrase
cargo run -p cli-wallet --bin wallet-cli -- --generate-new-wallet <name>
cargo run -p cli-wallet --bin wallet-cli -- --generate-new-wallet <name> --restore-deterministic-wallet --electrum-seed "<phrase>"

# Shell completions
cargo run -p cli-wallet --bin wallet-cli -- completions <bash|zsh|fish|...>

# Refresh the committed decoy-selection cache (rarely needed - see that
# bin's own doc comment)
cargo run -p cli-wallet --bin refresh-decoy-pool -- <node_url> <from_height> <to_height> <out_path>
```

See [`e2e/README.md`](e2e/README.md) for the full real-stagenet end-to-end
test setup this wallet backs, and
[`crates/cli-wallet/src/lib.rs`](crates/cli-wallet/src/lib.rs)'s module doc
comment for why this wallet is deliberately narrower than a general-purpose
Monero wallet.

## Coverage and UI evidence

Run `cargo xtask coverage all` for the deterministic Rust, browser, and
WooCommerce reports, then `cargo xtask coverage open` to browse the local
artifact. See [the coverage guide](docs/COVERAGE.md) for prerequisites,
per-component commands, screenshots, CI artifacts, and the separate paid
stagenet profile.
