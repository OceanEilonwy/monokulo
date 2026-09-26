# Work pack: Monokulo–engine boundary, WooCommerce fix, and abuse protection

Files in this folder:
- `README.md`: this work pack (the spec).
- `PROGRESS.md`: the running work notes, the resume point.
- `DECISIONS.md`: the decision log.

Read all three before doing anything; section 4 explains how to keep them.

You are implementing an agreed plan in an existing Rust/PHP/JS codebase. The plan below has been reviewed and approved by the project owner. Implement it **exactly**. Where the plan leaves a detail open, make the best engineering decision, record it in the decision log (see "Reporting"), and carry on. Do not stop to ask questions. There is nobody to answer them.

Your work will be reviewed independently, step by step, against this document. Commits that mix steps, skip acceptance criteria, or quietly deviate from the plan will be sent back.

---

## 0. Working environment and rules

- **Repository / worktree:** `/home/henry/Downloads/mokulo/.claude/worktrees/woocommerce-roadmap-doc` (a git worktree on branch `worktree-woocommerce-roadmap-doc`). Work only here. Never `cd` into the main checkout.
- **Git:**
  - Commit after each step, one or more commits per step, never one commit spanning two steps.
  - Message style matches the log: `area: summary` (e.g. `monokulo: ...`, `scanner: ...`, `scanner+monokulo: ...`, `woocommerce plugin: ...`, `docs: ...`). Follow it with a wrapped plain-English body saying what changed and why.
  - End every commit message with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - Do not push. Do not amend or rewrite existing commits.
  - Never use bare `git stash` / `git stash pop`. The stash stack is shared with other sessions.
- **Shell:** fish is the login shell. Use `bash -c` if you need bash syntax, or write plain commands.
- **Crates:**
  - `crates/scanner` is **the engine** (binary `moneropay-core`, a.k.a. "scanner"/"engine").
  - `crates/monokulo` is the public web app (dashboard, checkout, POS, embed library).
  - `crates/shared` holds shared code (rate limiter, migrations runner, money formatting).
  - `crates/scanner-test-support` spawns a real engine for tests.
  - `crates/mock-woocommerce` contains e2e tests driving the WooCommerce connect flow.
  - `plugins/woocommerce` is the PHP WooCommerce gateway plugin.
  - `e2e/pos-playwright` holds the Playwright tests. `surface.spec.js` is the fast mocked suite; `pos.spec.js` needs stagenet funds and must not be run.
- **Shared worktree (added mid-way; applies from step 9 on).** A separate POS redesign (Solid 2.0 POS app; task list `pos_redesign.md`) is being worked on in this same worktree by another agent, which may be editing files while you work. Its snapshot is commit `a93b4ca`.
  - **Don't modify POS-owned files:** `crates/monokulo/src/http/pos.rs`, `crates/monokulo/src/views/pos.rs`, `crates/monokulo/pos-ui/`, `crates/monokulo/static/pos-app.*`, `crates/monokulo/migrations/0022_pos_orders.sql`, `e2e/pos-playwright/helpers.js`, `e2e/pos-playwright/tests/pos.spec.js`, `pos_redesign.md`, `docs/pos-background-orders-sketches.*`.
    - The one exception is a mechanical change to an `AppState` literal in `http/pos.rs`'s tests, kept to that hunk only.
    - Where step 9 needs POS behaviour (the POS is never challenged; the POS stream is keyed by the signed-in user), do it in the shared middleware and router, not in POS files. If a POS file change is truly unavoidable, keep it minimal and log it in `DECISIONS.md`.
  - **`surface.spec.js` is shared.** Add your own tests; don't change the POS ones.
  - **Stage by explicit path only.** Never `git add -A`, `git add .` or `git commit -a`. If a file you need to commit also holds POS changes, stage only your hunks (build a patch and `git apply --cached`). Before every commit, check `git diff --cached` contains only your changes.
  - **Migrations:** the POS work owns `0022`; use `0023` or higher.
  - **Tests:** if the POS work breaks the build or the tests while you're working, don't fix it. Record it in `PROGRESS.md` and carry on, testing your own changes as far as you can.
