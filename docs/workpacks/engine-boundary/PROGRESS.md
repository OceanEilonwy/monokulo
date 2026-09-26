# Progress: Monokulo–engine boundary work pack

Work notes for `README.md` in this folder. Keep this current and commit it with every step (see README §4).

## Status

| Step | Title | Status | Commits |
|---|---|---|---|
| 1 | Make the engine private by default | done | `f4495b8` |
| 2 | Stop monokulo using the engine's public routes | done | `11838f0` |
| 3 | Stop monokulo reading or writing the engine's allowed origins | done | `74d4876` |
| 4 | Let a shop's server create orders with its secret key | done | `3600a96` |
| 5 | Give plugins monokulo's address, not the engine's | done | `171ba94` |
| 6 | Fix the WooCommerce plugin | done | `4f415dd` (PHP), `aa13374` (Rust) |
| 7 | Only show browser-created orders inside a verified frame | done | `8dfffd7` |
| 8 | Remove the engine's public surface | done | `e06bcfb` |
| 9a | Client identity (Tor circuit ID, trusted proxies) | in progress (uncommitted, see Resume here) | |
| 9b | Tor's own defences (torrc, docs) | done | `e65834a` |
| 9c | Tiered limits | done | `5387224` (with 9d) |
| 9d | The challenge (pages, JSON API) | done | `5387224` (with 9c) |
| 9e | Settings and screens | done | `9992123` |
| 9f | API and integration changes | done | `2cd674e` (code in `5387224`) |
| 9g | Tests | done | unit/HTTP/Playwright/synthetic PROXY in `2a198a5`, `5387224`, `9992123`; real Tor test: this commit |
| 10 | Docs and cleanup | not started | |

## Resume here

**Shared worktree, read first (added by the reviewer, 26 Sep 12:5x).** A separate POS redesign (Solid 2.0 POS app, `pos_redesign.md`) is being worked on in this same worktree by another agent. Its in-progress state was committed as `a93b4ca` ("WIP: POS redesign ..."), and that agent may resume and keep editing. The rules in README §0 ("Shared worktree") apply from now on: don't touch POS-owned files, stage by explicit path only, and use migration number 0023 or higher.

Steps 1-9 done. Next: step 10 (docs and cleanup: `docs/DESIGN.md` monokulo boundary / verified embed domains / abuse protection section, `docs/WOOCOMMERCE_ROADMAP.md` (+ `WOOCOMMERCE_WBS.md` if relevant) for the plugin integrating through monokulo, check `deploy/` notes, mark every step done). Was: 9g (real tor test `crates/monokulo/tests/e2e_tor.rs`, docs in `e2e/README.md` and `docs/TESTING.md`), step 10.

Known POS-side issue (not mine, not fixed per the shared-worktree rule): clippy `match_single_binding` warning at `crates/monokulo/src/http/pos.rs:344` from the POS redesign.

