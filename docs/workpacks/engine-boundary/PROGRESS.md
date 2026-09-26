# Progress: Monokulo–engine boundary work pack

Work notes for `README.md` in this folder. Keep this current and commit it with every step (see README §4).

## Status

| Step | Title | Status | Commits |
|---|---|---|---|
| 1 | Make the engine private by default | done | (this commit; see `git log --grep "engine private"`) |
| 2 | Stop monokulo using the engine's public routes | not started | |
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

Step 1 done. Next: step 2 (admin refund-address route on the engine; switch `EngineClient::set_refund_address` in `crates/monokulo/src/engine_client.rs` to it; fix the doc comment at the top of `crates/monokulo/src/http/checkout.rs`; add the route to `docs/DESIGN.md` §10.2). Baseline commit before this work pack: `e4d83da`.

Commit SHAs: each step's commit records its own SHA in the *next* step's PROGRESS update (a commit can't contain its own hash).

## Test status at last commit

After step 1:
- `cargo test --workspace`: 856 passed, 0 failed, 18 ignored.
- clippy: no new warnings in touched files (pre-existing ones in `crates/scanner/src/main.rs` at lines 126 and 419, and in `instance_admin.rs` doc comments, are unchanged).
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
