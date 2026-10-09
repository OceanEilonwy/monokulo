# Wallets

A merchant's stores take payments into **named wallets**. A new account sets
up its first store straight after signing up (`/setup`: Store, then
Wallet, then Done), adding its first wallet on the way; a later store uses
a wallet already added or adds another. Several stores can share a wallet.

Designs: the "Monokulo wallet flows" canvas (2026-10-08). This page records
how they were built and the decisions taken on the way.

## Try it

```sh
cargo run -p monokulo          # with an engine and a node for the network you use
```

1. Sign up at `/dashboard/signup`: you're logged in and sent to `/setup`.
2. **Store**: where you take payments (your own website, WooCommerce, or in
   person only), the store's name, and its site (a host: any page on it
   works; none for in person). A site another store on the instance has is
   refused.
3. **Wallet**: use a wallet you already added, or name a new one (the name
   is checked first) and **Create a new wallet** (needs JavaScript) or
   **Bring your own wallet** (paste a private view key and public spend key;
   works without it). Hardware wallets show as *Coming soon*.
4. For a new wallet: its words and QR code show straight away on each
   tab (Cake / Monero.com, Stack, Feather, Monero GUI / CLI, On paper);
   **Make a different phrase** makes another. Then pick two of its words,
   each from four, or go on after 20 seconds. Only the watch-only keys are
   sent, and the store is made with it.
5. **Done** says what's left. A website's store isn't taking payments yet:
   add the checkout (the docs), verify the domain, take a test payment.
6. The Wallets tab of `/account` lists them: mainnet wallets first, by name,
   then the stagenet and testnet ones folded below (open when there's no
   mainnet wallet), each network shown with one badge
   (`views::network_badge`), as on a wallet's page, the dashboard and the
   wallet dropdowns. **+ add a wallet** (`/account/wallets/add`) uses the
   same screens as setup's Wallet step. A wallet's page (below) says where
   it lives, lists its stores, renames it, shows its history and retires it
   once nothing uses it.

Tests: `cargo test -p engine -p wallet-setup -p monokulo`, and in
`e2e/browser`, `npx playwright test -c real-binaries.config.js
tests/wallet-setup.spec.js` (store setup and the new-wallet flow in a real
browser).

## How it fits together

| Piece | Where | What |
| --- | --- | --- |
| Engine wallets | `crates/engine` migration 0028, `POST/DELETE /api/v1/admin/wallets`, `wallet_id` on `POST /api/v1/admin/tenants` | A wallet's sealed keys and its one subaddress counter; stores are created on it |
| Browser module | `crates/wallet-setup` (WebAssembly) | Polyseed from the page's randomness, keys, address, 25-word form, QR codes |
| Monokulo wallets | migration 0031, `src/wallets.rs`, `src/http/{wallets,wallet_service}.rs`, `src/views/wallets.rs`, `static/wallet-setup.js` | Names, origin, backup, history; the pages |
| Store setup | migration 0034, `src/stores.rs`, `src/http/setup.rs`, `src/views/setup.rs` | Store, Wallet, Done; a store's name and site |

## Decisions

Each: what was decided, what else was possible, and why.

### Product

1. **The whole hardware wallet option is "Coming soon", not just Trezor.**
   You asked for Trezor to stay disabled until WebUSB work is done together.
   Ledger needs the same kind of device work (WebHID, its Monero app's
   commands), so offering it alone would ship an untested half. The card is
   drawn with both marks and a *Coming soon* tag, unavailable with and without
   JavaScript. The design's hardware screens wait for that work.
2. **A store can change its wallet** (see "Changing a store's wallet"
   below). It is called changing the wallet, never moving the store.
3. **Signing up logs the new account in** and goes on to store setup, or
   back to `next` (a plugin's connect page), so someone arriving from
   WooCommerce ends up back at their shop.
4. **WooCommerce finds the store by its site.** A store is a host and no two
   stores on an instance share one, so the connect link needs no question:
   a shop whose site one of the merchant's stores has connects to it with
   one button; another account's is refused; otherwise setup makes the
   store, with the kind and site fixed to the shop's, and its Done page's
   button gives the plugin its key. A link that can't work (no public
   address, a return address on another site) says why before anything
   else.
5. **The name comes before the kind** on the Wallet step, and is checked
   before anything is made: as it changes (fixi), and again when a kind is
   picked. A name taken in between gets a number ("Till (2)"), so a phrase
   already backed up is never thrown away over its name.
