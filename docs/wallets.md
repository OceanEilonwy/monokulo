# Wallets

A merchant's stores take payments into **named wallets**. A new account sets
up its first wallet straight after signing up; a store (custom, POS or
WooCommerce) then picks one. Several stores can share a wallet.

Designs: the "Monokulo wallet flows" canvas (2026-10-08). This page records
how they were built and the decisions taken on the way.

## Try it

```sh
cargo run -p monokulo          # with an engine and a node for the network you use
```

1. Sign up at `/dashboard/signup`: you're logged in and sent to
   `/dashboard/wallets/setup`.
2. **Create a new wallet** (needs JavaScript), or **Bring your own wallet**
   (paste a private view key and public spend key; works without it).
   Hardware wallets show as *Coming soon*.
3. For a new wallet: save the phrase in a wallet app (the QR code) or write
   it down, then type the three words asked for. Only the watch-only keys
   are sent.
4. **Add a store**: the custom store form and the WooCommerce connect page
   pick the wallet.
5. `/dashboard/wallets` lists them; a wallet's page renames it, shows its
   stores and history, and deletes it once no store uses it.

Tests: `cargo test -p engine -p wallet-setup -p monokulo`, and in
`e2e/browser`, `npx playwright test -c real-binaries.config.js
tests/wallet-setup.spec.js` (the new-wallet flow in a real browser).

## How it fits together

| Piece | Where | What |
| --- | --- | --- |
| Engine wallets | `crates/engine` migration 0028, `POST/DELETE /api/v1/admin/wallets`, `wallet_id` on `POST /api/v1/admin/tenants` | A wallet's sealed keys and its one subaddress counter; stores are created on it |
| Browser module | `crates/wallet-setup` (WebAssembly) | Polyseed from the page's randomness, keys, address, 25-word form, QR codes |
| Monokulo wallets | migration 0031, `src/wallets.rs`, `src/http/{wallets,wallet_service}.rs`, `src/views/wallets.rs`, `static/wallet-setup.js` | Names, origin, backup, history; the pages |

## Decisions

Each: what was decided, what else was possible, and why.

### Product

1. **The whole hardware wallet option is "Coming soon", not just Trezor.**
   You asked for Trezor to stay disabled until WebUSB work is done together.
   Ledger needs the same kind of device work (WebHID, its Monero app's
   commands), so offering it alone would ship an untested half. The card is
   drawn with both marks and a *Coming soon* tag, unavailable with and without
   JavaScript. The design's hardware screens wait for that work.
2. **Moving a store to another wallet is not built.** The design showed it on
   the wallet field's states. A store's engine tenant holds its keys and has
   handed out addresses from its wallet's counter; moving it means re-keying
   the tenant and deciding what happens to its open orders, which is its own
   piece of work. A store keeps the wallet it was made with; to change, make a
   new store. Store settings say so implicitly: their key storage section now
   explains that moving keys moves the wallet.
3. **Signing up logs the new account in** (it used to send it to the login
   page) and goes on to wallet setup, carrying `next` (a plugin's connect
   page) through both, so someone arriving from WooCommerce ends up back at
   their shop.
4. **WooCommerce asks first** ("Is this shop already a store in Monokulo?")
   with plain links, so it works without JavaScript. With no store yet the
   question is skipped. An account with no wallet is sent to set one up and
   comes back. A link that can't work (no public address, a return address on
   another site) says why before anything else.
5. **The wallet dropdown preselects only when there is exactly one wallet**;
   with more it starts on "Choose a wallet…" and is required.
6. **Names**: blank picks a friendly "Adjective Noun" name nobody on the
   account has yet. Names are unique per account (ignoring case) so a picker
   is never ambiguous. The same keys can't be added twice to one account: the
   form says which wallet already has them.
7. **Deleting a wallet** needs its name typed, and is refused while a store
   uses it. The engine forgets it first, then monokulo.
8. **A wallet's history** is its own events (made, brought in, renamed, store
   connected) plus payments to its stores, read live from the engine (not
   copied into monokulo).
9. **Skipping the backup is allowed**, behind the warning, a tick and typing
   `skip`. The wallet records `backup = skipped` and its ready page says the
   phrase wasn't saved. Refusing outright would leave someone who already has
   the phrase elsewhere stuck.
