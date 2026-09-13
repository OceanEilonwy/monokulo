# Work Notes — MoneroPay Cloud / WooCommerce MVP

Running hand-off brief for agents implementing `docs/WOOCOMMERCE_WBS.md`.
Read this file first, then the specific WBS item(s) you've been assigned —
this file gives you the state and context; the WBS gives you the spec.

## What this project is

`moneropay-core` (repo root) is an existing, working, self-hosted Monero
payment gateway (Rust/axum/SQLite). We're extending it into a hosted SaaS
("MoneroPay Cloud") with a WooCommerce integration as the first platform.
Full rationale: `docs/WOOCOMMERCE_ROADMAP.md`. Full task breakdown:
`docs/WOOCOMMERCE_WBS.md`. Read both before assuming anything not stated
here — this file is a summary, not the source of truth.

Key architectural facts an agent should not have to rediscover:
- The existing engine crate (root `Cargo.toml`, `src/`) is becoming one
  member of a Cargo workspace, alongside new `shared/`, `control-plane/`,
  and `mock-woocommerce/` crates. No existing engine file moves.
- `shared/` holds logic pulled out of the engine (secret-token hashing,
  HMAC webhook signing, the migration runner) plus genuinely new helpers
  the engine doesn't have (argon2 password hashing).
- The engine's `src/lib.rs` already exports everything (`http`, `store`,
  `key_custody`, etc.) as `pub mod` — it's a real, usable library
  dependency for other workspace crates, not just a binary.
- `tests/e2e_stagenet.rs` already demonstrates the pattern for driving the
  engine directly (build the router, use it against real or in-memory
  storage) — reuse that pattern, don't reinvent it.

## Current repo state

- Working in git worktree `/home/henry/Downloads/mokulo/.claude/worktrees/woocommerce-roadmap-doc`,
  branch `worktree-woocommerce-roadmap-doc`. **This branch is not pushed to
  origin** (push access denied under current credentials) — it only exists
  locally. Do not assume it's recoverable from GitHub.
- All work so far is documentation: `docs/WOOCOMMERCE_ROADMAP.md` and
  `docs/WOOCOMMERCE_WBS.md`, both current and cross-checked against the
  real code as of this session (see the WBS's "gaps found" commit for what
  was corrected).
- No implementation code has been written yet. WBS item 0.1 (workspace
  setup) has not started as of this note.

## Progress log

- Fallback Monero nodes (not a WBS item - requested directly by the user
  after diagnosing 1.4.5's stall, as a real production-reliability feature,
  separate from the test-only node swap): `MoneroNodeConfig` gained an
  optional `fallbacks: Vec<MoneroFallbackNodeConfig>` field
  (`#[serde(default)]`, fully backward compatible - every existing
  single-node config parses unchanged), configured via one or more
  `[[monero_node.<network>.fallbacks]]` array-of-tables entries alongside
  the existing `[monero_node.<network>]` primary table. New
  `src/daemon_fallback.rs::FallbackDaemonClient` implements
  `MoneroDaemonClient` by wrapping an ordered list of real
  `RpcDaemonClient`s (primary + fallbacks) and failing over between them:
  every call starts at whichever node last succeeded (not always the
  primary - a dead primary shouldn't be retried on every single scan
  tick forever) and walks forward through the rest on failure, wrapping
  around; no background health-check polling, since the next real call is
  the health check. Wired into `main.rs`'s daemon construction - each
  configured network's `Arc<dyn MoneroDaemonClient>` is now this wrapper
  instead of a bare `RpcDaemonClient`, transparent to everything
  downstream (the scanner has no idea more than one node might be
  involved). 6 new unit tests in `daemon_fallback.rs` (healthy-primary
  never touches fallback; failover within one call; stickiness - a proven
  fallback isn't abandoned to retry a still-down primary; recovery once
  the current node itself fails; every-node-failure returns a clear error
  rather than panicking; single-node-no-fallbacks behaves like a bare
  client), plus 3 new `config.rs` tests (parses with no fallbacks; parses
  multiple in order with correct defaults; empty-host/out-of-range-port
  fallback entries rejected exactly like a primary node's would be, both
  cases in one test). `config.rs::validate_bounds` extended to validate
  each fallback's host/port alongside the primary's. Documented in
  `docs/DESIGN.md` §7.1. Full `cargo test --workspace` clean at 278
  engine tests (was 269 after the 1.4.5 commit above; +6 daemon_fallback,
  +3 config) / 0 failed / 8 ignored before commit.

