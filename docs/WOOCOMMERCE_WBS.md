# MoneroPay Cloud — WooCommerce MVP Work Breakdown Structure

Companion to `docs/WOOCOMMERCE_ROADMAP.md` (read that first for the *why*;
this is the *what, in what order*). Scope: everything needed to reach a
private-beta go-live with a working WooCommerce integration and the SEV-SNP
key-custody gate in place. Excludes Stage 14 (legal/compliance — not
engineering work) and Stage 15 (Shopify — explicitly not being built yet).

Two tracks *can* run in parallel with each other on paper; within each
track, order is strict (each item depends on the one above it unless
noted). "Outcome" is what becomes true when the task is done; "test" is how
that's checked — every leaf is sized to be unit- or integration-testable on
its own.

## How we're actually working through this

On a Pro subscription, usage is one shared, account-wide pool (rolling
~5-hour windows) — running things in parallel doesn't add throughput, it
just spends the same budget faster and risks leaving several branches
half-finished when a limit hits, instead of one branch fully done. So,
despite the two tracks above being independent on paper:

- **Sequential, not parallel.** Work Track A to a reasonable stopping point
  before starting Track B, rather than both at once. Track A first: it's
  larger, has no external infra dependency, and every leaf is testable in a
  plain dev environment — a better fit for focused sessions than Track B's
  cloud provisioning and attestation waiting.
- **Foundations (0.x) first, in one pass** — small, mechanical, unblocks
  everything downstream.
- **Each leaf (or a small related group) is one unit of delegated work**:
  implement, test, commit, then move on — never leave a leaf half-done
  across a stopping point. This is exactly what "minimal testable chunk"
  in this doc's own design was for.
- **`work_notes.md`** (repo root) is the running hand-off brief: what's
  done, current state, and any judgment calls made without waiting for
  input. Read that alongside this file for where things actually stand,
  since this file is the plan and doesn't change as work completes.
- Track B's cloud/attestation steps (2.2.x) are a good candidate for a
  background job specifically because they involve real waiting — better
  than idling a foreground session on them.

