# Dropdowns and switches

Monokulo has one dropdown, `<mk-select>`, and one on/off control, the switch.
Both are in `crates/monokulo/src/views/controls.rs`; the dropdown's script is
`crates/monokulo/static/mk-select.js` and their styles are in `site.css`.

## The dropdown

The server renders an ordinary `<select>` inside `<mk-select>`:

```html
<mk-select>
  <select name="wallet_id" required>
    <option value="" disabled selected>Choose a wallet…</option>
    <option value="w_1" data-label="Copper Heron" data-detail="48xQ7…v3Rk"
            data-chip="Current" data-chip-tone="current" data-note="since 1 Sep 2026">
      Copper Heron (48xQ7…v3Rk) [Current] - since 1 Sep 2026
    </option>
  </select>
</mk-select>
```

Options are written with `controls::Choice`. Each one is built from a label
plus optional parts:

| Part | Shown as |
|---|---|
| logo | A small picture before the label (a wallet app's logo). Drawn by the component only; the option's text doesn't mention it. |
| detail | Monospace and muted, after the label (an address, a currency code). |
| network | A wallet's Monero network, as the site's network badge (`views::network_badge`): Mainnet or the test network's name. An option can be the badge alone, with an empty label: the setup flow's network dropdown. |
| chip | A tag. Only `Chip::Current` is green; any other chip is neutral, so green always means "the one in use now". |
| note | Muted, at the end (a count, a date, why it can't be picked). |

**Without JavaScript**, the select is all there is. Each option's text carries
every part in words: `Label (detail) [Network] [chip] - note`. An option
with no label starts at its first part: `[Stagenet]`.

**With JavaScript**, the component:

- hides the select and draws a button and a list over it;
- keeps the select as the form field, so forms, `required`, fixi,
  `fx-submit-on-change` and other scripts behave as before;
- follows a script that sets the select's `value` or `selectedIndex`, or
  enables or disables an option.

**The list:**

- It goes in the top layer (the Popover API), placed under the button, or
  above it when there's more room there.
- Nothing clips it, dialogs included, and it moves nothing on the page.
- A browser without the Popover API hangs it below the button instead.

**Attributes:**

- `compact`: the toolbar size (logs, engine page).
- `search="auto|show|hide"`: a find box at the top of the list. `auto`, the
  default, shows it from 12 options up.

**Keyboard:**

- Enter, Space or the arrow keys open the list.
- Up, Down, Home, End, PageUp and PageDown move through the options, skipping
  disabled ones.
- Typing jumps to an option, or types into the find box when there is one.
- Enter chooses; Escape closes the list and nothing else, not a dialog around
  it.

**Screen readers:** the button is a `combobox` named by the select's label,
with `aria-activedescendant` pointing into the `listbox`.

**Required fields:** a required select left on its prompt marks the
dropdown invalid when the form is refused.

A test (`every_select_is_the_dropdown_component`) fails if a view renders a
`select` outside `<mk-select>`.

## The switch

An on/off setting (the admin page's booleans) is `controls::switch`:

- a checkbox with `role="switch"`, drawn as a track and knob, with On or Off
  beside it;
- with no script, so it is the same with or without JavaScript.

A ticked checkbox sends `true`; an unticked one sends nothing. A hidden
`switches` field names each switch on the form, and the admin settings
handler reads a named switch that sent nothing as `false`.

**Colours:**

- an on switch uses the primary orange (`--switch-on`), not green, which is
  kept for Current;
- the list's shadow is `--popup-shadow`.
