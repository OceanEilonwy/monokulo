# POS terminal - real stagenet + real browser e2e

Full end-to-end coverage for the POS screen (`crates/monokulo/templates/pos.html.hbs`,
`crates/monokulo/src/http/pos.rs`): a real, network-bound `scanner` engine talking to
the real public Monero **stagenet** node, a real, network-bound `monokulo`, one real
account with one real store connected (the same reusable merchant watch-only wallet
`../README.md`'s own Rust e2e tests use), driven through an actual Chromium browser
via [Playwright](https://playwright.dev/), paying real orders with genuine signed and
broadcast stagenet transactions (via `scanner::e2e_wallet::StagenetSpendWallet`, the
same real-CLSAG-signing code `cargo test --test e2e_stagenet` already uses - no
external wallet-rpc process).

**Nothing here is mocked or simulated.** The keypad taps are real clicks on the real
page, the QR/`monero:` URI come from a real order the real engine created, the payment
is a real transaction the real stagenet chain confirms, and the tick/progress-ring/
background-stacking behavior you see is the real page's own JS reacting to real,
polled server state - not a scripted animation.

**Disabled by default, on purpose.** This is not part of `cargo test`, not part of
`npm test` anywhere else in this repo, and not wired into any CI. It costs real
(if worthless) stagenet fees, takes several real minutes (stagenet blocks land
roughly every ~2 minutes, and one of the two tests waits for a real confirmation),
and depends on a public node being reachable. Run it explicitly, deliberately, when
you actually want that level of confidence.

## What it proves (and what it doesn't)

Covers, against the real chain: order creation in the store's own base currency, the
real keypad's digit entry, the real QR code + amount-carrying `monero:` URI, the tick
appearing at real 0-conf mempool detection, a 0-conf-trusted order auto-returning to
the keypad with no merchant action, a confirming order's progress ring + "confirm in
background" + stacked completion, and that a POS-created order is a real order visible
on the normal dashboard orders list.

Does **not** attempt to reproduce a real error state (double-spend/under-paid/
over-paid/expired) by crafting a deliberately-wrong on-chain payment - that would cost
real time and money for coverage the pure-logic unit tests already give for free and
deterministically: see `crates/monokulo/src/http/pos.rs`'s own `pure_logic_tests`
module (`derive_payment_error`). Also only ever backgrounds one payment at a time
(proving multiple concurrent backgrounded payments stack for real would mean paying
for and waiting on two independent confirmations) - each is independently polled by
construction, so this generalizes without needing to prove it twice at real cost.

Best-effort NFC (`NDEFReader`) is not exercised here either - it needs real Android
Chrome + a real NFC reader, which a CI/dev machine's headless Chromium doesn't have.
The page already degrades to "QR code only" when `NDEFReader` isn't present (see
`pos.html.hbs`'s own `attemptNfcWrite`), which is exactly the code path this suite
does exercise.

## One-time setup

```sh
# from this directory:
npm install
npx playwright install chromium
```

The Rust binaries this suite drives are built automatically by `global-setup.js`
(`cargo build -p scanner --features e2e --bin pos-e2e-server --bin pos-e2e-send-payment`)
the first time you run it - that first build pulls in the real transaction-signing
dependencies (`monero-wallet`, `monero-daemon-rpc`, `curve25519-dalek`) and can take a
little while; every run after that is a fast no-op rebuild check.

## Running it

```sh
# from this directory:
npm test
```

Runs both scenarios sequentially against one shared real backend (see
`playwright.config.js`: `fullyParallel: false`, `workers: 1` - they share one real
customer wallet and must never race each other). Expect this to take several real
minutes. `npx playwright show-report` after a run opens the HTML report (with traces/
screenshots on failure).

## How it's wired together

- `global-setup.js` builds and spawns `crates/scanner/src/bin/pos_e2e_server.rs`
  (`target/debug/pos-e2e-server`), a real `[[bin]]` (not a `cargo test`) so this script
  can spawn/discover/kill it as a predictable, ordinary child process. That binary
  prints one `POS_E2E_READY {...}` JSON line to stdout once both real servers are up
  and the account/store exist, then blocks forever (both servers, a background
  scan-tick loop, and its own internal `/send-payment` endpoint keep running on their
  own tasks) until this script sends it `SIGTERM` at teardown. The JSON (base URLs,
  including `send_payment_url`, the store's `connection_id`, the test account's
  email/password) is written to `.pos-e2e-fixture.json` (gitignored) for the test
  files to read.
- `tests/pos.spec.js` drives the actual browser: real login through `/dashboard/login`,
  real clicks on the real keypad, reads the real order back off the real `POST
  .../pos/orders` response, then calls `pos-e2e-server`'s own `POST /send-payment`
  endpoint (`helpers.js::sendStagenetPayment`, a plain `fetch`) to sign and broadcast
  the real payment - the one place any key material is touched, deliberately kept in
  Rust, never reimplemented in JS. Not a separate child process any more: an earlier
  version shelled out to `pos-e2e-send-payment` directly, but running that concurrently
  with `pos-e2e-server`'s own scan loop hit a real, reproducible node-side reliability
  limit - see `crates/stagenet-test-wallet`'s own module doc comment and the git
  history around its introduction for the full story.
- Uses `crates/stagenet-test-wallet` (not `scanner::e2e_wallet`) to sign and broadcast -
  a fast, narrow, stagenet-only wallet with no chain scanning (informed of its own
  outputs directly, via the committed `e2e/stagenet-known-outputs.json` ledger) and
  decoy selection served from the committed `e2e/stagenet-decoy-distribution.json`
  snapshot rather than a live fetch. Still reads the shared `e2e/stagenet-wallets.json`
  customer wallet's own keys/address, the same fixture `../tests/e2e_stagenet.rs` uses.

## If it fails

- **"cannot reach the stagenet node"**: same node (`node.monerodevs.org:38089`)
  `../README.md`'s own tests use - check your network, or see that README for
  alternatives.
- **A `pos-e2e-send-payment` failure naming the customer wallet/faucet**: the shared
  customer wallet's spendable outputs ran dry - see `../README.md`'s own "Reproducing
  from scratch" section (same wallet, same fix).
- **A Playwright assertion timeout on the tick/progress-ring/background-stack**: check
  `playwright-report/` (screenshots + trace on failure) and `pos-e2e-server`'s own
  stderr, which `global-setup.js` forwards straight through to this process's own
  terminal output.
