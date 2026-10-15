# Shared UI and stylebook

The root npm workspace contains `packages/ui` (`@monokulo/ui`),
`apps/stylebook` and the existing `crates/monokulo/pos-ui`. One root lockfile
pins their tools. The browser integration suite in `e2e/browser` retains its
independent install and lockfile.

## Ownership

- `packages/ui`: theme tokens, native HTML/CSS contracts and progressive
  enhancement behaviours. No app routes, API calls or framework dependencies.
- `apps/stylebook`: independently runnable review site consuming public exports.
  Its examples are included in HTML at build time, not rendered by JavaScript.
- `crates/monokulo/src/views`: Maud markup and application composition. Rust
  helpers emit the documented HTML; they do not define alternative controls.
- `crates/monokulo/pos-ui`: Solid state, order workflows and terminal layout.
  It composes shared controls. The POS still requires JavaScript.
- `e2e/browser`: checks that components integrate with real workflows.

## Build and serving

Run `npm ci` from the root before building Monokulo. Cargo resolves tools from
root `node_modules` and watches the UI package sources and workspace lockfile.
Docker, CI and OpenWrt preparation use the same root install.

The application embeds the package's token CSS, component CSS and select script
through its existing fingerprinted asset pipeline. CSS and the select script
are already browser-native, so Rust can include their sources without a second
bundling step. POS imports the public component CSS and enhancement entry; the
custom element registration is idempotent. The application does not build or
serve the stylebook.

`npm run build:ui` produces standalone library assets. `npm run build:stylebook`
produces a static site with relative URLs, suitable for independent hosting.
The documentation site consumes the same package token source.

## Adoption

The initial extraction moves theme and self-hosted font ownership, enhanced selects, metadata badges and switches
into the library. Signup/login fields and buttons, checkout's refund input,
and POS primary/danger actions and text inputs use the new explicit primitives.
Existing admin selects and switches now use the package implementation.

Legacy page styles remain in `site.css`, and some specialized POS controls
(keypad, order tiles, scan actions) still have application styling. Migrate
these deliberately after reviewing their contracts in the stylebook. Do not
move application-specific CSS wholesale into the library or add a second
implementation of an existing primitive. The former `views/theme.css` is only
a CSS import compatibility entry for local HTML previews.

For each next adoption: choose an existing primitive or document a reusable
variant, render the stylebook example, update the page markup, remove redundant
page-level styling, and check the real workflow at its smallest supported
viewport. Keep domain components such as order/payment state composition in
the application until multiple consumers justify extracting a presentation
contract.

## Verification

Run `npm run test:ui` (install Chromium once with `npx playwright install chromium`),
`npm run build:ui`, and `npm run typecheck -w monokulo-pos-ui`.
Existing Rust palette checks also inspect the embedded package component CSS.
For POS adoption use the existing `pos-responsive-layout` and terminal browser
suites, plus `cargo test -p monokulo --lib http::pos::tests`.

Breadcrumbs/back links, nested wizard progress and action cards are also shared.
Application breadcrumb trails retain `context-nav` only for page spacing;
`mk-breadcrumbs` owns their appearance. Setup/wallet progress integrates the active task and position inside the
top-level segment, including on mobile, without a separate subtask row. Wallet choice cards use
`mk-action-card`, shared buttons and native GET submission; creating a wallet
still requires browser capabilities. The stylebook shows WooCommerce as a
navigation-card example without introducing a new application workflow.


## Stylebook coverage

The stylebook now covers foundations, typography and spacing; headers/layout;
buttons and icon buttons; the SVG icon set; fields, validation and input actions;
selects, switches and radio cards; breadcrumbs, back links, navigation, account
dropdowns, page tabs and in-page tabs; wizard steps; action cards; disclosures
and facts; network and order-status badges; settings page forms, save bars and
changed/failed states; alerts/toasts; spinners/progress; QR presentation;
ellipsis/copy; tables, empty states and confirmation dialogs.

Settings CSS/JS and order-status badge styling were extracted from the app,
not reimplemented for the preview. The fingerprinted `settings-form.js` URL
still serves the same behaviour from the package. The revised success toast
therefore appears in the app as well as the stylebook. Other new patterns are
available for adoption; this pass does not replace every legacy page's markup.
Specialized engine charts, POS keypads and wallet recovery workflows remain
application compositions. Their underlying reusable controls belong here;
their domain logic stays with the application.

Stylebook examples are static HTML, including expanded SVG icons and a real QR
fixture. Camera scanning is shown disabled until a consumer supplies a scanner;
QR generation remains server/application owned. Page-form examples submit only
a local GET, while production settings preserve their POST/fixi behaviour.

## Preventing new drift

Run `npm run lint:ui` and `npm run test:ui:unit` before the browser checks. CI
runs these for library, stylebook, view, static-script and POS changes.

The source linter parses HTML, JSX/TSX and CSS, and conservatively scans Maud
control declarations. It flags new native controls without shared classes,
DOM-created native controls outside the package, literal CSS colors, numeric
font sizes, visual overrides of `.mk-*` components outside the package,
unnamed icon-only buttons, button-links without destinations and unwrapped
selects. Current legacy violations are recorded by file, rule, normalized
source hash and count in `packages/ui/tools/lint-baseline.json`. Copying an old
violation or changing its declaration does not grant an exemption. All new
shared library and stylebook sources pass without baseline exceptions.

Treat baseline changes as a reviewed migration decision. The explicit command
`node packages/ui/tools/lint.mjs --write-baseline` records the current debt; CI
never runs it. Remove obsolete exceptions during adoption. Do not refresh the
baseline merely to make a failed check pass.

This is a guardrail, not proof of correct design or accessibility: dynamic
markup, business state and application compositions still need review. Browser
contracts check unique IDs, labels, ARIA/SVG references, clipboard semantics,
keyboard interactions, reduced motion, footer visibility and no-JS workflows.
Responsive screenshots support visual review. Real POS and settings browser
tests verify that extracted assets and styles work in their consumers.

The stylebook now also includes original generated logos and animated loading
marks, POS status art, monitoring charts/status summaries, wallet selection,
pagination, footer-based settings pages, and the selected grouped page form and its many-edits preview.
`/form-page.html` is a full-page footer specimen. `#page-hierarchy` demonstrates
using sections and separators instead of nested cards. The flat progress strip is the only step design; native progress values count completed tasks.
