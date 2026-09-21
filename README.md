# Monokulo

A self-hosted Monero payment processor: `scanner` (the engine - chain scanning,
tenants, webhooks, admin API) and `monokulo` (the control plane - the merchant
dashboard and checkout-facing HTTP surface) run as two separate processes,
`monokulo` talking to `scanner` over its own admin API. There is no config
file - every runtime setting lives in the engine's `settings` table, read/
written over its instance-admin HTTP API (`/api/v1/admin/settings`).

## Running in production

1. Build release binaries from the repository root:

   ```sh
   cargo build --release -p scanner --bin scanner -p monokulo --bin monokulo
   ```

2. Start the engine first. It needs a writable path for its SQLite database
   and a bind address; everything else (Monero node endpoints, confirmation/
   expiry thresholds, rate limits, webhook policy) is configured afterward
   through its admin API, not at boot:

   ```sh
   SCANNER_DB_PATH=/var/lib/monokulo/scanner.db \
   SCANNER_SERVER_BIND=0.0.0.0:8080 \
       ./target/release/scanner
   ```

   On first boot with no admin token yet configured, it prints a generated
   instance-admin token once - save it (or set `SCANNER_ADMIN_TOKEN`
   explicitly to control it yourself). Use that token to configure the
   Monero node(s) and payment thresholds via `POST /api/v1/admin/settings`,
   and to provision each merchant tenant.

3. Start the control plane, pointed at the engine:

   ```sh
   MONOKULO_ENCRYPTION_KEY=<64 hex chars, 32 bytes> \
   MONOKULO_ENGINE_URL=http://<engine-host>:8080 \
   MONOKULO_SCANNER_ADMIN_TOKEN=<the engine's instance-admin token> \
       ./target/release/monokulo
   ```

   `MONOKULO_ENCRYPTION_KEY` must be 64 hex characters decoding to exactly 32
   bytes - generate one with `openssl rand -hex 32` and keep it, since it's
   what encrypts data at rest in `monokulo`'s own database. `monokulo` opens
   its SQLite database at the relative path `monokulo.db`, so run it from a
   writable working directory dedicated to it.

4. Open `monokulo` in a browser. The first visit redirects to a first-run
   admin setup wizard to create the one admin account; from there its admin
   settings page already has the engine connection pre-wired.

Run both processes under whatever supervisor you normally use (systemd,
etc.) - each is a single long-running binary with no daemonization of its
own.

## Running in development

`scripts/dev-run.sh` builds and runs both processes locally the same way
they run in production, with all state kept under `.dev-run/` (gitignored)
and a real stagenet Monero node pre-configured so payments actually work
end-to-end without touching mainnet funds.

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

The dev instance-admin token and `MONOKULO_ENCRYPTION_KEY` are generated once
and persisted under `.dev-run/`, so they're reused across restarts.

## Using the CLI wallet during development

`crates/cli-wallet` is a fast, stagenet-only test wallet used to fund and pay
real dev/test orders without a full wallet-rpc process. It ships two `[[bin]]`
targets under the `cli-wallet` package:

```sh
# General wallet CLI - inspect/manage wallets, check balance, send payments
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --help

cargo run -p cli-wallet --bin stagenet-wallet-cli -- address
cargo run -p cli-wallet --bin stagenet-wallet-cli -- balance

# Send a stagenet test payment (piconero, i.e. 1 XMR = 1e12 piconero)
cargo run -p cli-wallet --bin stagenet-wallet-cli -- send <address> <piconero>

# Split existing outputs into more (smaller) outputs, useful for tests that
# need several independent spendable outputs
cargo run -p cli-wallet --bin stagenet-wallet-cli -- split 4
cargo run -p cli-wallet --bin stagenet-wallet-cli -- send <address> <piconero> --split 3

# Record a new output by hand (e.g. after a faucet payment) instead of
# waiting for it to be discovered
cargo run -p cli-wallet --bin stagenet-wallet-cli -- output add <txid>

# Add another named wallet from an existing seed phrase
cargo run -p cli-wallet --bin stagenet-wallet-cli -- wallet add <name> --seed "<phrase>"

# Shell completions
cargo run -p cli-wallet --bin stagenet-wallet-cli -- completions <bash|zsh|fish|...>

# Refresh the committed decoy-selection cache (rarely needed - see that
# bin's own doc comment)
cargo run -p cli-wallet --bin refresh-decoy-pool -- <node_url> <from_height> <to_height> <out_path>
```

See [`e2e/README.md`](e2e/README.md) for the full real-stagenet end-to-end
test setup this wallet backs, and
[`crates/cli-wallet/src/lib.rs`](crates/cli-wallet/src/lib.rs)'s module doc
comment for why this wallet is deliberately narrower than a general-purpose
Monero wallet.
