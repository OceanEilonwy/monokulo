# Shared UI and stylebook

## Components are written once, in Rust

A shared component is declared once in `crates/ui` with `component!`: its
props as a struct and its markup as a template in a small maud-like
language that only reads props. From that one declaration:

- Monokulo's views (and, as it migrates, the pages site) build it with a
  typed builder and render it through maud: `(Button::new("Save").variant(ButtonVariant::Primary))`.
  Required props are `new`'s arguments and choices are enums, so a missing
  label or a misspelt variant doesn't compile; the macro also type-checks
  every prop, field and enum variant a template names.
- `cargo xtask ui` writes the Solid component the POS imports
  (`@monokulo/ui/solid`, in `crates/ui/solid`), the stylebook's
  section for it (`apps/stylebook/examples/generated`) and fixtures of its
  examples (`crates/ui/browser/solid/generated`). Each specimen shows how to
  write the example in Rust and in Solid, and the HTML both render.
- `npm run test:ui` renders every example through the generated Solid
  component and compares its DOM with the Rust component's HTML
  (`solid-equivalence.spec.js`). CI runs `cargo xtask ui --check`, so the
  committed files are always what the Rust components write.

Never edit the generated files. To add or change a component: edit or add
it in `crates/ui/src/components`, give it examples (they are what the
stylebook shows and what Solid is checked against), add it to
`catalogue()`, run `cargo xtask ui`, and keep its CSS and behaviour in
`crates/ui/web/components`. A caller adds attributes (`fx-*`,
`data-*`, Solid's `onClick`) through the component's spread; `class` and
`style` belong to the component, so a new look is a new variant, and a
consumer positions a component with a data attribute or its container.

Truly custom visualisations (the engine page's block chart, the logs
waterfall, wallet backup) stay in the application.

## Interactive components: behaviour in Solid, islands on the server

A component with behaviour declares it in `component!`, between its props and
its template:

```rust
behaviour "select.behaviour.ts" {
    expanded: AriaBool,          // a value the template reads as `b.expanded`
    shown: Vec<SelectItem>,
    fn toggle,                   // a handler: `on:click=toggle`
    fn choose(usize),            //   with an argument: `on:click=choose(item.index)`
    ref native,                  // an element handed over: `ref=native`
}
```

The behaviour is Solid, in `web/components/<file>`: `setup(props)` returns
those values (as accessors), handlers and refs. `cargo xtask ui` writes the
interface it must meet into the generated component, so TypeScript checks it
against the Rust declaration. The template keeps all the markup: what only
the behaviour draws goes inside `@if enhanced { ... }`, attributes that change
once it runs say `if enhanced`, and `b.*` is only read inside `@if enhanced`
(the macro refuses it elsewhere, since the server renders without it).

- **The server** renders an interactive component's markup as it is with
  JavaScript off. Its root element is the island: `data-island="Select"`
  names the component, `data-props` holds the props its behaviour reads
  (`uses(...)` in the behaviour block, and what the attach itself reads),
  `data-mount` says when it attaches. A `data-mk` mark is on each element
  the behaviour attaches to, and a `<!--mk:N-->` marker where `@if enhanced`
  goes; an `@if enhanced { } @else { }`'s fallback sits between that marker
  and `<!--/mk:N-->`.
- **The browser** runs one script, `web/islands.ts` (`ui-islands.js`;
  monokulo builds it with the engine page's script in `apps/engine`, the
  Pages site as one file with `vite.islands.config.js`): one
  MutationObserver attaches every island on the page and those fixi or a
  script add, at once, when visible or on first interaction, and lets go of
  those taken away. Each behaviour is its own chunk (the generated
  `solid/islands.ts` imports it on first use), so a page fetches only the
  behaviours it has, and Solid only if one of them needs it. Attaching calls the
  generated `attach<Name>`: the behaviour's handlers and refs bind to the
  server's own elements, its enhanced markup is inserted at the markers, and
  any fallback goes. Nothing else the server rendered is replaced, so a
  field keeps its value and focus, and other scripts' listeners and fixi
  keep working. There are no custom elements, and no wrapper elements to
  change what CSS selectors match; an island inside another (a setting in a
  card in a form) attaches on its own.
- **The POS** renders the generated Solid component itself: the same
  template, `enhanced` true, the same behaviour.

The behaviour attaches to elements outside `@if enhanced` too: one with an
`on:` handler, a `ref`, or an attribute that only applies once enhanced is
marked. That includes each item of a loop over a prop's list (the Tabs'
tabs and panels), found by its position. An attribute that reads `b.*`
(`aria-selected="true" if tab.id == b.selected`) is absent from the
server's markup and kept up to date by the behaviour. An island's props
leave out slots: the server's markup holds them, and `@if enhanced` can't.

Write behaviour signals that are set while attaching (in a `ref`) with
`{ ownedWrite: true }`. A script that changes a component's field (a
select's value) says so with an `input` or `change` event; changes to its
options or attributes are followed by themselves.

