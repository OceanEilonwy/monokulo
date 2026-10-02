# Monokulo

A self-hosted Monero payment processor: the engine (`monokulo-engine`: chain
scanning, stores, webhooks and its admin API) and `monokulo` (the control plane:
the merchant dashboard and checkout-facing HTTP surface) run as two separate
processes, `monokulo` talking to the engine over the engine's admin API, which
answers only requests carrying the engine token the two share. There is no config
file - every runtime setting lives in the engine's `settings` table, read/
written over its instance-admin HTTP API (`/api/v1/admin/settings`) or the
admin settings page in monokulo (one tab per job: General, Monero nodes,
Payments, Key custody, Abuse protection, Server, Logging), and applies to
the running engine as soon as it's saved (only the listen address and
worker threads need a restart). Key custody is chosen per store: the admin enables backends and a
default, and each store can move by entering its keys again.

## Running in production

### With Docker

The `Dockerfile` builds one image with both binaries (and
`key-custody-server`); `compose.yaml` runs the engine and monokulo from it
as two containers, publishing only monokulo on port 8081. Put the two
secrets in a `.env` file beside it. `ENGINE_TOKEN` is the engine
token: `compose.yaml` gives it to the engine and, as
`MONOKULO_ENGINE_TOKEN`, to monokulo, and neither starts without it
(see step 2 below):

```sh
printf 'MONOKULO_ENCRYPTION_KEY=%s\nENGINE_TOKEN=%s\n' "$(openssl rand -hex 32)" "$(openssl rand -hex 32)" > .env
docker compose up -d
```

then open http://localhost:8081 and continue from step 5 below. Each
version tag's image is also on `ghcr.io/oceaneilonwy/monokulo`. Keep
`MONOKULO_ENCRYPTION_KEY`: it encrypts monokulo's data at rest. The
databases live in the `engine-data` and `monokulo-data` volumes.

### From release binaries or source

Each version tag's GitHub release has `monokulo-engine`, `monokulo` and
`key-custody-server` for Linux (x86_64, aarch64) and macOS (arm64); CI's
`publish` jobs also keep them for every push to main. With those, skip
step 1.

1. Build release binaries from the repository root. The project builds on
   the latest nightly Rust (`rust-toolchain.toml`; rustup installs it on
   first use, and `rustup update nightly` keeps it current). Building
   monokulo also builds its POS app, so it needs Node 24 or later and the
   app's dependencies, installed once from the lockfile:

   ```sh
   (cd crates/monokulo/pos-ui && npm ci)
   cargo build --release -p engine --bin monokulo-engine -p monokulo --bin monokulo
   ```

2. Generate the engine token, a secret the engine and monokulo
   share. The engine refuses every request that doesn't carry it, so only
   monokulo can use the engine, and neither process starts without it
   (at least 32 characters):

   ```sh
   openssl rand -hex 32
   ```

   Keep it with your other secrets. To change it, set the new value on both
   processes and restart both.

3. Start the engine. It needs the token, a writable path for its SQLite
   database and a bind address. Everything else (Monero node endpoints,
   confirmation/expiry thresholds, rate limits, webhook policy) is set
   afterward on monokulo's admin settings page:

   ```sh
   ENGINE_TOKEN=<the engine token> \
   ENGINE_DB_PATH=/var/lib/monokulo/engine.db \
   ENGINE_SERVER_BIND=127.0.0.1:8080 \
       ./target/release/monokulo-engine
   ```

   Bind it where only monokulo can reach it (the same machine, or a private
   network): the token keeps everything else out, but nothing outside
   monokulo has a reason to reach the engine at all.

