# Progress: Monokulo–engine boundary work pack

Work notes for `README.md` in this folder. Keep this current and commit it with every step (see README §4).

## Status

| Step | Title | Status | Commits |
|---|---|---|---|
| 1 | Make the engine private by default | not started | |
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

Nothing started yet. Begin with step 1. Baseline commit: `e4d83da`.

## Test status at last commit

Baseline (before step 1), at `e4d83da`:
- `cargo test --workspace`: 854 passed, 0 failed, 18 ignored.
- Playwright surface (`e2e/pos-playwright`, `npx playwright test -c surface.config.js`): 19 passed.
- PHP suite: not run (needs the wp-env test container).

## Notes per step

(none yet)
