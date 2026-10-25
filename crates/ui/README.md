# ui (`@monokulo/ui`)

Monokulo's shared UI: one package for both languages. This directory is the
Rust crate `ui` (`Cargo.toml`) and the npm package `@monokulo/ui`
(`package.json`), and holds everything a component is:

| Where | What | Written by |
|---|---|---|
| `src/components/*.rs` | each component's props and markup (`component!`) and its examples | hand |
| `web/components/*.css` | its appearance, painted with `web/tokens.css`'s roles | hand |
| `web/components/*.behaviour.ts` | an interactive component's behaviour, in Solid | hand |
| `web/islands.ts` | attaches behaviours to the islands in server-rendered markup | hand |
| `web/` (tokens, fonts, icons, brand, status) | the foundations | hand (`brand/` by `cargo xtask logo`) |
| `solid/` | the Solid components the POS imports | `cargo xtask ui` |
| `browser/solid/generated/` | the examples as fixtures, for the equivalence test | `cargo xtask ui` |
| `../../apps/stylebook/examples/generated/` | the stylebook's section per component | `cargo xtask ui` |

Native HTML is the baseline: a component's markup works with JavaScript off,
and its behaviour, when it has one, enhances it as an island. Nothing here
knows application routes, API clients or authentication.

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

- `@monokulo/ui/styles.css`: tokens, fonts and every component's CSS.
- `@monokulo/ui/tokens.css`, `fonts.css`: the palette, spacing, type and
  role tokens; the self-hosted Manrope faces.
- `@monokulo/ui/components.css`, `navigation.css`, `steps.css`,
  `action-card.css`, `settings.css`, `patterns.css`, `status.css`,
  `monitoring.css`: parts of `styles.css` on their own.
- `@monokulo/ui/islands`: attaches every interactive component's behaviour
  to server-rendered markup (`web/islands.ts`).
- `@monokulo/ui/solid`: the Solid components, generated from `src/` by
  `cargo xtask ui` (needs `solid-js` and `@solidjs/web`).
- `@monokulo/ui/icons/*`, `brand/*`, `status/*`: line icons, the logo and
  loading marks (`cargo xtask logo`), the POS's status artwork.

Rust consumers get the same from [`ui::assets`](src/assets.rs).

## Using the components

The [stylebook](../../apps/stylebook) shows every component: its props, each
example, and the Rust, Solid and HTML for it. The rules that hold across
them:

- A component takes no `class` or `style`. Position it from its container,
  or name it with a `data-*` attribute (through its `rest` attributes) for a
  page's own CSS.
- Buttons act, links go somewhere: `Button`, `LinkButton`, `IconButton`,
  `IconLink`. A page has at most one primary button.
- Every dropdown is `Select`; every on/off setting a `Switch`.
- Labels are required: `Field` labels its input, `Setting` its control,
  `IconButton` and `IconLink` themselves; an `Input` without a label on the
  page takes `label`.
- Status is words first; colour (a `Tag`'s tone, an `Alert`'s) backs them up.
- Validation is the application's: it marks a control invalid and says why.

`patterns.css` holds what several components share: the tabs, `CopyValue`,
the icon size. Use `--text-*` for type and `--space-*` for spacing; type is
in rem, so it follows the reader's root font size.

## Precise truncation

`CopyValue` takes `truncate` and the ranges to keep:

```rust
CopyValue::new(address).copy_label("Copy address").truncate(Truncate::Middle).head(8usize).tail(8usize)
CopyValue::new(address).truncate(Truncate::Segments).segments("0:8,32:6,-8:8")
```

`End` (the default) is the CSS ellipsis. `Middle` keeps the specified prefix/suffix;
`Segments` keeps an ordered list of `start:length` character ranges, adding an
ellipsis for each omitted gap. Negative starts count back from the end. Ranges
count Unicode code points, not UTF-16 units; invalid/overlapping ranges show the
full value. The shortened preview is hidden from assistive technology; focus,
copying and no-JS selection always use the whole value.

## Artwork

- `status.css` and `status/symbols.svg`: the POS's status artwork. Include
  the SVG definitions once per document, and pair each icon with words.
  `StatusIcon` draws them; a Rust page opts in with `ui::assets::STATUS`
  (`STATUS_CSS`) after `COMPONENTS`, and `ui::assets::STATUS_SYMBOLS` once
  in its body.
- `brand/{small,full,icon,loading,hero}.svg`: generated by `cargo xtask logo`
  from `xtask/src/logo.rs`; don't edit them. Inline them to take the theme's
  colours, namespacing clip-path ids when one is repeated.