- 1.4.5 done: the real stagenet end-to-end test
  (`mock-woocommerce/tests/e2e_stagenet_connect_flow.rs`, `#[ignore]`d, run
  via `cargo test -p mock-woocommerce --features e2e --test
  e2e_stagenet_connect_flow -- --ignored --nocapture`) now passes cleanly
  and repeatably (two consecutive clean runs, ~150-200s each). It signs up
  a real control-plane account, runs the real connect flow with the real
  stagenet **merchant** watch-only wallet (`e2e/stagenet-wallets.json`),
  creates a real order, pays it with a real signed transaction from the
  **customer** wallet (`moneropay_core::e2e_wallet::StagenetSpendWallet`,
  reused as a library dependency - not reimplemented), drives a real chain
  scan against a real stagenet node
  (`TestEngineHandle::run_scan_tick_now`), and asserts the mock's webhook
  receiver got a real, correctly-signed `order.paid` delivery.
  - **The real bug, after two wrong hypotheses**: every run reliably hung
    for minutes-plus on the very first post-payment scan tick, regardless
    of which public stagenet node was configured (ruled out via direct
    curl reproduction against three different nodes - all fast) and
    regardless of `#[tokio::test]`'s runtime flavor (switching to
    `flavor = "multi_thread"` on the theory of a `std::sync::Mutex`
    deadlock between the manual scan tick and the background
    webhook-delivery loop did not fix it, though it's still the right
    runtime choice here and was kept). The actual cause:
    `TestEngineConfig::with_background_loops`'s own scanner-tick loop
    (`engine-test-support/src/lib.rs`) runs on an inert `NoopDaemonClient`
    whose `get_height()` always returns `0`, and it shares the *same*
    per-network scanned-height watermark in `Store`
    (`max_scanned_height`/`set_scanned_block`, keyed by network string
    alone) with the real daemon this test drives directly via
    `run_scan_tick_now`. That loop's very first tick seeded
    `last_scanned = Some(0)` for stagenet; the real scan then computed
    `scan_range = (1, current_real_height)` and tried to fetch every
    stagenet block one at a time from block 1 up to the real chain tip
    (~2.2 million blocks) - which looks exactly like an indefinite,
    node-independent, low-CPU hang. Confirmed directly (not just inferred)
    by reading `src/scanner.rs::run_scan_tick` and
    `engine-test-support/src/lib.rs`'s loop, and by independently
    verifying via a stagenet block explorer / direct node RPC that a
    "stuck" payment's transaction was in fact already confirmed on-chain
    the whole time - the chain scan was fine, it just had an impossible
    amount of ground to cover.
  - **The fix**: `TestEngineConfig` gained `.without_background_scan_loop()`
    - keeps the real (and necessary) webhook-delivery-tick loop but skips
    spawning the `NoopDaemonClient` scan-tick loop entirely, so nothing
    else touches this test's network's watermark. The test now calls
    `.with_background_loops().without_background_scan_loop()`. Each real
    scan tick now takes ~3-5s (normal RPC latency), not minutes.
  - **Also fixed along the way (still real, kept)**: `e2e/moneropay-
    stagenet.toml`'s configured node changed from `node.monerodevs.org` to
    `stagenet.xmr-tw.org:38081` after observing genuine (if ultimately
    unrelated to this bug) multi-minute stalls against the former during
    diagnosis - not reproducible via plain curl, so likely specific to
    long-lived/pooled-connection behavior; the new node has been reliably
    fast throughout.
  - **Caution for future review**: a `cargo fmt -p mock-woocommerce -p
    engine-test-support -p moneropay-core` run mid-session reflowed the
    *entire* `moneropay-core` crate (this codebase is deliberately not
    rustfmt-compliant - wider, hand-formatted style). That inflated the
    diff to 5817+/1793- across 31 files before a dedicated review caught
    it; the pure-reflow noise (21 files, zero real content changes -
    verified by reformatting each file's pre-change HEAD version and
    diffing against working copy) was reverted with `git checkout --`
    before committing. Don't run `cargo fmt -p moneropay-core` (whole
    engine crate) again without a real reason to touch every file in it.
  - Deleted `examples/fund_and_split_customer_wallet.rs`, a throwaway
    one-time script (used once to split faucet funds across the customer
    wallet's UTXOs for the e2e test) that didn't compile without the `e2e`
    feature and broke plain `cargo build --workspace`.
  - Not done, explicitly out of scope for 1.4.5 and requested separately
    by the user afterward: a genuine multi-node/fallback-node-list feature
    in the engine's own daemon-client configuration (for production
    reliability, not just this test) - next up.
  - Full `cargo test --workspace` (269 engine + 26 shared + 80 control-plane
    + 8 mock-woocommerce + 2 engine-test-support, all passing, 0 failed)
    re-run clean after the fmt-noise revert, before this commit.