- **Tests you must run and keep green before every commit:**
  - `cargo test --workspace` (currently 854 passed, 0 failed, 18 ignored).
  - `cargo clippy --workspace --all-targets`: add no new warnings in files you touch. Pre-existing warnings elsewhere are fine.
  - Playwright surface tests when you touch JS/HTML/CSS: `cd e2e/pos-playwright && npx playwright test -c surface.config.js`. Use absolute paths or a subshell; don't leave the shell in another directory.
  - `node --check` on any JS file you edit.
  - PHP: `plugins/woocommerce/vendor/bin/phpunit` needs the wp-env test container (`WP_TESTS_DIR`). Docker is installed. Try `npx @wordpress/env start` / `npx wp-env run tests-cli ...` from `plugins/woocommerce` if it's configured (look for `.wp-env.json`). If the PHP suite genuinely can't be run here, at minimum run `php -l` on every PHP file you change, update the unit tests to the new behaviour anyway, and say clearly in your report that the PHP suite was not run and why.
  - Stagenet e2e tests (`*_stagenet*.rs`, `pos.spec.js`): don't run them (they need real funds and nodes), but they must still compile.
- **Code style:**
  - Match the surrounding code's naming, idiom and comment density.
  - This codebase uses long, explanatory doc comments explaining *why*. Write new ones in that spirit, but plainly and without filler.
  - Views are `maud` in `crates/monokulo/src/views/*.rs`. Shared CSS is in `crates/monokulo/src/views/head.html`.
- **Hard project rules (from the owner; violating these is a failed review):**
  1. **Progressive enhancement.** Customer-facing pages (checkout, share, and the new challenge interstitial) must work with JavaScript disabled. JavaScript only enhances. All formatting and rendering happens in Rust (server-side); never send raw timestamps or data for JS to format. Do not add frontend frameworks or libraries (htmx was explicitly rejected).
  2. **The checkout embed and whatever embeds it know nothing about each other.**
     - `static/checkout.js` must never use `postMessage`, `window.parent` or `window.top` (a test enforces this).
     - The POS page and `monokulo-client.js` follow orders through their own requests.
     - Presentation options are generic URL parameters (`?view=compact`, `?refund=false`), never named after an embedder.
  3. **Everything external goes through monokulo; the engine is private.**
     - Nothing outside the server talks to the engine.
     - Monokulo talks to the engine only through its admin API (authenticated with the store's `sk_` secret key), plus `/status`.
  4. **Separation of concerns between monokulo and engine.** Customer/merchant/plugin-facing concepts (pricing, checkout, verified domains, embed policy, CORS, rate limits, challenges) live in monokulo only. The engine keeps chain scanning, orders/payments and outbound webhooks.

---

## 1. Background: where things stand

Recent commits (read them for context; `git show --stat <sha>`):

- `f873b92`: SSE live updates.
  - The engine broadcasts order changes. `GET /api/v1/admin/tenant/events` streams them.
  - Monokulo's `crate::live::LiveHub` shares one upstream stream per store.
  - The checkout gets `/pay/{pk}/orders/{id}/events` (fragments plus status); the POS gets `/pos/events`.
  - The no-JS checkout meta-refreshes every 60s.
- `baa3df7`: no-JS "Auto Refresh: ON/OFF" toggle; `?view=pos` renamed to `?view=compact`; the embed is independent of the embedder.
- `8f1d3d0`: three changes.
  - Per-(IP, store pk) open-stream cap (`http/stream_limit.rs`, 16).
  - Any-origin CORS on `/pay/...` (`embed_cors_layer` in `http/mod.rs`).
  - The status indicator is server-rendered with the last known health and JS polling.
- `55d519b`: verified embed domains, phase 1.
  - `crate::embed_domains` (DNS TXT verification at `_monokulo.<domain>`, 3-day grace, daily re-check).
  - `http/embed_domains.rs` (settings UI and store-page warnings).
  - `store_domains` table (migration 0019).
- `e4d83da`: verified embed domains, phase 2.
  - "Only allow my verified domains" switch.
  - `embed_policy_middleware`: sends `frame-ancestors`, and returns `403` on `POST /pay/{pk}/orders` when the page's `Origin` isn't verified; a request with no `Origin` is let through.
  - Per-store CORS predicate.
  - Allowed-origins fields removed from the connect forms.
  - One-time startup import of site domains and engine allowed origins (`embed_domains::import_existing_domains`, `domains_imported` column, migration 0020).

Facts established during planning (verify each as you go; line numbers may have drifted):

- **The engine listens publicly by default:** `crates/scanner/src/settings.rs`, `SERVER_BIND` default `"0.0.0.0:8443"`. `scripts/dev-run.sh` pins it to `127.0.0.1:8080`. Also check `deploy/sev-snp/*.service` and its README.
- **Unauthenticated engine routes:**
  - `POST /api/v1/admin/tenants` (tenant creation; "none (DDoS layer only)" per `docs/DESIGN.md`).
  - Public order routes: `POST /api/v1/t/{pk}/orders`, `GET /api/v1/t/{pk}/orders/{order_id}` and `POST /api/v1/t/{pk}/orders/{order_id}/refund-address`. The engine applies CORS and an `allowed_origins` check to these (`crates/scanner/src/http/mod.rs` `build_cors_layer`, `crates/scanner/src/http/public.rs` `resolve_public_tenant`).
- **Monokulo still calls an engine public route:** `EngineClient::set_refund_address` posts to `/api/v1/t/{pk}/orders/{order_id}/refund-address` (`crates/monokulo/src/engine_client.rs`). The doc comment at the top of `crates/monokulo/src/http/checkout.rs` wrongly says it reads the public status API.
- **Monokulo still touches the engine's allowed origins:**
  - `connections::create_connection_for_user` forwards `allowed_origins` (the JSON `/connections` field) to `create_tenant`.
  - `EngineClient::set_allowed_origins` exists but is unused.
  - `embed_domains::import_existing_domains` reads `tenant.allowed_origins`.
- **The WooCommerce plugin is broken and bypasses monokulo:**
  - `/connect/{platform}/finish` (`crates/monokulo/src/http/connect.rs`, `FinishResponse`) returns `endpoint: state.engine_client.base_url()`, i.e. the **engine's** URL. It also returns `secret_token` (the store's `sk_`) and `public_key`, and registers the plugin's webhook with the engine.
  - The plugin (`plugins/woocommerce/includes/class-wc-gateway-monokulo.php`) saves that endpoint. At checkout it posts `{fiat_amount, fiat_currency, merchant_order_id, description}` to `{endpoint}/api/v1/t/{pk}/orders` and redirects the customer to `{endpoint}/pay/v1/{pk}/{order_id}`.
  - Both are broken. The engine's create-order request now requires `xmr_amount_piconero` (it is XMR-only since `docs/fx_refactor.md`), and the engine has no `/pay/v1` route at all.
  - The plugin's unit tests mock HTTP; `LiveEngineIntegrationTest` skips unless configured. Nothing caught this.
