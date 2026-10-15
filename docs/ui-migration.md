# Moving the UI onto `ui`

Every server-rendered page (monokulo's views, the pages site) and the POS
use `crates/ui` components for everything shared; only the custom
visualisations stay application markup (the engine page's block chart and
timeline, the logs histogram, trace waterfall and log rows, the
proof-of-work pip chain, wallet backup words, and the testing report's
figures, trees and charts). This file tracks what has moved.

| Pattern | Component | Status |
|---|---|---|
| Buttons, button links, icon buttons and links, launchers | `Button`, `LinkButton`, `IconButton`, `IconLink`, `FormActions`, `LaunchTile` | done |
| Text inputs (with actions inside), text areas, checkboxes, switches, radio cards | `Field` (one line or `multiline`), `Input`, `TextArea`, `Checkbox`, `Switch`, `RadioCards` | done |
| Notes under a field, values set elsewhere | `FieldNote`, `LockedValue` | done |
| Dropdowns | `Select` | done |
| Settings pages | `SettingsForm`, `SettingsCard`, `Setting` (name and help left, control right; a wide control and a phone stack), `SaveBar`, `Toast` | done |
| Progress | `WizardSteps` (setup), `ProgressSteps` (a payment's stage) | done (checkout and POS share the stage) |
| Panels, messages, hints, headings | `Panel` (raised or tile), `Alert` (with a title and actions), `Hint`, `Heading` | done |
| Facts, tags, network badges, short values | `Facts`, `Tag` (tones, order states, an icon), `NetworkBadge`, `ShortValue` | done |
| Disclosures, help, checklists, timelines | `Disclosure` (plain, boxed, More options, compact, row; an alert edge), `HelpTip` (from the tip or its container), `Checks`, `Timeline` | done |
| Ordered lists the user reorders | `SortableList`, `SortableItem`, `SortHandle` | admin's Monero nodes |
| Tables, cards, paging | `Table`, `Row`, `Cell`, `Card`, `Pagination` | done (logs' Older loads more lines in place: not paging) |
| Dialogs | `Dialog`, `DialogTitle`, `DialogActions`, `DialogPage` | done; no `window.confirm` left |
| Copying | `CopyValue`, `CopyBlock` | checkout and POS address; the site's commands |
| Tabs and filters | `Tabs` (labels may hold logos; `mk-tab`), `TabLinks`, `SegmentedControl` | done |
| Site bar, menus, theme switch, status dot | `SiteNav`, `Menu`, `MenuItem`, `ThemeToggle`, `StatusDot` | done |
| Shown only with or without scripts, on screen or paper | `data-only` on any component | done |

### Left as page markup, and why

- The custom visualisations above.
- The POS's background-order stack (small horizontal chips) and its hidden
  file input.
- The wallets' test-networks fold; `span.tag-active`; `dl.dns-record`.

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
| `select` | `ui::Select` (done) |

A component takes no `class` or `style`: a page that positions or styles
one names it with a `data-*` attribute (`.attr("data-table", "orders")`)
or styles its container, and its CSS selectors follow. A page's own
layout classes stay on its own elements. Where nothing fits, the markup
stays and the gap is noted here.