PHP suite: see decision 13 for how to run it (wp-env's plugin mount collides with WooCommerce).

Note on `crates/mock-woocommerce/tests/e2e_stagenet_connect_flow.rs`: after step 5 it still reads order status from `{credentials.endpoint}/api/v1/t/...`, which now points at monokulo and would 404. It can't run here (stagenet), but step 6 must switch it to monokulo's routes.

Adding an `AppState` field: every literal has `event_streams: Default::default(),`; a one-line script that inserts the new field after that line in every file from `grep -rl 'event_streams: Default::default(),' crates` (except `crates/monokulo/src/main.rs`, edited by hand) covers them all.

Gotcha: never hold `state.db.lock()` in a `for` loop header (`for x in db.lock().unwrap().list(..)`) and lock again inside: the guard lives for the whole loop and the test deadlocks. Also never `pkill -f` a pattern that appears in your own command line.

Note: the reviewer committed `a0abcca` (README only) mid-step 2: step 9g's Tor test must now be a real end-to-end test against the installed tor 0.4.9.12 (`#[ignore]`d, real tor process, SOCKS isolation per visitor). Re-read README 9g before step 9.

Commit SHAs: each step's commit records its own SHA in the *next* step's PROGRESS update (a commit can't contain its own hash). Baseline commit before this work pack: `e4d83da`.

## Test status at last commit

After 9g:
- `cargo test --workspace`: 890 passed, 0 failed, 19 ignored (the new ignored one is the real Tor test). One run in this session hit a pre-existing flake: `scanner http::tests::saving_an_out_of_range_scalar_is_rejected_and_nothing_changes` failed once because another scanner test (`http/tests.rs:1363`) sets `SCANNER_PAYMENT_CONFIRMATIONS_REQUIRED` process-wide while it runs; it passed on the next three runs and the rerun of the whole workspace. Not caused by this work pack; not fixed., 0 failed, 18 ignored (includes the POS redesign snapshot `a93b4ca`)., 0 failed, 18 ignored.
- clippy: per-file warning counts identical to baseline in files I touched; one new warning in POS-owned `http/pos.rs:344` from the POS work.
- Playwright surface: 23 passed (4 new challenge tests in 9d; includes the POS redesign's own surface tests).
- PHP suite: run (decision 13): 43 tests OK; `--group live-monokulo`: 1 skipped (no local config).

Baseline (before step 1), at `e4d83da`:
- `cargo test --workspace`: 854 passed, 0 failed, 18 ignored.
- Playwright surface (`e2e/pos-playwright`, `npx playwright test -c surface.config.js`): recorded here as 19 when the pack was written, but `surface.spec.js` at `e4d83da` contains 18 tests and all 18 pass.
- PHP suite: not run (needs the wp-env test container).

## Notes per step

### Step 1: engine private by default
- `crates/scanner/src/settings.rs`: `SERVER_BIND` default is now `127.0.0.1:8443`. New `is_private_bind_address(IpAddr)` (loopback, RFC 1918, `fc00::/7`, link-local; unspecified and public are not private). Unit tests: `the_default_bind_address_is_loopback_only`, `bind_addresses_are_classified_as_private_or_public`.
- `crates/scanner/src/main.rs`: after binding, prints a `WARNING` to stderr if the listener's local address is not private.
- `crates/scanner/src/http/instance_admin.rs`: example in the `server.bind` validation error now says `127.0.0.1:8443`.
- `deploy/sev-snp/moneropay-engine.service` sets no bind (keeps the default); `scripts/dev-run.sh` already pins `127.0.0.1:8080`. `deploy/sev-snp/README.md` and `docs/DESIGN.md` §4 and §13 now say the engine is private.
- Verified: unit tests above; workspace tests green. Weakness: the warning is only a log line (decision 1).

### Step 2: monokulo off the engine's public routes
- Engine: new `POST /api/v1/admin/tenant/orders/{order_id}/refund-address` (`admin::set_order_refund_address` in `crates/scanner/src/http/admin.rs`, routed in `http/mod.rs`'s admin group). Tenant from `sk_`, stores verbatim like the public route did (the public route had no validation; monokulo's checkout validates the address network before calling). Test: `the_admin_refund_address_route_is_scoped_to_the_tenant_behind_the_secret_key` (no key 401, other tenant 404, owner 200 and value visible in the order detail).
- Monokulo: `EngineClient::set_refund_address(sk, order_id, addr)` uses the admin route; `checkout::set_refund_address` passes the decrypted `sk`. Stale doc comments at the top of `http/checkout.rs` and on the handler fixed.
- Monokulo tests' `seed_real_order` helpers (`http/home.rs`, `http/orders.rs`) now seed through the engine's admin API with the decrypted `sk_` (they take `&AppState`; the tests clone `state` before `build_router`).
- Guard: `engine_client::tests::every_engine_call_uses_the_admin_api_or_status` scans `engine_client.rs`'s own source: every `format!("{}/...` URL must start with `/api/v1/admin/` or be `/status`, and the string `/api/v1/t/` must not appear.
- `docs/DESIGN.md` §10.2 lists the new route.
- Not yet moved (later steps): `scanner-test-support`, the stagenet tests and `mock-woocommerce` tests still call `/api/v1/t/...` (steps 6 and 8).

### Step 3: monokulo no longer touches the engine's allowed origins
- `crates/monokulo/src/http/connections.rs`: `CreateConnectionRequest.allowed_origins` renamed `domains` (optional; `allowed_origins` accepted as alias, decision 2). `create_connection_for_user` always sends `allowed_origins: []` to the engine (decision 4) and puts `domains` into `store_domains` via the new `embed_domains::suggest_domain` (bare domain or URL).
- `crates/monokulo/src/engine_client.rs`: `set_allowed_origins` deleted; `TenantView` and `PatchTenantRequest` no longer carry `allowed_origins`.
- `crates/monokulo/src/embed_domains.rs`: `import_existing_domains(db)` imports only each store's own site domain, once (`domains_imported`), no engine client or key (decision 3). `main.rs` calls it synchronously.
- Tests: `api_domains_join_the_store_and_existing_stores_get_their_site_imported_once`, `the_old_allowed_origins_field_is_accepted_as_an_alias_for_domains` (in `http/embed_domains.rs`); `connect.rs`'s second-site test no longer reads the engine's list. Test JSON bodies across monokulo now send `domains`.
- Verified "no monokulo code path reads or writes the engine's allowed origins" by grep: the only remaining `allowed_origins` in `crates/monokulo/src` are the always-empty create field, the alias, and form-field names in tests proving the old form field is ignored.

### Step 4: secret-key order creation
- `crates/monokulo/src/http/store_key.rs` (new): `store_key_middleware`, `check`, `key_matches` (constant-time via `subtle`). Layered on the `/pay/...` sub-router in `http/mod.rs` before the per-IP limiter (decision 5).
- `http/rate_limit.rs`: a `StoreKeyAuthenticated` request skips the per-IP limiter. `AppState::store_key_rate_limiter` (per store pk) with setting `rate_limit.per_store_key_per_min` (default 600, decision 6), validated in `http/admin_settings.rs`, built in `main.rs`.
- `http/embed_domains.rs::embed_policy_middleware`: for a restricted store, `POST /pay/{pk}/orders` needs a verified `Origin` or the key; neither gives `403` with a message mentioning the secret key (closes phase 2's no-`Origin` gap).
- Migration `crates/monokulo/migrations/0021_order_created_with_key.sql` (default 1 for old rows, decision 7); `Db::create_order_currency_metadata` takes `created_with_key`; `OrderCurrencyMetadataRow.created_with_key`. `pay::create_order` records whether the key was presented; dashboard (`orders::create_order`) and POS (`pos::create_order`) record `true`.
- Tests: `secret_key_orders_are_accepted_and_recorded_and_restricted_stores_need_a_key_or_a_verified_page` (`http/embed_domains.rs`): unrestricted no key/no Origin ok and not keyed; right key ok and keyed; wrong key, other store's key, `Basic` all 401 JSON; failures spend per-IP budget while keyed requests bypass it; restricted: no key+no Origin 403, verified page ok (not keyed), key ok with or without Origin, other store's key 401; per-store key budget 429. Phase 2's restricted test updated (no Origin now 403). POS and dashboard order tests assert `created_with_key`.

### Step 5: plugins get monokulo's address
- `crates/monokulo/src/settings.rs`: `PUBLIC_URL` (`public_url`, `MONOKULO_PUBLIC_URL`), `validate_public_url`, `public_url(db)`, `help(key)` (decision 9). Unit tests for validation and normalisation.
- `http/admin_settings.rs`: validates `public_url`; each monokulo field carries help text (`views/admin.rs` renders `span.field-help`).
- `http/connect.rs`: `public_url_for_plugins`; confirm screen shows the reason and no form while unset (`PlatformConnectViewModel.unavailable`), submission refused, `/finish` returns `503` JSON before redeeming the token; `FinishResponse.endpoint` = `public_url` (decision 8). Test: `plugins_cannot_connect_until_the_public_url_is_set_and_are_told_why`; the round-trip test asserts `endpoint == public_url` and not the engine address.
- Views: `integration_help::fragment(public_key, public_url, is_woocommerce)` (absolute snippets when set); `ConnectViewModel`/`StoreDetailData` carry `public_url` instead of the engine `endpoint` (decision 10).
- `crates/mock-woocommerce` test monokulos (lib and stagenet connect test) set `public_url` to their own listener; assertions now expect monokulo's address.
- Verified: no view, JSON response or plugin-facing value contains `engine_client.base_url()` any more (grep: the only remaining use is `connections.rs` storing it in the internal `moneropay_endpoint` column).

### Step 6: WooCommerce plugin
PHP half (committed first):
- `plugins/woocommerce/includes/class-wc-gateway-monokulo.php`: `create_monokulo_order()` posts `{amount, currency, merchant_order_id}` to `{endpoint}/pay/{pk}/orders` with `Authorization: Bearer {secret_token}`; customer-safe messages for 401/403/429/other with logged detail; redirect to `{endpoint}/pay/{pk}/orders/{order_id}`. `CONNECTION_VERSION`, `needs_reconnect()`, `settings_need_reconnect()`, `render_reconnect_notice()` (decision 11); `is_available()` needs endpoint, public key, secret key and a current connection. Connect: saves `connection_version`; a 503 from `/finish` gives a "Monokulo not ready" notice. Settings labels now say Monokulo address / store keys. Doc comments no longer describe the engine as the plugin's peer.
- `plugins/woocommerce/monokulo.php`: `admin_notices` hook for the reconnect notice.
- Tests: `ProcessPaymentTest.php` (new request shape and header, redirect, 401/403/429 messages, amount formatting, old-install reconnect + notice, never-connected), `ConnectFlowTest.php` (`connection_version` saved, 503 notice), `LiveEngineIntegrationTest.php` renamed `LiveMonokuloIntegrationTest.php` (`@group live-monokulo`, config `tests/live-monokulo.local.json`, checks monokulo's status route and checkout page); `phpunit.xml.dist` and `.gitignore` follow the rename.
Rust half:
- `crates/scanner/src/scanner.rs`: `recompute_and_notify` is now `pub` (documented) so test support can settle an order like a scan does.
- `crates/scanner-test-support/src/lib.rs`: `TestEngineHandle::mark_order_paid(order_id)` records a synthetic confirmed full payment and runs `recompute_and_notify`, queueing `order.paid`.
- `crates/mock-woocommerce/src/lib.rs`: `create_order` takes the secret key and sends `Authorization: Bearer`; `CreatedOrder` carries `address` and `xmr_amount_piconero` from monokulo's response. New default-run test `a_full_woocommerce_checkout_is_created_with_the_key_opened_and_paid`: connect (endpoint = monokulo), keyed order, wrong key 401, checkout page shows the address, `mark_order_paid`, signed `order.paid` webhook delivered by the engine's background delivery loop and verified, monokulo's `/status` says paid.
- Stagenet tests (`crates/mock-woocommerce/tests/e2e_stagenet_connect_flow.rs`, `e2e_stagenet_confirmation_threshold.rs`) now create orders with the key and read status from monokulo's `/pay/{pk}/orders/{id}/status`, and take address/amount from monokulo's create response. Compile-checked only (need stagenet).
- Acceptance ("a WooCommerce checkout reaches a payment page and gets paid, end to end, in a test that runs by default"): the new mock-woocommerce test. The PHP plugin itself is covered by its mocked unit tests; nothing runs real PHP against a real monokulo by default (the live PHP test is opt-in).

### Step 7: browser-created orders only inside a frame
- `crates/monokulo/src/http/checkout.rs`: `must_open_from_shop` (restricted store + `created_with_key = 0` + `Sec-Fetch-Dest` present and not `iframe`/`frame`), `open_from_shop_response` (403), `with_vary_on_fetch_dest`; applied in `checkout_page` and `checkout_share_page` (decision 14).
- `crates/monokulo/src/views/checkout.rs`: `open_from_shop_page`.
- Tests: `a_restricted_stores_browser_created_orders_only_open_inside_a_frame` (`http/embed_domains.rs`): unrestricted opens; restricted browser order: `document` 403 with the page, no script, no address, `Vary`; `iframe`/`frame`/absent render; share page 403 as document; `/status` still 200; keyed order opens for `document`/`iframe`/absent and via share. Playwright: `a browser says whether it is loading a page as a frame...` (decision 15).
- Weakness: the rule trusts the browser's `Sec-Fetch-Dest` and lets header-less requests through (by design, per the plan); old orders from before migration 0021 count as keyed (decision 7).

### Step 8: engine public surface removed
- `crates/scanner/src/http/mod.rs`: router has an unauthenticated group (tenant creation, `/status`), the `sk_` admin group and the instance-settings group; no `/api/v1/t/...` routes, no CORS layer, no `public_orders_route_pk`. `AppState::rate_limiter` removed.
- `crates/scanner/src/http/public.rs` renamed `orders.rs`: only `create_order_for_admin` and its request/response types remain.
- `crates/scanner/src/http/rate_limit.rs`: only the admin (per-token, address fallback) middleware (decision 17). `server.rate_limit_per_ip_per_min` removed from `settings.rs`, `instance_admin.rs`, `main.rs`.
- `allowed_origins` gone from `Tenant`/`NewTenant`/`TenantConfigPatch`/SQL (`store.rs`), admin create/patch/view (`admin.rs`), `local_admin.rs`, `cli.rs` (flag removed), `main.rs --show-tenant`; migration `crates/scanner/migrations/0014_drop_tenant_allowed_origins.sql` + test `migration_0014_drops_the_tenant_allowed_origins_column_and_keeps_the_tenant` (decision 18).
- Monokulo `EngineClient::CreateTenantRequest` has no `allowed_origins`. `scanner-test-support`, `e2e_harness`, `crates/scanner/tests/e2e_stagenet.rs` (+ `tests/support/mod.rs`), `scripts/dev-run.sh`, `e2e/moneropay-stagenet.toml`, `tower-http` `cors` feature dropped from the scanner crate.
- Tests: engine HTTP tests now create/read orders through the admin API; new `the_engine_serves_no_public_order_routes_and_no_cors` and `unauthenticated_routes_are_limited_per_address_by_the_admin_limiter`; removed public-only tests (public confirmation override, disallowed origin, two CORS tests). Monokulo's scanner-settings admin test no longer lists the removed setting and now counts against `ALL_SCALAR`.
- Docs: `docs/DESIGN.md` (§5 table, §8 schema note, §10.1, §10.2 table, §10.3 "No public API", §10.4 note, §12 rewritten, §13 `[ddos]`, §14 wording), `docs/TESTING.md` (rows for the engine's absent public surface, monokulo's engine-call guard, the per-token limiter, and the §11 origin row now pointing at monokulo's embed policy).
- Shared instance token: not added (decision 16).

### Step 9a: client identity
- `crates/monokulo/src/abuse/mod.rs` (`AbuseConfig`, `AbuseProtection`: per-client and per-store-key limiters, stream cap, `reload`), `abuse/identity.rs` (`ClientIdentity`, `/64` grouping, `IpNet`, `TrustedProxies`, `client_address` for `X-Forwarded-For`), `abuse/proxy_protocol.rs` (`parse_v1_header`, `OnionPeer::identity`, `OnionListener`, `validate_onion_listener`), `abuse/streams.rs` (was `http/stream_limit.rs`, now keyed by `ClientIdentity`, adjustable cap).
- `crates/monokulo/src/http/abuse.rs`: `abuse_middleware` + `anonymous_identity` (decision 19). `http/rate_limit.rs` deleted; `http/store_key.rs` keeps only `check`/`key_matches`/`StoreKeyAuthenticated`.
- `checkout_events` takes the identity from extensions for the stream cap. `main.rs` builds `AbuseProtection` from settings and starts the onion listener when set. Admin settings validate and hot-reload `abuse.trusted_proxies`, `abuse.onion_listener`, `abuse.stream_cap`.
- Tests: unit (`abuse::identity`, `abuse::proxy_protocol`, `abuse::streams`), HTTP `clients_behind_a_trusted_proxy_get_their_own_budgets_and_untrusted_forwarding_is_ignored`, and the default-run socket test `crates/monokulo/tests/onion_listener.rs` (per-circuit budgets, header-less connections dropped, ordinary listener ignores PROXY).

### Steps 9c + 9d: tiers and the challenge
- `crates/monokulo/src/abuse/limiter.rs` (`TieredLimiter`, `Tier`, `Limits`, pass, LRU cap), `abuse/challenge.rs` (`Challenges`: issue/redeem proof and wait tokens, replay store, `solve` for tests), `abuse/stats.rs` (last-hour issued/solved/refused), `abuse/mod.rs` (`AbuseConfig` with soft/hard/signed-in/store-key/stream cap/bits/under-attack, `limits_for`, `AbuseProtection::check`).
- `crates/monokulo/src/http/abuse.rs`: `pay_middleware` (on `/pay/...`) and `site_middleware` (all other routes except static files) share `guard`; route classes and responses per decision 23. Signed-in sessions are `User` identities.
- `crates/monokulo/src/views/challenge.rs` (interstitial + too-many-requests page), `crates/monokulo/static/challenge.js` (Web Crypto solver, falls back to the wait URL), served at `/static/challenge.js`.
- `crates/monokulo/static/monokulo-client.js`: `fetchSolvingChallenges` for `createOrder` and the `/status` poll; header comment documents the 429 shape.
- CORS (`http/mod.rs::cors_layer_base`) allows `Monokulo-Proof`, exposes `Monokulo-Challenge` and `Retry-After`.
- Settings: `abuse.soft_per_min`, `abuse.hard_per_min`, `abuse.signed_in_per_min`, `abuse.challenge_bits`, `abuse.under_attack` (validated; hard must exceed soft; hot-reloaded). `rate_limit.per_ip_per_min` removed.
- Tests: unit (limiter tiers/pass/rolling minute/LRU cap, challenge signing/expiry/replay/wrong client/wait not-before/fail-closed cap, stats window, interstitial markup), HTTP (`http::abuse::tests`: interstitial + proof + redirect, JSON challenge + header proof + wrong connection, hard limit 429 + Retry-After for API/page/stream, signed-in never challenged, under-attack, CORS), Playwright (JS solve, no-JS wait, cross-site frame with/without JS, client library solves order challenge) against a Node stand-in server that implements the same protocol with the real `challenge.js`/`monokulo-client.js`.
- Weakness: the Playwright tests use a stand-in server's interstitial markup (same data-attribute contract, pinned by the Rust view test), not monokulo's own rendered page.

### Step 9e: settings and screens
- Admin settings (`crates/monokulo/src/views/admin.rs`): an "Abuse protection" heading groups `abuse.*` and `rate_limit.*` fields with an explanation; each field has help text (`settings::help`) and is validated (`http/admin_settings.rs`, incl. hard > soft); saving hot-reloads (`AbuseProtection::reload`).
- Status page (`http/status_page.rs`, `views/status.rs`): admins see under-attack on/off and last-hour challenges issued/solved/refused; anonymous visitors and merchants don't. Test `only_operators_see_challenge_activity_on_the_status_page`.
- Merchants: nothing to configure.

### Step 9b: Tor's own defences
- `deploy/tor/torrc.snippet`: onion service to `127.0.0.1:8082` (monokulo's onion listener), `HiddenServiceExportCircuitID haproxy`, `HiddenServicePoWDefensesEnabled 1` (+ `PoWQueueRate 50`/`Burst 250`), `HiddenServiceEnableIntroDoSDefense 1` (rate 25/s, burst 200), `HiddenServiceMaxStreams 64` + `MaxStreamsCloseCircuit 1`.
- `docs/TOR.md`: why the onion listener, how to check tor has PoW (`tor --list-modules` -> `pow: yes`), every line explained, monokulo settings, how to run the real-tor test.
- Verified: `tor --verify-config` accepts the snippet (tor 0.4.9.12, `pow: yes`).

### Step 9f: API and integration changes
- Code landed with 9d (`5387224`): the `429` + `challenge` shape, `Monokulo-Challenge`/`Monokulo-Proof`, CORS allow/expose, `monokulo-client.js` header comment and solver. WooCommerce plugin and engine unchanged (key-authenticated / private).
- Docs: new `docs/ABUSE_PROTECTION.md` (identities, tiers, matrix, JSON and page challenge flows, headers, settings, why not Anubis). DESIGN.md links to it in step 10.

### Step 9g: tests
- Unit: `abuse::identity`, `abuse::proxy_protocol`, `abuse::limiter`, `abuse::challenge`, `abuse::stats`, `abuse::streams`, `views::challenge`.
- HTTP: `http::abuse::tests` (7 + status page), plus the key/embed tests from steps 4 and 7.
- Browser (Playwright surface): JS solve, no-JS wait, cross-site frame both ways, client library solving an order challenge.
- Synthetic PROXY listener (default run): `crates/monokulo/tests/onion_listener.rs`.
- Real Tor (`#[ignore]`d): `crates/monokulo/tests/e2e_tor.rs`, new dev-dependency `tokio-socks 0.5`; `TieredLimiter::clients()` added for it. **Run here and passed** against tor 0.4.9.12 and the live network: `test result: ok. 1 passed ... finished in 315.90s` (bootstrap, descriptor reachable, 2 then 4 distinct circuits, A challenged then proof accepted then 429+Retry-After, B unaffected, C capped at 3 streams, D allowed, tor accepted PoW/export/intro-DoS/stream settings).
- Docs: `e2e/README.md` (real Tor section), `docs/TESTING.md` (rows for challenge, identity, tiers, embed policy/key auth, real Tor, browser tests, default-run WooCommerce checkout).
- Weakness: the Tor test uses soft 5/hard 12/stream cap 3 to keep it short, not the production defaults.
