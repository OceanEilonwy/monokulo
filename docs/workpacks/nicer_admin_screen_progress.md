# Progress: nicer admin screen

Work pack: `docs/workpacks/nicer_admin_screen.md`. Decisions:
`docs/workpacks/nicer_admin_screen_decisions.md`. Branch:
`nicer-admin-screen`.

## Status

| Step | What | Status | Commits |
|---|---|---|---|
| 1 | One map from setting to tab | done | see `git log` (`admin settings: one map from setting to tab`) |
| 2 | One save for a whole tab | done | `admin settings: one save for a whole tab` |
| 3 | The tabbed page | done | `admin settings: the tabbed page` |
| 4 | Engine learns a node's network | not started | |
| 5 | The node form | not started | |
| 6 | JavaScript enhancements | not started | |
| 7 | Playwright | not started | |
| 8 | Docs and cleanup | not started | |

## Resume here

Start step 4: the engine learns which network a node is on. `get_info` on
`MoneroDaemonClient` (`crates/scanner/src/daemon.rs`, default "unknown"),
implemented in `daemon_rpc.rs`; `NodeStatus.network` in
`crates/scanner/src/http/status_page.rs` and monokulo's copy in
`crates/monokulo/src/engine_client.rs`; the wrong-network and duplicate
checks in `update_settings` (`crates/scanner/src/http/instance_admin.rs`);
`--nettype` for `fake-monerod`.

## Test status at last commit

- Baseline before any change (origin/main `f7e6d60`):
  `cargo test --workspace` 1245 passed, 0 failed, 24 ignored.
  Playwright real-binaries: 26 passed.
- Building needs `npm ci` in `crates/monokulo/pos-ui` first (build.rs
  builds the POS app) and `npm ci` in `e2e/pos-playwright` for Playwright.
- Clippy baseline: 69 warnings workspace-wide, none new. Pre-existing ones
  in files this work touches: `monokulo/src/http/admin_settings.rs` 1
  (a test helper's doc comment), `scanner/src/daemon.rs` 1,
  `scanner/src/engine_settings.rs` 1, `scanner-test-support/src/lib.rs` 2.
- After step 1: `cargo test --workspace` 1249 passed, 0 failed, 24 ignored;
  clippy 69 warnings (unchanged).
- After step 2: `cargo test --workspace` 1256 passed, 0 failed, 24 ignored;
  clippy 68 warnings (the pre-existing one in `admin_settings.rs` fixed, no
  new ones); Playwright real-binaries 26 passed.
- After step 3: `cargo test --workspace` 1264 passed, 0 failed, 24 ignored;
  clippy 68 warnings (none in touched files but the pre-existing ones);
  Playwright real-binaries 26 passed.

## Notes per step

### Step 1

- `crates/monokulo/src/views/admin.rs`: `SettingOwner`, `SettingsTab`
  (ids, labels, `from_id` falling back to General, `href`, `groups`,
  `engine_only`) and `setting_placement(key, owner) -> (tab, heading)`.
- `SettingsTab::groups` is the order a tab shows its groups in: Payments is
  engine payment settings, then "Webhooks", then "Exchange rates"; Logging
  is "Monokulo" then "Engine"; Server is engine then monokulo with no
  headings.
- Acceptance: `every_setting_known_today_has_a_named_tab` lists every key
  of both registries with its expected tab and heading;
  `the_tab_list_covers_both_registries` checks that list against
  `crate::settings::ALL` and `scanner::engine_settings::ALL` so it can't go
  stale; `a_setting_the_map_does_not_know_goes_to_other` covers unknown
  keys.
- The old helpers (`is_abuse_field`, `is_logging_field`, `engine_group`,
  `in_group`) are still used by the two-section page and go in step 3.

### Step 2

- `crates/monokulo/src/http/admin_settings.rs`: `save` is the one handler
  for `POST /dashboard/admin/settings` (and, until step 3 removes it,
  `/dashboard/admin/scanner-settings`). `SplitForm::new` splits the form by
  owner: names `crate::settings::ALL` knows (and their `clear:` boxes) are
  monokulo's, `tab` is dropped, `monero_node_<network>` textareas become
  `monero_node`, everything else is an engine scalar. `save_tab` saves
  monokulo's half first (`save_monokulo`) and stops if it's refused, then
  the engine's (`save_engine`); both return a `SaveOutcome` (error, the key
  it names and its owner, notices, whether the engine connection changed).
