# Progress: Monokulo–engine boundary work pack

Work notes for `README.md` in this folder. Keep this current and commit it with every step (see README §4).

## Status

| Step | Title | Status | Commits |
|---|---|---|---|
| 1 | Make the engine private by default | done | `f4495b8` |
| 2 | Stop monokulo using the engine's public routes | done | (this commit) |
| 3 | Stop monokulo reading or writing the engine's allowed origins | not started | |
| 4 | Let a shop's server create orders with its secret key | not started | |
| 5 | Give plugins monokulo's address, not the engine's | not started | |
| 6 | Fix the WooCommerce plugin | not started | |
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

Steps 1-2 done. Next: step 3 (stop monokulo reading or writing the engine's allowed origins: `connections::create_connection_for_user`, `EngineClient::set_allowed_origins`, `embed_domains::import_existing_domains`; rename the JSON `/connections` field `allowed_origins` to `domains`).

Note: the reviewer committed `a0abcca` (README only) mid-step 2: step 9g's Tor test must now be a real end-to-end test against the installed tor 0.4.9.12 (`#[ignore]`d, real tor process, SOCKS isolation per visitor). Re-read README 9g before step 9.

Commit SHAs: each step's commit records its own SHA in the *next* step's PROGRESS update (a commit can't contain its own hash). Baseline commit before this work pack: `e4d83da`.

## Test status at last commit

After step 2:
- `cargo test --workspace`: 858 passed, 0 failed, 18 ignored.
- clippy: per-file warning counts identical to baseline (checked with a per-file count diff).
- Playwright surface: not re-run (no JS/HTML/CSS touched).
- PHP suite: not run.

Baseline (before step 1), at `e4d83da`:
- `cargo test --workspace`: 854 passed, 0 failed, 18 ignored.
- Playwright surface (`e2e/pos-playwright`, `npx playwright test -c surface.config.js`): 19 passed.
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
