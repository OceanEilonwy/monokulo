<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/readme/logo-dark.svg">
    <img src="docs/readme/logo.svg" width="128" height="128" alt="Monokulo">
  </picture>
</p>

<h1 align="center">Monokulo</h1>

<p align="center">
  <strong>Accept Monero on your own computer or router.</strong><br>
  A self-hosted, watch-only payment gateway. Payments go straight to your wallet.
</p>

<p align="center">
  <a href="https://github.com/OceanEilonwy/monokulo/actions/workflows/release.yml"><img src="https://github.com/OceanEilonwy/monokulo/actions/workflows/release.yml/badge.svg?branch=main" alt="Release"></a>
  <a href="https://docs.monokulo.com/testing/coverage.html"><img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fdocs.monokulo.com%2Ftesting%2Fbadge.json" alt="Coverage"></a>
  <a href="https://github.com/OceanEilonwy/monokulo/releases/tag/latest-main"><img src="https://img.shields.io/badge/download-latest--main-e05d00" alt="Download latest-main"></a>
  <a href="https://docs.monokulo.com/guide/"><img src="https://img.shields.io/badge/docs-docs.monokulo.com-0969da" alt="Docs"></a>
</p>

<p align="center">
  <a href="https://docs.monokulo.com/"><b>Website</b></a> ·
  <a href="https://docs.monokulo.com/install-computer/"><b>Install</b></a> ·
  <a href="https://github.com/OceanEilonwy/monokulo/releases/tag/latest-main"><b>Downloads</b></a> ·
  <a href="https://docs.monokulo.com/introduction/"><b>Docs</b></a> ·
  <a href="https://docs.monokulo.com/testing/"><b>How it's tested</b></a> ·
  <a href="https://github.com/OceanEilonwy/monokulo-woocommerce"><b>WooCommerce</b></a>
</p>

## Why Monokulo

- **Watch-only.** Each store gives Monokulo its view key, never its spend key. It sees payments arrive and can never move your coins.
- **Fast.** Payments show up about a second after they reach the network and confirm as blocks arrive.
- **Checkout anywhere.** A hosted checkout page, an embed for static sites, a point-of-sale screen, signed webhooks and a [WooCommerce plugin](https://github.com/OceanEilonwy/monokulo-woocommerce). The checkout works without JavaScript.
- **One small program.** One executable and SQLite. Runs on Linux, macOS, Windows, Docker or a GL.iNet Flint 2 router.
- **Many stores, one install.** Each store has its own wallet, view key and webhooks.
- **Checks the chain itself.** Verifies every block's proof of work, so a node can't make up confirmations.
- **Tested in depth.** Property tests against an independent ledger model, fuzzing and mutation testing run alongside the unit, browser and stagenet tests. See [how it's tested](https://docs.monokulo.com/testing/): every test, coverage, the property and fuzz runs, and screenshots of every screen.

## Get started

**1. Download** the build for your computer from [latest-main](https://github.com/OceanEilonwy/monokulo/releases/tag/latest-main) (Linux, macOS, Windows), or use the [OpenWrt package](https://docs.monokulo.com/install-router/) or Docker (the [Dockerfile](Dockerfile)).

**2. Start it** with an encryption key you keep:

```sh
openssl rand -hex 32 > monokulo.key
MONOKULO_ENCRYPTION_KEY="$(cat monokulo.key)" ./monokulo
```

**3. Open <http://127.0.0.1:8081>**, create your admin account, then add a Monero node and your first store.

Step-by-step instructions are in the install guides, **[for a computer](https://docs.monokulo.com/install-computer/)** and **[for a router](https://docs.monokulo.com/install-router/)**.

## Contributing

Monokulo builds on nightly Rust, Node 24 and CMake. To run it locally against stagenet:

```sh
npm ci
scripts/dev-run.sh start    # then open http://127.0.0.1:8081
```

### Shared UI and stylebook

The shared components live in [`crates/ui`](crates/ui): one package, the Rust
crate the server renders them with and the npm package `@monokulo/ui` the POS
imports, with their CSS and behaviour. The stylebook, generated from them in
part, is in [`apps/stylebook`](apps/stylebook).
Run `npm ci` at the repository root, then `npm run stylebook`.
See [the UI architecture and adoption guide](docs/ui-components.md).
