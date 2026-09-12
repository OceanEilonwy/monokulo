# MoneroPay Cloud — WooCommerce MVP Work Breakdown Structure

Companion to `docs/WOOCOMMERCE_ROADMAP.md` (read that first for the *why*;
this is the *what, in what order*). Scope: everything needed to reach a
private-beta go-live with a working WooCommerce integration and the SEV-SNP
key-custody gate in place. Excludes Stage 14 (legal/compliance — not
engineering work) and Stage 15 (Shopify — explicitly not being built yet).

Two tracks run in parallel with each other; within each track, order is
strict (each item depends on the one above it unless noted). "Outcome" is
what becomes true when the task is done; "test" is how that's checked —
every leaf is sized to be unit- or integration-testable on its own.

- **0. Foundations** (blocks both tracks below)
  - 0.1 Add the Cargo workspace and empty crate skeletons
    - outcome: `cargo build --workspace` and `cargo test --workspace` succeed
      across the existing engine plus three new empty crates
      (`shared`, `control-plane`, `mock-woocommerce`)
    - what: add `[workspace]` to the root `Cargo.toml`; scaffold each new
      crate with a minimal `lib.rs`/`main.rs`
    - test: `cargo build --workspace && cargo test --workspace` in CI; a
      trivial placeholder test in each new crate is enough at this step
  - 0.2 Move secret-token generation/hashing into `shared`
    - outcome: `shared::auth` provides hash/verify/generate; the engine's
      `src/auth.rs` calls into it instead of duplicating the logic
    - what: cut the logic and its existing tests from the engine into
      `shared`; update the engine's call sites and imports
    - test: the existing token hash/verify unit tests now live in and pass
      from `shared`; the engine's own test suite still passes unmodified,
      proving no behavioral drift from the move
  - 0.3 Move HMAC webhook signing/verification into `shared`
    - outcome: `shared::webhook_sign` provides sign/verify; the engine's
      `src/webhook_sign.rs` calls into it
    - what: same cut/move pattern as 0.2
    - test: existing signature unit tests move and pass; add one
      known-vector test (fixed payload → fixed signature) as a drift guard

- **1. Track A — WooCommerce protocol** (parallel with Track B)
  - 1.1 Control-plane accounts
    - 1.1.1 `users` table + signup
      - outcome: `POST /signup {email, password}` creates a hashed-password
        row, rejects a duplicate email
      - what: migration for `users`; handler using `shared::auth`
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
        `POST /api/v1/admin/tenants`
      - what: `reqwest`-based client, in `shared` or `control-plane`
      - test: integration test against a real (dev/local) engine instance —
        call the client, assert a genuine `pk_`/`sk_` comes back and the
        tenant exists via the engine's own `GET /api/v1/admin/tenant`
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
        plugin's settings page, plus a CLI driver
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
        before starting 1.5
  - 1.5 Real WooCommerce plugin (each step ports something 1.4 already proved)
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
        status mapping, `event_id` dedupe via order meta
      - test: table-driven PHPUnit unit tests (each engine status →
        expected WC status) plus signature verification; one full
        `wp-env` + stagenet e2e test mirroring 1.4.5 with the real plugin
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
    - 2.1.1 Socket-based `KeyCustody` implementation
      - outcome: `PlainKeyCustody`'s existing test suite passes verbatim
        against a new implementation that talks over a Unix socket to a
        separate process, instead of running in-process
      - what: a small server binary plus a client adapter implementing the
        `KeyCustody` trait by forwarding calls over the socket
      - test: port `src/key_custody/plain.rs`'s test suite to run against
        this implementation — identical assertions, different backend —
        this *is* the acceptance test for the step
    - 2.1.2 Engine wired to the socket-based implementation
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
      - outcome: the engine + `key-custody-service` pair from 2.1.2 running
        inside the confidential VM, reachable the same way as on a plain
        dev box
      - what: deployment scripting/service units for both processes
      - test: re-run 2.1.2's regression suite against the deployed
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