6. **Names**: blank picks a friendly "Adjective Noun" name nobody on the
   account has yet. Names are unique per account (ignoring case) so a picker
   is never ambiguous. The same keys can't be added twice to one account: the
   form says which wallet already has them.
7. **Wallets are retired, not deleted** (see "Retiring a wallet" below).
8. **A wallet's history** is its own events plus payments to its stores,
   read live from the engine (not copied into monokulo). Its events are:
   - made;
   - brought in;
   - renamed;
   - store connected;
   - a store changed to it, or changed to another wallet;
   - retired (its keys deleted), or brought back.

   The payments listed are those for orders a store made while it used the
   wallet. The wallet page lists the stores on it now and, under "Before",
   the stores that used it and changed to another wallet.
9. **Skipping the backup is allowed**, behind the warning, a tick and typing
   `skip`. The wallet records `backup = skipped` and the page after says the
   phrase wasn't saved. Refusing outright would leave someone who already has
   the phrase elsewhere stuck.
10. **The check asks two random words, each picked from four**: the right
    one and three from the same word list, never from the phrase (the words
    of other wallets made in the page and thrown away). It checks what the
    tab shows: the 16 words, or the 25-word form for the Monero GUI. Both
    right enables Next; after 20 seconds Next works without answering
    ("Continue without checking"), and the backup is recorded as the tab's.
