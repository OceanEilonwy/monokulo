# Monokulo stylebook

Run `npm ci` then `npm run stylebook` from the repository root. Vite prints the
local address. `npm run build:stylebook` emits an independently deployable static
site in `apps/stylebook/dist` with relative asset URLs; no backend is required.

Examples live in `examples/*.html`. A Vite HTML transform includes each example
and its escaped source in the delivered HTML, so the whole catalogue works with
JavaScript disabled. Edit examples directly: they are the single source for
both the live specimen and its displayed markup.

The stylebook imports only the UI package's public exports. Its own CSS is for
navigation, typography and specimen framing; it must not fix component styles.
Use the theme selector and narrow browser widths to review variants. Disable
JavaScript in the browser to inspect real native fallbacks. Demo forms submit
locally by GET and do not call application APIs or create accounts.

`npm run test:ui` builds the static site and checks native form submission,
disclosures, keyboard interaction, programmatic select updates, reset and narrow
light/dark layouts, plus native styling on the development server. Screenshots are written to `test-results/` for review.

The selected page form is `/form-grouped.html`; `?edits=many` previews seven
edited fields across three sections using real change tracking. The catalogue
includes this preview under `#form-options`. `/form-page.html` uses the same
grouped design. The flat progress strip is the sole step design.
Original logos and loading marks use generated assets from `@monokulo/ui/brand`.
The HTML transform expands those SVGs and namespaces each instance's clip IDs.
POS status artwork comes from the same shared symbols that the POS imports.
The monitoring examples are static snapshots with labelled data alternatives.

Run `npm run lint:ui` to catch new drift before review; see
[component adoption and validation](../../docs/ui-components.md).