4. Start the control plane, pointed at the engine, with the same token:

   ```sh
   MONOKULO_ENCRYPTION_KEY=<64 hex chars, 32 bytes> \
   MONOKULO_ENGINE_URL=http://127.0.0.1:8080 \
   MONOKULO_ENGINE_TOKEN=<the engine token> \
       ./target/release/monokulo
   ```

   `MONOKULO_ENGINE_URL` is where monokulo reaches the engine (default
   `http://127.0.0.1:8443`, the engine's default bind). Both it and
   `MONOKULO_ENGINE_TOKEN` are read only when monokulo starts: the
   admin settings page shows them locked, and changing either means
   changing them where monokulo is started and restarting.

   `MONOKULO_ENCRYPTION_KEY` must be 64 hex characters decoding to exactly 32
   bytes - generate one with `openssl rand -hex 32` and keep it, since it's
   what encrypts data at rest in `monokulo`'s own database. `monokulo` opens
   its SQLite database at `monokulo.db` in the working directory unless
   `MONOKULO_DB_PATH` says otherwise, and listens on `127.0.0.1:8081` unless
   `MONOKULO_BIND` does.

5. Open `monokulo` in a browser. The first visit redirects to a first-run
   admin setup wizard to create the one admin account. Its admin settings
   page then manages both processes: the Monero nodes tab has a row per
   node, and the other tabs hold payment thresholds, rate limits and the
   rest.

Run both processes under whatever supervisor you normally use (systemd,
etc.) - each is a single long-running binary with no daemonization of its
own.

### Settings

Every setting of either process can be given three ways: as a
command-line option, as an environment variable, or on the admin settings
page. An option wins over the environment variable, which wins over the
saved value, which wins over the default. `monokulo-engine --help` and
`monokulo --help` list every option with its environment variable, its
default and what it does; `-h` gives the short version. The option is
the setting's key with `-` for `.` and `_`: `payment.reorg_check_depth`
is `--payment-reorg-check-depth` and `ENGINE_PAYMENT_REORG_CHECK_DEPTH`.

A few settings can't be saved on the admin page, only given at start:
each database path, the engine token, monokulo's encryption key and
engine URL, and the log format. The page shows them locked. Prefer the
environment variable for a secret (a token or key): an option is visible
to other users of the machine in its process list.

An invalid value from the environment or the database is logged as a
warning when the process starts, and the setting's default is used. An
invalid option stops the process with a message saying what the option
takes. A missing engine token or encryption key stops it too.

## Running in development

Install the POS app's dependencies once (`cd crates/monokulo/pos-ui &&
npm ci`); a debug build of monokulo then embeds the
POS with Solid's development diagnostics, a release build the minified one.

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

The dev engine token and `MONOKULO_ENCRYPTION_KEY` are generated once
and persisted under `.dev-run/`, so they're reused across restarts.

## Using the CLI wallet during development

`crates/cli-wallet` is a fast, stagenet-only test wallet used to fund and pay
real dev/test orders without a full wallet-rpc process. It ships two `[[bin]]`
targets under the `cli-wallet` package:

```sh
# monero-wallet-cli's commands over one JSON file per wallet (e2e/wallets/).
# With no command it opens the wallet and prompts, like the reference wallet:
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --wallet-file spender
#   [wallet 5648a3]: balance
#   [wallet 5648a3]: transfer <address> 0.001      (amounts in XMR)
#   [wallet 5648a3]: show_transfers
#   [wallet 5648a3]: help

# ...or runs one command and exits
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --wallet-file spender balance

# Split the largest output into 16 equal outputs (the most one transaction
# holds), so the e2e tests have enough mature outputs to run fast - see
# e2e/README.md "Keeping enough outputs"
cargo run -p cli-wallet --bin stagenet-wallet-cli -- pocketchange

# Record a payment by hand (e.g. after a faucet payment)
cargo run -p cli-wallet --bin stagenet-wallet-cli -- add_output <txid>

# New wallet file: fresh keys, or restored from a seed phrase
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --generate-new-wallet <name>
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --generate-new-wallet <name> --restore-deterministic-wallet --electrum-seed "<phrase>"

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

## Coverage and UI evidence

Run `cargo xtask coverage all` for the deterministic Rust, browser, and
WooCommerce reports, then `cargo xtask coverage open` to browse the local
artifact. See [the coverage guide](docs/COVERAGE.md) for prerequisites,
per-component commands, screenshots, CI artifacts, and the separate paid
stagenet profile.
