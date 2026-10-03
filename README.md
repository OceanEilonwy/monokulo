# Monokulo

A self-hosted Monero payment processor in two parts: the engine (chain
scanning, stores, webhooks and its admin API) and `monokulo` (the control
plane: the merchant dashboard and the checkout-facing HTTP surface), which
uses the engine only through its admin API. By default the engine runs inside
monokulo, as a library on threads of its own: one binary, one process, one
options file (`docs/engine_as_library.md`). It can also run on its own
(`monokulo-engine`), on another host if need be, with monokulo reaching it
over HTTP with an engine token the two share (`engine.mode = "remote"`).
Settings live in the options file (and a few runtime switches in the
database), edited on monokulo's admin settings page (one tab per job:
General, Monero nodes, Payments, Key custody, Abuse protection, Server,
Logging) and applied as soon as they're saved, except the few that say they
need a restart. Key custody is chosen per store: the admin enables backends
and a default, and each store can move by entering its keys again.

## Running in production

### With Docker

The `Dockerfile` builds one image with the binaries (`monokulo`, and
`monokulo-engine` and `key-custody-server` for the other setups);
`compose.yaml` runs monokulo from it, the engine inside it, publishing port
8081. Put the one secret in a `.env` file beside it:

```sh
printf 'MONOKULO_ENCRYPTION_KEY=%s\n' "$(openssl rand -hex 32)" > .env
docker compose up -d
```

then open http://localhost:8081 and continue from step 3 below. Each
version tag's image is also on `ghcr.io/oceaneilonwy/monokulo`. Keep
`MONOKULO_ENCRYPTION_KEY`: it encrypts monokulo's data at rest. Both
databases (`monokulo.db` and the engine's `engine.db`) live in the
`monokulo-data` volume. `compose.yaml` also shows, commented out, the engine
as a container of its own.

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

Each version tag's GitHub release has `monokulo`, `monokulo-engine` and
`key-custody-server` for Linux (x86_64, aarch64) and macOS (arm64); CI's
`publish` jobs also keep them for every push to main. With those, skip
step 1.

1. Build a release binary from the repository root. The project builds on
   the latest nightly Rust (`rust-toolchain.toml`; rustup installs it on
   first use, and `rustup update nightly` keeps it current). Building
   monokulo also builds its POS app, so it needs Node 24 or later and the
   app's dependencies, installed once from the lockfile. The engine builds
   RandomX (to check blocks' proof of work, docs/proof_of_work.md) from C++,
   so it needs CMake and a C++ compiler:

   ```sh
   (cd crates/monokulo/pos-ui && npm ci)
   cargo build --release -p monokulo --bin monokulo
   ```

2. Start it:

   ```sh
   MONOKULO_ENCRYPTION_KEY=<64 hex chars, 32 bytes> ./target/release/monokulo
   ```

   `MONOKULO_ENCRYPTION_KEY` must be 64 hex characters decoding to exactly 32
   bytes - generate one with `openssl rand -hex 32` and keep it, since it's
   what encrypts data at rest in monokulo's database. monokulo keeps it at
   `~/.local/share/monokulo/monokulo.db` unless `--database-path` says
   otherwise, the engine's `engine.db` beside it, and listens on
   `127.0.0.1:8081` unless `--server-bind` says otherwise. The engine
   inside it scans on threads of its own (`engine-worker`, as many as
   `--engine-server-worker-threads` says), and its lines in the log are
   named `engine`.

3. Open monokulo in a browser. The first visit redirects to a first-run
   admin setup wizard to create the one admin account. Its admin settings
   page then manages monokulo and the engine: the Monero nodes tab has a row
   per node, and the other tabs hold payment thresholds, rate limits and the
   rest.

Run it under whatever supervisor you normally use (systemd, etc.): a single
long-running binary with no daemonization of its own.

**The engine on its own.** For a hardened setup that keeps the engine (and
its key custody) away from the public-facing process, run `monokulo-engine`
separately and point monokulo at it:

```sh
cargo build --release -p engine --bin monokulo-engine -p monokulo --bin monokulo
ENGINE_TOKEN=<openssl rand -hex 32> ./target/release/monokulo-engine --server-bind 127.0.0.1:8080
MONOKULO_ENCRYPTION_KEY=<...> MONOKULO_ENGINE_TOKEN=<the same token> \
    ./target/release/monokulo --engine-mode remote --engine-url http://127.0.0.1:8080
```

The engine refuses every request without the token (at least 32
characters), so only monokulo can use it; bind it where only monokulo can
reach it. Its settings are then in its own options file, `engine.toml`.

### Settings

monokulo keeps its settings in an options file, TOML, at
`~/.config/monokulo/monokulo.toml` (`$XDG_CONFIG_HOME` if set; the working
directory if that can't be used), or wherever `--options <PATH>` says. The
engine inside it keeps its settings in the same file, under `[engine.*]`
tables (`[engine.payment]` holds its `payment.…`); a standalone engine keeps
them in its own `engine.toml`. `monokulo --init` (and `monokulo-engine
--init`) write one with every setting, its default commented out and what
it does, and print where. The admin settings page edits the file in place,
keeping your comments, and applies the change at once; after editing it by
hand, press Reload options file on that page. A save is refused if the file
changed on disk since it was read, and every setting the file holds is
locked on the page if the process can't write it.

Every setting can also be given as a command-line option: the setting's
key with `-` for `.` and `_` (`payment.reorg_check_depth` is
`--payment-reorg-check-depth`), and the embedded engine's with `--engine-`
in front (`--engine-payment-reorg-check-depth`). An option wins over the
file, and the admin page shows that setting locked. `monokulo --help` and
`monokulo-engine --help` list every option with its default and what it
does; `-h` gives the short version.

Secrets are only ever taken from the environment, never an option or the
file, since every user of the machine can read a process's arguments:
`MONOKULO_ENCRYPTION_KEY` and `MONOKULO_LOGGING_OTLP_HEADERS` for monokulo;
with a remote engine, `MONOKULO_ENGINE_TOKEN` too, and `ENGINE_TOKEN` and
`ENGINE_LOGGING_OTLP_HEADERS` for the engine. `--help` lists them after the
options. Two runtime switches, `abuse.under_attack` and
`logging.dev_mode_until`, are kept in each database instead, since they
are flipped while running rather than configured. Each field on the admin
page has a chip saying where its value comes from.

An invalid value anywhere - the file (named by line), an option, the
environment or the database - stops the process at start with every
problem listed, so a value an upgrade no longer accepts is fixed, not
silently replaced. So does a setting that only the other engine mode uses
(an engine URL or token with the engine inside monokulo, the standalone
engine's own server and logging settings under `[engine.*]`, or
`[engine.*]` tables with a remote engine), and a missing encryption key.

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
