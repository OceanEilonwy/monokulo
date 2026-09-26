# POS frontend

The merchant POS is a Solid 2 mini-app mounted by `views/pos.rs`. The payment and refund form remains the shared checkout page in an iframe.

Run `pnpm install --frozen-lockfile` and `pnpm run build` here after changing `src/`. Vite writes `../static/pos-app.js` and `../static/pos-app.css`; these generated files are checked in because the Rust binary embeds them with `include_str!` and must build without Node on the production host. Run `cargo test -p monokulo --lib http::pos::tests` and the deterministic Playwright `surface.spec.js` suite when changing the POS flow.

The app uses Solid `2.0.0-rc.9` and `@solidjs/vite-plugin` `3.0.0-next.44`. The local POS records hold merchant workflow state; the engine remains the source of truth for payment state and checkout details.
