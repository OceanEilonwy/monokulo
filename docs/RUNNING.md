# Running Monokulo

A self-hosted Monero payment processor in two parts: the engine (chain
scanning, stores, webhooks and its admin API) and `monokulo` (the control
plane: the merchant dashboard and the checkout-facing HTTP surface), which
uses the engine only through its admin API. By default the engine runs inside
monokulo, as a library on threads of its own: one binary, one process, one
options file ([engine_as_library.md](engine_as_library.md)). It can also run on its own
(`monokulo-engine`), on another host if need be, with monokulo reaching it
over HTTP with an engine token the two share (`engine.mode = "remote"`).
Settings live in the options file (and a few runtime switches in the
database), edited on monokulo's admin settings page (one tab per job:
General, Monero nodes, Payments, Key custody, Abuse protection, Server,
Logging) and applied as soon as they're saved, except the few that say they
need a restart. Key custody is chosen per store: the admin enables backends
and a default, and each store can move by entering its keys again.

The short version, for each computer, is on the
[install page](https://oceaneilonwy.github.io/monokulo/#install). This page
has the rest: Docker, the OpenWrt package's files, building from source and
running the engine on its own. Settings are in
[CONFIGURATION.md](CONFIGURATION.md).

## With Docker

The `Dockerfile` builds one image with the binaries (`monokulo`, and
`monokulo-engine` for the other setups); `compose.yaml` runs monokulo from
it, the engine inside it, publishing port 8081. Put the one secret in a
`.env` file beside it:

```sh
printf 'MONOKULO_ENCRYPTION_KEY=%s\n' "$(openssl rand -hex 32)" > .env
docker compose up -d
```

then open http://localhost:8081 and continue from step 3 under [From release binaries or source](#from-release-binaries-or-source). Each
version tag's image is also on `ghcr.io/oceaneilonwy/monokulo`. Keep
`MONOKULO_ENCRYPTION_KEY`: it encrypts monokulo's data at rest. Both
databases (`monokulo.db` and the engine's `engine.db`) live in the
`monokulo-data` volume. `compose.yaml` also shows, commented out, the engine
as a container of its own.

## On an OpenWrt router (GL.iNet Flint 2)

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
- `/etc/monokulo/secrets` holds the encryption key, made on first start and
  readable by root only. Back it up.
- One procd instance runs monokulo, the engine inside it, as the `monokulo`
  user; `logread -e monokulo` shows its logs.

The design and the CPU and capacity figures are in [flint2_openwrt_apk.md](flint2_openwrt_apk.md).

## From release binaries or source

Download `monokulo` (one executable, the engine inside it) as a zip for
Windows (x86_64), macOS (Apple silicon and Intel) or Linux (x86_64, ARM64):

- [the latest release](https://github.com/OceanEilonwy/monokulo/releases/latest),
  for each version tag;
- [latest-main](https://github.com/OceanEilonwy/monokulo/releases/tag/latest-main),
  a prerelease brought up to date with every push to main that passes CI.

Each release's notes link the file for each computer and say how to start
it (and what macOS and Windows ask of an unsigned binary). Each also has
`key-custody-cli` (what merchants run to encrypt their keys for an SEV-SNP
engine without a browser; monokulo's key entry forms link the release that
matches them) for the same computers. With a download, skip step 1. The
standalone `monokulo-engine` isn't in them: build it (below), or take it
from the Docker image.

1. Build a release binary from the repository root. The project builds on
   the latest nightly Rust (`rust-toolchain.toml`; rustup installs it on
   first use, and `rustup update nightly` keeps it current). Building
   monokulo also builds its POS app, so it needs Node 24 or later and the
   app's dependencies, installed once from the lockfile. The engine builds
   RandomX (to check blocks' proof of work, [proof_of_work.md](proof_of_work.md)) from C++,
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
   `~/.local/share/monokulo/monokulo.db` (on Windows,
   `%LOCALAPPDATA%\monokulo\monokulo.db`, and its options file in
   `%APPDATA%\monokulo`) unless `--database-path` says otherwise, the engine's `engine.db` beside it, and listens on
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

## The engine on its own

 For a hardened setup that keeps the engine (and
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