- **0. Foundations** (blocks both tracks below)
  - 0.1 Add the Cargo workspace and empty crate skeletons
    - outcome: `cargo build --workspace` and `cargo test --workspace` succeed
      across the existing engine plus three new empty crates
      (`shared`, `control-plane`, `mock-woocommerce`)
    - what: add `[workspace]` to the root `Cargo.toml`; scaffold each new
      crate with a minimal `lib.rs`/`main.rs`; give `mock-woocommerce` (and
      any control-plane test target that wants one) `moneropay-core` itself
      as a path dependency — confirmed via `src/lib.rs` that every module
      (`http`, `store`, `key_custody`, ...) is already `pub`, so this is a
      real, usable library dependency, not just the binary
    - test: `cargo build --workspace && cargo test --workspace` in CI; a
      trivial placeholder test in each new crate is enough at this step
  - 0.2 Move secret-token generation/hashing into `shared`
    - outcome: `shared::auth` provides hash/verify/generate; the engine's
      `src/auth.rs` calls into it instead of duplicating the logic
    - what: cut the logic and its existing tests from the engine into
      `shared`; update the engine's call sites and imports. Confirmed this
      is SHA-256 (`src/auth.rs`'s `hash_secret_token`), not argon2 — the
      engine has no argon2 dependency at all, despite `docs/DESIGN.md` §15's
      table listing one; that table looks stale against the real code.
      Nothing to fix in the engine itself (SHA-256 is the *correct* choice
      for a machine-generated, high-entropy token, per that file's own doc
      comment) — just don't assume this helper does what a password hash
      needs; see 0.4.
    - test: the existing token hash/verify unit tests now live in and pass
      from `shared`; the engine's own test suite still passes unmodified,
      proving no behavioral drift from the move
  - 0.3 Move HMAC webhook signing/verification into `shared`
    - outcome: `shared::webhook_sign` provides sign/verify; the engine's
      `src/webhook_sign.rs` calls into it
    - what: same cut/move pattern as 0.2. Confirmed format: hex-encoded
      HMAC-SHA256 over the raw payload bytes, header `X-MoneroPay-Signature`,
      verified with a constant-time comparison (`Mac::verify_slice`, not
      `==`) specifically to avoid a byte-at-a-time forgery oracle — that
      constant-time requirement matters again at 1.5.4, where it has to be
      re-implemented in PHP.
    - test: existing signature unit tests move and pass; add one
      known-vector test (fixed payload → fixed signature) as a drift guard
      — this vector is also what 1.5.4's PHP implementation must be checked
      against
  - 0.4 Add an argon2 password-hashing helper to `shared` (new, not a move)
    - outcome: `shared::password` provides hash/verify for human-chosen
      passwords, backed by `argon2`
    - what: add the `argon2` crate as a dependency (it isn't one anywhere in
      the workspace today) and a small hash/verify wrapper with sane
      defaults (current OWASP-recommended cost parameters)
    - test: unit tests — a hashed password verifies against the original and
      rejects a wrong one; hashing the same password twice produces two
      different hashes (salt is per-call, not fixed)
  - 0.5 Extract the migration runner into `shared`
    - outcome: `shared::migrations::apply(conn, migrations: &[(i64, &str)])`
      exists; `store.rs` calls it instead of its own copy of
      `apply_migration_list`; behavior is unchanged
    - what: move the function (transactional per-migration apply, tracked in
      a `schema_migrations` table, all-or-nothing on a failing migration) —
      found already written generically enough to reuse as-is, so this is a
      pure move, not a rewrite
    - test: the engine's existing migration tests move with it and still
      pass, notably `a_failing_migration_leaves_neither_its_schema_changes_
      nor_its_version_row` and `reopening_an_existing_database_file_does_
      not_reapply_migrations` — both are regression guards for exactly the
      failure modes a second, hand-rolled migration mechanism for the
      control-plane database would otherwise risk reintroducing
  - 0.6 Cross-crate test harness for a real, bound engine instance
    - outcome: any crate in the workspace can start a real
      `moneropay-core` instance bound to an ephemeral local port for the
      duration of a test and get back its real address, then tear it down
    - what: a small helper (in `shared`, or a dev-only sibling crate) that
      takes a `Config`, builds the router via the now-confirmed-public
      `moneropay_core::http::build_router`, and binds it with `axum::serve`
      on `127.0.0.1:0` in a background task — the same construction
      `tests/e2e_stagenet.rs` already uses, just packaged for reuse instead
      of copied. Deliberately *not* the `tower::ServiceExt::oneshot`
      no-real-socket pattern the engine's own router tests use — that skips
      the network stack entirely, which doesn't work for 1.2.1's test, where
      a genuine `reqwest` client (standing in for the control plane calling
      a *separately deployed* engine) needs a real socket to connect to
    - test: a smoke test in the harness's own crate — start it, `GET
      /static/moneropay-client.js` (unauthenticated, no side effects) with a
      real `reqwest::Client` against the returned address, assert 200

- **1. Track A — WooCommerce protocol** (parallel with Track B)
  - 1.1 Control-plane accounts
    - 1.1.1 `users` table + signup
      - outcome: `POST /signup {email, password}` creates a hashed-password
        row, rejects a duplicate email
      - what: migration for `users` (applied via 0.5's `shared::migrations`,
        not a second mechanism); handler using 0.4's `shared::password`
        helper — *not* `shared::auth` (0.2), which hashes machine-generated
        `sk_` tokens with SHA-256 and is the wrong tool for a human password
      - test: integration test — duplicate signup returns a conflict;
        direct DB read after signup shows no plaintext password
    - 1.1.2 Login + session
      - outcome: `POST /login` returns a session token; a protected route
        accepts it and rejects a wrong password or missing token
      - what: `sessions` table; login handler; an axum extractor resolving
        the session from the token
      - test: integration test covering correct login, wrong password
        (401), valid session on a dummy protected route (200), no session
        (401)
    - 1.1.3 Logout / session revocation
      - outcome: `POST /logout` invalidates a session; reusing that token
        afterward gets 401
      - what: delete/mark the session row
      - test: integration test — logout, then re-use the old token, assert
        401
  - 1.2 Tenant provisioning against the engine
    - 1.2.1 Engine admin-API client
      - outcome: a typed function `create_tenant(wallet_fields) ->
        Result<TenantCreds>` that calls the engine's real
        `POST /api/v1/admin/tenants`, and a matching `get_tenant(sk_)`
        wrapping `GET /api/v1/admin/tenant` for 1.2.2's verification and the
        later "adopt"-equivalent needs
      - what: `reqwest`-based client, in `shared` or `control-plane`.
        Confirmed the real auth format from `AuthedTenant`'s extractor in
        `src/http/mod.rs`: a plain `Authorization: Bearer sk_...` header —
        worth stating explicitly here so the client is right on the first
        try rather than discovered by a failing request
      - test: using 0.6's harness, start a real engine instance bound to a
        real local port; call the client with a genuine `reqwest::Client`
        against it; assert a genuine `pk_`/`sk_` comes back and the tenant
        exists via `get_tenant`
    - 1.2.2 `store_connections` + authenticated "create tenant" endpoint
      - outcome: a logged-in user `POST`ing wallet fields to `/connections`
        ends up with a `store_connections` row pointing at a real engine
        tenant
      - what: migration; handler combining 1.1.2's session auth and 1.2.1's
        client
      - test: integration test — as a logged-in user, submit valid wallet
        fields, assert the row exists and the referenced tenant is real
    - 1.2.3 `sk_` at-rest encryption
      - outcome: the stored `sk_` is encrypted, not plaintext, and
        decrypts back to the exact original value when needed
      - what: wrap 1.2.2's insert/read with encrypt/decrypt using an
        env-provided key
      - test: unit test — encrypt/decrypt round-trip; integration test —
        raw DB row does not contain the plaintext `sk_` substring
  - 1.3 Dashboard UI (thin wrapper over 1.1/1.2)
    - 1.3.1 Signup/login pages
      - outcome: a human can sign up and log in via a browser form, ending
        in a session cookie
      - what: two templates + form-post handlers wrapping 1.1.1/1.1.2
      - test: integration test posting form-encoded data at the same
        assertions as 1.1.1/1.1.2
    - 1.3.2 Wallet-connection form
      - outcome: a logged-in user submitting the form ends up with a
        `store_connections` row, same effect as 1.2.2 via browser form-post
      - what: HTML form + handler wrapping 1.2.2
      - test: integration test, form-encoded version of 1.2.2's assertions
    - 1.3.3 Order list/detail + webhook management pages
      - outcome: a logged-in user sees their tenant's real orders/webhooks
      - what: handlers proxying the engine's own
        `GET /api/v1/admin/tenant/orders` etc. using the decrypted `sk_`
      - test: integration test — seed an order via the engine's public API
        directly, assert it appears in the dashboard's response
  - 1.4 Connect-flow protocol, proven with a mock before any PHP exists
    - 1.4.1 Generic connect start/finish endpoints
      - outcome: hitting start redirects to a confirm screen; confirming
        redirects back to `return_url` with a single-use signed token;
        posting that token to finish returns real credentials once, and
        fails on reuse
      - what: token issuance/validation (table or signed token, either
        works); two handlers
      - test: integration test — full round trip asserting the token
        works exactly once; a second finish call with the same token fails
    - 1.4.2 Mock WooCommerce: fake merchant site
      - outcome: a standalone binary that drives 1.4.1's flow the way a
        real plugin will, ending up holding real `pk_`/`sk_`
      - what: new `mock-woocommerce` crate — small axum app playing the
        plugin's settings page, plus a CLI driver. This is **Rust, not
        PHP** — it's a stand-in that speaks the same HTTP protocol a real
        plugin would, not a lightweight WordPress install, so no second
        language is needed to prove the protocol. PHP only becomes
        necessary at 1.5, and that's a WordPress platform constraint (only
        PHP can be loaded as a WordPress plugin — confirmed even the modern
        Blocks-based checkout still needs a PHP-side `wp_register_script`
        call to register a payment method at all), not a stylistic choice —
        see 1.5's note
      - test: run it against a live control plane in CI; exit 0 with valid
        credentials in hand is the pass condition
    - 1.4.3 Mock order creation + checkout redirect
      - outcome: the mock calls the engine's public order-creation
        endpoint with its `pk_` and gets a valid redirect target back
      - what: driver function in the mock crate
      - test: integration test — assert response shape, and that
        `/pay/v1/{pk}/{payment_id}` returns 200 HTML when fetched directly
    - 1.4.4 Webhook registration + mock receiver
      - outcome: connect registers a webhook at the mock's receiver URL;
        the mock verifies a real signed delivery
      - what: extend 1.4.1's finish step to register the webhook; add a
        receiver route to the mock checking `X-MoneroPay-Signature`
      - test: unit test for signature verification against known vectors
        (via `shared::webhook_sign`); integration test — force a real
        delivery and assert the mock logs a verified event
    - 1.4.5 Full stagenet end-to-end test
      - outcome: one test running signup → connect → order → real
        stagenet payment → webhook received & verified, mock ending in the
        correct final state
      - what: wire 1.4.1–1.4.4 together using `e2e/stagenet-wallets.json`'s
        existing fixtures
      - test: this task *is* the test — its own pass/fail is the gate
        before starting 1.5. Follows the existing convention
        `tests/e2e_stagenet.rs` already established for exactly this reason
        (real network dependency, not hermetic): `#[ignore]`d by default, run
        explicitly via `cargo test -- --ignored --nocapture`, wired into CI
        as its own separate job rather than the default `cargo test` pass —
        worth deciding this now rather than having it silently skipped in CI
        by accident later
  - 1.5 Real WooCommerce plugin, in PHP by necessity (each step ports
    something 1.4 already proved). PHP is required here because WordPress
    can only load plugins written in PHP, and — checked directly, not
    assumed — even the newer Blocks-based checkout still needs a PHP-side
    `wp_register_script` call before a payment method can register at all;
    the public REST/Store API can drive an *already-registered* payment
    method but has no way to add one to the checkout from outside. Kept
    deliberately thin regardless: registration, one outbound HTTP call, and
    a webhook receiver — every real decision stays in the Rust engine.
    - 1.5.1 Gateway skeleton registers in WooCommerce
      - outcome: the plugin, installed on a WordPress site, shows "Monero
        (via MoneroPay Cloud)" as a checkout option (disabled state is
        fine at this step)
      - what: `WC_Gateway_MoneroPay` class + plugin bootstrap file
      - test: `wp-env`/WooCommerce PHPUnit test asserting the gateway ID
        appears in the available-gateways list
    - 1.5.2 `process_payment` → order creation → redirect
      - outcome: placing an order with this gateway calls the real order
        API and redirects the customer to `/pay/v1/...`
      - what: `process_payment()` using `wp_remote_post`
      - test: PHPUnit test with a mocked HTTP client asserting the request
        body and redirect target; one live integration test against a dev
        engine for the full round trip
    - 1.5.3 Connect flow ported into the settings screen
      - outcome: clicking "Connect your Monero wallet" performs 1.4.1's
        flow for real; the gateway becomes enabled
      - what: settings-page button + `wp_remote_post` calls mirroring
        1.4.2's already-proven logic
      - test: PHPUnit test simulating the redirect-back request, asserting
        settings save and the gateway enables; reuse 1.4.5's e2e harness
        with a headless WordPress instance in place of the mock
    - 1.5.4 Webhook receiver + order status mapping
      - outcome: a real stagenet payment through a real WooCommerce
        checkout ends with the WC order in `processing`/`completed`
      - what: `woocommerce_api_{id}` hook handler, HMAC verification,
        status mapping, `event_id` dedupe via order meta. The HMAC check is
        a fresh PHP implementation — `shared::webhook_sign` (0.3) is Rust
        and can't be called from PHP — but it must match byte-for-byte: hex
        HMAC-SHA256 over the raw body, header `X-MoneroPay-Signature`, and
        **compared with `hash_equals()`, never `===`**. The Rust side uses a
        constant-time comparison specifically to avoid a byte-at-a-time
        forgery timing oracle (see `src/webhook_sign.rs`'s own doc comment);
        a naive `===` in PHP would quietly reintroduce exactly that
        vulnerability on the merchant-facing side
      - test: a unit test running the PHP verifier against 0.3's fixed
        known-vector test case, asserting the identical result — catches a
        cross-language mismatch immediately rather than in a live webhook
        failing silently later; table-driven PHPUnit tests for the status
        mapping (each engine status → expected WC status); one full
        `wp-env` + stagenet e2e test mirroring 1.4.5, under the same
        `#[ignore]`/explicit-run convention
  - 1.6 Distribution
    - 1.6.1 wordpress.org submission
      - outcome: plugin installable from wp-admin's plugin search
      - what: SVN repo, `readme.txt`, directory-guideline compliance pass
      - test: not code — acceptance criterion is a clean install on a
        fresh WordPress site, checked manually once
    - 1.6.2 Deep install link
      - outcome: a link from our site lands a logged-in wp-admin merchant
        directly on the plugin's install button
      - what: static link construction
      - test: manual check; optionally a link-liveness smoke test
  - 1.7 Exchange-rate automation (parallel within Track A, non-blocking)
    - 1.7.1 Coingecko provider
      - outcome: `provider = "coingecko"` yields live per-currency rates
        instead of hand-entered ones
      - what: new implementation of the existing exchange-rate provider
        trait + config wiring
      - test: unit test against a recorded HTTP fixture (no live network
        call in CI); one manual smoke test against the real API

- **2. Track B — Go-live gate: SEV-SNP key custody** (parallel with Track A)
  - 2.1 `key-custody-service`, plaintext first (no hardware TEE yet)
    - 2.1.1 Wire protocol: serializable DTOs for every `KeyCustody` operation
      - outcome: every value that needs to cross the socket boundary — each
        method's arguments and its `Result` — has a serializable
        representation, proven to round-trip
      - what: checked the actual trait (`src/key_custody/mod.rs`) rather
        than assuming from `docs/DESIGN.md` §6.2's paraphrase — signatures
        match closely, but **none of `KeyCustodyError`, `MatchedOutput`, or
        `WalletHandle` derive `Serialize`/`Deserialize` today**, and the
        trait's real arguments/returns include `monero`-crate types
        (`Address`, `Transaction`, `ViewPair`) that don't obviously serialize
        cleanly either. This needs a small set of wire-level DTOs in the new
        `key-custody-service`/client crate (not changes to the engine's own
        types) that convert at the boundary — e.g. `WalletHandle`'s
        underlying `Uuid` serializes trivially, `KeyCustodyError` maps to a
        wire enum by variant, `Transaction` likely needs its existing
        wire/consensus-encoding (`monero`-rs already has one, since it comes
        off the chain) rather than a fresh serde derive
      - test: unit tests — encode/decode round-trip for each DTO, including
        the redacted `WalletMaterial` (must round-trip the real key bytes
        for a real implementation, even though its `Debug` impl redacts
        them for logs — a serialization bug here is exactly the kind of
        thing that fails silently as "empty view key" rather than loudly)
    - 2.1.2 Socket-based `KeyCustody` implementation
      - outcome: `PlainKeyCustody`'s existing test suite passes verbatim
        against a new implementation that talks over a Unix socket to a
        separate process, instead of running in-process
      - what: a small server binary plus a client adapter implementing the
        `KeyCustody` trait by forwarding calls over the socket, using 2.1.1's
        DTOs on the wire
      - test: port `src/key_custody/plain.rs`'s test suite (confirmed: 12
        existing tests) to run against this implementation — identical
        assertions, different backend — this *is* the acceptance test for
        the step
    - 2.1.3 Engine wired to the socket-based implementation
      - outcome: the full engine (scanning, order creation, everything)
        works with key material living in the separate process
      - what: swap which `KeyCustody` implementation the engine constructs
        at startup, behind a config flag
      - test: the engine's existing integration tests (order creation,
        scanning) pass unmodified against this configuration — a
        regression check, not a new test
  - 2.2 SEV-SNP confidential VM
    - 2.2.1 Provision and verify attestation
      - outcome: a running VM whose attestation report verifies as
        genuinely SEV-SNP-protected, with AMD's July 2025 microcode patch
        confirmed present
      - what: provider console/API provisioning; attestation-verification
        tooling
      - test: an attestation-verification script — its pass/fail on the
        signature chain and reported patch level is the test
    - 2.2.2 Deploy 2.1's split inside the VM
      - outcome: the engine + `key-custody-service` pair from 2.1.3 running
        inside the confidential VM, reachable the same way as on a plain
        dev box
      - what: deployment scripting/service units for both processes
      - test: re-run 2.1.3's regression suite against the deployed
        instance over the network
  - 2.3 Hardening
    - 2.3.1 Backup + restore drill
      - outcome: a backup taken from the running instance restores cleanly
        on a fresh box, with all tenants and orders intact
      - what: backup script/cron; a documented restore procedure
      - test: actually perform the restore once against a copy; diff
        tenant/order counts before and after as the pass condition
    - 2.3.2 Incident runbook
      - outcome: a written runbook exists covering "box compromised" as a
        privacy incident, not a funds-loss one
      - what: write it
      - test: not code-testable; a tabletop walkthrough is the closest
        equivalent

- **3. Convergence — private beta go-live** (depends on both tracks)
  - outcome: the real plugin (1.5) works end-to-end against the hardened,
    SEV-SNP-backed hosted engine (2.2/2.3), with the admin API
    network-isolated per the Option A decision, ready to invite the first
    private-beta merchants
  - what: point the production hosted deployment at the remaining Stage-1
    infra items this plan assumed throughout but hadn't yet built for real
    (domain, TLS, admin-API network isolation, basic monitoring) — this is
    where those get done against the real box, now that there's a real
    `KeyCustody` backend worth protecting
  - test: re-run 1.5.4's full stagenet end-to-end test one more time,
    against the real hosted+SEV-SNP instance instead of a dev box — its
    pass/fail is the actual "ready to invite beta users" gate