- **Monokulo's own order creation:**
  - `POST /pay/{pk}/orders` (`http/pay.rs::create_order`): used by `monokulo-client.js`. It prices fiat into XMR and records `order_currency_metadata`.
  - POS: `http/pos.rs::create_order`.
  - Dashboard: `http/orders.rs::create_order`.
- **Monokulo's current rate limiting:**
  - `shared::rate_limit::RateLimiter<IpAddr>`, 20/min per IP by default (`RATE_LIMIT_PER_IP_PER_MIN`), layered on the whole `/pay/...` sub-router in `http/mod.rs`.
  - Peer IP comes from `ConnectInfo`, failing open when absent (tests).
  - Monokulo binds `127.0.0.1:8081` (hard-coded in `crates/monokulo/src/main.rs`).
- **Several places construct monokulo's `AppState` directly:** the `http/*.rs` test modules, `crates/mock-woocommerce/src/lib.rs`, `crates/mock-woocommerce/tests/*.rs` and `crates/scanner/src/bin/e2e_harness.rs`. When you add fields, update all of them. `cargo test --workspace` catches it; `-p monokulo` alone does not.
- **Toolchain:** `tor` 0.4.9.12 is installed at `/usr/bin/tor`, built with the proof-of-work module (`tor --list-modules` shows `pow: yes`). `php` 8.5 and `composer` are installed, and so is `docker`.

---

## 2. Target architecture

- **Engine (private):**
  - Watches the chain, stores orders and payments, sends webhooks out to shops.
  - Reached only by monokulo, through its admin API with the store's `sk_` (plus `/status`).
  - No public routes, no CORS, no origin lists.
- **Monokulo (public):**
  - Everything people and plugins touch: pricing, checkout, POS, dashboard, verified domains, embed policy, CORS, rate limits and challenges.
  - The only public address, on clearnet, Tor or both.
- **WooCommerce plugin:** talks to monokulo only. It creates orders with the store's secret key, sends customers to monokulo's checkout, and receives webhooks.
- **Kept as-is:**
  - Webhooks go directly from the engine to the shop (the engine calling out, set up by monokulo).
  - Phase 2's browser-`Origin` check stays for the embed library; step 4 adds the key path next to it.

---

## 3. The steps (implement in this order)

