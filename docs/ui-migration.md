# Moving the UI onto `ui`

Every server-rendered page (monokulo's views, the pages site) and the POS
use `crates/ui` components for everything shared; only the custom
visualisations stay application markup (the engine page's block chart and
timeline, the logs histogram and trace waterfall, the proof-of-work pip
chain, wallet backup, the checkout's stage tracker and the testing
report's charts). This file tracks what has moved.

| Pattern | Component | Status |
|---|---|---|
| Buttons, button links, icon buttons and links | `Button`, `LinkButton`, `IconButton`, `IconLink`, `FormActions` | monokulo done; pages site's landing |
| Text inputs, text areas, checkboxes, switches, radio cards | `Field`, `Input`, `TextArea`, `Checkbox`, `Switch`, `RadioCards` | monokulo done |
| Notes under a field, values set elsewhere | `FieldNote`, `LockedValue` | done |
| Dropdowns | `Select` | done (pages site's OpenWrt version too) |
| Settings pages | `SettingsForm`, `SettingsCard`, `Setting`, `SaveBar`, `Toast` | done |
| Setup progress | `WizardSteps` | done |
| Panels, messages, hints, headings | `Panel`, `Alert`, `Hint`, `Heading` | monokulo done; pages site's notes |
| Facts, tags, network badges, short values | `Facts`, `Tag`, `NetworkBadge`, `ShortValue` | monokulo done |
| Disclosures, checklists, timelines | `Disclosure`, `Checks`, `Timeline` | monokulo done |
| Tables | `Table`, `Row`, `Cell` | monokulo done |
| Dialogs | `Dialog`, `DialogTitle`, `DialogActions`, `DialogPage` | done; `confirm-dialogs.js` gone |
| Copy a value | `CopyValue` | component; app adoption to do |
| The POS | `Button`, `Field`, `Input`, `ShortValue`, `Dialog` (cancel asks in a dialog, not `window.confirm`) | its own order cards, keypad, status art and stage tracker stay the POS's |
| In-page tabs, page tabs | `Tabs`, `TabLinks` | pages site (install tabs, docs, testing report) |
| Site bar, account menu, theme switch, status dot | `SiteNav`, `Menu`, `ThemeToggle`, `StatusDot` | done (pages site and POS too) |

### Left as page markup, and why

- Custom visualisations, as above, and the engine page's `details.mini`
  panels and help popover, the node rows' drag-to-reorder `details`, the
  log rows' `details`, the wallets' test-networks fold.
- The checkout's copy-address and refund scan/upload buttons, drawn inside
  the field by the checkout's own CSS; its read-only address text area.
- The store page's launcher tiles (`.widget-link`: the POS one has no
  `href` until its script gives it one) and embed warnings.
- The backup screen's wallet-app tabs: logos in their labels, driven by
  `wallet-setup.js`.
- The account menu's Log out (a menu item), pagination links,
  `span.tag-active`, `dl.dns-record`.

### Gaps worth a component or variant

A `Field` for a text area; `CellPart::Detail`; a dashed, flex-summary
disclosure look; a pill-shaped toggle button (the query help's chips);
utility visibility (`no-js-only`, `js-only`, `no-print`) on a component,
for now a wrapping `span`.

## Converting a page

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
| `h1` with actions beside it (`.page-heading`) | `ui::Heading` with `.actions(...)` |
| `dl.facts` | `ui::Facts` |
| `span.tag…` | `ui::Tag` (`.tone(TagTone::…)`, or `.state(OrderState::…)`; `.short(…)` for a phone) |
| network badge | `views::network_badge` (`ui::NetworkBadge`) |
| shortened id/address | `views::short_id` / `short_address` (`ui::ShortValue`) |
| `details` (plain, `.keys-help`, `.more-options`) | `ui::Disclosure` with a `DisclosureLook` |
| `ul.checks` | `ui::Checks` |
| `ol.timeline` | `ui::Timeline` |
| `table` (`.table-cards`, `.table-stack`, `.kv-table`), `div.table-scroll` | `ui::Table` with `TableLook`, rows `ui::Row`/`ui::Cell` (`.part(CellPart::…)`, `.label(…)` for `data-label`, `.optional(true)` for `col-optional`) |
| `dialog`, its title and actions, its page | `ui::Dialog`, `DialogTitle`, `DialogActions`, `DialogPage` |
| `select` | `ui::Select` (done) |

A component takes no `class` or `style`: a page that positions or styles
one names it with a `data-*` attribute (`.attr("data-table", "orders")`)
or styles its container, and its CSS selectors follow. A page's own
layout classes stay on its own elements. Where nothing fits, the markup
stays and the gap is noted here.