Generated so far: Button, LinkButton, Alert, Field, WizardSteps,
TabLinks, and the interactive Select, Tabs, CopyValue and the settings
pages' SettingsForm, SettingsCard, Setting, SaveBar and Toast. Every
dropdown and every settings page on monokulo is these. The settings
islands share one store per form (`settings.store.ts`). Only the menu
(`menu.js`) still upgrades hand-written markup.
Monokulo's setup steps and sign-up/log-in forms and the POS's Charge,
order and Reference controls use them. The hand-written examples below
cover what hasn't moved yet.

The root npm workspace contains `crates/ui` (`@monokulo/ui`, also the Rust crate `ui`),
`apps/stylebook`, the existing `apps/pos` and the engine page's
`apps/engine`. One root lockfile
pins their tools. The browser integration suite in `e2e/browser` retains its
independent install and lockfile.

## Ownership

- `crates/ui`: one package for Rust and npm. The components (`src/`), theme
  tokens, CSS and progressive-enhancement behaviours (`web/`), and the
  generated Solid components (`solid/`). No app routes or API calls.
- `apps/stylebook`: independently runnable review site consuming public exports.
  Its examples are included in HTML at build time, not rendered by JavaScript.
- `crates/monokulo/src/views`: Maud markup and application composition. Rust
  helpers emit the documented HTML; they do not define alternative controls.
- `apps/pos`: Solid state, order workflows and terminal layout.
  It composes shared controls. The POS still requires JavaScript.
- `apps/engine`: the engine page's live view, an app that requires
  JavaScript, like the POS. Monokulo renders only its shell (the header and
  network picker, `views::engine`) and sends data: the state machine's
  states, effects and marks (`engine_view`) and the machine's figures. The
  app writes every word (`present.ts`, `machine.ts`) and draws it in Solid
  views of signals the stream and the playback loop set, reusing the shared
  components (Select, HelpTip, Disclosure, Alert, Table, Checkbox); what is
  placed by measured pixels, and the timeline's canvas, stay as code around
  the measuring. Its unit tests (`npm run test:engine`) check the words
  against states the Rust machine writes (`test/fixtures.json`). Its build
  also makes monokulo's islands script, so the engine page shares their
  copy of Solid.
- `e2e/browser`: checks that components integrate with real workflows.

## Build and serving

Run `npm ci` from the root before building Monokulo. Cargo resolves tools from
root `node_modules` and watches the UI package sources and workspace lockfile.
Docker, CI and OpenWrt preparation use the same root install.

The application serves the package's tokens (`theme.css`), component CSS
(`ui.css`, after its own `site.css`), scripts and fonts from `ui::assets`
through its fingerprinted asset pipeline: the crate bakes them in, so
monokulo never reads the package's files by path. They are already
browser-native, so there is no second bundling step. POS imports the public component CSS and enhancement entry; the
custom element registration is idempotent. The application does not build or
serve the stylebook.

`npm run build:ui` produces standalone library assets. `npm run build:stylebook`
produces a static site with relative URLs, suitable for independent hosting.
The documentation site consumes the same package token source.

## Adoption