11. **Restore QR codes**: Cake Wallet and Monero.com scan
    `monero_wallet:<address>?seed=…&height=…&label=…` (their restore code
    accepts a polyseed); Stack Wallet scans `{"mnemonic": [words]}` into its
    word boxes; Feather has no full-wallet QR restore, so it gets the words;
    the Monero GUI can't read polyseed, so it gets the **25-word version of the
    same wallet** (the polyseed's spend key as a legacy seed, as Feather
    offers) and a restore height. The words and the QR show straight away:
    the page is the private moment. Feather's tab records `feather`.
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
    no-store` and drops the words once submitted. It doesn't warn before
    leaving: nothing is made until the check passes or is skipped, so Back
    loses nothing. If registering fails, the page says plainly that the
    phrase backed up is not connected.
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
23. **The network for a new wallet** is under "More options" on the Wallet
    step (mainnet by default), and isn't asked again.
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

## A wallet's page

`/account/wallets/{id}` is one centred column
(`docs/design/user-testing/wallet-page.html`, "Agreed · 2 + 3"):

- **The header**: the name and its network badge, nothing else. A retired
  wallet adds a "retired" tag and "Keys deleted (date). History kept."
- **Where it lives**: the app holding its keys and recovery phrase, with
  the app's logo. A made wallet says how it was backed up
  (`wallets.backup`: "Backed up to Cake Wallet", "Backed up on paper", or
  "Backup skipped", tinted as a warning). A brought-in one says which app
  it's in (`wallets.app`, migration 0034, from the optional "Which app is
  it in?" on Bring your own wallet): "Brought in from Feather", or
  "Brought in · app not recorded".
- **Stores**: each store taking payments into it, with its host or "in
  person" and a "change the wallet" link to its Wallet section; those that
  did before are muted under "Before".
- **Details**: the rename, the address (cut in the middle) and its kind.
- **History**, folded, with a count.
- **Retire this wallet**, a row at the bottom with the red "Retire
  wallet…" button. It opens the retire dialog; without JavaScript it is a
  link to `/account/wallets/{id}/retire`, a page with the same content
  (one render function draws both).

The retire dialog is a checklist: "No store takes payments into it" and
"No order on it can still be paid", each ticked or crossed, a crossed one
with its fix beside it (the store, with a link to change its wallet; until
about when orders can be paid). When the engine can't say, one row says
so with "try again". When both are ticked, the name is typed to confirm;
until then the red button is there but off. A refused retire (the name
wrong, the engine's 409) shows the retire page again with why.

A retired wallet's bottom row is "Restore this wallet" with a neutral
"Restore wallet…" button, opening the key form in a dialog (or
`/account/wallets/{id}/restore` without JavaScript).

## Changing a store's wallet

A store's settings start with a **Wallet** section:

- the wallet payments go to, and since when;
- a dropdown of the account's wallets, with the current one marked
  Current;
- the wallet history, folded under "Wallet history (N wallets)". The
  history lists each wallet, from, until and how many orders the store made
  then, with the current row highlighted.

Picking another wallet asks first, saying how many orders are still open on
the current wallet and that they keep being paid into it. Confirming makes
the change. With fixi, picking posts at once and the question appears in
place. Without JavaScript, **Change wallet** asks and the next button
confirms.

30. **Changing is allowed while orders are open.** Orders keep their
    addresses on the old wallet and go on being watched with its keys, open
    or closed within the grace period, so a late payment is still seen.
31. **Only to a wallet on the same network.** The dropdown offers only
    wallets on the store's network, and its help says so. A form that sends
    another anyway is refused, by monokulo and again by the engine.
32. **The history lives in monokulo** (`store_wallet_periods`, migration
    0032). Each store has one open period, its current wallet; a change
    closes it and opens the next at the same moment. Existing stores got a
    period from the day they were connected. A deleted wallet leaves its
    periods behind, shown as "A deleted wallet". The order counts come from
    monokulo's own order records, by when each order was made.
33. **In the engine, orders are watched by a scan row** (migration 0029).
    Each order records its wallet (`wallet_id`) and the tenant row whose
    keys watch it (`scan_tenant_id`). A change does three things in one
    transaction:
    - it moves the store's orders to a scan-only row (`watches_for` = the
      store) holding the old wallet's keys and starting at the store's
      cursor;
    - it gives the store the new wallet's keys and counter;
    - it records when (`wallet_changed_at_utc`).

    The scanner treats a scan row like any other tenant: its own cursor,
    handle, windows and mempool claims. So nothing in the scanning code had
    to learn about wallets. A scan row has keys for the API that nobody is
    given, and is never listed or counted as a store.
34. **One scan row per wallet a store left.** Changing back to a wallet and
    away again reuses the row. Its indices stay unique because they come
    from that wallet's one counter. `orders` is now unique on
    `(scan_tenant_id, minor_index)`, not `(tenant_id, minor_index)`: a
    store's orders on two wallets can share an index. That needed the
    orders table rebuilt, which SQLite only allows with foreign keys off.
    The migration runner turns them off for a migration whose first line is
    `-- foreign_keys: off`, and refuses it if `PRAGMA foreign_key_check`
    finds anything.
35. **A scan already running when the wallet changes is not trusted for the
    store.** It may have used the old keys, and an index it found may now
    name a new order on the new wallet. So the change handler takes the
    store's handle out of the map before committing. A result from a scan
    that began at or before `wallet_changed_at_utc` is then not recorded
    against the store: the scan row, starting from the store's cursor, looks
    at those blocks itself. The mempool records with the time its scan
    began, for the same check.
36. **The txid lookup tries every row**: the store, then each wallet it left.
37. **Deleting a store turns its scan rows off too.** Retiring a wallet
    turns off the scan rows on it and deletes their keys.

## Retiring a wallet

What deleting a wallet was for is making Monokulo forget its keys. Retiring
does that and keeps everything else. A retired wallet:

- is offered nowhere: not in wallet dropdowns, not to a new store;
- has its keys deleted, the private view key and public spend key, from the
  engine's database and from key storage;
- keeps its name, address, page and history. Stores that used it still
  name it in their wallet history, with a Retired chip.

The wallets page folds retired ones under "Retired wallets (N)".

38. **Retire, never delete.** There is no delete. A wallet nobody ever used
    is retired like any other.
39. **Only a wallet not in use can be retired.** That means no store uses
    it and no order on it can still be paid (open, or closed less than the
    grace period ago). Until then the retire dialog's checklist says what
    it waits for, including roughly until when orders can be paid, and its
    Retire button is off. There is no waiting
    "retiring" state and no "delete the keys now".
40. **The page says the keys are deleted.** Under the name, a "retired"
    tag and "Keys deleted (date). History kept.", and a history entry says
    the same. It doesn't mention backups: whether old copies exist
    elsewhere depends on the key storage backend.
41. **In the engine**: `POST /api/v1/admin/wallets/{id}/retire` does all of
    this in one transaction:
    - it marks the wallet (`deleted_at_utc`);
    - it empties the sealed keys on the wallet row and on every tenant row
      on it (disabled stores, scan rows);
    - it turns those rows off.

    Their live handles leave key custody. The row stays, so its address
    counter is never reused. `GET /api/v1/admin/wallets/{id}` says what a
    retirement would wait for.
42. **Bringing one back** takes its keys again, on its page.
    `POST /api/v1/admin/wallets/{id}/restore` checks that they make the
    wallet's address, and refuses another wallet's. Adding the same keys
    through "Bring your own wallet" points to the retired wallet instead.
43. **Names stay taken** by retired wallets: a picker never shows two
    alike, and bringing one back can't clash.
