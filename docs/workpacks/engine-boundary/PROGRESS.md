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
| 6 | Fix the WooCommerce plugin | in progress (PHP half done) | (PHP half: this commit) |
| 7 | Only show browser-created orders inside a verified frame | not started | |
| 8 | Remove the engine's public surface | not started | |
| 9a | Client identity (Tor circuit ID, trusted proxies) | not started | |
| 9b | Tor's own defences (torrc, docs) | not started | |
| 9c | Tiered limits | not started | |
| 9d | The challenge (pages, JSON API) | not started | |
| 9e | Settings and screens | not started | |
| 9f | API and integration changes | not started | |
| 9g | Tests | not started | |
| 10 | Docs and cleanup | not started | |

## Resume here

Steps 1-5 done; step 6's PHP half is committed. Next (step 6, Rust half):
- Extend `crates/mock-woocommerce` so a full checkout runs by default: `create_order` there must send `Authorization: Bearer {secret_token}`; add a test named `a_full_woocommerce_checkout_is_created_with_the_key_opened_and_paid` (the PHP live test's doc comment already names it) that connects, creates an order with the key, opens the checkout page, marks the order paid on the test engine and waits for the signed `order.paid` webhook at the mock receiver.
- Switch `crates/mock-woocommerce/tests/e2e_stagenet_connect_flow.rs` and `e2e_stagenet_confirmation_threshold.rs` status checks from the engine's `/api/v1/t/...` to monokulo's `/pay/{pk}/orders/{id}/status` (address/amount come from monokulo's create-order response).

PHP suite: see decision 13 for how to run it (wp-env's plugin mount collides with WooCommerce).

Note on `crates/mock-woocommerce/tests/e2e_stagenet_connect_flow.rs`: after step 5 it still reads order status from `{credentials.endpoint}/api/v1/t/...`, which now points at monokulo and would 404. It can't run here (stagenet), but step 6 must switch it to monokulo's routes.

Adding an `AppState` field: every literal has `event_streams: Default::default(),`; a one-line script that inserts the new field after that line in every file from `grep -rl 'event_streams: Default::default(),' crates` (except `crates/monokulo/src/main.rs`, edited by hand) covers them all.

Gotcha: never hold `state.db.lock()` in a `for` loop header (`for x in db.lock().unwrap().list(..)`) and lock again inside: the guard lives for the whole loop and the test deadlocks. Also never `pkill -f` a pattern that appears in your own command line.

Note: the reviewer committed `a0abcca` (README only) mid-step 2: step 9g's Tor test must now be a real end-to-end test against the installed tor 0.4.9.12 (`#[ignore]`d, real tor process, SOCKS isolation per visitor). Re-read README 9g before step 9.

Commit SHAs: each step's commit records its own SHA in the *next* step's PROGRESS update (a commit can't contain its own hash). Baseline commit before this work pack: `e4d83da`.

## Test status at last commit

After step 5:
- `cargo test --workspace`: 864 passed, 0 failed, 18 ignored.
- clippy: per-file warning counts identical to baseline (checked with a per-file count diff).
- Playwright surface: 18 passed (run after step 5; views changed).
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
