# Monokulo stylebook

Run `npm ci` then `npm run stylebook` from the repository root. Vite prints the
local address. `npm run build:stylebook` emits an independently deployable static
site in `apps/stylebook/dist` with relative asset URLs; no backend is required.

Every section is generated: `cargo xtask ui` writes `examples/generated`
from the components in `crates/ui`, each example rendered by the Rust
component with its Rust, Solid and HTML source. Don't edit them; change the
component and run it again. A Vite HTML transform puts them in the page, so
the whole catalogue works with JavaScript disabled.

The stylebook imports only the UI package's public exports. Its own CSS is for
navigation and specimen framing; it must not fix component styles. Use the
theme selector and narrow browser widths to review variants, and disable
JavaScript in the browser to see the native fallbacks.

`npm run test:ui` builds the static site and checks the specimens with and
without JavaScript, their keyboard behaviour and narrow light and dark
layouts, and that each Solid component renders what its Rust one does.
Screenshots are written to `test-results/` for review.

Run `npm run lint:ui` to catch new drift before review; see
[component adoption and validation](../../docs/ui-components.md).
