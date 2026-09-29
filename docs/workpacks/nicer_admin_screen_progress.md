# Progress: nicer admin screen

Work pack: `docs/workpacks/nicer_admin_screen.md`. Decisions:
`docs/workpacks/nicer_admin_screen_decisions.md`. Branch:
`nicer-admin-screen`.

## Status

| Step | What | Status | Commits |
|---|---|---|---|
| 1 | One map from setting to tab | done | see `git log` (`admin settings: one map from setting to tab`) |
| 2 | One save for a whole tab | done | `admin settings: one save for a whole tab` |
| 3 | The tabbed page | not started | |
| 4 | Engine learns a node's network | not started | |
| 5 | The node form | not started | |
| 6 | JavaScript enhancements | not started | |
| 7 | Playwright | not started | |
| 8 | Docs and cleanup | not started | |

## Resume here

Start step 3: the tabbed page. Replace `monokulo_section` /
`engine_section` in `crates/monokulo/src/views/admin.rs` with a tab bar and
one `#settings-panel`; make `page()` in `http/admin_settings.rs` use
`query.tab` (currently read and ignored); drop `SettingsSection` /
`saved_section` (decision D5) and the `/dashboard/admin/scanner-settings`
route; make the fixi answer the panel plus `#settings-banners` and the tab
bar as `data-fx-oob`.

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
