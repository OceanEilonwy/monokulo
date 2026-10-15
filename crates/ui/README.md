# ui (`@monokulo/ui`)

Monokulo's shared UI: one package for both languages. This directory is the
Rust crate `ui` (`Cargo.toml`) and the npm package `@monokulo/ui`
(`package.json`), and holds everything a component is:

| Where | What | Written by |
|---|---|---|
| `src/components/*.rs` | each component's props and markup (`component!`) and its examples | hand |
| `web/components/*.css` | its appearance, painted with `web/tokens.css`'s roles | hand |
| `web/components/*.behaviour.ts` | an interactive component's behaviour, in Solid | hand |
| `web/islands.ts` | `<mk-island>`: attaches behaviours to server-rendered markup | hand |
| `web/components/*.js` | older behaviours: custom elements that upgrade hand-written markup, until they move to `component!` | hand |
| `web/` (tokens, fonts, icons, brand, status) | the foundations | hand (`brand/` by `cargo xtask logo`) |
| `solid/` | the Solid components the POS imports | `cargo xtask ui` |
| `browser/solid/generated/` | the examples as fixtures, for the equivalence test | `cargo xtask ui` |
| `../../apps/stylebook/examples/generated/` | the stylebook's section per component | `cargo xtask ui` |

Native HTML is the baseline: a component's markup works with JavaScript off,
and its custom element, when it has one, enhances it. Nothing here knows
application routes, API clients or authentication.

## Consumers