### Step 1: Make the engine private by default
- Change the engine's `server.bind` default to `127.0.0.1:8443`.
- Log a clear startup warning when the configured bind address is not loopback and not a private range (RFC 1918, IPv6 ULA `fc00::/7`, link-local).
- Check `deploy/sev-snp` and `scripts/dev-run.sh` keep the engine private, and fix them if not. Say so in the deploy docs (`deploy/sev-snp/README.md` and/or `docs/DESIGN.md`).
- Update any test or doc that asserts the old default.
- **Done when:** a default engine can't be reached from another machine, and a public bind is loudly flagged (unit-test the address classification).

### Step 2: Stop monokulo using the engine's public routes
- Add an admin, `sk_`-authenticated route to the engine for setting an order's refund address (e.g. `POST /api/v1/admin/tenant/orders/{order_id}/refund-address`, same validation as the public one, tenant taken from `sk_`). Switch `EngineClient::set_refund_address` to it.
- Grep monokulo for any other `/api/v1/t/` call and move it off too. Fix the stale doc comment in `http/checkout.rs`.
- Add the new route to the admin API table in `docs/DESIGN.md` §10.
- **Done when:** monokulo only calls `/api/v1/admin/...` and `/status` on the engine (add a test or grep-guard if practical).

### Step 3: Stop monokulo reading or writing the engine's allowed origins
- Monokulo always creates tenants with an empty `allowed_origins`.
- The JSON `POST /connections` field `allowed_origins` is renamed `domains`. Those go into monokulo's `store_domains` list only (as today via `suggest_site_domain`).
  - Decide how to treat old callers that still send `allowed_origins`: either ignore it or accept it as an alias for `domains`. Pick one, document it, test it.
- Delete `EngineClient::set_allowed_origins`.
- Cut `embed_domains::import_existing_domains` down to each store's own site domain (local to monokulo). Nothing is read from the engine, so it no longer needs the engine client or the encryption key. Keep it once-only via `domains_imported`.
- **Done when:** no monokulo code path reads or writes the engine's allowed origins.

### Step 4: Let a shop's server create orders with its secret key
- **Key auth:**
  - `POST /pay/{pk}/orders` accepts `Authorization: Bearer sk_...`.
  - Check it against the store's stored key (`store_connections.tenant_secret_token_encrypted`; decrypt with `crate::crypto`) using a **constant-time** comparison.
  - A present but wrong key gets `401` (JSON error). The key must belong to the store in the path.
- **Restricted stores:** with embedding restricted (`embed_restricted`), an order must come from either a browser page on a verified domain (existing `Origin` check) or a request carrying the valid key. A request with **neither** is refused (`403`, clear JSON message). This closes phase 2's "no `Origin`, let it through" gap. Unrestricted stores keep accepting unauthenticated requests as today.
- **Record the source:** record on each order whether it was created with the key. Add a column to monokulo's order metadata (`order_currency_metadata`, via a new migration).
  - Orders created by the dashboard and the POS are also key-equivalent (trusted, created by the merchant), so mark them the same way.
  - Orders created by `POST /pay/{pk}/orders` without a key are not.
  - Step 7 uses this.
- **Rate limits:** unauthenticated requests keep the per-IP limit. Key-authenticated requests bypass the per-IP limiter and get their own per-store limit (choose a sensible default and make it a setting). Step 9 folds this into the tiered design.
- **Done when:** a restricted store accepts orders from its own server (key) and its verified pages, and nothing else. Tests cover right key, wrong key, other store's key, no key plus no `Origin` on restricted and unrestricted stores, and the recorded flag.

### Step 5: Give plugins monokulo's address, not the engine's
- Add a `public_url` setting to monokulo (`crates/monokulo/src/settings.rs`, env var in the same style, e.g. `MONOKULO_PUBLIC_URL`).
  - It is monokulo's external base URL, clearnet or `.onion`, and must parse as an http(s) URL with no path/query beyond `/`.
  - Editable in admin settings (`http/admin_settings.rs` and its view), with validation and help text.
- `/connect/{platform}/finish` returns `public_url` as `endpoint`.
  - While `public_url` is unset, the connect flow refuses to finish with a clear error the plugin can show, rather than handing out a wrong address. Decide where this check belongs (the confirm screen, `/finish`, or both) so the merchant sees it early.
  - Webhook registration stays as it is: monokulo registers it with the engine for the plugin.
- Remove the engine address still passed around, unrendered, to `integration_help` and the store/connect views (their `endpoint` fields, `let _ = endpoint;`). If `integration_help` should show monokulo's public URL instead, do that.
- **Done when:** nothing a merchant or plugin receives contains the engine's address.