- The tab a result shows on: the tab of the setting a refusal names (the
  first `FieldError` key from monokulo, or `fields[0].key` from the
  engine's 400 body), else the submitted `tab`.
- Without fixi: success is a 303 to `?tab=<id>&saved=<token>` with the
  banners in a one-time flash (decision D3); a refusal renders the page.
  With fixi: the section fragment, as before (step 3 changes it).
- Success text is now "Settings saved and applied." (decision D4).
- `crates/monokulo/src/http/dashboard.rs`: `redirect_303`.
- `crates/scanner-test-support/src/lib.rs`: `with_live_nodes()` (decision
  D6).
- Acceptance, all HTTP tests in `admin_settings.rs`:
  `a_tab_with_only_monokulo_settings_saves_only_monokulo` (engine address
  unreachable, so any engine call would refuse the save),
  `a_tab_with_only_engine_settings_saves_only_the_engine` (monokulo's stored
  settings compared before and after),
  `a_mixed_tab_saves_both_halves` (Payments: both values read back from the
  engine API, the db and the page),
  `an_invalid_monokulo_value_in_a_mixed_tab_saves_neither_half`,
  `an_engine_refusal_shows_the_engines_own_message` (the message fetched
  from the engine directly is on the page verbatim),
  `restart_and_unserved_network_notices_still_show`, and
  `a_saved_banner_is_shown_once`. "The error shows on that tab" is checked
  again in step 3, once the page shows tabs. The tests that post every
  monokulo setting and every engine setting now post to the combined route
  and follow the redirect.

### Step 3

- `crates/monokulo/src/views/admin.rs`: `admin_settings_page` renders the
  breadcrumb, `h1`, `banners` (`#settings-banners`, page-wide, T4),
  `tab_bar` (`nav#settings-tabs`, plain links with `fx-action`,
  `fx-target="#settings-panel"`, `fx-push-url`, `aria-current="page"` on
  the open tab, a dot plus visually hidden " (needs attention)" on marked
  tabs) and `settings_panel` (`section#settings-panel`: heading, the
  environment-variable hint, the Abuse explanation on Abuse, and one form
  with a hidden `tab`, the tab's groups and one `.btn-primary` "Save").
  `settings_fragment` is what fixi gets: the panel plus banners and tab bar
  marked `data-fx-oob`. `tab_fields` walks `SettingsTab::groups`;
  `group_fields` picks each group's fields with `setting_placement`.
  Engine-only tabs (Monero nodes, Key custody) show only the T6 message
  when the engine is down; mixed tabs show it once where the engine's
  groups would be and still save monokulo's fields. "Other" only appears
  in the tab bar when something is in it (`tab_shown`). The Monero nodes
  tab still shows the JSON textareas (step 5 replaces them).
- Removed: `SettingsSection`, `monokulo_section`, `engine_section`,
  `is_abuse_field`, `is_logging_field`, `engine_group`, `in_group`, the
  `/dashboard/admin/scanner-settings` route.
- Markers (`needs_attention`): Nodes when `unreachable_networks` is not
  empty or a network with stores has no node; any tab holding a field with
  `pending_restart`. `unreachable_networks` is filled by the handler from
  `/status` (decision D10).
- `crates/monokulo/src/http/admin_settings.rs`: `page` reads `?tab=` and,
  for fixi, answers the fragment with the heading marked `data-fx-focus`.
  `save` answers fixi with the fragment (`422` on refusal) and sets
  `saved_tab`, which the "Saved." / "Not saved" word by the button uses.
  Engine fields sharing a key with monokulo are named `engine:<key>`
  (decision D7); empty engine secrets are kept (decision D8).
- CSS: `.tab-bar` (scrolls sideways inside itself; `position: relative` so
  the visually hidden text can't widen the page), `.tab-marker`,
  `.settings-actions`, `.visually-hidden` in `site.css`; roles
  `--tab-current` and `--tab-marker` in `theme.css`.
- Acceptance, view tests in `views/admin.rs`:
  `each_tab_shows_only_its_own_settings` (every known key, on every tab),
  `tabs_with_two_owners_head_each_group`,
  `general_is_the_default_and_the_open_tab_is_marked_current`,
  `a_tab_is_found_by_its_id_and_anything_else_is_general` (step 1),
  `banners_are_above_the_tab_bar_on_every_tab`,
  `a_tab_is_marked_while_something_on_it_needs_attention`,
  `every_tab_copes_with_an_engine_it_cannot_reach` (T6 for unreachable and
  unconfigured, on every tab), `the_other_tab_shows_only_with_something_in_it`.
  HTTP tests: `every_tab_opens` (200 for every tab, unknown falls back to
  General), `a_fixi_save_answers_with_the_panel_and_the_banners_and_tab_bar_out_of_band`,
  `a_fixi_save_that_breaks_the_engine_connection_shows_on_the_engine_tabs`
  (a fixi tab link gets the panel with a focused heading),
  `the_logging_tab_keeps_each_processs_secret_apart`, and
  `an_invalid_monokulo_value_in_a_mixed_tab_saves_neither_half` now also
  checks the refused save opens on Payments.
- Phone width: `real-3`'s first test now opens every tab at 390px and
  1280px and checks nothing scrolls sideways; 320px is added in step 7.
- Playwright helpers and specs adapted (decision D12).
