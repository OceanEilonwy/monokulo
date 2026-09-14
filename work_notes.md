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

- WBS 1.5.2 done: `WC_Gateway_MoneroPay::process_payment( $order_id )` - placing
  a real WooCommerce order with this gateway selected now calls the real
  engine's `POST /api/v1/t/{pk}/orders` and returns the `redirect` WooCommerce
  needs to send the customer to the engine's own real
  `/pay/v1/{pk}/{payment_id}` checkout page. Read WooCommerce's real installed
  source directly before writing anything, not just its doc comments: `WC_
  Payment_Gateway::process_payment()`'s own doc comment
  (`includes/abstracts/abstract-wc-payment-gateway.php`), `WC_Gateway_BACS`/
  `WC_Gateway_COD`'s own implementations (`includes/gateways/{bacs,cod}/
  class-wc-gateway-{bacs,cod}.php`), and `WC_Checkout::process_order_payment()`
  / `process_checkout()` (`includes/class-wc-checkout.php`) for exactly what
  happens to the returned array and to a thrown `Exception` - all three
  findings are documented in `process_payment()`'s own doc comment, not just
  here, since a future reader of that method shouldn't have to come back to
  this log to find them again.
  - **The settings-field gap, and the naming decision**: `process_payment()`
    needs an engine base URL and a tenant public key, and 1.5.1 left nothing
    but `enabled`/`title`/`description`. Added exactly two new plain text
    settings fields - `endpoint` and `public_key` - named to match
    `mock-woocommerce/src/lib.rs`'s own `ConnectedCredentials` struct field
    names verbatim (checked directly, not from memory), since that struct is
    what a real WBS 1.5.3 connect-flow callback will eventually have in hand
    and write back into these same two option keys - so 1.5.3 never has to
    rename anything here, just start writing to it programmatically instead of
    a merchant pasting it in by hand. No third field for a secret key: `POST
    /api/v1/t/{pk}/orders` is a public endpoint (confirmed directly against
    `src/http/public.rs::create_order` and its route registration in
    `src/http/mod.rs` before writing this, exactly as the brief said) - order
    creation only ever needs the public key in the URL path. Both the "why
    these two fields, why now" and "why these exact names" reasoning live on
    `WC_Gateway_MoneroPay::$api_base_url`'s own doc comment, not just here.
    Also added `is_available()` override (enabled *and* both fields
    non-empty) - the same "don't offer what you can't honor" principle 1.5.1's
    own disabled-by-default default already established, applied to the one
    new failure mode this step introduces (an enabled-but-unconfigured
    gateway would otherwise throw on a real customer's first attempt).
  - **Failure signaling, resolved against WooCommerce's real source, not
    guessed**: neither bundled gateway (`BACS`/`COD`) ever fails, so neither
    demonstrates the failure path directly - but `WC_Checkout::
    process_checkout()`, read directly, calls `process_order_payment()` (and
    therefore this gateway's `process_payment()`) from inside its own
    top-level `try { ... } catch ( Exception $e ) { wc_add_notice(
    $e->getMessage(), 'error' ); }`. So every failure branch in
    `create_engine_order()` (unconfigured gateway, `wp_remote_post()`
    returning a `WP_Error`, a non-200 engine response, a response missing
    `payment_id`) throws a plain `Exception` with a customer-safe message,
    never the raw engine/HTTP error text (that's logged instead, via a small
    `wc_get_logger()` wrapper, for the merchant to actually diagnose) - `wc_
    add_notice()`/checkout re-render is WooCommerce's own real mechanism for
    this, not a bespoke `'result' => 'fail'` shape this gateway invented.
  - **Deliberately does not mark the order paid or change its status** -
    `$order->payment_complete()` is never called here. The order WooCommerce
    just created is already `pending`/awaiting payment, and it stays exactly
    that until a real payment is actually observed on-chain (WBS 1.5.4's job,
    not built here). `process_payment()` does record the engine's own
    `payment_id` as order meta (`_moneropay_cloud_payment_id`) plus an order
    note, though - the one point in this whole flow that ever sees the
    WC-order/engine-order mapping, and 1.5.4's webhook receiver will need
    exactly that lookup later. Nothing here *consumes* that meta key - no
    webhook receiver exists yet - just not thrown away.
  - **`fiat_amount` as a plain two-decimal-place decimal string**
    (`number_format( (float) $order->get_total(), 2, '.', '' )`, never a
    locale-formatted `(string) $order->get_total()`), matched against the real
    parser it has to satisfy: read `src/exchange_rate.rs::compute_xmr_amount`
    directly and confirmed it rejects more than two decimal places, thousands
    separators, and scientific notation.
  - **The mocked-HTTP unit test** (`tests/ProcessPaymentTest.php`, 5 tests):
    uses WordPress's own real short-circuit mechanism, the `pre_http_request`
    filter (`wp-includes/class-http.php::WP_Http::request()`, read directly -
    confirmed it returns whatever the filter returns, verbatim, before ever
    touching the network), not a hand-rolled mock object - WordPress's HTTP
    API is procedural, there's no client to inject. Builds a real, saved
    `WC_Order` via WooCommerce's own `wc_create_order()`, asserts the *exact*
    request `process_payment()` sent (URL, method, `Content-Type`, and the
    full decoded JSON body - `fiat_amount`, `fiat_currency`,
    `merchant_order_id`), and the exact `/pay/v1/{pk}/{payment_id}` redirect
    shape using the canned response's own `payment_id`. Plus three negative
    tests (unconfigured gateway, non-200 engine response, `WP_Error`
    transport failure) proving `create_engine_order()`'s failure branches are
    real, exercised behavior, not just comments describing intent.
  - **The live integration test - the genuinely hard part, and a real,
    machine-specific networking obstacle actually hit, not assumed away**:
    `tests/LiveEngineIntegrationTest.php`, tagged `@group live-engine` and
    excluded from the default run in `phpunit.xml.dist` (identical reasoning
    to `tests/e2e_stagenet.rs`'s own `#[ignore]` convention on the Rust side -
    a real network/process dependency the default hermetic run must never
    silently acquire), run explicitly with `--group live-engine`.
    - Built the real engine: `cargo build --release` from the repo root,
      clean build, `target/release/moneropay-core`.
    - A minimal config (`/tmp/moneropay-engine-test/config.toml`, not
      committed - scratch state for this session) bootstraps one real
      self-hosted tenant via the existing `[wallet]` path in `main.rs`
      (reusing the exact same test view/spend key pair
      `e2e/moneropay-stagenet.toml` already uses - not secret, not funded,
      just real key material so `WalletMaterial::from_hex`/`KeyCustody::seal`
      succeed), `[exchange_rate] provider = "fixed"` with a `USD` rate (no
      live network needed), and a placeholder, deliberately-unreachable
      `[monero_node.stagenet]` entry - `Config::validate()` requires at least
      one `monero_node` entry and the wallet's network to match one, but
      order creation itself never dials it (confirmed for real: the engine
      boots and serves orders fine with the scanner loop failing/retrying
      forever in the background against a closed local port, exactly the
      failover behavior `daemon_fallback.rs` already documents). Boot log
      really did print `bootstrapped self-hosted tenant:
      public_key=pk_f7c59419dc4c50fcfe98ed28ebcb5d43ca3adecf54757d84` -
      confirmed order creation worked from the host with a plain `curl`
      before ever touching wp-env.
    - **The container-to-host networking question, actually investigated, not
      assumed**: read wp-env's own generated `docker-compose.yml` directly
      (`~/.wp-env/<instance>/docker-compose.yml`, the same file 1.5.1's own
      entry already learned to check) and found it already sets `extra_hosts:
      ['host.docker.internal:host-gateway']` on every one of its containers -
      so `host.docker.internal` genuinely does resolve inside `tests-cli` on
      this plain-Linux-Docker host (`getent hosts` confirmed it - Docker
      bridge gateway IP, not Docker Desktop magic), unlike the brief's own
      caution that this isn't a given. It just didn't help here: a `curl`
      from *inside* `tests-cli` to the engine (bound to `0.0.0.0:8180`) at
      that same IP genuinely timed out (`exit 7`), even though the identical
      `curl` from the *host itself* to that same bridge-gateway IP:port
      worked fine (`HTTP 415`, meaning the engine really received it) -
      root-caused to `ufw` being `active` on this host
      (`systemctl is-active ufw`) and Docker's well-documented interaction
      with it: a container reaching *out* to a host-bound port crosses the
      host's own `INPUT` chain, which `ufw`'s default-deny policy blocks,
      while a host-local process never crosses that boundary at all. No
      passwordless `sudo` was available to add a `ufw`/`DOCKER-USER` allow
      rule, and a system-wide firewall change felt out of scope for what one
      WBS step should be doing to someone's machine, so this wasn't forced
      through - flagged explicitly in case a future reader has root and wants
      the simpler path.
    - **What actually worked, verified end-to-end, not just reasoned about**:
      running the engine as a *container* on wp-env's own Docker network
      instead, reached by container name - container-to-container traffic on
      the same user-defined bridge network never crosses the host's `ufw`
      `INPUT` chain at all. `docker inspect` on the running `tests-cli`
      container gave the real network name
      (`wp-env-moneropay-cloud-9652ae59_default`, this specific wp-env
      instance's own project-scoped network - it changes if the instance hash
      changes). The compiled binary needed a container base with a
      compatible-or-newer glibc, since it was built against this build host's
      own (very new - 2.44, a rolling-release distro) glibc and a container
      with an older one fails to even exec it - checked, not assumed
      (`ld-linux-x86-64.so.2 --version` inside `archlinux:latest` matched
      2.44 exactly, so that's the base image used; a statically-linked musl
      build would sidestep this entirely but wasn't needed here). Launched
      with `docker run -d --name moneropay-engine-test --network
      wp-env-moneropay-cloud-9652ae59_default -v .../target/release/
      moneropay-core:/moneropay-core:ro -v /tmp/moneropay-engine-test:/cfg -w
      /cfg archlinux:latest /moneropay-core /cfg/config.toml`, reusing the
      same bind-mounted config/db directory the host-run instance had already
      bootstrapped a tenant into (so the container instance skipped
      bootstrap, idempotently, and reused the same `pk_...` - confirmed by
      its boot log *not* printing a second "bootstrapped" line). `curl
      http://moneropay-engine-test:8180/...` from inside `tests-cli`
      (real command, via `wp-env run tests-cli curl ...`) returned a real
      `200 OK` with the order-status JSON for the order created earlier via
      the host `curl` - proof the path works before ever running PHPUnit
      against it. Full exact commands for both the working path and the
      `host.docker.internal` path that didn't work here are in
      `tests/LiveEngineIntegrationTest.php`'s own class doc comment, not just
      this log.
    - `tests/live-engine.local.json` (gitignored - see `.gitignore`'s own
      comment on it: the container name and freshly-bootstrapped `pk_...` are
      both specific to whichever local Docker setup started the engine, and a
      fresh DB mints a new `pk_...` every time, so there's nothing stable
      here for two developers to share) is how the test learns the
      endpoint/public_key to use, rather than environment variables - checked
      first and confirmed `wp-env run` shells out to plain `docker compose
      exec` with no host-env-var passthrough and no `.wp-env.json` mechanism
      for injecting per-run values, so a file under `tests/` (already
      bind-mounted into the container) was the direct option, not env
      plumbing that doesn't exist yet.
    - **The real, observed test runs** (both via `newgrp docker -c "..."` per
      this environment's own docker-group quirk, and
      `NODE_OPTIONS='--no-network-family-autoselection'` before every
      `wp-env` invocation per 1.5.1's own toolchain note):
      - Mocked-HTTP suite (now includes `ProcessPaymentTest.php` alongside
        1.5.1's `GatewayRegistrationTest.php`, `LiveEngineIntegrationTest.php`
        correctly excluded by its group):
        `NODE_OPTIONS='--no-network-family-autoselection' npx @wordpress/env
        run tests-cli --env-cwd=wp-content/plugins/moneropay-cloud
        vendor/bin/phpunit --testdox` ->
        ```
        PHPUnit 9.6.36 by Sebastian Bergmann and contributors.

        Gateway Registration
         ✔ Gateway is registered with woocommerce
         ✔ Gateway is disabled by default

        Process Payment
         ✔ Process payment sends expected request and returns engine redirect
         ✔ Process payment throws when gateway is not configured
         ✔ Process payment throws when engine returns non 200
         ✔ Process payment throws when the engine is unreachable

        Time: 00:00.102, Memory: 89.00 MB

        OK (6 tests, 23 assertions)
        ```
      - Live-engine test, against the real container-based engine described
        above: same command plus `--group live-engine` ->
        ```
        PHPUnit 9.6.36 by Sebastian Bergmann and contributors.

        Live Engine Integration
         ✔ Process payment creates a real engine order and redirects to it

        Time: 00:00.052, Memory: 89.00 MB

        OK (1 test, 12 assertions)
        ```
      - Not just trusted: independently queried the engine's own SQLite
        database directly afterward (`sqlite3 /tmp/moneropay-engine-test/
        moneropay.db "select id, merchant_order_id, fiat_amount,
        fiat_currency, status, created_at from orders order by created_at
        desc limit 5;"`), a *third*, fully independent check beyond the
        test's own assertions and its own HTTP-status-endpoint cross-check -
        real output:
        ```
        pay_6f4d376b8c7148cb93d9e720d2a0a75a|10|5.00|USD|pending|1789377475
        pay_0e1e4b4c41514db5840d850fff0ff3ce|host-smoke-test|42.50|USD|pending|1789377076
        ```
        (`10` is the real WooCommerce order id `wc_create_order()` assigned
        during the PHPUnit run; the second row is the earlier host-`curl`
        smoke test that first proved the engine itself worked, before wp-env
        was involved at all.)
      - Torn down afterward (`docker rm -f moneropay-engine-test`, host
        `moneropay-core` process killed) - nothing was left running. Publishes
        no port to the host, so it never conflicted with anything; the
        `tests/live-engine.local.json` left in the tree points at now-stopped
        infrastructure and needs regenerating (per its own referenced doc
        comment) before the live-engine group can pass again.
  - **Files touched**: `plugins/moneropay-cloud/includes/
    class-wc-gateway-moneropay.php` (the two new settings-field properties,
    `init_form_fields()` additions, `is_available()` override,
    `process_payment()`, `create_engine_order()`, and three small private URL/
    logging helpers), `plugins/moneropay-cloud/phpunit.xml.dist` (the
    `live-engine` group exclusion), `plugins/moneropay-cloud/.gitignore`
    (`tests/live-engine.local.json`), new `plugins/moneropay-cloud/tests/
    {ProcessPaymentTest.php,LiveEngineIntegrationTest.php}`. No engine-side
    (Rust) file touched - this step only ever *calls* the engine's existing,
    already-shipped public API.
  - **Not done / explicitly out of scope, matching this step's own stated
    boundary**: the real one-click connect flow (WBS 1.5.3 - these two
    settings fields are explicitly a manual stand-in, documented as such in
    three places: the field descriptions themselves, `$api_base_url`'s doc
    comment, and here), the webhook receiver and any consumption of the
    `_moneropay_cloud_payment_id` meta this step writes (WBS 1.5.4), and any
    change to the order's WooCommerce status beyond what order creation itself
    already sets.

- WBS 1.5.1 done: `WC_Gateway_MoneroPay` + a plugin bootstrap file, registering
  "Monero (via MoneroPay Cloud)" as a (disabled) WooCommerce checkout option -
  the first PHP/WordPress code in this repo, and the first step of Track A's
  1.5 (real WooCommerce plugin). This entry is unusually long because getting
  a working `wp-env`/PHPUnit toolchain running for the first time surfaced
  several real, hit-not-guessed environment problems worth a future reader
  not having to rediscover - the plugin code itself, once the toolchain
  actually worked, was comparatively simple.
  - **Where it lives, and why not the roadmap's own literal sketch**:
    `plugins/moneropay-cloud/` - not `plugins/woocommerce/`, the path
    `docs/WOOCOMMERCE_ROADMAP.md` §3.1's directory sketch used. Checked, not
    assumed: `.wp-env.json`'s own plugin-source basename derivation (its
    `parse-source-string.js`, read directly) means whatever a plugin's own
    directory is named becomes literally what gets mounted under
    `wp-content/plugins/` - if this plugin's own directory were named
    `woocommerce`, it would collide with the real WooCommerce plugin's own
    directory the moment both are listed in the same `.wp-env.json` (which
    they have to be, since this plugin's tests need real WooCommerce running
    too). `plugins/moneropay-cloud/` keeps the roadmap's own `plugins/`
    top-level convention (room for a future `plugins/shopify/` per Stage 15)
    while giving this plugin its own real, collision-free slug - also the
    correct convention regardless, since WordPress' own norm is folder name =
    plugin slug = text domain, all three of which are `moneropay-cloud` here.
  - **Gateway identity**: `id = 'moneropay_cloud'` (permanent per its own
    doc comment - it's the WooCommerce settings option-array key, the
    `$order->get_payment_method()` value on every order, and the future
    `woocommerce_api_{id}` webhook suffix WBS 1.5.4 will register), plugin
    slug/text-domain `moneropay-cloud`, checkout title "Monero (via MoneroPay
    Cloud)" (the WBS's own outcome text, verbatim, as the field's default).
  - **The `payment_gateways()` vs. `get_available_payment_gateways()`
    ambiguity, resolved against WooCommerce's real installed source, not
    guessed at or left for later**: once `wp-env` had a real WooCommerce
    11.1.0 checkout, read
    `wp-content/plugins/woocommerce/includes/class-wc-payment-gateways.php`
    directly. `payment_gateways()` returns every gateway WooCommerce's own
    `init()` built from the `woocommerce_payment_gateways` filter, with *no*
    enabled/applicable filtering at all. `get_available_payment_gateways()`
    additionally requires `$gateway->is_available()` per gateway - and
    `WC_Payment_Gateway::is_available()`
    (`includes/abstracts/abstract-wc-payment-gateway.php`, also read
    directly) short-circuits `false` the instant `$this->enabled !== 'yes'`,
    before any currency/cart check. Since this gateway genuinely ships
    disabled by default (WBS 1.5.1's own stated outcome), asserting its
    presence in `get_available_payment_gateways()`'s result would assert
    something this step's own correct behavior makes false by construction.
    `tests/GatewayRegistrationTest.php` therefore asserts against
    `payment_gateways()` - "real, registered checkout option, independent of
    enabled state" is what the WBS's own outcome language actually describes
    - and additionally asserts the *negative* against
    `get_available_payment_gateways()` (a disabled gateway is correctly
    excluded), so both of WooCommerce's real, distinct gateway-listing
    methods are exercised for the behavior actually true of each, not just
    the one this step happens to need. Full reasoning is in the test file's
    own class doc comment, not just here.
  - **Toolchain trouble #1 - Node's Happy Eyeballs vs. this sandbox's missing
    IPv6 route, a real problem, not a flaky network**: `npx @wordpress/env
    start` failed immediately and deterministically with a generic
    `AggregateError [ETIMEDOUT]` at its very first "Reading configuration"
    step, every time, even though plain `curl` to the exact same URLs
    (`downloads.wordpress.org`, `raw.githubusercontent.com`) succeeded
    instantly. Root-caused by not trusting "the network must be flaky" and
    instead reproducing it minimally: `curl -6` to any external host fails
    immediately in this sandbox (no IPv6 route at all), and Node 24 enables
    `net.getDefaultAutoSelectFamily()` (Happy Eyeballs dual-stack racing,
    RFC 8305) *on by default* - confirmed by a standalone Node script hitting
    the same URL directly with `node:https`, which reproduced the identical
    failure with zero `wp-env`/Docker involvement, then fixed by calling
    `net.setDefaultAutoSelectFamily(false)` in-process. `--dns-result-order=
    ipv4first` (the first fix tried) does **not** help - it only reorders
    which address `dns.lookup()` returns first, not whether Happy Eyeballs
    still races the unreachable IPv6 address at all. The real fix, applied
    before every single `npx @wordpress/env ...` invocation in this
    environment (not just `start` - `run` hit the identical failure until
    this was set too):
    `NODE_OPTIONS='--no-network-family-autoselection'`. Worth flagging
    prominently for 1.5.2-1.5.4: this is an environment quirk of this
    sandbox specifically (no IPv6 route), not of `wp-env` itself, and will
    need re-discovering (or this note re-reading) on a differently-networked
    machine if it's not already muscle memory by then.
  - **Toolchain trouble #2 - a real `wp-env` plugin-source basename gotcha**:
    the natural URL to try for "latest stable WooCommerce" is
    `https://downloads.wordpress.org/plugin/woocommerce.latest-stable.zip`
    (a real, working URL) - but `.wp-env.json` mounts it as
    `wp-content/plugins/woocommerce.latest-stable/`, not `.../woocommerce/`.
    Confirmed by reading `@wordpress/env`'s own
    `lib/config/parse-source-string.js` directly: its basename derivation
    only strips a trailing *purely numeric* version suffix (e.g. `.1.2.3`)
    from a zip URL's filename, and `latest-stable` doesn't match that
    pattern, so it survives into the mounted directory name verbatim - not
    obvious from the URL alone, and it silently produced a working WordPress
    install that nonetheless didn't match the directory name every real
    production install (and this plugin's own `tests/bootstrap.php`, which
    hardcodes `wp-content/plugins/woocommerce/woocommerce.php`) assumes.
    Fixed by switching to the version-suffix-free
    `https://downloads.wordpress.org/plugin/woocommerce.zip` (confirmed via
    `curl -I` to genuinely serve the current stable release, same as the
    `.latest-stable.zip` alias), which derives the correct `woocommerce`
    basename. Caught by actually inspecting the mounted container's
    `wp-content/plugins/` listing (`docker exec ... ls`), not assumed correct
    because `wp plugin list` initially showed it "active" under the wrong
    slug (`woocommerce.latest-stable`) - active-but-wrong-slug would have
    broken every future step that hardcodes the real WooCommerce plugin
    directory name (webhook paths, `wp_remote_post` target discovery, etc.).
  - **Toolchain trouble #3 - a real WordPress-core/PHPUnit-10 incompatibility,
    not a bug in this plugin**: `wp-env run tests-cli ... phpunit` (the
    container's global PHPUnit, 10.5.64) got past test discovery but every
    test errored with `Call to undefined method
    PHPUnit\Util\Test::parseTestMethodAnnotations()`, thrown from *WordPress
    core's own* bundled `/wordpress-phpunit/includes/abstract-testcase.php`
    (`WP_UnitTestCase`'s `expectDeprecated()`, called from every test's
    `set_up()`), not from this plugin's test file at all. That method is a
    real, confirmed-by-reading-the-vendored-source casualty of PHPUnit 10's
    annotation-system removal - WP core's bundled test library (matching the
    WordPress version `wp-env` downloaded) only branches on PHPUnit `<9.5` vs
    `>=9.5`, predating PHPUnit 10 entirely. Fixed by pinning this plugin's
    own `composer.json` to `"phpunit/phpunit": "^9.6"` (still within
    `yoast/phpunit-polyfills`' supported range, and 9.6.36 - the version
    Composer resolved - supports PHP 8.3, the container's runtime) and
    running the plugin's own `vendor/bin/phpunit` rather than the container's
    global one. Not a workaround for a mistake in this plugin - a real,
    documented compatibility ceiling of the WordPress version this specific
    environment's `wp-env` pulled, worth 1.5.2-1.5.4 knowing about upfront
    rather than rediscovering.
  - **Toolchain trouble #4 - a real PHPUnit 10.x `TestSuiteLoader` naming
    rule, read from its source, not guessed at**: with #3 not yet fixed,
    test discovery itself failed first, with a misleading-sounding "Class
    test-gateway-registration cannot be found" warning despite the class
    genuinely existing (confirmed by manually `require`ing the bootstrap and
    test file by hand and checking `class_exists()` - `true` - before
    concluding this wasn't a real load failure). Root cause, read directly
    from `vendor/phpunit/phpunit/src/Runner/TestSuiteLoader.php`: PHPUnit's
    loader derives an expected class-name suffix from each discovered file's
    own basename and requires the declared class's short name to
    case-insensitively *end with* it, character-for-character - and a
    WordPress-core-style filename (`test-gateway-registration.php`, hyphens)
    paired with an underscored class name (`Test_Gateway_Registration`) never
    satisfies that literal check, regardless of the class being real and
    loadable. Fixed by renaming to the plain PHPUnit-native convention
    (filename equals class name exactly: `tests/GatewayRegistrationTest.php`
    / `class GatewayRegistrationTest`) and updating `phpunit.xml.dist`'s
    `<directory suffix="Test.php">` pattern to match - documented in the test
    file's own doc comment for whoever adds the next test file in 1.5.2+.
  - **License header, caught and fixed before finishing, not shipped
    wrong**: initially wrote `AGPL-3.0-or-later` into the plugin header and
    `composer.json` with no real basis - checked and this repo has **no**
    `LICENSE` file and no `license` field anywhere in the root `Cargo.toml`,
    so there was nothing to actually inherit. Switched to
    `GPL-2.0-or-later` instead: not arbitrary either, but tied to a real,
    already-planned downstream requirement - `docs/WOOCOMMERCE_WBS.md`'s own
    1.6.1 explicitly targets wordpress.org distribution, which requires
    GPLv2-or-later-compatible licensing, and GPL-2.0-or-later is what
    WordPress' own plugin boilerplate and the vast majority of the plugin
    ecosystem default to. Flagged here explicitly for the user: this repo's
    real, project-wide license is still an open question this plugin's
    header shouldn't be read as having silently settled - revise this header
    if/when that's decided differently.
  - **The real, observed test run** (after all four toolchain fixes above),
    run twice for confidence, identical result both times:
    `NODE_OPTIONS='--no-network-family-autoselection' npx @wordpress/env run
    tests-cli --env-cwd=wp-content/plugins/moneropay-cloud vendor/bin/phpunit
    --testdox` (via `newgrp docker -c "..."` in this shell, per this
    environment's own docker-group-membership quirk) ->
    ```
    PHPUnit 9.6.36 by Sebastian Bergmann and contributors.

    Gateway Registration
     ✔ Gateway is registered with woocommerce
     ✔ Gateway is disabled by default

    Time: 00:00.013, Memory: 89.00 MB

    OK (2 tests, 4 assertions)
    ```
    Additionally verified directly against the *dev* `wp-env` WordPress
    instance (separate from the PHPUnit-only tests instance, same plugin
    code, real browser-reachable site at `http://localhost:8888`), via `wp
    eval-file` iterating `WC_Payment_Gateways::instance()->payment_gateways()`:
    real output `moneropay_cloud => MoneroPay Cloud (title: Monero (via
    MoneroPay Cloud), enabled: no)` alongside WooCommerce's own bundled BACS/
    Cheque/COD gateways - the literal, human-readable form of the WBS's own
    stated outcome, not just a PHPUnit assertion proving the same thing
    indirectly.
  - **Files added** (all new, nothing existing touched):
    `plugins/moneropay-cloud/moneropay-cloud.php` (bootstrap: plugin header,
    `plugins_loaded`-deferred class load, `woocommerce_payment_gateways`
    filter registration), `plugins/moneropay-cloud/includes/
    class-wc-gateway-moneropay.php` (`WC_Gateway_MoneroPay`),
    `plugins/moneropay-cloud/composer.json` + `composer.lock` (dev-only:
    `yoast/phpunit-polyfills`, `phpunit/phpunit` ^9.6 - the plugin itself has
    zero runtime PHP dependencies beyond WordPress/WooCommerce),
    `plugins/moneropay-cloud/.wp-env.json` (WordPress core `null` = latest,
    WooCommerce + this plugin as `"plugins"`), `plugins/moneropay-cloud/
    phpunit.xml.dist`, `plugins/moneropay-cloud/tests/bootstrap.php`,
    `plugins/moneropay-cloud/tests/GatewayRegistrationTest.php`,
    `plugins/moneropay-cloud/.gitignore` (`/vendor/`,
    `/.phpunit.result.cache`).
  - **Not done / explicitly out of scope, matching WBS 1.5.1's own stated
    boundary**: `process_payment()` (WBS 1.5.2), the connect-flow settings
    button (WBS 1.5.3), the webhook receiver (WBS 1.5.4), a checkout icon, a
    `.pot` translation file, and `readme.txt`/wordpress.org directory
    compliance (WBS 1.6.1) - none of these were needed to satisfy this step's
    own outcome ("shows as a checkout option, disabled is fine") and adding
    them now would be exactly the unrequested-scope pattern this project's
    practice has repeatedly avoided elsewhere. Also not done: silencing
    `wp-env`'s own "starts both development and tests environments by
    default... deprecated" warning by setting `"testsEnvironment": false` -
    checked its own source first, and that flag doesn't just silence the
    warning, it actually removes the `tests-cli`/`tests-wordpress` containers
    this plugin's whole PHPUnit setup depends on, so left as harmless noise
    rather than "fixed" into a broken state.

- WBS 2.1.3 done: the engine wired to the socket-based `KeyCustody`
  implementation behind a config flag - `main.rs` no longer unconditionally
  constructs `PlainKeyCustody::default()`.
  - **The real obstacle, found by trying it rather than assuming it would work**:
    the naive plan ("`main.rs` depends on `key-custody-service` for
    `SocketKeyCustody`") is impossible as the crate graph stood after 2.1.1/2.1.2.
    `key-custody-service` depended on `moneropay-core` (for the real
    `KeyCustody`/`WalletHandle`/etc. types its DTOs convert to/from, and for
    `server.rs`'s real `PlainKeyCustody`); `main.rs` depending on
    `key-custody-service` back would be `moneropay-core -> key-custody-service ->
    moneropay-core`, a real Cargo dependency cycle. Not reasoned about in the
    abstract - actually attempted (`cargo check` after adding the dependency
    edge) and confirmed with Cargo's own `error: cyclic package dependency`
    before doing anything else. Also empirically ruled out the tempting shortcut
    of making the back-edge `optional`/feature-gated (moneropay-core depending on
    key-custody-service with `default-features = false`, minus a "server"
    feature) - Cargo still reports the identical cycle, since the cyclic-package
    check operates on the manifest's declared edges before any feature
    activation is resolved, not on which symbols a build actually uses.
  - **The fix, not a workaround**: moved the `KeyCustody` trait and every type
    that crosses it (`WalletHandle`, `WalletMaterial`, `KeyCustodyError`,
    `MatchedOutput`) from `moneropay-core`'s `src/key_custody/mod.rs` to a new
    `shared/src/key_custody.rs` (`shared` depends on nothing that could cycle
    back), with `moneropay-core::key_custody` now just `pub use`-re-exporting
    them - a re-export is the same type, not a wrapper, so every one of the
    ~30 existing call sites across the engine crate kept compiling completely
    unchanged, confirmed by `cargo check -p moneropay-core --lib` passing with
    zero other files touched at that point. `network_str`/`parse_network` moved
    the same way (`shared/src/network.rs`) for the same reason -
    `key-custody-service`'s `NetworkWire` needed them too. The one real code
    change this forced (not just a re-export): `WalletHandle::new()` had to go
    from private to `pub` (it's still only ever meant to be called from inside a
    `KeyCustody` implementation, per its own doc comment) - `plain.rs`, now in a
    different crate from the type's definition, could no longer reach a
    module-tree-private method across the crate boundary. Confirmed no similar
    problem existed for `WalletMaterial`'s private fields: grepped `plain.rs` and
    found it only ever uses the already-`pub` `to_view_pair`/`to_raw_bytes`
    accessors, never direct field access.
  - Splitting the trait out wasn't sufficient by itself, though -
    `key-custody-service`'s `server.rs` (wrapping a real `PlainKeyCustody`) still
    needed `moneropay-core`, and it lived in the *same* crate as `client.rs`
    (what `main.rs` actually needs), so the cycle would have just come back
    through that edge instead. Split `key-custody-service` into two crates:
    `key-custody-service` keeps `client.rs`/`protocol.rs`/`lib.rs` (DTOs) and now
    depends only on `shared`, never `moneropay-core`; a new sibling crate
    `key-custody-server` (`git mv`d `server.rs`, `bin/key-custody-server.rs`, and
    `tests/socket_key_custody.rs` there, since that test needs a real server)
    depends on *both* `moneropay-core` (for `PlainKeyCustody`) and
    `key-custody-service` (for the protocol/DTO types). `moneropay-core` now
    depends on `key-custody-service` only - never on `key-custody-server` - so
    the graph is a clean DAG:
    `key-custody-server -> {moneropay-core, key-custody-service} -> shared`.
    Verified with `cargo check --workspace --all-targets` clean at every
    intermediate step, not just at the end. All 39 of 2.1.2's key-custody tests
    (22 unit + 17 integration) still exist and still pass, just split across the
    two crates the same way the code that exercises them now is (`key-custody-service`
    22, `key-custody-server` 17) - nothing was dropped or rewritten, confirmed by
    diffing the ported test file's content against its pre-move version.
  - **Config**: new `[key_custody]` section (`src/config.rs::KeyCustodyConfig`),
    mirroring `ExchangeRateConfig`'s existing "string field selects the backend,
    `Config::validate_bounds` checks conditionally-required fields" shape rather
    than a `#[serde(tag = ...)]` enum - the same shape every other conditionally-
    required section in this file already uses, and an unrecognized value gets
    the identical "rejected at boot with a clear error" treatment either way.
    `backend`: `"plain"` (default, unchanged) or `"socket"`; `socket_path:
    Option<String>`, required and validated non-empty-after-trim only under
    `"socket"` (`ConfigError::SocketBackendMissingSocketPath`); an unrecognized
    `backend` is `ConfigError::UnknownKeyCustodyBackend`, exactly mirroring
    `exchange_rate.provider`'s own unknown-value handling. 4 new `config.rs`
    tests: default-with-no-section-present, socket-with-path parses/validates,
    socket-with-no-path (both omitted and empty/whitespace) rejected, unknown
    backend rejected.
  - **The `key_custody_backend` question, investigated as asked, not guessed
    at**: grepped every read site of `tenants.key_custody_backend` (the stored
    column), not just write sites. Found it is read back from SQLite into
    `Tenant`/`NewTenant` (`store.rs`'s `row.get("key_custody_backend")` and the
    `INSERT` binding) but **never matched on or dispatched on anywhere** in this
    codebase - every prior write site hardcoded the literal `"plain"` regardless
    of anything. `migrations/0001_init.sql`'s own comment on the column and
    `docs/DESIGN.md` §8.1 agree on what it's actually *for*: letting a **future**
    migration to a different backend detect a mismatch and fail loudly on
    `unseal_and_register` rather than silently misinterpreting bytes sealed by a
    different backend - `docs/TESTING.md`'s own gap list already flags that
    specific check ("seal() output is versioned by key_custody_backend...") as
    not yet implemented, unrelated to this task and not built here either, since
    it wasn't asked for. It is **not** a per-tenant dispatch key: `main.rs` holds
    exactly one `Arc<dyn KeyCustody>` for the whole process
    (`AppState.key_custody`, `register_all_tenants`, `run_scanner_loop` all take
    a single shared instance), and nothing in the schema or the code anticipates
    otherwise - a single running instance can only ever use one backend for
    every tenant it holds, which is exactly what this task's `[key_custody]`
    config section (one value, process-wide) matches. What *was* a real,
    previously-latent bug this task's own change would have made concretely
    wrong: two production write sites (`main.rs::bootstrap_self_hosted_tenant`
    and `http/admin.rs::create_tenant`) hardcoded `key_custody_backend: "plain"`
    regardless of which backend actually sealed the material - harmless before
    this task (only "plain" existed), actively misleading the moment a second
    backend exists for real (a tenant created under `backend = "socket"` would
    have its row claim "plain" while `key-custody-server` genuinely sealed it).
    Fixed both to record the real configured backend: `bootstrap_self_hosted_tenant`
    now takes it from `config.key_custody.backend` directly; `create_tenant`
    needed a new `AppState.key_custody_backend: String` field (nothing about
    `Arc<dyn KeyCustody>` lets a caller ask "which implementation is this," by
    design, so `main.rs` hands the string down alongside the trait object rather
    than inventing a downcast/introspection surface this boundary was
    deliberately never given).
  - **Startup-failure-handling decision, made deliberately, not left
    unconsidered**: a bounded retry loop (`main.rs::connect_socket_key_custody`,
    10 attempts, 500ms apart, ~4.5s total), not a single fail-fast attempt.
    `key-custody-server`'s own binary doc comment explicitly pushes *its own*
    restart-policy ownership onto an external process supervisor
    ("supervisor is what should own restart policy... not this binary guessing
    at them") - read closely, that's a claim about who restarts a process that
    has genuinely died, not about how a *client* dialing it should react to an
    ordinary two-independently-started-processes race at boot, which is exactly
    the scenario this task named by name. A single failed connect attempt cannot
    tell "server not scheduled onto a thread yet" (resolves in milliseconds)
    apart from "server genuinely down," and failing fast on the former just
    pushes a second restart-and-backoff cycle onto whatever supervises this
    process, for a race a few hundred milliseconds of patience resolves for
    free. This project's own prior art agrees, not just reasoning from
    principle: `key-custody-server/tests/socket_key_custody.rs`'s own
    `connect_with_retry` helper (2.1.2, written before this task) hit and solved
    the identical race between spawning an in-process test server and dialing
    it, the same way. What it deliberately does *not* do: retry forever, or
    silently fall back to `PlainKeyCustody` - past ~5 seconds this stops being
    ordinary scheduling jitter, and the operator needs a loud, specific,
    actionable failure (exact socket path, attempt count, the real last error,
    a pointed question about whether the server is even running) with a clean
    `std::process::exit(1)`, never a panic, never an indefinite hang. Verified
    for real, not just by reading the code: ran the compiled binary against a
    `socket_path` nothing is listening on (clean exit 1 after ~4.5s with the
    expected message) and against a real `key-custody-server` process (boots to
    "moneropay listening on ..." with no key-custody errors, only the expected,
    unrelated failures from a Monero node this smoke test never started).
  - **Testing harness**: `engine-test-support` (not a new harness - confirmed
    this was the right home by reading how `mock-woocommerce`/`control-plane`
    already depend on it for a real, network-bound engine before adding
    anything). `TestEngineConfig` gained `with_socket_key_custody(socket_path)`
    - unlike `main.rs`'s retrying connect, this does *not* retry (a test
    controls both sides of the race and starts the server first), documented as
    a deliberate difference in its own doc comment. `key-custody-service` became
    a real (not dev) dependency of this crate, since `spawn()` itself (not just
    tests) needs to construct a `SocketKeyCustody`; `key-custody-server` is a
    dev-dependency, needed only by this crate's own regression test.
  - **The regression test itself, and an honest account of what "unmodified"
    could and couldn't mean here**: the WBS's acceptance bar
    ("the engine's existing integration tests... pass unmodified against this
    configuration") can't be taken *completely* literally - the existing test
    that proves this exact scenario against `PlainKeyCustody`
    (`src/scanner.rs::run_scan_tick_matches_mempool_tx_recomputes_status_and_
    enqueues_a_webhook`) lives inside `moneropay-core`'s own `#[cfg(test)]`
    build and constructs `PlainKeyCustody`/`Store` directly in-process - it
    structurally cannot be "pointed at" a different backend without becoming a
    different test, and `moneropay-core` itself can never depend on
    `SocketKeyCustody` at all (see the cycle above). So `engine-test-support`
    reproduces that exact scenario end to end through the real HTTP API instead
    (same fixture transaction and view/spend keys `plain.rs`'s and `scanner.rs`'s
    own tests use - not a new one invented here), run twice - once per backend -
    and asserts the two runs are pixel-for-pixel identical
    (`order_creation_and_chain_scanning_behave_identically_through_the_socket_
    backed_key_custody_path`), not just each individually plausible. First
    version of this test picked too large a target order amount and got
    `partial` instead of `unconfirmed` from both backends identically - caught
    immediately since the assertion checks the *specific* expected outcome too,
    not just equality between the two runs; fixed by using a trivially-small
    rate, same reasoning `scanner.rs`'s own `xmr_amount_piconero: 1` comment
    already documents.
  - **Files touched**: `Cargo.toml` (workspace members +
    `key-custody-service` dependency), `shared/Cargo.toml` +
    new `shared/src/{key_custody,network}.rs` + `shared/src/lib.rs`,
    `src/key_custody/mod.rs` + `src/network.rs` (trimmed to re-exports),
    `src/config.rs`, `src/main.rs`, `src/http/mod.rs` + `src/http/admin.rs`
    (`AppState.key_custody_backend`), `src/http/tests.rs` +
    `tests/e2e_stagenet.rs` (new `AppState` field), `key-custody-service/
    Cargo.toml` + `src/lib.rs` + `src/client.rs` (now depends on `shared`, not
    `moneropay-core`), new `key-custody-server/` crate (`Cargo.toml`,
    `src/lib.rs`, `git mv`d `server.rs`/`bin/key-custody-server.rs`/
    `tests/socket_key_custody.rs`), `engine-test-support/Cargo.toml` +
    `src/lib.rs`. `Cargo.lock` diff is 20 lines, all of it the new
    `key-custody-server` package entry - no new external crate entered the
    workspace's dependency graph (monero/uuid/zeroize/async-trait were already
    resolved elsewhere; only `hex`, already used pervasively, is new to
    `engine-test-support`, as a dev-dependency).
  - Full `cargo test --workspace`, before this task (confirmed by actually
    running it, not trusting this log's prior "Counts" line): engine 311
    passed/9 ignored, shared 26, control-plane 80, engine-test-support 2,
    key-custody-service 39 (22 unit + 17 integration, one crate). After: engine
    312/9 ignored (+4 new `config.rs` tests, -1 `WalletHandle` round-trip test
    and -2 `network` round-trip tests moved out to `shared`, net +1), shared 29
    (+3: the 3 tests that moved in), control-plane 80 (unchanged),
    engine-test-support 3 (+1, the new socket-vs-plain regression test),
    key-custody-service 22 (unchanged - just the unit tests, integration tests
    moved out), key-custody-server 17 (the integration tests that moved, now in
    their own crate - combined with key-custody-service's 22, still 39 total,
    confirming nothing was lost in the split), mock-woocommerce 8+1
    (unchanged). `cargo build --workspace --all-targets` clean, zero warnings,
    confirmed by grepping the full build log for "warning" and finding nothing.
    No `cargo fmt` run anywhere; every new/moved file hand-formatted to match
    its crate's existing style, and every edit to an existing file matched the
    surrounding style by hand.
  - **Not done / explicitly out of scope**: the `unseal_and_register`
    backend-mismatch check `docs/TESTING.md` already flags as a gap (versioning
    `seal()` output by `key_custody_backend` and failing loudly on a mismatch) -
    genuinely related to this column, but a different, not-yet-asked-for piece
    of work; noted here so a future reader doesn't assume this task silently
    fixed it. `init_wizard.rs`'s interactive setup flow was not extended to
    offer `backend = "socket"` as a choice, matching the same judgment call
    1.7.1's entry above made for `provider = "coingecko"` - a self-hoster (or a
    future control-plane-generated config) can still hand-write it into the
    TOML directly.

- WBS 2.1.2 done: socket-based `KeyCustody` implementation, extending the
  existing `key-custody-service` crate (not a third crate) with the socket
  half 2.1.1 deliberately left unbuilt - `protocol.rs` (envelope + framing),
  `server.rs` (`KeyCustodyServer`, wrapping a real `PlainKeyCustody`) plus its
  `bin/key-custody-server.rs` standalone binary, and `client.rs`
  (`SocketKeyCustody`, a real `KeyCustody` impl that forwards every call over
  a Unix socket). Read `src/key_custody/mod.rs` (trait), `src/key_custody/
  plain.rs` (`PlainKeyCustody` + its 12-test suite), and 2.1.1's own
  `key-custody-service/src/lib.rs` DTOs directly before writing anything, per
  this project's standing practice.
  - **Framing**: a 4-byte big-endian `u32` length prefix + that many bytes of
    `serde_json`-encoded payload, same in both directions
    (`protocol.rs::{read_frame,write_frame}`). `serde_json` because this whole
    workspace already depends on it pervasively and nothing here is
    performance-sensitive (a `KeyCustody` call is bounded by scalar-
    multiplication cost, not serialization); a length prefix rather than a
    delimiter because a `TransactionWire`'s hex string has no character
    `serde_json` promises never to emit. `MAX_FRAME_BYTES` (16 MiB) bounds a
    corrupted/hostile length prefix from claiming up to 4 GiB - same shape of
    guard as `plain.rs`'s own `MAX_SCAN_TABLE_ENTRIES`, applied to the framing
    layer instead of the scan-table layer. `read_frame` distinguishes a clean
    EOF *before* any byte of a new frame's length prefix (`Ok(None)` - the
    ordinary way a connection ends between requests) from every other failure
    (a partial prefix, an oversized length, a short payload read, invalid
    JSON - all `Err`), so a genuinely broken peer never gets mistaken for an
    ordinary disconnect.
  - **Envelope**: `KeyCustodyRequest`/`KeyCustodyResponse`, one variant per
    trait method, each wrapping 2.1.1's existing `{Name}Request`/
    `{Name}Response` DTOs verbatim - no new per-method wire shape invented.
  - **Concurrency, chosen deliberately, not left racy**: `SocketKeyCustody`
    opens one persistent connection at `connect` time (not a fresh connection
    per call - a real engine will make many calls/second) and serializes
    every call onto it with a `tokio::sync::Mutex`, rather than a request-ID/
    correlation scheme letting several calls be in flight over the wire at
    once. Chose the simpler option: a correlation scheme is real, permanent
    wire-format complexity to buy back concurrency this workload doesn't
    obviously need (every call is already bounded by the same scalar-
    multiplication costs `plain.rs` documents; a future TEE-backed backend is
    unlikely to parallelize arbitrarily within one enclave either). Documented
    in `client.rs`'s module doc comment as a decision to revisit at 2.1.3 if
    it becomes a real bottleneck, not a permanent commitment.
  - **A bounded per-call timeout (30s default) plus poison-on-failure**: without
    a timeout, a wedged or malicious server that withholds a response would
    hang a call forever - and because the connection is shared, mutex-
    serialized state, that one hung call would silently stall *every* other
    concurrent caller too. Beyond the timeout itself, any transport-level
    failure (timeout, I/O error, decode error, clean close) marks the
    connection `None` (poisoned) rather than trying to keep using it: after a
    timeout specifically, the peer might still write a late response for the
    call that just gave up on it, and a later call reusing the same stream
    would misread those stale bytes as its own reply - silent framing
    corruption, not a clean error. Poisoning trades that risk for a simple,
    loud "this `SocketKeyCustody` is dead, make a new one" - no auto-reconnect
    in this step, called out explicitly as a documented limitation for 2.1.3
    to address with real requirements instead of this step guessing at them.
  - **Server-side decode-failure policy**: a request whose own fields don't
    convert back into real types (bad handle hex, an unparseable address, a
    non-consensus-encoded transaction) closes the connection rather than
    trying to shoehorn it into one of `KeyCustodyErrorWire`'s four variants -
    none of which mean "your bytes were corrupted in transit," and forcing it
    into e.g. `UnknownWallet` would mislead a caller matching on that variant
    for a real reason. This is exactly the decision 2.1.1's own
    `WireConversionError` doc comment left open for this step; resolved in
    `server.rs::handle_connection`'s doc comment, treating a decode failure
    the same way a raw framing error is already treated.
  - **The two whitebox tests, handled honestly, not silently dropped**:
    `src/key_custody/plain.rs`'s test suite has 12 tests; 10 port verbatim
    (same scenario, same assertions, only the concrete `KeyCustody` value
    changes) in `key-custody-service/tests/socket_key_custody.rs`. The other
    two each assert on `PlainKeyCustody`'s own private internals in the
    original:
    - `repeated_scans_over_same_range_reuse_the_cached_table` asserts
      `rebuild_count(&custody, handle) == 1` then `== 2` via a private,
      `#[cfg(test)]`-gated `AtomicU64` field on the private `WalletEntry`
      struct. Not portable for *two* independent reasons, not just one:
      (1) it's genuinely unobservable through the `KeyCustody` trait -
      `scan_tx_outputs` returns the same correct result whether or not the
      table rebuilt, exactly as `plain.rs`'s own module doc comment says; and
      (2) even a same-process test harness holding a live
      `Arc<PlainKeyCustody>` could never reach it, because `wallets` (and
      therefore anything inside a `WalletEntry`) is private to
      `src/key_custody/plain.rs`'s own module under Rust's privacy rules -
      not visible even from `src/key_custody/mod.rs`, its own parent module,
      let alone from a separate crate - and `rebuild_count` is additionally
      `#[cfg(test)]`-gated, so it isn't even *compiled into* `WalletEntry`
      when `moneropay-core` is built as an ordinary path dependency the way
      this crate builds it. Considered and rejected: adding an accessor to
      `PlainKeyCustody`/`WalletEntry` purely to satisfy this one assertion -
      that would mean lifting the `cfg(test)` gate on a permanent-looking
      struct field (a bigger, unrequested change to `plain.rs`'s production
      layout) to test something a real socket deployment structurally cannot
      observe either, which is precisely the kind of "faking a port" the WBS
      2.1.2 brief warned against. **Kept**: three same-range scans plus a
      genuinely wider fourth all still return the *correct* result (a broken
      cache would surface as wrong matches, so this isn't a no-op).
      **Dropped, with this exact reasoning left as a comment on the test
      itself**: the caching-efficiency assertion.
    - `removing_a_wallet_scrubs_its_view_key_rather_than_leaving_it_in_freed_
      memory` asserts `custody.wallets.read().unwrap().is_empty()` directly,
      plus two assertions that are pure black-box `KeyCustody` behaviour
      (`remove_wallet` again returns `UnknownWallet`; a post-removal
      `scan_tx_outputs` returns `UnknownWallet`). **Kept**: both black-box
      assertions, verbatim. **Dropped, with the same reasoning as above
      documented on the test**: `wallets.is_empty()` - same private-field
      problem, no `cfg(test)` gate this time but a plain private field is
      exactly as unreachable from another crate regardless of that.
    - A `KeyCustodyServer::backend()` accessor was tried and removed during
      this session once it became clear it doesn't actually solve either
      problem: it only ever exposes `PlainKeyCustody`'s own `pub` surface
      (i.e. the `KeyCustody` trait impl itself, already reachable through the
      client), never a private field, no matter which process or module holds
      the `Arc` - "same process" is necessary but nowhere near sufficient for
      "same-module field access" in Rust. Worth flagging in case a future
      reader wonders why that accessor isn't here: it was genuinely
      considered, built, and then correctly discarded as not fit for purpose,
      not overlooked.
  - **Beyond the ported suite, 7 new tests proving the socket mechanism
    itself** (`tests/socket_key_custody.rs`): one launches the *compiled*
    `key-custody-server` binary as a real, separate OS process via
    `env!("CARGO_BIN_EXE_key-custody-server")` and drives a full
    register→derive→scan→remove round trip against it over a real socket
    path, killing the child afterward - the one test in the whole file that
    actually proves the "compromising the main engine process alone never
    yields the keys" claim has a mechanism behind it, since every other test
    (ported or new) legitimately runs the server as an in-process background
    task for speed. The rest: connecting to a socket nothing is listening on
    is a clean `BackendUnavailable`, not a panic or hang; a server that
    answers once correctly then closes the connection (simulating a mid-
    session crash/restart) makes the *next* call on the same client fail
    cleanly rather than hang or corrupt the next read; a peer that sends a
    well-framed but non-JSON payload back to `SocketKeyCustody` produces a
    clean client-side error, not a panic; and two raw-socket cases against a
    real running server - a length prefix claiming a frame far past
    `MAX_FRAME_BYTES`, and a valid length prefix followed by non-JSON bytes -
    both close cleanly without taking the server down, proven by a
    subsequent well-behaved client still being served normally afterward.
  - **Dependencies**: `key-custody-service/Cargo.toml` promotes `serde_json`
    from dev- to a real dependency (2.1.1 had explicitly flagged this as the
    trigger for doing so) and adds `async-trait` + `tokio` (`features =
    ["full"]`, matching the engine crate's own choice rather than hand-picking
    a narrower feature set to keep in sync separately). `Cargo.lock`'s diff is
    two lines (`async-trait`, `tokio` added to this crate's dependency list) -
    both were already resolved elsewhere in the workspace, so no new external
    crate entered the graph.
  - **Not touched**: `src/key_custody/mod.rs` and `src/key_custody/plain.rs`
    (the engine crate) - the brief's "ideally you won't need to expose
    anything new from it" held; nothing new was needed beyond what 2.1.1
    already added (`WalletHandle::as_bytes`/`from_bytes`). No engine wiring to
    actually *use* `SocketKeyCustody` in `main.rs` - that's WBS 2.1.3.
  - Full `cargo test --workspace`: engine 311 passed/9 ignored (unchanged),
    key-custody-service 22 (unit, unchanged) + 17 (new
    `tests/socket_key_custody.rs` integration tests) = 39, control-plane 80
    (unchanged), shared 26 (unchanged), engine-test-support 2 (unchanged),
    mock-woocommerce 8+1 (unchanged). `cargo build --workspace` clean, no
    warnings, confirmed by touching every new/changed file in
    `key-custody-service` and rebuilding before relying on a "no warnings"
    claim. Files touched: `key-custody-service/Cargo.toml`,
    `key-custody-service/src/lib.rs` (module declarations + doc comment
    update only - no DTO changed), new `key-custody-service/src/{protocol,
    server,client}.rs`, new `key-custody-service/src/bin/key-custody-server.rs`,
    new `key-custody-service/tests/socket_key_custody.rs`. `Cargo.lock`. No
    `cargo fmt` run anywhere - every new file hand-formatted to match this
    crate's existing (2.1.1) style throughout.

- WBS 2.1.1 done: `key-custody-service`, a new workspace crate holding *only*
  wire-level DTOs (and their conversions) for every `KeyCustody` trait method's
  arguments and `Result` - the first step of Track B (SEV-SNP key custody).
  Deliberately no socket/server/client code - that's 2.1.2, a separate future
  step, and the WBS is explicit that this step stands alone.
  - **Read the real trait, not the WBS's paraphrase, and found the paraphrase is
    stale in a way worth flagging**: the trait's own module-level doc comment
    (`src/key_custody/mod.rs`, right above the `trait KeyCustody` block) still
    says "the `major_range`/`minor_range` parameters shared by `derive_subaddress`
    and `scan_tx_outputs`" - but the real `derive_subaddress` signature today is
    `(handle, index: SubaddressIndex, network: Network) -> Result<Address, ...>`,
    with **no** range parameters at all; only `scan_tx_outputs` takes
    `major_range`/`minor_range`. So the doc comment itself is out of date, not
    just the WBS document quoting it - the six DTOs here were built against the
    actual `fn` signatures (confirmed by reading them directly), which is why
    `DeriveSubaddressRequest` carries `network` but no ranges, and
    `ScanTxOutputsRequest` carries both ranges but no `network`. Left the stale
    doc comment in place (out of scope to fix here) but called it out explicitly
    in `ScanTxOutputsRequest`'s own doc comment so a future reader isn't misled by
    it a second time.
  - **`WalletHandle` had no accessor at all** (private `Uuid` field, only
    `Debug`/`Clone`/`Copy`/`PartialEq`/`Eq`/`Hash`) - exactly the gap the WBS
    flagged as a possible finding. Added `pub fn as_bytes(&self) -> [u8; 16]` and
    `pub fn from_bytes([u8; 16]) -> Self` directly on `WalletHandle` in
    `src/key_custody/mod.rs` (with a doc comment explaining why this doesn't
    weaken the "opaque handle" framing - a `WalletHandle` was never a secret or
    unguessable-by-design, just an index into a process-local map), plus one
    direct test in a new `#[cfg(test)] mod tests` in that same file (mod.rs had
    none before - `PlainKeyCustody`'s own tests live in `plain.rs`). `from_bytes`
    is needed both by this step's own round-trip tests and by the future 2.1.2
    socket *client*, which will need to reconstruct the exact handle value a
    remote implementation issued so it can hand it back on later calls.
  - **DTO design, one struct/type-alias pair per trait method** (`RegisterWallet`,
    `RemoveWallet`, `Seal`, `UnsealAndRegister`, `DeriveSubaddress`,
    `ScanTxOutputs` - `{Name}Request` struct + `type {Name}Response =
    Result<TWire, KeyCustodyErrorWire>`). Chose to reuse `std::result::Result`
    directly for every response rather than a hand-rolled `Ok`/`Err` enum -
    confirmed first that `serde` does provide a real `Serialize`/`Deserialize`
    impl for `Result<T, E>` (grepped the vendored `serde` crate source rather than
    assuming), so a hand-rolled version would just be more code for an identical
    wire shape. The WBS explicitly allows this ("reuse `Result` directly ... if it
    serializes the way you want").
  - **Byte-blob fields are all hex-encoded `String`s**, not raw byte arrays or
    base64: `[u8; 64]` (`WalletMaterialWire`) doesn't implement `Serialize`
    directly - checked the vendored `serde` source and confirmed its array impls
    are macro-generated only up to length 32, nothing further - and hex, not
    base64, matches this codebase's existing convention (`hex` is already a
    pervasive dependency here; nothing in this workspace uses base64 anywhere).
    `WalletHandleWire`'s 16 bytes would fit serde's direct array support but were
    hex-encoded anyway for consistency with every other byte-blob DTO in this
    crate.
  - **`SubaddressIndexWire` carries `major`/`minor` as plain `u32`s, not via
    `monero`-rs's own `serde` feature** - checked and `cryptonote::subaddress::
    Index` *does* derive `Serialize`/`Deserialize` upstream, but only behind that
    crate's own `serde` cargo feature, which this workspace's `monero` dependency
    doesn't enable (default features are `full`). Deliberately did not turn that
    feature on in this new crate's `Cargo.toml`: Cargo's feature unification means
    doing so would silently enable `monero/serde` (and therefore
    `curve25519-dalek/serde`, `serde-big-array`) for every other workspace member
    too whenever built together (e.g. plain `cargo build --workspace`) - a
    non-obvious, action-at-a-distance change to the rest of the tree from what
    should be a one-crate addition. Two plain `u32` fields cost nothing and avoid
    it entirely.
  - **`Address`/`Transaction`/`Network` all reuse an existing encoding rather than
    inventing one**, per the WBS's own steer: `AddressWire` wraps `Address`'s own
    base58 `Display`/`FromStr`; `TransactionWire` wraps `monero::consensus::
    encode::serialize`/`deserialize` (the same functions `src/scanner.rs`'s and
    `src/key_custody/plain.rs`'s own tests already use to load
    `tests/fixtures/subaddress_tx.hex`) as hex; `NetworkWire` reuses
    `moneropay_core::network::network_str`/`parse_network` directly rather than a
    second string mapping that could drift from the one the config file and admin
    API already use.
  - **`WalletMaterialWire` and `SealedMaterialWire` are `ZeroizeOnDrop`**
    (matching `WalletMaterial`'s own convention exactly) and have a hand-written
    `Debug` impl that redacts the hex string, rather than deriving `Debug`. Not
    explicitly asked for by the WBS, but the obvious extension of this codebase's
    existing rule that raw key material never survives in a plain-`Debug`-able or
    un-scrubbed form - these two DTOs are the wire copies of exactly the same
    bytes `WalletMaterial` already treats this way, so there was no real argument
    for treating the wire form more casually than the in-memory one. Confirmed
    (didn't just assume) that `String: Zeroize` exists in the pinned `zeroize`
    version by actually building against it, rather than trusting memory of the
    crate's API.
  - **`SealedMaterialWire` is deliberately not assumed to be 64 bytes** even
    though `PlainKeyCustody::seal` happens to produce exactly `WalletMaterial::
    to_raw_bytes()`'s 64 bytes today - a future TEE-backed `seal` will produce
    something sealed *to that enclave*, almost certainly a different length. One
    test round-trips a 96-byte blob specifically to prove this isn't silently
    assumed.
  - **`serde_json` is a dev-dependency only, not a main one** - nothing in
    `src/lib.rs` actually calls it (no socket/server code exists yet to serialize
    anything for real), only this crate's own tests do. `key-custody-service`'s
    main dependencies are `hex`, `moneropay-core` (path), `monero`, `serde`
    (derive only), `thiserror`, `zeroize` (derive only) - noted in the crate's own
    `Cargo.toml` comment so 2.1.2 knows to promote `serde_json` (or whatever
    encoding is chosen then) to a real dependency once something actually sends
    bytes over a socket.
  - **`WireConversionError`** (new, local to this crate): covers hex-decode
    failures, wrong byte lengths, unrecognized network names, unparseable
    addresses, malformed transaction bytes, and platform `usize`/`u64` overflow -
    deliberately kept separate from `KeyCustodyErrorWire` (which mirrors
    `KeyCustodyError` variant-for-variant and carries a real `KeyCustody` method's
    *application* result across the wire). `WireConversionError` only ever
    originates locally, turning a possibly-corrupted wire value back into a real
    type; what a future socket server does with one (close the connection? map it
    into `KeyCustodyErrorWire::BackendUnavailable`?) is explicitly left as a 2.1.2
    decision, not resolved here.
  - **Tests**: 22 in `key-custody-service/src/lib.rs`, plus 1 new in
    `src/key_custody/mod.rs` for the `WalletHandle` accessor pair (engine crate).
    Covers, per the WBS's explicit acceptance list: `WalletMaterial`'s real raw
    key bytes (constructed with every byte value 0..64 present at least once, not
    a degenerate all-same-byte fixture) extracted and compared byte-for-byte after
    the round trip - not `assert_eq!` on the wire value, not a `Debug` string,
    which is exactly the shortcut the WBS warned would let a redaction bug hide as
    "empty view key" silently; a separate assertion that the `Debug` string really
    is redacted and really doesn't leak the hex; all four `KeyCustodyError`
    variants round-tripped (compared by rendered message, since neither
    `KeyCustodyError` nor `KeyCustodyErrorWire`'s restored form derive
    `PartialEq` across the crate boundary in a way `assert_eq!` could use
    directly - `KeyCustodyErrorWire` itself does derive `PartialEq`, used directly
    for the request/response DTO tests); `MatchedOutput` with both `Some(_)` and
    `None` amounts; a real, non-trivial `Transaction` deserialized from
    `tests/fixtures/subaddress_tx.hex` (not an empty/default one) round-tripped
    through its consensus encoding; a real standard address *and* a real derived
    subaddress (both built from the same fixture view/spend keys `plain.rs`'s own
    tests use); every `Network` variant; `Range<u32>`; both directions of every
    per-method request/response DTO, including a `Vec<MatchedOutputWire>` with
    more than one element. Several negative tests too (truncated handle hex,
    wrong-length key material, garbage address text, truncated transaction bytes,
    unrecognized network name) - not strictly asked for, but cheap given the
    conversions already return `Result` rather than panicking, and directly
    useful for whoever builds the 2.1.2 socket server against these.
  - Full `cargo test --workspace`: engine 311 passed/9 ignored (was 310/9, +1 -
    the new `WalletHandle` accessor test), key-custody-service 22 passed (new
    crate), control-plane 80 (unchanged), shared 26 (unchanged), engine-test-support
    2 (unchanged), mock-woocommerce 8+1 (unchanged). `cargo build --workspace`
    clean, no warnings. Files touched: `Cargo.toml` (workspace members list),
    `src/key_custody/mod.rs` (the accessor pair + its test), new
    `key-custody-service/Cargo.toml` + `key-custody-service/src/lib.rs`. No
    `cargo fmt` run anywhere, including on the new crate - hand-formatted to match
    the rest of this codebase's style throughout.
  - **Not done / explicitly out of scope**: no socket, no server binary, no
    client adapter implementing `KeyCustody` - that's 2.1.2. No change to
    `PlainKeyCustody` or the `KeyCustody` trait itself beyond the two new
    `WalletHandle` methods. No fix to the stale doc-comment prose on the trait
    itself (documented the discrepancy instead of silently correcting scope this
    task wasn't asked to touch).

- WBS 1.7.1 done: `CoingeckoRateProvider`, a second `ExchangeRateProvider`
  implementation backed by live rates from Coingecko's public API, plus config
  wiring and a `main.rs` background-refresh loop.
  - **Design followed as specced, not re-derived**: the trait stays synchronous.
    `CoingeckoRateProvider` holds an `Arc<std::sync::RwLock<HashMap<String, u64>>>`
    cache that `piconero_per_unit` just reads; a separate `pub async fn
    refresh(&self) -> Result<(), ExchangeRateError>` does the real HTTP round trip
    and updates the cache. A currency the cache doesn't (yet) have is `None`,
    same as `FixedRateProvider`'s existing behavior for an unconfigured currency -
    nothing downstream needed to change.
  - **Verified the real API before writing the parser, and found a real gotcha
    doing it**: `GET https://api.coingecko.com/api/v3/simple/price?ids=monero&vs_currencies=usd,eur`
    really does return `{"monero":{"usd":530.68,"eur":457.01}}` (live numbers at
    the time, XMR was ~$530-531) - confirmed via `curl` first. An unknown currency
    is simply absent from the inner object (`{"monero":{}}`), not an error or
    `null`; an unknown coin id under `ids=` (not reachable in practice here, since
    `monero` is hardcoded) comes back as a bare `{}`, handled by treating "no
    `\"monero\"` object in the body" as `ExchangeRateError::UnexpectedResponse`.
    **The real gotcha**: a bare `reqwest::Client::new()` gets a flat `403` from
    the live endpoint - Coingecko's edge rejects any request without a
    "descriptive User-Agent" (its own error message's exact wording). Not
    theorized, hit for real: the first run of the live smoke test (below) failed
    with `Status(403, None)` before this was fixed by setting `.user_agent(...)`
    on the client `CoingeckoRateProvider::new` builds. Worth flagging because it
    would have been invisible in every unit test here (the local axum test server
    doesn't care about `User-Agent`) and would only have surfaced once someone
    finally ran this against the real internet - which is exactly why the task
    asked for a live smoke test rather than trusting the fixture tests alone.
  - **The inversion arithmetic and its guardrails**: `piconero_per_unit =
    round(1e12 / price_of_1_xmr_in_that_currency)`, `f64` throughout, matching the
    task's explicit carve-out from `docs/DESIGN.md` §8.1 (documented in the
    module's own doc comment as an exception, not a violation - §8.1 governs
    computing a customer's charge from an already-fixed rate via
    `compute_xmr_amount`, untouched here; Coingecko's own market price has no
    "exact" form to preserve in the first place). Per-currency, not
    all-or-nothing: an absent, non-numeric, non-finite, zero, negative, or
    u64-overflowing price for one currency is logged and that currency alone is
    skipped for the round (a Coingecko bug/outage returning `0`/`null` for one
    currency must not silently price every order in it at zero), while every
    other currency in the same response still updates normally. A failed
    request/non-2xx/malformed-body fails the *whole* `refresh()` call before the
    cache is touched at all, so a transient outage never wipes previously-cached
    rates - mirrors the "a lagging/failing fallback node doesn't corrupt stored
    state" resilience shape this codebase's fallback-node tests already
    established, applied here to the rate cache instead of chain-scan state.
  - **Config** (`src/config.rs`): `ExchangeRateConfig` gained `currencies:
    Vec<String>` (`#[serde(default)]`, empty by default - the exact casing
    written here is what `piconero_per_unit` gets looked up by later, matching
    `FixedRateProvider`'s already-existing case-sensitive-verbatim behavior) and
    `cache_seconds: u64` (`#[serde(default)]` = 60, validated 10-3600 in
    `validate_bounds` - the lower bound exists so a typo can't turn the
    background loop into a hot loop against a free public API this deployment
    depends on for every order's price). `Config::validate` now accepts
    `provider = "coingecko"` alongside the existing `"fixed"`, and rejects
    coingecko mode with an empty `currencies` list as a new
    `ConfigError::CoingeckoNoCurrenciesConfigured` (nothing would ever be
    fetched - a real misconfiguration, not a valid "no currencies yet" state).
    New `ExchangeRateConfig::build_coingecko_rate_provider()` mirrors the
    existing `build_fixed_rate_provider()`, always pointed at the real
    `https://api.coingecko.com` (the base-url override exists on the provider
    type for tests, not as an operator-facing config knob - not asked for, and
    a config surface for pointing production at an arbitrary URL felt like
    unrequested scope).
  - **`main.rs` wiring**: the exchange-rate provider construction now matches on
    `config.exchange_rate.provider` (already validated to be one of the two known
    values by this point). `"coingecko"` builds the provider, calls `.refresh()`
    once synchronously (logs and continues rather than panicking on failure - a
    transient outage at boot shouldn't stop the whole service starting, and every
    order in every configured currency will 400 as unsupported until a refresh
    succeeds, which is loud in the logs, not silent), then `supervise`s a new
    `run_coingecko_refresh_loop` that sleeps `cache_seconds` and calls
    `.refresh()` again, forever - same `supervise` pattern as the existing
    webhook-delivery/scanner/double-spend-revalidation loops, so a panic inside
    it gets logged and restarted rather than silently killing rate updates
    forever. `"fixed"` (and anything else, though `validate` already excludes
    other values) takes the existing unchanged path.
  - **Tests**: 8 new in `exchange_rate.rs` (successful refresh's inversion
    arithmetic checked against a hand-computed value for a known price of 149.23
    USD/XMR -> 6,701,065,469 piconero/USD; an untracked/absent currency stays
    `None`, no panic; a non-object and a non-JSON response body are each a clean
    `Err`, not a panic; a zero price and a negative price in the same response
    each individually skip only their own currency, leaving a third good value in
    that same response intact; an unreachable base URL (`http://127.0.0.1:0` -
    deterministic, no bind/drop race) is a clean `Err`; a later failed refresh
    (simulated outage via a scripted second response) leaves an already-cached
    rate untouched; lookup casing matches configured casing exactly, mirroring
    `FixedRateProvider`) plus a `#[ignore]`d live smoke test, run manually (see
    below). 3 new in `config.rs` (coingecko mode with a real currency list
    parses/validates and `build_coingecko_rate_provider` hands back a working,
    empty-cache provider; empty `currencies` under coingecko mode is rejected,
    both omitted and explicit `[]`; `cache_seconds` bounds - 0, 5, and 3601
    rejected, 10 and 3600 accepted). All pre-existing `provider = "fixed"` tests
    pass unmodified.
  - **Live smoke test, run for real** (not simulated): `cargo test -p
    moneropay-core --lib
    exchange_rate::tests::coingecko::manual_smoke_test_against_the_real_coingecko_api
    -- --ignored --nocapture`, against the real `https://api.coingecko.com`,
    requesting USD and EUR. Actual output: `piconero_per_unit("USD") =
    1883629377, piconero_per_unit("EUR") = 2187274437` - i.e. roughly $530.90 and
    €457.20 per XMR at the time, consistent with the earlier `curl` check (also
    run live: `{"monero":{"usd":530.68,"eur":457.01}}` moments earlier - price
    moved slightly between the two calls, as expected for a live market feed).
  - **Not done / explicitly out of scope**: `init_wizard.rs`'s interactive setup
    flow was not extended to offer `provider = "coingecko"` as a choice - it still
    only ever writes `provider = "fixed"` configs. The task's spec named
    `config.rs` and `main.rs` for wiring, not the wizard; adding a third
    provider-choice branch to an already-scripted interactive prompt sequence
    felt like a separate, non-trivial UI task rather than an oversight worth
    silently folding in here. A self-hoster (or the control-plane's own generated
    config, once that exists) can still hand-write `provider = "coingecko"` into
    the TOML directly - `Config::validate`/`build_coingecko_rate_provider` don't
    care how the file was produced. Also not built: any operator-facing override
    of the Coingecko base URL (see above) or a config knob for which specific
    coin id to query (hardcoded to `monero`, the only one this service could ever
    need).
  - Full `cargo test --workspace`: engine 310 passed/9 ignored (was 299/8, +11
    tests, +1 new `#[ignore]`d live test), control-plane 80 (unchanged), shared
    26 (unchanged), engine-test-support 2 (unchanged), mock-woocommerce 8+1
    (unchanged). `cargo build --workspace` clean, no warnings. Files touched:
    `src/exchange_rate.rs`, `src/config.rs`, `src/main.rs` - no engine schema/
    migration changes, no other crate touched.

- Corroborated key-image voiding + bounded false-positive recovery sweep (not a
  WBS item - the user asked directly whether reconnecting to an honest node would
  ever undo a wrongful void caused by a single lying node's `is_key_image_spent`
  answer; traced the code and found the answer was no - the only un-void path
  (`check_for_reorg_and_reconcile`'s reverse check) only runs when a reorg is
  *also* independently detected, which a key-image lie alone never triggers - then
  asked for a sustainable, low-overhead fix, which this implements in full).
  - **Prevention**: `MoneroDaemonClient` gained a new trait method,
    `is_key_image_spent_corroborated` (default: delegates to the plain method,
    so every existing single-node client - `RpcDaemonClient`, every test double -
    is unaffected). `FallbackDaemonClient` overrides it: polls *every* configured
    node (not just the sticky `current` one), affirms `SpentInBlockchain` only on
    unanimous agreement among however many actually answered, and refuses to
    affirm on disagreement (logged, since it means a configured node is wrong or
    lying) rather than majority-voting - a missed double-spend just gets re-checked
    later, a false one permanently voids real money, so the safe direction to be
    wrong in is clear. `scanner::void_if_double_spend_proven` now calls this
    instead of the bare method. No hot-path cost: `is_key_image_spent` was already
    only ever called from this same rare "a payment vanished" path, never from the
    per-second scan loop.
  - **Recovery**: new `scanner::revalidate_recent_double_spend_voids`, a bounded
    sweep (only payments voided within `DOUBLE_SPEND_RECHECK_WINDOW_SECS` = 48h)
    that re-runs the corroborated check on already-voided payments and reverses
    (`unvoid_as_false_positive`, new) any that no longer hold up. Deliberately its
    own, much slower background loop (`main.rs`, every 5 minutes) rather than
    folded into `run_scan_tick`'s per-second loop - double-spend voids are
    healthily rare, so cost is proportional to how many voids happened recently,
    not to how often the sweep runs. Reversing this way (unlike the pre-existing
    reorg-driven reversal, which deliberately leaves `double_spend_detected_at`
    set - a real conflicting transaction genuinely existed there for a time even
    if later reorged away) clears that sticky order-level flag too, but only once
    *every* voided payment on the order has been cleared - an order with two
    independently-voided payments where only one turns out to be a false
    accusation keeps the flag, since the other still genuinely justifies it. Fires
    a new `order.double_spend_reversed` webhook rather than silently folding into
    whatever status the recompute lands on.
  - New `Store` methods: `find_payments_voided_since` (network- and
    recency-scoped, the sweep's candidate query) and `clear_double_spend_flag`
    (the only way that flag is ever cleared, deliberately separate from
    `mark_double_spend_detected`'s own "first occurrence only" stickiness).
  - **17 new tests**: 7 in `daemon_fallback.rs` (unanimous agreement affirms;
    disagreement refuses and is per-key-image independent of other key images in
    the same call; a single node or only-one-reachable node is trusted as-is;
    every-node-unreachable is an error, not a silent false negative), 2 in
    `scanner.rs` proving the prevention end-to-end through `run_scan_tick`
    (a disagreeing fallback actually prevents the wrongful void a lying single
    node would cause; a genuine, unanimously-corroborated double-spend is still
    voided - no regression), 6 more in `scanner.rs` for the recovery sweep
    (reverses a no-longer-supported void; leaves a still-supported one alone;
    ignores voids outside the recheck window; keeps the flag set when another
    voided payment on the order still justifies it; aborts cleanly - no partial
    writes - if the chain height is unreachable; skips one failed per-payment
    recheck but still processes the rest of the batch), 2 in `store.rs`
    (`find_payments_voided_since`'s recency bounding; `clear_double_spend_flag`'s
    idempotency). `docs/DESIGN.md` §7.7 updated in both the original
    `is_key_image_spent` bullet and the "Fallback nodes widen this trust
    boundary" section, naming every test above by name. Full
    `cargo test --workspace` clean at 299 engine tests (was 282), 0 failed.
  - Explicitly out of scope, matching what was actually asked for: no new config
    knob for the recheck window (a documented constant instead, to avoid
    unrequested config-surface growth); single-node deployments get no benefit
    from the prevention half (nothing to corroborate against) - only the recovery
    sweep helps them, and only within its bounded window.

- Fallback nodes: composed reorg/lagging/all-down tests, plus a real found
  gap (not a WBS item - the user asked directly, after the fallback-node
  feature above, whether the test suite covered a fallback presenting
  different/lagging/split chain state - it didn't, so this closes that).
  `daemon::fake::FakeDaemonClient` gained `set_online(bool)` (defaults
  online via `new()`; every `MoneroDaemonClient` method returns `Err`
  while offline, scripted chain state untouched) so a test can drive a
  real `FallbackDaemonClient` failover rather than hand-swapping daemons.
  4 new tests in `src/scanner.rs`, composing `FallbackDaemonClient` with
  real `FakeDaemonClient`/`DaemonFailingBlockHashAt` doubles through
  `run_scan_tick` (not just unit-testing the fallback client in isolation
  the way `daemon_fallback.rs`'s own 6 tests do):
  - `failing_over_through_a_real_fallback_client_to_a_node_serving_a_different_chain_reconciles_like_a_reorg`
    - the explicit ask: a real failover (primary goes offline, not
    swapped by the test) landing on a genuinely diverging fallback
    reconciles exactly like an ordinary reorg.
  - `failing_over_to_a_lagging_but_honest_fallback_neither_rewinds_nor_corrupts_the_window`
    - the more realistic case (a resyncing backup node, not a malicious
    fork) doesn't rewind or corrupt anything.
  - `every_fallback_node_being_down_fails_the_tick_cleanly_without_corrupting_stored_state`.
  - `a_node_that_dies_between_fetching_a_blocks_transactions_and_its_hash_can_pair_them_with_a_different_nodes_hash`
    - **a genuine, previously-undocumented correctness gap, confirmed
    real by this test, not just theorized**: `run_scan_tick` fetches a
    block's transactions and its hash as two separate daemon calls; since
    `FallbackDaemonClient` fails over per-call, those two calls for the
    same height aren't guaranteed to land on the same node. If the
    primary answers the first and dies before the second, the stored
    (height, hash) pair ends up describing a block that never existed as
    such on any single chain - transactions from one node, hash from
    another. Deliberately **not fixed** - the per-call granularity that
    causes it is also what lets a tick survive a node dying mid-tick,
    which is a real resilience win; pinning failover to one node per tick
    would trade this narrow, low-probability inconsistency for aborting
    the whole tick's remaining work on any transient blip. Documented as
    an accepted, sharper version of the pre-existing "replication lag
    across a pool of backend nodes" tradeoff, not silently left unknown.
  `docs/DESIGN.md` §7.1 and §7.7 updated: §7.7 gained a full "Fallback
  nodes widen this trust boundary" subsection naming both what's covered
  and this one open gap, cross-referenced from §7.1's fallback paragraph.
  Scanner's own test-module coverage-index doc comment (§5 "Dishonest or
  swapped daemons") extended to list all 4 new tests. Full
  `cargo test --workspace` clean at 282 engine tests (was 278), 0 failed.

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

- **WBS 1.5 (the real WooCommerce PHP plugin) is blocked on missing tooling
  in this environment - skipped ahead to 1.7.1 instead, which the WBS itself
  marks as parallel/non-blocking.** Checked directly (not assumed): this
  sandbox has no `php`, `composer`, `docker`, or `wp` (wp-cli) binary, and no
  passwordless `sudo` - `pacman` (this is Arch/CachyOS, not
  apt/dnf) needs root to install anything. 1.5.1's own acceptance test
  explicitly requires `wp-env` (Docker-based WordPress) and WooCommerce
  PHPUnit, neither of which can exist here without installing a real PHP +
  Docker toolchain onto your actual machine (not a disposable container) -
  a system-level change I'm not willing to make unilaterally, and one only
  you can authorize (with your `sudo` password, typed via `!` in the
  terminal, or by setting the toolchain up yourself). Writing the PHP plugin
  code without being able to run it against real WooCommerce would break
  this whole project's established practice of never treating anything as
  "done" without an independently-run, real test proving it - I'd rather
  flag this than fake it. **What I did instead**: moved to WBS 1.7.1
  (Coingecko exchange-rate provider), which only needs the existing Rust
  toolchain and is explicitly marked "parallel within Track A, non-blocking"
  in the WBS - see the progress log entry once it lands. **What you'll want
  to decide**: whether to install `php`, `composer`, and `wp-env`'s Docker
  dependency yourself, grant me the access to do it, or hold Track 1.5 until
  you're working from an environment that already has them.

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