- 1.4.4 done: webhook registration folded into `/finish`, plus a real
  receiver in `mock-woocommerce` and a genuinely forced end-to-end delivery
  test, completing Track 1.4 short of 1.4.5 (deliberately not built here).
  - **Part A (control-plane)**: `POST /connect/{platform}/finish` now
    accepts `{"token", "webhook_url"?}` (`webhook_url` optional, for
    backward compatibility with every pre-existing caller/test). When
    present, `finish` calls a new `EngineClient::create_webhook(sk, url) ->
    Result<(webhook_id, signing_secret), EngineClientError>` (mirrors the
    engine's real `CreateWebhookRequest`/`CreateWebhookResponse`
    field-for-field, same convention as every other `EngineClient` method)
    and returns `webhook_signing_secret` in the response
    (`#[serde(skip_serializing_if)]`, so a caller with no webhook sees the
    exact same wire shape as before this task). A failed registration
    collapses the *whole* `/finish` call to `401`, per this task's own spec
    - documented in `finish`'s own doc comment as a deliberate policy
      choice, not just pattern reuse, along with the accepted tradeoff (the
      connect token is already consumed by that point, so the plugin must
      restart the whole flow rather than retry).
    - Also threaded `ConfirmForm::order_expiry_seconds` (new, optional,
      `#[serde(default)]`) through to `CreateConnectionFields` - previously
      hardcoded to `None` at the confirm step. Needed as a real way to give
      a tenant a short expiry through this flow (see Part C), and is a
      genuine, narrow gap this task's own scope justified filling rather
      than a JSON-endpoint-only capability (`POST /connections` already had
      it).
  - **Part B (mock-woocommerce)**: new `shared` dependency (plain library
    dep, no circularity). Added a real, long-lived `WebhookReceiver`
    (`POST /moneropay/webhook`) - deliberately *not* tied to the short-lived
    callback server's lifetime, since a delivery can arrive well after the
    connect flow itself has returned. Verifies `X-MoneroPay-Signature` via
    `shared::webhook_sign::verify_signature` against the raw `axum::body::
    Bytes` *before* any JSON parsing, rejects (without recording) a missing/
    invalid signature or one arriving before the receiver's `signing_secret`
    is even known yet, and dedupes recorded events on `event_id` (real
    at-least-once delivery per `docs/DESIGN.md` §11).
    `run_connect_flow` now always spawns this receiver and passes its URL
    as `webhook_url` to `/finish`, storing the real `webhook_signing_secret`
    and the still-running receiver in an extended `ConnectedCredentials` -
    all 3 pre-existing tests needed zero changes to their assertions (only
    gained new fields). A new `run_connect_flow_with_order_expiry_seconds`
    sibling (generalizing rather than duplicating, same judgment call
    `TestEngineConfig` already modeled) exists for Part C's test.
  - **Part C (engine background loops)**: investigated thoroughly before
    touching anything - **zero engine-crate changes were needed**.
    `moneropay_core::webhook_delivery::run_delivery_tick` was already public
    (already used by `main.rs`); the one genuine gap was a
    `MoneroDaemonClient` for `run_scan_tick` to drive, since the engine's own
    `daemon::fake::FakeDaemonClient` is `#[cfg(test)]`-gated and therefore
    invisible to any downstream crate, dev-dependency or not. Fixed
    entirely inside `engine-test-support` by implementing the (ungated,
    public) `MoneroDaemonClient` trait fresh with a trivial `NoopDaemonClient`
    (height stuck at 0, empty blocks/mempool) - confirmed by directly reading
    `src/scanner.rs` that `run_scan_tick`'s non-terminal-order recompute sweep
    (`docs/DESIGN.md` §7.6, the one that reaches `expired`) is keyed off
    `network` alone via `non_terminal_order_ids`, entirely independent of the
    `tenants`/watchlist parameter that gates real chain-scanning, so an inert
    daemon and an empty `tenants` list are sufficient to force a real expiry
    purely from wall-clock time. `TestEngineConfig::with_background_loops()`
    (opt-in, off by default - every existing caller/test unaffected) spawns
    both the scanner-tick and delivery-tick loops on a 150ms interval.
    Verified directly in `engine-test-support`'s own new test
    (`background_loops_genuinely_deliver_a_real_expired_webhook`) purely
    through the engine's public/admin HTTP API, with no `mock-woocommerce`/
    `control-plane` involved, before relying on it anywhere else.
  - **The forced-delivery test**
    (`mock-woocommerce`'s `a_genuinely_forced_order_expired_webhook_is_delivered_and_verified`):
    real engine (`with_background_loops`) + real control-plane +
    `run_connect_flow_with_order_expiry_seconds(url, 1)` + a real order via
    `create_order` with no payment ever made, then polls (not a fixed sleep)
    the receiver until the real `order.expired` delivery lands. Genuine, not
    a shortcut: nothing in the test touches either database directly, mints
    an event/delivery row itself, or calls any scanner/delivery function by
    hand - every step is a real HTTP call or an already-independently-proven
    background loop. The final assertion re-verifies the *exact* raw bytes +
    signature the receiver actually recorded (not a re-serialized
    reconstruction) against `credentials.webhook_signing_secret` - a value
    obtained from a completely different channel (`/finish`'s own JSON
    response) than the receiver's internal state, plus a negative check that
    a wrong secret does not verify the same bytes.
  - Also added: 3 new control-plane tests (`engine_client.rs`'s
    `create_webhook_then_list_webhooks_round_trips_against_a_real_engine`;
    `connect.rs`'s `finish_with_a_webhook_url_registers_a_real_webhook_and_
    carries_the_signing_secret` and `finish_with_a_rejected_webhook_url_
    fails_the_whole_call`); 4 more mock-woocommerce tests beyond the forced-
    delivery one (a direct signature-verification unit test reusing
    `shared::webhook_sign`'s own documented cross-language known vector
    rather than inventing a new one; dedupe-on-retry; reject-invalid-
    signature; reject-before-secret-is-known).
  - Counts: control-plane 80 passed (+3 from this task; the pre-task
    baseline was already 77, not the 57 last recorded in this log under
    1.3.3 - 1.4.1/1.4.2/1.4.3 added tests without updating a "Counts:" line
    here, not this task's doing), engine-test-support 2 passed (was 1, +1),
    mock-woocommerce 8 passed (was 3, +5), engine 269/8 ignored (unchanged,
    confirmed no engine-crate file was touched), shared 26 (unchanged,
    untouched). `cargo build --workspace` and `cargo test --workspace` both
    clean, no warnings.
  - Files touched: `control-plane/src/engine_client.rs`,
    `control-plane/src/http/connect.rs`, `engine-test-support/Cargo.toml`,
    `engine-test-support/src/lib.rs`, `mock-woocommerce/Cargo.toml`,
    `mock-woocommerce/src/lib.rs`. No `docs/`, no migrations, no engine
    (`src/`) file.

- 1.4.2 done: `mock-woocommerce` is now a real driver — `run_connect_flow`
  is the synthetic browser (cookie-persisting `reqwest::Client`, auto-
  following redirects) walking the whole WBS 1.4.1 flow: connect-start →
  signup → login-with-`next` → confirm → auto-followed straight into the
  driver's own locally-bound callback server. The callback handler is
  where the real "plugin-side" logic lives (nonce check, then a separate
  server-to-server `/finish` call) — deliberately placed there rather than
  in the outer driver, since that's exactly where the real WordPress
  plugin's callback will live at WBS 1.5.3. `main.rs` is a thin CLI
  wrapper (control-plane URL from arg/env/default), exiting 0 with
  credentials printed or non-zero on failure, matching the WBS's own
  acceptance criterion. No shared `control-plane-test-support` crate yet
  (only `engine-test-support` exists) — a small private harness lives in
  this crate's own tests for now, correctly judged not worth generalizing
  until a second consumer needs it. Nonce-mismatch handling is proven
  load-bearing with a strong test: a substituted-nonce callback is
  rejected before `/finish` is ever called, then the *same* token is
  proven still-unconsumed by successfully finishing it directly
  afterward. mock-woocommerce grew from 1 → 3 tests (2 real + 1
  placeholder). Independently re-verified (full `lib.rs` review, `cargo
  test --workspace` re-run twice) before commit.
- 1.4.1 done: (see the WBS 1.4.1 commit for the full writeup — generic
  connect start/finish endpoints, the open-redirect-safe `next` handling,
  and atomic single-use token consumption.)
- 1.3.3 done: order list/detail + webhook list pages — read-only, per the
  WBS's own "what" bullet (only the engine's `GET` admin routes). Three new
  `EngineClient` methods (`list_orders`, `get_order_detail`,
  `list_webhooks`), mirroring the engine's real `OrderView`/
  `OrderDetailResponse`/`PaymentView`/`WebhookView` field-for-field, same
  convention as `create_tenant`/`get_tenant`. New `control-plane/src/
  http/orders.rs` adds three routes behind `AuthedUser`:
  `GET /dashboard/connections/{id}/orders`,
  `GET /dashboard/connections/{id}/orders/{payment_id}`,
  `GET /dashboard/connections/{id}/webhooks`.
  - **Ownership design**: a user can have more than one `store_connections`
    row, so every route is scoped by `{id}` in the path.
    `load_owned_connection` looks the row up by `id` and filters it through
    `row.user_id == user.id` in one step — a mismatch and a nonexistent id
    both collapse to the same `Ok(None)`, mapped to a bare `404` by every
    caller, exactly like the account-enumeration defense `login`/
    `AuthedUser` already apply to accounts, just applied to object-level
    access here. Verified this is a real, load-bearing check, not
    decoration: temporarily changed `load_owned_connection` to skip the
    `user_id` filter and re-ran the cross-user test — it failed (`200` where
    it expected `404`), then reverted and reconfirmed green.
  - **First real `crypto::decrypt` consumer outside a test**: each handler
    decrypts the connection's stored `sk_...` via `crypto::decrypt` +
    `state.encryption_key` before calling `EngineClient`. A decryption
    failure (shouldn't happen for a row this service itself wrote) maps to
    a plain `500`, never unwrapped/panicked.
  - **Engine-404 vs. internal-error split**: `order_detail` distinguishes
    the engine's own `404` (unknown `payment_id`, or one belonging to a
    different tenant) — rendered as a real `404` with a clear "Order not
    found" page — from every other `EngineClientError`, which stays a
    generic `500`. Same "caller-caused vs. our problem" split
    `connections.rs`'s `CreateConnectionError::BadRequest`/`Internal`
    already established, just keyed off `404` instead of `400` here.
  - **`engine-test-support` extended again**, generalizing rather than
    adding a third near-duplicate spawn function: a new
    `TestEngineConfig` (builder: `with_networks`/`with_rate`, `.spawn()`
    does the actual construction) now backs both `spawn_test_engine`
    (`TestEngineConfig::new().spawn()`) and
    `spawn_test_engine_with_networks` (`TestEngineConfig::new()
    .with_networks(networks).spawn()`) — neither's signature or behavior
    changed; both crates' own smoke tests (`engine-test-support`'s
    `client_library_route_is_reachable_over_a_real_socket`,
    `engine_client.rs`'s and `connections.rs`'s real-engine tests) still
    pass unchanged. This task's own tests use
    `TestEngineConfig::new().with_networks(&[Mainnet]).with_rate("USD",
    ...).spawn()` — needed because seeding a real order means calling the
    engine's *public* `POST /api/v1/t/{pk}/orders`, which 400s without a
    configured exchange rate for the requested `fiat_currency` (the same
    kind of gap `spawn_test_engine_with_networks` closed for
    `configured_networks` at 1.2.1).
  - Three new templates (`orders.html.hbs`, `order_detail.html.hbs`,
    `webhooks.html.hbs`), following the existing minimal, no-CSS style,
    registered in `templates.rs` alongside new view-model structs
    (`OrdersViewModel`, `OrderDetailViewModel`/`OrderDetailData`,
    `WebhooksViewModel`, plus their row types).
  - **Tests** (`control-plane/src/http/orders.rs`'s own `#[cfg(test)] mod
    tests`, 6 new): a real order seeded via a raw `reqwest` call against the
    spawned engine's public API (using the connection's `pk_`, no `Origin`
    header so the tenant's empty `allowed_origins` never comes into play)
    then shown by `GET .../orders` (its `payment_id` appears in the
    response) and `GET .../orders/{payment_id}` (full detail, including the
    fiat currency); an unknown `payment_id` renders a real `404` "not
    found" page; `GET .../webhooks` on a connection with none registered
    renders a valid, empty table (`200`, not an error); a second signed-up
    user hitting the first user's connection id gets `404` (verified
    load-bearing, see above — not their orders, and not a `403` that would
    confirm the id exists); all three routes reject an unauthenticated
    request with `401` before any ownership check runs (checked with a
    connection id that doesn't even exist, since `AuthedUser` must reject
    before the handler ever looks anything up).
  - Counts: control-plane 57 passed (was 51, +6), engine-test-support 1
    passed (unchanged), engine 269/8 ignored (unchanged), shared 25
    (unchanged), mock-woocommerce 1 (unchanged). `/signup`, `/login`,
    `/logout`, `/connections`, `/dashboard/connect` and their existing
    tests untouched. No webhook create/delete or order mutation built —
    out of scope per the WBS's own "what" bullet for this task.
- 1.3.2 done: `GET`/`POST /dashboard/connect` — the browser form for wallet
  provisioning, behind `AuthedUser` (works via either bearer or cookie,
  same as everything else). Core logic factored out of the JSON
  `/connections` handler into `connections::create_connection_for_user`
  (async, since — unlike the sync `create_account`/`authenticate`
  factorings from 1.3.1 — this genuinely awaits a real network call to the
  engine), called by both surfaces. `platform` hardcoded to `"woocommerce"`
  for now (no platform-choice UI yet); the three optional wallet-limit
  fields left `None`. `allowed_origins` arrives as one comma-separated
  text field, split/trimmed/empty-filtered into a `Vec<String>`. No
  session → plain `401`, same as everywhere else (no redirect-on-401
  invented). New `connect.html.hbs` (one template, form/error/confirmation
  via `{{#if}}`, correctly auto-escaped, no triple-stash). Test reused the
  strong "decrypt then authenticate against the real engine" proof from
  1.2.3 rather than a weaker string check. control-plane 51 passed
  (was 43, +8). Independently re-verified (full diff review of
  connections.rs's refactor, dashboard.rs's new handlers, the template)
  and `cargo test --workspace` re-run before commit.
- (1.3.1 done, see git log for details — dashboard signup/login pages
  with cookie sessions.)
- 1.2.3 done: `sk_` at-rest encryption, completing WBS 1.2. AES-256-GCM
  (`aes-gcm` crate) in a new `control_plane::crypto` module — pure
  key-as-parameter functions (`Db` and the crypto module itself stay
  ignorant of *where* the key comes from), encoded as
  `hex(nonce || ciphertext+tag)` in the same `TEXT` column, no schema
  change. `main.rs` sources the key from `CONTROL_PLANE_ENCRYPTION_KEY`
  (64 hex chars) with three distinct, precise panic messages rather than
  a checked-in placeholder — correctly treated as a materially different
  case from the earlier placeholder engine URL (a stub secret is a real
  future credential leak; a stub URL isn't). `http/connections.rs` now
  encrypts before calling `Db::create_store_connection`. Genuinely good
  engineering under a real obstacle: `aes-gcm` 0.11's API had changed
  significantly from older docs/examples (moved to the `hybrid-array`-based
  `aead` 0.6 crate) — resolved by reading the actual vendored source rather
  than guessing. Tamper-detection is proven, not just asserted possible: a
  test flips a real ciphertext byte and confirms `AuthenticationFailed`
  (plus separate tests for truncated input, non-hex input, and a wrong
  key). The updated `/connections` test proves genuine encryption via a
  stronger check than a literal-string comparison: it decrypts the stored
  value with the known test key, then authenticates *as that tenant*
  against the real spawned engine (`EngineClient::get_tenant`) — only the
  real `sk_...` could pass that. control-plane 31 passed (was 25, +6).
  Independently re-verified (full crypto.rs/main.rs/mod.rs diff review,
  `cargo test --workspace` re-run) before commit.
- 1.2.2 done: `store_connections` table + `POST /connections`, the first
  endpoint wiring together 1.1.x session auth and 1.2.1's `EngineClient`.
  Migration 3 (`control-plane/migrations/0003_store_connections.sql`) adds
  the table exactly as specced, with an explicit doc comment (mirrored on
  `Db::create_store_connection`) that `tenant_secret_token_encrypted`
  currently holds the engine's raw `sk_...` value, UNENCRYPTED — named for
  its WBS-1.2.3 final form so that task doesn't need a rename migration.
  Added `Db::create_store_connection` and `Db::get_store_connection_by_id`
  (`StoreConnectionRow`), following the existing `UserRow`/`SessionRow`
  pattern.
  - `EngineClient` gained `#[derive(Clone)]` (free — `reqwest::Client` is
    `Arc`-backed internally, `base_url` is a plain `String`) and a
    `base_url(&self) -> &str` accessor, so a handler can record which
    engine endpoint a tenant lives on without threading the URL through
    separately. `AppState` gained `pub engine_client: EngineClient`;
    `build_router`'s only real call sites (`main.rs`, `http/tests.rs`'s
    `test_app_state`, `connections.rs`'s own test helper) all updated.
    `main.rs` hardcodes `EngineClient::new("http://127.0.0.1:8080")` with a
    `// TODO: real config` comment — there's no config-file system in
    `control-plane` yet (a later task), so this is type-level wiring only,
    not a claim that `main.rs` actually reaches a real engine today.
  - `POST /connections` (new `control-plane/src/http/connections.rs`) sits
    behind `AuthedUser`. Request:
    `{platform, site_url, view_key_hex, spend_pubkey_hex, network?,
    allowed_origins, confirmations_required?, zero_conf_max_piconero?,
    order_expiry_seconds?}` — the wallet fields map straight through into
    `engine_client::CreateTenantRequest`. On success: inserts a
    `store_connections` row (new UUID id, the authed user's id,
    `platform`/`site_url` from the request, `tenant_public_key`/
    `tenant_secret_token_encrypted` from the engine's response,
    `moneropay_endpoint` = `state.engine_client.base_url()`) and returns
    `201 {"connection_id": "...", "public_key": "pk_..."}` —
    **`secret_token` is deliberately never returned**, per the roadmap: the
    control plane keeps it for its own future server-to-server use
    (webhook registration, dashboard proxying), never re-shown to the
    merchant after this one-time creation.
  - **Error-shape judgment call**: added a new `ApiError::BadRequest(String)`
    variant (the only variant that carries a real, caller-visible message —
    every other variant stays fixed/generic on purpose, per the existing
    doc comment) rather than collapsing an engine-rejected request into
    the generic `Internal`/500. `EngineClientError::EngineError { status,
    .. }` maps to `ApiError::BadRequest(message)` specifically when
    `status == 400` (the engine's own `ApiError::BadRequest` — confirmed by
    reading `src/http/admin.rs::create_tenant` at the repo root, which
    returns exactly that for bad hex or an unconfigured network); any other
    engine status, or a transport-level failure reaching the engine at all,
    stays `Internal` — that's this service's problem, not something the
    caller caused or should see details about.
  - **Tests** (`control-plane/src/http/connections.rs`'s own
    `#[cfg(test)] mod tests`, using
    `engine_test_support::spawn_test_engine_with_networks(&[Mainnet])`,
    same fixed-scalar view-key/spend-pubkey construction as
    `engine_client.rs`'s own test): a full round trip (signup → login →
    `POST /connections` with valid wallet fields) asserts `201`, a
    non-empty `connection_id`, a `public_key` starting `pk_`, and that
    neither `secret_token` nor `tenant_secret_token_encrypted` appears
    anywhere in the response body; then reads the `store_connections` row
    back directly via `Db::get_store_connection_by_id` and asserts
    `user_id`/`platform`/`site_url` match and
    `tenant_secret_token_encrypted` is a real value starting `sk_`. A
    second test asserts `POST /connections` with no `Authorization` header
    is rejected `401` by `AuthedUser` before ever touching the engine
    client. Verified the round trip is genuine (not a false-positive pass)
    by temporarily corrupting the stored-row `public_key` assertion and
    re-running — it failed showing the actual `pk_...` the live engine
    returned, then reverted.
  - Also added 2 `Db`-level tests for `create_store_connection`/
    `get_store_connection_by_id` (round-trip; unknown-id lookup returns
    `None`).
  - Counts: control-plane 25 passed (was 21, +4), engine 269/8 ignored
    (unchanged), shared 25 (unchanged), engine-test-support 1 (unchanged),
    mock-woocommerce 1 (unchanged). `/signup`, `/login`, `/logout` and
    their tests untouched. No encryption of the stored `sk_` implemented —
    that's WBS 1.2.3, left for a separate task.
- 1.2.1 done: `control-plane/src/engine_client.rs` — a `reqwest`-based
  `EngineClient` the control plane uses to call a *separately-running*
  engine's admin API (a different role from `control_plane::http`, which is
  the control plane's own router). `EngineClient::new(base_url)` takes the
  engine's externally-reachable URL explicitly, no default/guessing.
  `create_tenant(CreateTenantRequest) -> Result<CreateTenantResponse,
  EngineClientError>` does `POST {base_url}/api/v1/admin/tenants` with no
  auth header (confirmed the engine leaves that endpoint open by design);
  `get_tenant(sk) -> Result<TenantView, EngineClientError>` does
  `GET {base_url}/api/v1/admin/tenant` with `Authorization: Bearer sk_...`,
  matching `AuthedTenant`'s real parsing at the repo root. Request/response
  structs are control-plane's own, matched field-for-field against the
  engine's real `src/http/admin.rs` types (`CreateTenantRequest`/
  `CreateTenantResponse`/`TenantView`) rather than imported — the two
  crates only ever talk over HTTP. `EngineClientError` (`thiserror`)
  covers a failed request (`#[from] reqwest::Error`) and a non-success
  status (`EngineError { status, message }`, `message` pulled from the
  engine's own `{"error": "..."}` body shape, falling back to the raw body
  if that ever doesn't parse). Added `reqwest = { version = "0.13.4",
  default-features = false, features = ["rustls", "json"] }` to
  `control-plane/Cargo.toml`, matching the engine's own pin exactly.
  - **`configured_networks` problem**: `engine-test-support::spawn_test_engine`
    (0.6) configures no Monero networks, but `create_tenant`'s handler
    rejects any request for a network not in `state.configured_networks` —
    so a test that needs a *real* tenant created (not just the route
    reachable) can't use `spawn_test_engine` as-is. Extended
    `engine-test-support` with a new `spawn_test_engine_with_networks(&[Network])`
    that `spawn_test_engine()` now delegates to (passing `&[]`) — same
    engine construction, just a configurable `configured_networks` set
    instead of a hardcoded empty one. No behavior change to
    `spawn_test_engine` itself or its signature; its existing WBS 0.6 smoke
    test (`client_library_route_is_reachable_over_a_real_socket`) passes
    unchanged, still 1 passed for the crate. This felt like the cleanest
    fix in scope — 0.6 just hadn't anticipated a caller needing a
    successful `create_tenant`, and the gap is narrow and additive.
  - **Test**: `control-plane/src/engine_client.rs`'s own `#[cfg(test)] mod
    tests`, one integration test
    (`create_tenant_then_get_tenant_round_trips_against_a_real_engine`):
    spawns a real engine via `spawn_test_engine_with_networks(&[Network::Mainnet])`,
    points an `EngineClient` at `http://{engine.addr}`, calls `create_tenant`
    with a valid-format (fixed-scalar, same construction as the engine's own
    `src/http/tests.rs::valid_view_key_hex`/`valid_spend_pubkey_hex`, values
    pre-computed via a throwaway example rather than pulling `monero` into
    `control-plane`'s main dependencies) view key + spend pubkey, asserts a
    real `tenant_id`/`pk_.../sk_...` come back, then calls `get_tenant` with
    the returned `sk_` and asserts its `public_key` matches. Verified this is
    a genuine round trip (not a false-positive pass) by temporarily
    corrupting the final assertion and re-running — it failed showing the
    *actual* `pk_...` value the live engine returned, then reverted.
    `monero = "0.22.0"` and `engine-test-support = { path =
    "../engine-test-support" }` added to `control-plane`'s
    `[dev-dependencies]` only (not main dependencies).
  - Counts: control-plane 21 passed (was 20, +1), engine-test-support 1
    passed (unchanged), engine 269/8 ignored (unchanged), shared 25
    (unchanged), mock-woocommerce 1 (unchanged). `/connections` and
    `store_connections` (1.2.2) deliberately not built here — separate
    task. `/signup`, `/login`, `/logout` untouched.
- 1.1.3 done: `POST /logout` — reuses the existing `AuthedUser` extractor
  rather than duplicating its `Bearer`-header parsing; extended `AuthedUser`
  from a one-field tuple struct (`AuthedUser(UserRow)`) to two fields
  (`AuthedUser(UserRow, String)`), the second being the session's
  `token_hash` already computed inside the extractor — exactly what
  `Db::delete_session` needs, and the only thing missing before. Updated
  the one existing call site (`test_whoami`'s destructuring) to match; no
  behavior change there. Handler calls `Db::delete_session` and returns
  `204` unconditionally, including the (currently unreachable without
  concurrency) case where the row was already gone — no session-expiry
  concept exists yet to make that reachable in practice, and a client
  logging out an already-logged-out session isn't an error worth
  surfacing. 4 new HTTP tests: logout returns 204; logout then reusing the
  *same* token against `/_test/whoami` now gets 401 (proves the session
  row is actually gone, not just that `/logout` responded); missing/
  unknown bearer both still 401 via the same extractor. control-plane 20
  passed (was 16); engine 269/8 ignored and shared 25 both unchanged.
  Touched: `control-plane/src/http/mod.rs` (extractor shape + route table),
  new `control-plane/src/http/logout.rs`, `control-plane/src/http/tests.rs`.
  `/signup` and `/login` untouched.
- 1.1.2 done: `POST /login` + session auth. Good independent judgment call
  worth recording: session tokens are hashed at rest exactly like tenant
  `sk_` tokens (same reasoning — high-entropy, machine-generated, a fast
  hash is enough), reusing `shared::auth::hash_secret_token` rather than
  writing a near-duplicate, with a new `generate_session_token()`
  (`sess_` prefix, same `random_hex` primitive as `sk_`/`pk_`) added
  alongside it. `sessions` table added via migration 2. Login handles the
  "unknown email" case by running a real `verify_password` against a fixed
  dummy Argon2 hash rather than short-circuiting, specifically so it can't
  be distinguished from "wrong password" by status, body, *or* an obviously
  cheaper code path — both return an identical 401. New `AuthedUser`
  extractor mirrors the engine's own `AuthedTenant` exactly (`Authorization:
  Bearer <token>`, hash it, look up, 401 on anything missing/invalid,
  never distinguishable). A `#[cfg(test)]`-gated `/_test/whoami` route
  (confirmed genuinely compiled out of the real binary, not just
  undocumented) gives the test suite something to exercise the extractor
  against ahead of any real protected endpoint existing. `delete_session`
  added to `Db` now (unused) for 1.1.3 to call next. 9 new tests (3 `Db`,
  6 HTTP) — reviewed the full diff directly, including the enumeration
  defense and the cfg-gating, before independently re-running
  `cargo test --workspace` (control-plane 16, shared 25, rest unchanged)
  and `cargo test -p shared` in isolation (still fine after the earlier fix).
- 1.1.1 done: `control-plane` has its first real code — restructured into
  `lib.rs`/`main.rs` (mirroring the engine's own split) plus `db.rs` (its own
  SQLite database, entirely separate from the engine's, same `Store`-style
  pattern: `Db` wrapping one `rusqlite::Connection`, `Arc<Mutex<..>>`-shared,
  migrated via `shared::migrations::apply`) and `http/` (`AppState`,
  `build_router`, tested via `tower::ServiceExt::oneshot` — no bound socket
  needed for control-plane testing its own router, that's a different need
  from `engine-test-support`). `POST /signup` hashes with
  `shared::password::hash_password`, returns `201 {user_id}` or
  `409 {"error":"email already in use"}` on a duplicate email (detected via
  `rusqlite`'s `ConstraintViolation` error code, same shape the engine's own
  code checks elsewhere) — any other failure is a fixed, generic `500`, no
  message ever varies with the underlying cause. 7 new tests, all reviewed
  directly and independently re-run: valid signup, duplicate-email conflict,
  and a direct DB-row check confirming the stored value is a real
  `$argon2...` PHC hash (verified via `shared::password::verify_password`),
  never the plaintext.
  - **Fixed while reviewing** (not the delegated agent's fault, a real
    latent gap it correctly flagged rather than silently working around):
    `shared/Cargo.toml` was missing an explicit `rand_core` dependency with
    the `getrandom` feature — `argon2`'s `password_hash::rand_core::OsRng`
    needs it, and it was only compiling as part of `cargo test --workspace`
    by accident, via feature unification with some other member's
    dependency graph. `cargo test -p shared` in isolation failed outright.
    Added `rand_core = { version = "0.6", features = ["getrandom"] }`
    directly to `shared/Cargo.toml`; confirmed both `-p shared` alone and
    `--workspace` build cleanly now.
- **Foundations (0.1-0.6) complete.** Track A starts next (control-plane
  accounts, WBS 1.1).
- 0.6 done: real-engine test harness landed as its **own new crate**,
  `engine-test-support/`, not inside `shared` — correctly identified a real
  circular-dependency problem (`moneropay-core` depends on `shared`, so a
  harness needing `moneropay-core` can't live in `shared` without a cycle)
  and resolved it exactly as the WBS's own hedge anticipated ("in `shared`,
  or a dev-only sibling crate"). Layering:
  `engine-test-support -> moneropay-core -> shared`. Public API:
  `spawn_test_engine() -> TestEngineHandle` (in-memory `Store`,
  `PlainKeyCustody`, empty `FixedRateProvider`, no configured networks,
  bound to a real `127.0.0.1:<ephemeral-port>` via `axum::serve` in a
  background task; `Drop` aborts the task). No `#[cfg(test)]`/feature gate
  needed on the crate itself — being reached only via `[dev-dependencies]`
  is what keeps it out of real builds. `mock-woocommerce` now depends on it
  as a dev-dependency. Smoke test does a genuine `reqwest` round trip
  (confirmed: real TCP, not `tower::ServiceExt::oneshot`) against
  `/static/moneropay-client.js` (verified dependency-free — no tenant/node/
  scanner needed). Engine/shared test counts unaffected (269/24); new
  crate at 1 passed. Independently re-verified (full file read, workspace
  member diff, test re-run) before commit.
- 0.5 done: `shared::migrations::apply` now holds the generic transactional
  migration runner (moved from `src/store.rs`'s `apply_migration_list`,
  renamed since it's namespaced now — pure move, `unchecked_transaction`
  usage and all-or-nothing semantics unchanged). Engine keeps its own
  `MIGRATIONS` list (the `include_str!(...)` paths, meaningless outside
  the engine crate), the `apply_migrations` wrapper (now a one-line call
  into `shared::migrations::apply`), and `configure_connection`
  (`PRAGMA foreign_keys` etc.) — confirmed the required ordering
  (`configure_connection` then `apply_migrations`, outside any
  transaction) is untouched at both call sites. Good judgment call on
  which tests moved: the generic-mechanism test
  (`a_failing_migration_leaves_neither_its_schema_changes_nor_its_version_row`,
  built on an ad-hoc migration list) moved to `shared`; two tests that
  drive the *real* `MIGRATIONS`/`Store` schema
  (`reopening_an_existing_database_file_does_not_reapply_migrations`,
  `migration_0004_rebuilds_order_payments_without_losing_existing_rows`)
  correctly stayed in `store.rs` since they test engine schema content,
  not the runner itself — only their call site was updated to
  `shared::migrations::apply(...)`. Engine 269 passed/8 ignored (was 270,
  −1 moved), `shared` 24 passed (was 23, +1). Independently re-verified
  (diff read in full, ordering confirmed, tests re-run) before commit.
- 0.4 done: `shared::password` — new, genuinely new logic (not a move),
  Argon2id via the `argon2` crate for the control plane's future human
  account passwords, explicitly separate from `shared::auth`'s SHA-256
  token hashing (different threat model, documented in the module's own
  doc comment). Pinned `argon2 = "0.5"` (resolved to 0.5.3) rather than
  the `cargo add`-default 0.6.0 — 0.6 ships a rewritten `password-hash`
  0.6.1 API without `SaltString`/`rand_core`, not the standard
  SaltString+OsRng+PHC-string pattern; 0.5's API is the well-documented,
  idiomatic one and was what was actually wanted here. `hash_password`
  returns a self-describing PHC-format string; `verify_password` returns
  `false` uniformly for both "wrong password" and "malformed hash string"
  (no panic, no distinguishable side channel). 4 new tests (round-trip,
  wrong password, per-call-random-salt via two different hashes of the
  same password, malformed-input handling). `shared` now 23 passed
  (was 19); engine unaffected at 270. Independently re-verified before
  commit.
- 0.3 done: `shared::webhook_sign` now holds HMAC signing/verification
  *and* the SSRF URL-validation logic (`validate_webhook_url`,
  `is_disallowed_address`, `WebhookUrlError`) moved from `src/webhook_sign.rs`
  — same file covered both concerns originally. Same thin-re-export
  pattern as 0.2; only real call site is `src/webhook_delivery.rs`. Added
  a new known-vector test (on top of one that already existed and moved
  over) specifically for the later PHP webhook-receiver task (WBS 1.5.4) to
  cross-check against:
  - secret: `known_vector_secret_for_php_crosscheck`
  - payload: `{"event":"order.paid","order_id":"12345","amount_piconero":"1000000000000"}`
  - expected signature (hex, lowercase): computed by the test itself from
    the real `sign_payload` function — see
    `shared::webhook_sign::tests::known_vector_for_cross_language_php_verification`
    for the exact value rather than retyping it here (avoids a transcription
    error propagating into the eventual PHP test).
  - PHP-side equivalent: `hash_hmac('sha256', PAYLOAD, SECRET)`, compared
    with `hash_equals()`, not `==` (per the constant-time requirement noted
    in the WBS at 1.5.4).
  Engine 270 passed/8 ignored (was 285, −15 moved), `shared` 19 passed
  (3 + 15 moved + 1 new). Independently re-verified before commit.
- 0.2 done: `shared::auth` now holds the token generation/hashing logic
  moved from `src/auth.rs` (SHA-256, unchanged). `src/auth.rs` is a thin
  `pub use shared::auth::*;` re-export so `src/http/admin.rs` and
  `src/store.rs` (the only call sites) needed no changes. Root `Cargo.toml`
  depends on `shared` by path now. Tests moved intact: engine 285
  passed/8 ignored (was 287 — the 2 moved tests now run from `shared`,
  which is at 3 passed total including its own placeholder). Independently
  re-verified (diff + full `cargo test --workspace` re-run) before commit.
- 0.1 done: root `Cargo.toml` gained a `[workspace]` table
  (`members = ["shared", "control-plane", "mock-woocommerce"]` — the root
  package is included implicitly since it already has `[package]`; no
  separate "." entry needed or accepted). Added `shared/` (lib crate,
  empty placeholder + trivial test), `control-plane/` (bin crate, `fn
  main() {}` + trivial test), and `mock-woocommerce/` (bin crate, `fn
  main() {}` + trivial test, with `moneropay-core = { path = ".." }` as a
  real dependency for later integration tests). `cargo build --workspace`
  and `cargo test --workspace` both succeed: engine 287 passed/8 ignored
  (unchanged from pre-workspace baseline), shared/control-plane/
  mock-woocommerce each 1 passed. No existing engine file touched other
  than the new `[workspace]` table in `Cargo.toml`.

## Judgment calls & open questions for the user

- **Important: this worktree is built on an older baseline than your main
  checkout, and it matters for one specific area.** While reviewing 0.1's
  work I found that the docs (`docs/WOOCOMMERCE_WBS.md`,
  `docs/WOOCOMMERCE_ROADMAP.md`) and my briefing to the 0.1 agent both
  contained a claim — "the engine's `Cargo.toml` already has an `[[test]]`
  section and optional `e2e` feature gating `monero-wallet`/
  `monero-daemon-rpc`/etc." — that came from reading
  `/home/henry/Downloads/mokulo/Cargo.toml` (your main checkout) during the
  earlier gap-analysis pass, *not* this worktree. Your main checkout has
  uncommitted local changes (`git status` there shows `Cargo.toml`,
  `e2e/.gitignore`, `e2e/README.md`, `e2e/stagenet-wallets.json`,
  `src/cli.rs`, `src/config.rs`, `src/http/mod.rs`, `src/lib.rs`,
  `src/main.rs`, `tests/e2e_stagenet.rs`, `tests/support/mod.rs` modified,
  plus new untracked `src/e2e_wallet.rs`, `src/http/e2e_dev.rs`,
  `static/e2e-shop.html`) that this worktree — branched from the last
  *commit*, per how `EnterWorktree` works — never received, since
  uncommitted changes in one working tree aren't visible from another.
  - **What I checked to scope the actual impact**: re-read this worktree's
    real `src/lib.rs`, `src/http/mod.rs` (router table, `AuthedTenant`'s
    `Bearer` parsing), and `Cargo.toml` directly. The router paths, the
    `Authorization: Bearer sk_...` auth format, the migration runner, the
    `KeyCustody` trait, the rate limiter, HMAC signing, and the absence of
    `argon2`/`governor` — everything this WBS's implementation tasks
    actually depend on — are identical in both places. The only thing
    that's genuinely different is your in-progress e2e-tooling refactor
    (feature-gating the real-transaction-construction dependencies, a
    `--e2e` dev-server mode, a demo shop page) — unrelated to the
    WooCommerce/control-plane/SEV-SNP work, as far as I can tell.
  - **What I did about it**: nothing destructive — I left your main
    checkout completely untouched and am continuing to build in this
    worktree, since copying or guessing at unfinished WIP from outside it
    seemed riskier than proceeding on the last committed state. I fixed the
    incorrect claim in the 0.1 agent's task (it correctly reported the
    `[[test]]`/`e2e`-feature structure wasn't actually present, which I'd
    initially mis-read as *it* being wrong — it wasn't, I was, for briefing
    it off the wrong checkout).
  - **What you'll want to do when you're back**: decide whether to commit
    that e2e-tooling WIP, and if so, merge/rebase this branch
    (`worktree-woocommerce-roadmap-doc`, currently local-only — recall push
    to `origin` is denied under current credentials) on top of it once it
    lands. Until then I'll keep treating this worktree's committed baseline
    as ground truth and will flag it again if a later task's diff would
    touch any of the files listed above, since those are the ones a future
    merge will need to reconcile.
