# Moving the UI onto `ui`

Every server-rendered page (monokulo's views, the pages site) and the POS
use `crates/ui` components for everything shared; only the custom
visualisations stay application markup (the engine page's block chart and
timeline, the logs histogram and trace waterfall, the proof-of-work pip
chain, wallet backup, the checkout's stage tracker and the testing
report's charts). This file tracks what has moved.

| Pattern | Component | Status |
|---|---|---|
| Buttons, button links | `Button`, `LinkButton` | sign-up/log-in, POS actions |
| Text inputs | `Field` | sign-up/log-in, POS reference |
| Dropdowns | `Select` | done |
| Settings pages | `SettingsForm`, `SettingsCard`, `Setting`, `SaveBar`, `Toast` | done |
| Setup progress | `ProgressSteps` | done |
| Copy a value | `CopyValue` | component; app adoption to do |
| In-page tabs, page tabs | `Tabs`, `TabLinks` | components; app adoption to do |
| Site bar, account menu, theme switch, status dot | `SiteNav`, `Menu`, `ThemeToggle`, `StatusDot` | done (pages and POS) |

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