10. **The check asks three random words**, exact match, from what was saved:
    the 16 words, or the 25-word form for the Monero GUI. Instructions say
    where to find the words in the app chosen (from each app's docs or
    source: Cake and Monero.com Settings, Recovery & Keys; Stack Wallet's
    Wallet backup; Feather's Wallet, Seed; the GUI's Settings, Seed & keys).
11. **Restore QR codes**: Cake Wallet and Monero.com scan
    `monero_wallet:<address>?seed=…&height=…&label=…` (their restore code
    accepts a polyseed); Stack Wallet scans `{"mnemonic": [words]}` into its
    word boxes; Feather has no full-wallet QR restore, so it gets the words;
    the Monero GUI can't read polyseed, so it gets the **25-word version of the
    same wallet** (the polyseed's spend key as a legacy seed, as Feather
    offers) and a restore height. The QR stays hidden until asked for.
12. **Wallet app logos** are the projects' own app icons, vendored in
    `static/wallet-logos/` with a SOURCE file; Trezor and Ledger are their
    marks from their own repositories, inline. They name the apps only.

### Engine

13. **The address counter moved to the wallet.** With one counter per store,
    two stores on one wallet would both hand out minor index 1, 2, … — the
    same addresses — and the scanner would credit one payment to both.
    Alternatives: a different account (major index) per store, which would
    put a shop's money in "Account 1" of the merchant's app where they may not
    look; or monokulo partitioning index ranges, which leaks engine concerns.
    A wallet-level counter keeps everything in account 0, and each store's
    scan window (its own orders' indices) still finds only its own payments.
14. **Stores keep their own copy of the sealed keys** (and backend, address,
    network), copied from the wallet when made, and `tenants.next_minor_index`
    mirrors the wallet's counter. Boot registration, scanning, order creation
    and the payment lookup read tenant rows unchanged. The cost is the copy;
    the gain is a much smaller change to the money path.
15. **Moving a store's keys to another custody backend moves its wallet and
    every store on it** in one transaction; the other stores' live handles are
    dropped and re-register from their rows on next use.
16. **Every existing store got a wallet of its own** in migration 0028
    (`wl_<tenant id>`), carrying its counter on. A store made with keys still
    works the same: a wallet is made for it.
17. **A wallet is registered in key custody only to check its keys and derive
    its address**, then removed; each store registers its own handle, so
    removing one store never pulls keys from another.

### The browser module and page

18. **The phrase is made in the page** from 32 bytes of
    `crypto.getRandomValues`, with the wallet's birthday from the page's clock;
    the WebAssembly module has no imports at all. The seed crates are built
    without their `std` feature, which would read a system clock and an OS
    random source the browser doesn't have.
19. **Only the private view key, public spend key and address are posted**,
    with how the phrase was saved. The server checks the address it derives
    from the keys matches the page's, so a wallet can't be registered with
    keys the phrase doesn't stand for. The page sends `Cache-Control:
    no-store`, warns before leaving, and drops the words once submitted. If
    registering fails, the page says plainly that the phrase backed up is not
    connected and offers a fresh wallet.
20. **Without JavaScript the create card is drawn unavailable with its
    reason**, server-side; the script turns it on (and says so if the browser
    lacks WebAssembly or secure randomness). Bring your own wallet needs no
    script.
21. **SEV-SNP key storage**: when the engine's default backend is `snp`, the
    new-wallet form carries the attestation bundle and `key-custody.js`
    encrypts the derived keys before they're posted, as for typed keys.
22. **The module is ~810 KB** (uncompressed): both seed crates carry every
    language's word list. Acceptable for one page; trimming means patching
    the crates.
23. **The network for a new wallet** is under "More options" on the setup
    page (mainnet by default).
24. **The restore height** shown with the 25-word form is the highest node
    height the engine reports for the network when the page loads: the wallet
    is new, so nothing earlier is its.

### Monokulo

25. **Monokulo holds no keys**: a wallet row is its name, network, address,
    origin, backup and the engine's wallet id.
26. **Stores made before wallets are matched lazily**: opening the wallets
    page or a connect link asks the engine which wallet each unlinked store
    uses and links it (adding it as "Brought in" with a friendly name, or to
    the account's wallet with that address).
27. **`POST /connections` (the API) still takes keys**; they become a
    brought-in wallet, or the account's existing one with that address. It
    also takes `wallet_id`.
28. **Colours stay in `theme.css`**: new roles for the danger button and the
    setup steps, with contrast checks in both themes.
29. **Dead code**: the connect forms' network select helper
    (`templates::network_selected_flags`) went with the key fields.