### Step 6: Fix the WooCommerce plugin (can't take payments today)
- **Order creation:**
  - Create orders at `{endpoint}/pay/{pk}/orders` with `Authorization: Bearer {secret_token}`.
  - Body: `{amount, currency, merchant_order_id}`, using monokulo's `POST /pay/{pk}/orders` request shape (read `http/pay.rs`). Amount is a plain decimal string, currency the WooCommerce order currency.
  - Monokulo prices it into XMR. Handle monokulo's error responses (including 401/403/429) with customer-safe messages and logged detail, as the plugin already does.
- **Redirect:** send the customer to monokulo's checkout at `{endpoint}/pay/{pk}/orders/{order_id}`. Webhook handling is unchanged.
- **Existing installs:** they hold the engine's address. Detect them by recording a settings/schema version in the plugin when it connects through the new flow. An install connected before this version (no marker) is treated as needing a reconnect: show a WooCommerce admin notice asking the merchant to reconnect, and make `is_available()` false until they do, rather than failing orders. Decide the exact detection mechanism; it must not rely on guessing URLs.
- **Tests:**
  - Update the PHP unit tests' mocked calls (`plugins/woocommerce/tests`).
  - Point `LiveEngineIntegrationTest` at monokulo, renaming it if the name no longer fits.
  - Extend the Rust mock-WooCommerce e2e coverage so a full checkout runs **by default**, without stagenet: connect, create an order with the key through monokulo, open the checkout page, and receive the paid webhook. Use the test engine; the scanner test support can mark an order paid, see how other tests do it.
  - Switch any test status checks from the engine's public route to monokulo's `/pay/{pk}/orders/{id}/status`.
- **Done when:** a WooCommerce checkout reaches a payment page and gets paid, end to end, in a test that runs by default.

### Step 7: Only show browser-created orders inside a verified frame
- For a **restricted** store, the checkout page (`GET /pay/{pk}/orders/{order_id}`) of an order created **without** the key renders only when framed. Framed means the request's `Sec-Fetch-Dest` is `iframe` (or `frame`).
  - As a full page (`Sec-Fetch-Dest: document`), it instead shows a plain server-rendered page: "Open this payment from the shop's website", styled like the checkout's not-found page, with no JS needed.
- Orders created with the key (WooCommerce, dashboard, POS, payment links) open either way.
- Browsers that don't send `Sec-Fetch-Dest` are let through.
- The `/share` page frames the checkout from monokulo itself, so key-created orders shared from the dashboard keep working.
- `frame-ancestors` (phase 2) already stops framing on unverified sites.
- Apply the rule to the `/events` and `/status` routes of such orders only if it's clearly right; document the choice.
- **Done when:** a restricted store's browser-created orders can't be opened as a full page in a modern browser. Add HTTP tests with the header set and unset, and a Playwright surface test if practical.