Every server-rendered page (monokulo's views, the pages site) and the POS
use `crates/ui` components for everything shared; only the custom
visualisations stay application markup (the engine page's block chart and
timeline, the logs histogram, trace waterfall and log rows, the
proof-of-work pip chain, wallet backup words, and the testing report's
figures, trees and charts).

### Left as page markup, and why

- The custom visualisations above.
- The POS's background-order stack (small horizontal chips) and its hidden
  file input.
- The wallets' test-networks fold; `span.tag-active`; `dl.dns-record`.

### Converting a page

A page's markup becomes `ui` components wherever one fits; what's left is
layout (the page's own `div`s and their classes), plain text elements
(`p`, `h3`, `ul`, `code`, `strong`, links in text) and the custom
visualisations listed above. Components keep the classes the app shipped
(`.box`, `.error`, `.hint`, `.facts`, `.tag`, `.checks`, …), so a converted
page looks the same.

| Hand-written | Component |
|---|---|
| `button` (any `.btn*` class or none) | `ui::Button` (`.variant(Primary/Danger/Quiet)`, `.size(Compact)` for `.btn-sm`, `.kind(Submit)`) |
| `a class="btn…"` | `ui::LinkButton` |
| `button.icon-only` | `ui::IconButton` |
| a row of buttons, or a lone submit under fields | `ui::FormActions` (gap and space above; `span class="spacer"` pushes the rest right) |
| `input type=text/email/url/number/password/search` | `ui::Input` (its label elsewhere) or `ui::Field` (label, hint and error together) |
| `textarea` | `ui::TextArea` |
| `input type=checkbox` | `ui::Checkbox` (`.text(...)` beside it, or `.label(...)` in a table) |
| on/off switch | `ui::Switch` (`views::controls::switch` wraps it) |
| radio cards | `ui::RadioCards` |
| `div.box` | `ui::Panel` |
| `p.error/.warning/.success/.notice` | `ui::Alert` with a `Tone` (an error is `role="alert"`, the rest `role="status"`; `.quiet(true)` for neither; `.block(true)` for a `div`) |
| `p.hint`, `span.hint` | `ui::Hint` (`.inline(true)` for the span) |
| a page's `h1`, its breadcrumbs, actions beside it, a wizard's steps | `ui::PageHeader`, required by `views::Page` (`Page::new(title, header).render(chrome, body)`) |
| `h2`–`h4` | `ui::Heading` |
| a dialog's content, also a page without JavaScript | `views::DialogContent`: `.in_dialog()` in the `ui::Dialog`, `.page(crumbs, size)` for the page's header and body |
| `dl.facts` | `ui::Facts` |
| `span.tag…` | `ui::Tag` (`.tone(TagTone::…)`, or `.state(OrderState::…)`; `.short(…)` for a phone) |
| network badge | `views::network_badge` (`ui::NetworkBadge`) |
| shortened id/address | `views::short_id` / `short_address` (`ui::ShortValue`) |
| `details` (plain, `.keys-help`, `.more-options`) | `ui::Disclosure` with a `DisclosureLook` |
| `ul.checks` | `ui::Checks` |
| `ol.timeline` | `ui::Timeline` |
| `table` (`.table-cards`, `.table-stack`, `.kv-table`), `div.table-scroll` | `ui::Table` with `TableLook`, rows `ui::Row`/`ui::Cell` (`.part(CellPart::…)`, `.label(…)` for `data-label`, `.optional(true)` for `col-optional`) |
| `dialog`, its title and actions, its page | `ui::Dialog`, `DialogTitle`, `DialogActions`, `DialogPage` |
| `select` | `ui::Select` |

A component takes no `class` or `style`: a page that positions or styles
one names it with a `data-*` attribute (`.attr("data-table", "orders")`)
or styles its container, and its CSS selectors follow. A page's own
layout classes stay on its own elements. Where nothing fits, the markup
stays and the gap is noted under "Left as page markup" above.

## Verification

Run `npm run test:ui` (install Chromium once with `npx playwright install chromium`),
`npm run build:ui`, `npm run typecheck -w @monokulo/pos` and
`npm run typecheck -w @monokulo/engine`.
Existing Rust palette checks also inspect the embedded package component CSS.
For POS adoption use the existing `pos-responsive-layout` and terminal browser
suites, plus `cargo test -p monokulo --lib http::pos::tests`.

Every page under the site's bar starts with a `PageHeader`: breadcrumbs (one
link is a back link), the required title, then a wizard's progress; a badge or
status beside the title and the page's actions at its right. `views::Page`
takes one, so a page can't be built without a title. Setup/wallet progress integrates the active task and position inside the
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

Settings CSS and order-status badge styling were extracted from the app,
not reimplemented for the preview; the settings components are the app's own,
so what the stylebook shows is what the pages render. Other new patterns are
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
source hash and count in `crates/ui/tools/lint-baseline.json`. Copying an old
violation or changing its declaration does not grant an exemption. All new
shared library and stylebook sources pass without baseline exceptions.

Treat baseline changes as a reviewed migration decision. A fixed finding
left in the baseline fails the check too, so the baseline only shrinks:
`node crates/ui/tools/lint.mjs --prune-baseline` drops what's fixed and
never adds. `--write-baseline` records the current debt whole; CI never runs
either. Do not refresh the baseline merely to make a failed check pass.

This is a guardrail, not proof of correct design or accessibility: dynamic
markup, business state and application compositions still need review. Browser
contracts check unique IDs, labels, ARIA/SVG references, clipboard semantics,
keyboard interactions, reduced motion, footer visibility and no-JS workflows.
Responsive screenshots support visual review. Real POS and settings browser
tests verify that extracted assets and styles work in their consumers.

## fixi and islands

Islands (Solid behaviours) are for what happens inside a component: a
dropdown opening, a card counting its changes, a toast closing. fixi
(with ssexi for server-sent events, and `fx-glue.js`) is for server round
trips: submit or navigate, and swap in the HTML the server answers with.
Every `fx-*` element is a real form or link, so without JavaScript the
browser does the same request and gets the whole page; one endpoint and
one renderer serve both. Its uses: settings saves and tabs, paging and
search on orders, logs and invites, loading a log row's details and older
rows, the status, logs, order and checkout live streams.

Islands and fixi compose: what a swap brings in attaches, what it takes
away lets go. A behaviour that needs to know about a request listens for
fixi's events (the settings form re-enables Save on `fx:finally`). Don't
use fixi for component behaviour, and don't fetch-and-swap from a
behaviour: that is fixi again. A page that becomes one island with JSON
saves is the case to revisit, one page at a time.

fixi, ssexi and the glue are 6.4 KB gzipped. The islands script is 1.8 KB
brotli on its own; a page with a Select adds Solid's runtime and the
Select's behaviour, 20.6 KB in all. monokulo sends every script, style and
module brotli- or gzip-compressed to a browser that takes it, in copies
made when the binary is built (`static_assets::bake!`, kept between builds
in `target/<profile>/static-assets-cache/`), so serving compresses
nothing.
