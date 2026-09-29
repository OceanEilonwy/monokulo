# Progress: nicer admin screen

Work pack: `docs/workpacks/nicer_admin_screen.md`. Decisions:
`docs/workpacks/nicer_admin_screen_decisions.md`. Branch:
`nicer-admin-screen`.

## Status

| Step | What | Status | Commits |
|---|---|---|---|
| 1 | One map from setting to tab | done | see `git log` (`admin settings: one map from setting to tab`) |
| 2 | One save for a whole tab | not started | |
| 3 | The tabbed page | not started | |
| 4 | Engine learns a node's network | not started | |
| 5 | The node form | not started | |
| 6 | JavaScript enhancements | not started | |
| 7 | Playwright | not started | |
| 8 | Docs and cleanup | not started | |

## Resume here

Start step 2: refactor `save_monokulo` / `save_scanner` in
`crates/monokulo/src/http/admin_settings.rs` into functions returning a
result, and make `POST /dashboard/admin/settings` take a whole tab.

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
