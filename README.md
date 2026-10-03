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

### On an OpenWrt router (GL.iNet Flint 2)

One signed package, `monokulo`, holds both binaries, a procd service that
runs them, and a LuCI page (Services › Monokulo). It is for OpenWrt 25.12 on
`aarch64_cortex-a53` (the Flint 2 and other MediaTek Filogic routers). The
install steps are on the landing page, https://oceaneilonwy.github.io/monokulo/,
which is also the package repository; `web/index.html` is its source.

`scripts/build-openwrt.sh` builds the package, the signed repository and the
landing page into `dist/` and `site/` (it needs Docker, for the OpenWrt
SDK). The `OpenWrt package` workflow runs it on every pull request and push
to main, uploads `dist/` and `site/` as artifacts, and deploys the site to
GitHub Pages from main. On the router:

- `/etc/config/monokulo` (or LuCI) sets the port, listen address (LAN only
  by default), data folder (`/srv/monokulo`) and the engine's CPUs (2 and 3,
  at nice 10, so catching up with the chain leaves the rest for routing).
  These are passed as options, so the admin page shows them locked; every
  other setting is on the admin page as usual.
- `/etc/monokulo/secrets` holds the encryption key and engine token, made on
  first start and readable by root only. Back it up.
- Both processes run as the `monokulo` user; `logread -e monokulo` shows their logs.

The design and the CPU and capacity figures are in `docs/flint2_openwrt_apk.md`.

### From release binaries or source

Each version tag's GitHub release has `monokulo-engine`, `monokulo` and
`key-custody-server` for Linux (x86_64, aarch64) and macOS (arm64); CI's
`publish` jobs also keep them for every push to main. With those, skip
step 1.

1. Build release binaries from the repository root. The project builds on
   the latest nightly Rust (`rust-toolchain.toml`; rustup installs it on
   first use, and `rustup update nightly` keeps it current). Building
   monokulo also builds its POS app, so it needs Node 24 or later and the
   app's dependencies, installed once from the lockfile. The engine builds
   RandomX (to check blocks' proof of work, docs/proof_of_work.md) from C++,
   so it needs CMake and a C++ compiler:

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

3. Start the engine with the token. Everything else (Monero node
   endpoints, confirmation/expiry thresholds, rate limits, webhook policy)
   is set afterward on monokulo's admin settings page:

   ```sh
   ENGINE_TOKEN=<the engine token> \
       ./target/release/monokulo-engine --server-bind 127.0.0.1:8080
   ```

   Bind it where only monokulo can reach it (the same machine, or a private
   network): the token keeps everything else out, but nothing outside
   monokulo has a reason to reach the engine at all. Its database is
   `~/.local/share/monokulo/engine.db` unless `--database-path` (or its
   options file) says otherwise.

4. Start the control plane, pointed at the engine, with the same token:

   ```sh
   MONOKULO_ENCRYPTION_KEY=<64 hex chars, 32 bytes> \
   MONOKULO_ENGINE_TOKEN=<the engine token> \
       ./target/release/monokulo --engine-url http://127.0.0.1:8080
   ```

   `--engine-url` is where monokulo reaches the engine (default
   `http://127.0.0.1:8443`, the engine's default bind); left off the command
   line, it can be changed on the admin settings page instead, taking effect
   when monokulo restarts. `MONOKULO_ENGINE_TOKEN` is read only when
   monokulo starts: the admin settings page shows it locked, and changing it
   means changing it where monokulo is started and restarting.

   `MONOKULO_ENCRYPTION_KEY` must be 64 hex characters decoding to exactly 32
   bytes - generate one with `openssl rand -hex 32` and keep it, since it's
   what encrypts data at rest in `monokulo`'s own database. `monokulo` keeps
   its SQLite database at `~/.local/share/monokulo/monokulo.db` unless
   `--database-path` says otherwise, and listens on `127.0.0.1:8081` unless
   `--server-bind` does.

5. Open `monokulo` in a browser. The first visit redirects to a first-run
   admin setup wizard to create the one admin account. Its admin settings
   page then manages both processes: the Monero nodes tab has a row per
   node, and the other tabs hold payment thresholds, rate limits and the
   rest.

Run both processes under whatever supervisor you normally use (systemd,
etc.) - each is a single long-running binary with no daemonization of its
own.

### Settings

Each process keeps its settings in an options file, TOML, at
`~/.config/monokulo/engine.toml` and `~/.config/monokulo/monokulo.toml`
(`$XDG_CONFIG_HOME` if set; the working directory if that can't be used),
or wherever `--options <PATH>` says. `monokulo-engine --init` and
`monokulo --init` write one with every setting, its default commented out
and what it does, and print where. The admin settings page edits the file
in place, keeping your comments, and applies the change at once; after
editing it by hand, press Reload options file on that page. A save is
refused if the file changed on disk since it was read, and every setting
the file holds is locked on the page if the process can't write it.

Every setting can also be given as a command-line option: the setting's
key with `-` for `.` and `_` (`payment.reorg_check_depth` is
`--payment-reorg-check-depth`). An option wins over the file, and the
admin page shows that setting locked. `monokulo-engine --help` and
`monokulo --help` list every option with its default and what it does;
`-h` gives the short version.

Secrets are only ever taken from the environment, never an option or the
file, since every user of the machine can read a process's arguments:
`ENGINE_TOKEN` and `ENGINE_LOGGING_OTLP_HEADERS` for the engine,
`MONOKULO_ENGINE_TOKEN`, `MONOKULO_ENCRYPTION_KEY` and
`MONOKULO_LOGGING_OTLP_HEADERS` for monokulo. `--help` lists them after
the options. Two runtime switches, `abuse.under_attack` and
`logging.dev_mode_until`, are kept in each database instead, since they
are flipped while running rather than configured. Each field on the admin
page has a chip saying where its value comes from.

An invalid value anywhere - the file (named by line), an option, the
environment or the database - stops the process at start with every
problem listed, so a value an upgrade no longer accepts is fixed, not
silently replaced. A missing engine token or encryption key stops it too.

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