### Step 8: Remove the engine's public surface
- Delete the engine's `/api/v1/t/{pk}/...` routes and handlers, its CORS layer, its origin checks, and the rate limiter that only served public routes. Keep the admin limiter if it's still used.
- Drop `allowed_origins` from tenant create, patch and view in the engine API. Add an engine migration dropping the column (follow the scanner's migration conventions in `crates/scanner/migrations`, including how columns are dropped in SQLite there). Update monokulo's `EngineClient` request/response types, `scanner-test-support`, `e2e_harness`, the stagenet configs and all tests.
- `POST /api/v1/admin/tenants` stays unauthenticated at the application layer but is reachable only by monokulo now. Decide whether to add a shared instance token as defence in depth; if you add one, wire it through monokulo's existing scanner admin token setting. Record the decision.
- Update `docs/DESIGN.md` (API tables, §12 origin enforcement, CORS mentions) and `docs/TESTING.md` rows that describe the removed public API and origin checks.
- **Done when:** the engine's router has only admin, status and internal routes, and the whole workspace (including stagenet tests, compile-only) builds and passes.

### Step 9: Abuse protection that works for Tor and clearnet

Today every limit keys on the connecting address. Behind a reverse proxy, or on an onion service where every visitor arrives from `127.0.0.1`, one busy address throttles everyone, and one attacker throttles them too.

**Why not Anubis** (decided; don't use it):
- It needs JavaScript; the checkout must work without it.
- Its pass is a cookie, and our checkout is a third-party frame where Tor Browser, Safari and Firefox block or partition cookies.
- The embed library's `fetch`, SSE streams, CORS preflights and server integrations can't solve a challenge page.
- It's another service in front of a payment path.

The alternative:

#### 9a. Who is the client?
- **Onion:**
  - Tor can export the circuit a connection came from (`HiddenServiceExportCircuitID haproxy` sends a PROXY protocol v1 header; the circuit ID is encoded as an IPv6 address in `fc00::/16`, check the tor manual).
  - Monokulo gets an optional **second listener**, bound to loopback only and configured by a setting, which requires and parses that header. Its client identity is the circuit ID. Tor Browser keeps one circuit per site per session, so this behaves like a per-visitor address.
  - The ordinary listener must **never** trust a PROXY header (anyone could fake one).
  - Implement the listener with axum's `serve` over a custom listener or equivalent. Keep `ConnectInfo`-style access so middleware can read the client identity.
- **Clearnet:**
  - Use the peer address. When the peer is in the trusted-proxy list (setting: addresses and CIDR ranges), take the last untrusted address in `X-Forwarded-For` instead.
  - Group IPv6 by `/64`.
- **Authenticated callers:** a merchant's session keys by user; a shop's secret key (step 4) keys by store. They have their own higher limits and are **never challenged**.
- One client identity type is used by the rate limiter, the per-(client, store) stream cap (replace the `IpAddr` key in `http/stream_limit.rs`) and the challenge.

#### 9b. Tor's own defences (onion installs; config, not code)
- In `torrc`, set `HiddenServicePoWDefensesEnabled 1`: under load Tor makes clients solve a puzzle whose difficulty rises with the attack. Tor Browser solves it automatically, with no JS.
  - Needs tor ≥ 0.4.8 built with the PoW module; say how to check.
- Also set:
  - `HiddenServiceEnableIntroDoSDefense 1`, with sensible rate and burst values;
  - `HiddenServiceMaxStreams` with `HiddenServiceMaxStreamsCloseCircuit 1`;
  - `HiddenServiceExportCircuitID haproxy`, pointed at the loopback onion listener.
- Ship a documented `torrc` snippet in `deploy/` and a doc page explaining each line.

#### 9c. Tiered limits in monokulo
- Each client identity gets a token bucket on the public routes.
  - Under the **soft limit**, nothing changes.
  - Past it, the client must solve a challenge (9d).
  - Past the **hard limit**, everything gets `429` with `Retry-After`.
- A solved challenge gives that identity a fresh allowance for 10 minutes, held in memory. **No cookie** is set.
- Memory is capped: evict least-recently-seen identities first, so a flood of new circuits can't exhaust it.
- **Under-attack mode (9e):** every client that isn't signed in is treated as past the soft limit on pages and the JSON API. It doesn't change streams, which can't be challenged.
- Choose defaults for the soft and hard limits, the challenge difficulty and the pass duration, and record them. The current flat limit is 20/min per IP across `/pay/...`. Pick values that don't challenge a normal customer's checkout visit plus its polling/streaming, and explain the arithmetic in the decision log.

#### 9d. The challenge
- **Shape:**
  - A signed, short-lived challenge carrying difficulty, expiry and the client identity, signed with a per-process random key (HMAC), so the server stores nothing per challenge.
  - The answer is a nonce such that SHA-256(challenge ‖ nonce) has at least *difficulty* leading zero bits.
  - Single-use (track used challenges until they expire, memory-capped) and bound to the client identity it was issued to.
- **Pages** (checkout, share, landing, login, sign-up): an interstitial styled like monokulo, which works inside a frame.
  - **With JS:** solves the puzzle with Web Crypto and continues by itself, usually in a second or two, with a progress line.
  - **Without JS:** "Checking your connection, this page continues in 10 seconds", then a `<meta http-equiv="refresh">` to a URL carrying a signed wait token that is only valid 10 seconds after issue. Waiting is the proof.
  - After success, redirect back to the original URL (strip the challenge parameters).
  - The page explains in plain words why it's there and is accessible (it announces itself to screen readers).
  - Rendering is in Rust (maud). The solver script is a small static file in the style of `checkout.js`, with no libraries.
- **JSON API** (`POST /pay/{pk}/orders`, `GET .../status`):
  - Past soft, return `429` with a `challenge` object in the JSON body and a `Monokulo-Challenge` header. The client retries with a `Monokulo-Proof` header.
  - `monokulo-client.js` does this automatically, so merchants change nothing.
  - CORS allows the `Monokulo-Proof` request header and exposes `Monokulo-Challenge` (both the per-store CORS layer and anything else relevant).
- **SSE streams:** they can't solve anything. Over the limit they get `429`; `checkout.js` already backs off (5s doubling to 60s). The page load before them will have been challenged.
- **Never challenged:**
  - the dashboard after login;
  - the POS;
  - key-authenticated plugin calls;
  - webhooks;
  - static files (`/static/...`).

#### 9e. Settings and screens
- **Admin settings, new "Abuse protection" section.** Each field has a short explanation, and invalid values are refused.
  - trusted proxies (addresses/CIDRs);
  - onion listener address (off by default);
  - soft and hard limits per minute;
  - stream cap;
  - challenge difficulty;
  - under-attack switch.
- **Status page, operators only:** admins see challenges issued, solved and refused in the last hour, and whether under-attack mode is on. Anonymous visitors don't see this.
- **Merchants:** nothing to configure. Customers see the interstitial rarely, and never on a second request within the allowance.

#### 9f. API and integration changes
- A new response shape: `429` with `challenge` and `Retry-After`. Document it in the API docs and in the embed library's header comment.
- New headers: `Monokulo-Challenge` (response) and `Monokulo-Proof` (request), added to the CORS allow/expose lists.
- WooCommerce plugin: no change (uses the key).
- Engine: no change (private).

#### 9g. Tests
- **Unit:**
  - client identity: trusted vs untrusted `X-Forwarded-For`, IPv6 `/64` grouping, PROXY v1 parsing, and rejection or ignoring of PROXY headers on the public listener;
  - challenge signing, expiry, replay, wrong identity, the wait token's not-before;
  - bucket tiers and the memory cap.
- **HTTP:**
  - soft limit gives a challenge and the proof is accepted;
  - hard limit gives `429` with `Retry-After`;
  - authenticated callers are never challenged;
  - the CORS headers are correct;
  - under-attack mode.
- **Browser (Playwright surface):**
  - the interstitial solves and continues with JS;
  - it waits and continues without JS;
  - both work inside a cross-site frame;
  - `monokulo-client.js` solves an order-creation challenge on its own.
- **Tor: a real tor end-to-end test (owner's requirement: real tor, not a mocked environment).** Tor is installed (see §1).
  - **Test file:** add a Rust e2e test (e.g. `crates/monokulo/tests/e2e_tor.rs`). It needs the live Tor network, so follow the stagenet tests' convention: `#[ignore]` by default, run with `cargo test -p monokulo --test e2e_tor -- --ignored --nocapture`.
  - **Server side:** the test launches a **real `tor` process** with a temporary `DataDirectory` and a v3 onion service pointing at monokulo's loopback onion listener. It uses exactly the `torrc` lines this step ships in `deploy/`: `HiddenServiceExportCircuitID haproxy`, `HiddenServicePoWDefensesEnabled 1`, the intro DoS defence and `HiddenServiceMaxStreams`. Monokulo runs in-process on loopback.
  - **Client side:** the test connects to the `.onion` through tor's own `SocksPort`. Each simulated visitor uses a different SOCKS username/password, so `IsolateSOCKSAuth` gives it its own circuit. Adding a SOCKS-capable dev-dependency is fine: `reqwest`'s `socks` feature or `tokio-socks`.
  - **Wait for readiness:** wait for bootstrap and for the onion descriptor to be reachable, by retrying a request with a generous timeout of several minutes. Fail with a clear message on timeout; never pass silently.
  - **It must verify:**
    - monokulo sees a distinct client identity (circuit ID) per visitor;
    - pushing one visitor past the soft limit challenges only that visitor while another keeps getting normal responses;
    - past the hard limit, only that visitor gets `429`;
    - the open-stream cap applies per (circuit, store);
    - tor accepted the proof-of-work settings, checked through the control port (`GETCONF`) or tor's startup log, not assumed.
  - **Docs:** document how to run it in `e2e/README.md` and `docs/TESTING.md`.
  - **Keep the fast synthetic PROXY-header integration test as well.** It runs by default and guards the parser and the rule that the public listener rejects PROXY headers. It doesn't replace the real test.

#### Rate-limiting matrix (this is the agreed behaviour)

"Soft" and "hard" are the per-client limits from 9c, counted over a rolling minute.

| | Pages (checkout, share, landing, login, sign-up) | JSON API (create order, status) | Live updates (SSE) |
|---|---|---|---|
| **Tor** | Client: Tor circuit ID (loopback onion listener). Before monokulo: Tor PoW (automatic in Tor Browser), intro rate limit, connections per circuit capped. Past soft: interstitial (JS puzzle, or 10s wait without JS). Past hard: `429` page, readable without JS. | Client: circuit ID. Past soft: `429` + challenge, solved by `monokulo-client.js`. Past hard: `429` + `Retry-After`. | Client: circuit ID. At most 16 open streams per (circuit, store); the 17th gets `429`. Past soft or hard: `429`, and `checkout.js` backs off 5s → 60s. |
| **HTTPS** | Client: peer IP, or the forwarded IP from a trusted proxy; IPv6 by `/64`. Nothing of ours before monokulo. Past soft/hard: same as Tor. | Same IP rule. Past soft/hard: same as Tor; CORS lets any site solve the challenge. | Same IP rule; 16 open streams per (IP, store); `429` plus back-off. |
| **Signed in** (merchant session or a shop's secret key) | Keyed by user, own higher limit, never challenged (dashboard, POS). Past limit: `429` page. | Keyed by store (key) or user (session), own higher limit, never challenged (WooCommerce). Past limit: `429` + `Retry-After`. | POS stream keyed by user (32 orders per stream), never challenged. Past limit: `429`; the POS shows "Lost connection" and retries. |

Never counted: static files and webhooks.

- **Done when:** one abusive Tor circuit or IP is slowed down and challenged on its own, while everyone else is unaffected.

### Step 10: Docs and cleanup (as each step lands, finish here)
- `docs/DESIGN.md`:
  - the engine is private;
  - its API tables lose the public routes and gain the admin refund-address route;
  - add a section on the monokulo boundary, verified embed domains and abuse protection.
- `docs/TESTING.md`: replace engine origin-check rows with monokulo's embed-policy, key-auth, `Sec-Fetch-Dest` and abuse-protection tests.
- `docs/WOOCOMMERCE_ROADMAP.md` (and `WOOCOMMERCE_WBS.md` if relevant): the plugin integrates through monokulo (secret-key order creation, monokulo checkout redirect).
- `deploy/`: the torrc snippet and the engine-private note (from steps 1 and 9).
- Do **not** edit anything under `~/.claude` (memory). The reviewer handles that.
- Leave this work-pack folder in place when done (it records how the work was done); mark every step done in `PROGRESS.md`.

---

## 4. Progress notes, decisions and reporting

All three live in this folder (`docs/workpacks/engine-boundary/`), are committed, and are how work resumes if a session ends mid-way. **A new agent must be able to pick up from these files and `git log` alone.**

### Before you start (and whenever you resume)
1. Read this `README.md` in full.
2. Read `PROGRESS.md` and `DECISIONS.md`.
3. Run `git log --oneline` and `git status`.
4. Continue from `PROGRESS.md`'s "Resume here" section. Don't redo finished steps.
5. If `git status` shows uncommitted work, check it against the notes: finish it or revert it deliberately, and write down which you did.

### `PROGRESS.md` (work notes): keep it current
- **Update it and commit it with every step's commit.** Also commit it at any meaningful milestone inside a long step, e.g. step 6's PHP half done or step 9a done, so a mid-step stop loses little.
- A work-in-progress commit is fine when you must stop mid-step. Mark it `WIP:` in the subject and say so in "Resume here". The step's own final commit comes after.
- Keep this structure:
  - **Status table:** one row per step (1–10, with 9a–9g as sub-rows), status (`not started` / `in progress` / `done`), and commit SHAs.
  - **Resume here:** the exact next action, any half-finished work and where it is (files and functions), and anything a newcomer must know that isn't obvious from the code.
  - **Test status at last commit:** `cargo test --workspace` totals, clippy status for touched files, Playwright surface totals, and whether the PHP suite was run.
  - **Notes per step:** what was done, where (files), how its acceptance criteria were verified, and known weaknesses.
- Write it for someone with no context: plain, specific, no shorthand.

### `DECISIONS.md` (decision log)
Add an entry every time the plan leaves something open or you have to deviate. Number the entries and commit them with the step. Each entry gives:
- the step;
- the decision;
- the alternatives considered;
- why.

### Final report (your last message)
List:
1. Each step: its commits (sha and subject), what was done, and its acceptance criteria and how each was verified.
2. Test results: the exact `cargo test --workspace` totals, clippy status for touched files, Playwright surface totals, and PHP suite status (run or not, and why).
3. Every decision from `DECISIONS.md`, one line each.
4. Anything not done, done differently from the plan, or known to be weak. Be explicit; don't bury it.

Work through all ten steps. If a step turns out much bigger than expected, still finish it; don't skip ahead. Report failures faithfully: if a test fails and you can't fix it, say so with the output.