- **Rust** (monokulo's views, the pages site): build a component and render
  it with maud, `(Button::new("Save").variant(ButtonVariant::Primary))`.
  Serve [`ui::assets`](src/assets.rs): the tokens, the components'
  stylesheet, the scripts, fonts and icons, baked into the binary.
- **npm** (the POS, the stylebook): import the stylesheets and scripts, and
  the Solid components from `@monokulo/ui/solid`.

## Changing a component

Edit its `component!` in `src/components` and its CSS or script in
`web/components`, give it examples (they are what the stylebook shows and
what the Solid version is checked against), list it in `catalogue()`, then
run `cargo xtask ui` and commit what it writes. Never edit generated files;
CI runs `cargo xtask ui --check`.

From the repository root:

```sh
npm ci
cargo xtask ui        # regenerate solid/, fixtures and stylebook sections
npm run stylebook     # review
npm run test:ui       # stylebook contracts, and Solid against Rust
cargo nextest run -p ui
```

Install the test browser once with `npx playwright install chromium`.

## Public exports

- `@monokulo/ui/styles.css`: tokens and all component CSS.
- `@monokulo/ui/fonts.css`: self-hosted Manrope font faces (also in styles.css).
- `@monokulo/ui/tokens.css`: palette, spacing, typography and role tokens.
- `@monokulo/ui/components.css`: native layout and control primitives.
- `@monokulo/ui`: registers select, copy, tabs and menu enhancements.
- `@monokulo/ui/solid`: the Solid components, generated from `src/` by
  `cargo xtask ui` (needs `solid-js` and `@solidjs/web`).

```js
import '@monokulo/ui/styles.css';
import '@monokulo/ui';
```

`npm run build:ui` emits standalone `dist/ui.css` and `dist/ui.js`; include the
stylesheet and a module script to use the library without a bundler. Source
exports let consumers bundle just the code they use. Generated output is not
committed. This is a private workspace package pending external publication.

## Contracts

Use the [stylebook examples](../../apps/stylebook/examples) as canonical HTML
examples. `mk-btn`, `mk-input`, `mk-field`, `mk-fieldset`, `mk-check`, `mk-panel`,
`mk-header`, `mk-stack`, `mk-grid`, `mk-actions`, `mk-container`, `mk-alert`,
`mk-badge`, `mk-facts` and `mk-disclosure` are CSS contracts on semantic HTML,
not JavaScript rendering components. Buttons support `data-variant` (primary,
danger, quiet; omit for secondary), `data-size` (compact, touch; omit for normal)
and `data-block`. Inputs support the same size variants.

Use native disabled/readonly/required attributes. Associate labels and hints
with controls. Application validation supplies `aria-invalid` and linked error
text; the library does not validate business rules. Use buttons for actions,
anchors for navigation, and real form actions for server-rendered workflows.

Every dropdown is the `Select` component (`ui::Select` with `ui::Choice`
options): a native select, which remains the submitted field, and, once its
island attaches, a button and a list drawn over it (`select.behaviour.ts`).
Set the select's `value` and dispatch `input` or `change` to update it; listen
to its native `change` event. It takes `compact` and `search` (auto, show,
hide); each `Choice`'s parts (logo, detail, network, chip, note) are written
into the option's text too, so without JavaScript the native select says
everything. See [interactive components](../../docs/ui-components.md#interactive-components-behaviour-in-solid-islands-on-the-server). The `.switch` contract styles
a native checkbox, retaining native submission and keyboard behaviour.

Keep component appearance here. Consumers may position components, but should
add a documented library variant instead of overriding a component's border,
colour, padding or focus treatment. Token overrides can provide another theme.
All component changes should have a stylebook example; behavioural changes need
keyboard and fallback checks. Native behaviour does not need custom JS wrappers.

## Navigation, progress and action cards

Included in `@monokulo/ui/styles.css`, or import the public `navigation.css`,
`steps.css` and `action-card.css` exports alongside tokens and primitives.

- `.mk-breadcrumbs`: a labelled `nav` with an ordered list of links and explicit
  `.mk-breadcrumb-separator` elements marked `aria-hidden="true"`. Mark the
  current page with `aria-current="page"`. Direct links and separators are also
  supported for existing app trails; a sole direct ancestor link has a back
  arrow. `.mk-back-link` is a standalone anchor with a real destination.
- `.mk-progress-steps[data-design="strip"]`: selected wizard design B.
  Each item contains a label and native `progress.mk-step-progress` with explicit
  `value`, `max` and `aria-label`. Count completed tasks: Back up is 1/3,
  Check is 2/3; upcoming stages are 0/1 and completed stages 1/1.
  Mark the current item with `aria-current="step"`; completed items use `.done`.
  Put the active task and its position in a `small` element inside the main
  segment label (e.g. Wallet / Back up · step 2 of 3), at every viewport size.
  Do not add a separate subtask row. Standalone wallet flows use one segment.
  These are progress indicators, not navigation. No JavaScript is required.
- `.mk-card-grid` and `.mk-action-card`: responsive action choices with a
  heading, optional `.mk-action-card-head`, a growing
  `.mk-action-card-description`, and one native link/submit button. Optional
  `.mk-action-card-body` groups content inside a card. `data-recommended` and
  `data-unavailable` affect appearance; include a visible explanation and
  native `disabled` on unavailable buttons. Neither attribute disables links.
  There is no JS click handler on the card itself and no nested click targets.

All three patterns work without JavaScript. The application owns step state,
URLs, form actions and whether a capability is available. The wallet creation
flow removes `data-unavailable` and enables its button only when the required
browser capabilities exist. See the navigation, steps and action-card examples
in the stylebook for their complete HTML contracts.

## Expanded patterns

- `patterns.css`: typography (`mk-type-*`), icons (`mk-icon`), input actions,
  spinner/progress, navigation and account disclosure, tabs, QR presentation,
  copy values, tables, empty states, radio cards and dialogs.
- `settings.css` and optional `@monokulo/ui/settings`: the existing settings
  form/card/field/save-bar and toast implementation, now shared with Rust.
  Import the script **once**, on pages with one settings form. It retains the
  existing application lifecycle (native submission or fixi swaps), dirty-state
  events, discard and navigation protection. Do not import it in a POS that
  owns its own form state. `data-placement="inline"` embeds a save bar; otherwise
  it sits at the viewport bottom. `data-persistent` keeps review toasts visible.
- `@monokulo/ui` additionally registers `mk-copy`, `mk-tabs` and menu dismissal.
  It does not import the optional settings lifecycle.
- `icons/*.svg`: canonical local 24px line icons. Inline SVG inherits text color;
  give the owning button a name, and hide decorative SVG from screen readers.

`mk-copy` requires one `[data-copy-value]` with `class="mk-copy-value"` and
`tabindex="0"`. Keep the full literal value as its text content, without child
controls. CSS ellipsis changes presentation only. Focus reveals the text, and
`user-select: all` allows native copying without JavaScript. The injected Copy
button reads the current value on each click and uses the Clipboard API. If
permission is denied or the API is unavailable, it selects the value and gives
manual-copy instructions. Set `copy-label` to name the value being copied.
Do not nest this component inside a link or button.

`mk-tabs` wraps `.mk-tabs` anchors and local panel sections with unique IDs.
Without JavaScript all panels remain visible and anchors scroll to them. The
upgrade adds tab roles, selection, roving focus and Left/Right/Home/End keys.
For separate page URLs use `.mk-tabs` alone with `aria-current="page"`. A
framework may own tab state and reuse the same CSS/ARIA contract instead.

`.mk-menu` is a native `details` with a named summary and `.mk-menu-content`.
Use ordinary links/buttons, not `role="menu"`: Tab follows native navigation.
Escape closes from within and returns focus to the summary; outside click also
closes. `.mk-nav` wraps naturally on small screens without a JS-only hamburger.

`.mk-qr` presents server-generated SVG or images with a complete text/link
alternative. Never truncate the encoded data or remove its quiet zone. The
stylebook fixture was generated by the app's existing qrcode crate for
`https://example.com/`; it is a real QR, not a decorative placeholder.

Use `--text-xs/sm/base/lg/xl/2xl/display` for type and
`--space-xs/sm/md/lg/xl/2xl/3xl` for spacing. The stylebook shows each token,
its intended role, and measured padding/gap examples. Type uses rem so it
follows the host document's accessible root font size.

## Precise truncation

Keep the full value as the text of `[data-copy-value]`. Use:

```html
<mk-copy truncate="middle" head="8" tail="8" copy-label="Copy address">
  <span class="mk-copy-value" data-copy-value tabindex="0">FULL_VALUE</span>
</mk-copy>
<mk-copy truncate="segments" segments="0:8,32:6,-8:8">
  <span class="mk-copy-value" data-copy-value tabindex="0">FULL_VALUE</span>
</mk-copy>
```

Omit `truncate` for CSS end ellipsis. `middle` keeps the specified prefix/suffix;
`segments` keeps an ordered list of `start:length` character ranges, adding an
ellipsis for each omitted gap. Negative starts count back from the end. Ranges
count Unicode code points, not UTF-16 units; invalid/overlapping ranges show the
full value. Applications choose meaningful ranges (for example a namespace,
internal ID and checksum); the library does not guess address semantics.
The visual preview is hidden from assistive technology and cannot be selected
as if it were the original value. Clicking it reveals/selects the full value.
Focus, clipboard copy and no-JS selection always use the original. Long retained
segments may wrap at narrow widths rather than drop meaningful characters.
Changes to text or these attributes update the preview automatically.

## Composition and original app artwork

- `.mk-page-frame`, `.mk-page-body`, `.mk-page-footer`: a flex page shell with a
  scrolling content region and a footer that stays visible. Use the real
  settings form as the frame, and put the save bar in the footer using
  `data-placement="inline"`. The application’s existing viewport-fixed bar is
  also supported. `/form-page.html` demonstrates the footer at viewport size.
- `.mk-pagination`: native page links and visible range/count. Use
  `aria-current="page"` and disabled controls for unavailable destinations.
- `.mk-section`: a heading/spacing/divider alternative to nested cards.
- `.mk-chart`, `.resource-chart`, `.sparkline`, `.mk-status-card`, `.mk-metrics`:
  SVG/data and network summary contracts. Include units, time range, freshness,
  labels and a table/text alternative. The app owns samples and live updates.
- `status.css` and `status/symbols.svg`: the actual POS artwork and presentation.
  Include the SVG definitions once per document. The POS owns status mapping,
  confirmation percentage and the user-facing labels. Always pair icons with
  text; decorative icons use `aria-hidden`.
- `brand/{small,full,icon,loading,hero}.svg`: original generated brand variants.
  Run `cargo xtask logo` after editing `xtask/src/logo.rs`; do not edit generated
  SVGs. Inline the assets to inherit theme roles. Namespace clip-path IDs when
  rendering repeated inline instances (the stylebook does this at build time).
  `.mk-mark` supplies sizing and reduced-motion handling.
- Wallet selector: a `Select` whose `Choice`s carry a detail, a network, the
  Current chip and a note; the stylebook's Select section shows one.

The flat progress strip is the only supported step design.

The selected page-form design uses `mk-settings-card.settings-card[data-surface="grouped"]`.
The `.card-head` remains outside the bordered `.mk-setting-group` containers
inside `.card-body`. Each group contains `.mk-setting-row` fields with a
description and `.mk-setting-control`; adjacent rows share divider lines.
Controls stack below descriptions on narrow screens. Existing dirty/error
badges, group discard and footer save behaviour still apply.

Status text is the default: settings counts, changed/saved metadata and monitoring
health use plain text beside their label or heading. Use `.mk-status-text`
with an optional `data-tone="warning|error|success"`; wording must communicate
the state independently of color. Chips are reserved for network identity in
mixed-network lists, removable selected filters/tags, and compact categorical
states in dense tables. Avoid duplicate identity chips and strings of status
chips. See the stylebook's `#status-language` for examples and rationale.

Chips use Title Case and fully rounded pill borders. Pending uses the warning
state palette, never a neutral generic badge. Wallet Current metadata renders
as plain green `.mk-current` text beside the network identity pill.
