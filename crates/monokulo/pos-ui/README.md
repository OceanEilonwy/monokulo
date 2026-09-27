# POS frontend

The merchant POS is a Solid 2 mini-app mounted by `views/pos.rs`. It renders its own payment card (QR, address, copy, refund address with QR scan) from the authenticated POS order API; it no longer embeds the public checkout page. The POS requires JavaScript; only the public checkout must work without it.

Run `pnpm install --frozen-lockfile` and `pnpm run build` here after changing `src/`. Vite writes `../static/pos-app.js` and `../static/pos-app.css`; these generated files are checked in because the Rust binary embeds them with `include_str!` and must build without Node on the production host. Run `cargo test -p monokulo --lib http::pos::tests` and the deterministic Playwright `surface.spec.js` suite when changing the POS flow.

The top bar is the store name (a link back to the store page), the "All orders" button, and the site's own status indicator and System / Light / Dark theme toggle. `views/pos.rs` renders those two with the same Rust functions as the site nav and the app moves them into its top bar, so they cannot drift; the toggle applies a theme in place rather than reloading the terminal. Design sketches: `docs/pos-background-orders-sketches.html`.

The order list has two tabs. Active is every open POS order, however old (`GET …/pos/orders?state=active`, no paging), and also feeds the background stack. Finished is kept in the page only: orders that finish while this POS is open move there and drop off 24 hours later, and nothing is loaded into it, so a reload starts it empty. The store's Orders page (`/dashboard/stores/{id}/orders`, searchable, with source and POS cancellations) is the full history, linked from Finished and from a search with no matches.

The engine rate-limits each store (120 requests a minute by default). The POS list and live stream therefore read orders from the engine in batches (`?ids=`), never one request per order; keep it that way when adding reads.

The app uses Solid `2.0.0-rc.9` and `@solidjs/vite-plugin` `3.0.0-next.44`. The local POS records hold merchant workflow state; the engine remains the source of truth for payment state and checkout details.
