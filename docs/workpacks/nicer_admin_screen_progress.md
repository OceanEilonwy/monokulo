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
| 4 | Engine learns a node's network | done | `scanner: a node saved for the wrong network is refused` |
| 5 | The node form | done | `admin settings: node form rows and addresses`, `admin settings: the Monero nodes form` |
| 6 | JavaScript enhancements | done | `admin settings: node form scripts` |
| 7 | Playwright | not started | |
| 8 | Docs and cleanup | not started | |

## Resume here

Start step 7: Playwright. New tests (with and without JavaScript) for tab
switching and Back, every tab's help and width at 320/390/1280, adding,
moving and removing nodes, a wrong-network node (second fake-monerod with
`--nettype mainnet`), the TLS toggle, the confirmation, the Nodes marker
with `/fake/offline`, and gallery captures of every tab in
`real-8-theme.spec.js`. Helpers already exist in `real-helpers.js`
(`saveEngineSettings`, `openSettingsTab`, `saveNodes`, `fillNodes`).

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
- After step 4: `cargo test --workspace` 1270 passed, 0 failed, 24 ignored;
  clippy 68 warnings (none new; `scanner/src/daemon.rs` keeps its one
  pre-existing warning); Playwright real-binaries 26 passed.

- After step 5: `cargo test --workspace` 1288 passed, 0 failed, 24 ignored;
  clippy 68 warnings (none new); Playwright real-binaries 20 passed,
  6 failed, all waiting on step 6's confirmation script (decision D20).

- After step 6: `cargo test --workspace` 1288 passed, 0 failed, 24 ignored;
  clippy 68 warnings (none new); Playwright real-binaries 26 passed (the
  six red at step 5 are green again). `node --check` passes on the three
  inline scripts (extracted to files to check).

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

### Step 4

- `crates/scanner/src/daemon.rs`: `DaemonInfo { nettype }` with
  `DaemonInfo::unknown()` and `network()` (only mainnet/stagenet/testnet
  map to a network); `MoneroDaemonClient::get_info` defaults to unknown, so
  no test double changed.
- `crates/scanner/src/daemon_rpc.rs`: `RpcDaemonClient::get_info` calls
  JSON-RPC `get_info` through `post_json_rpc` (same client, 15s timeout and
  response cap as `get_height`); `GetInfoResult` reads `nettype`, or the
  older `mainnet`/`stagenet`/`testnet` flags, else "unknown".
- `crates/scanner/src/http/status_page.rs`: `NodeStatus.network`; the probe
  runs `get_height` and `get_info` together, each within the 5s timeout
  (decision D14).
- `crates/scanner/src/http/instance_admin.rs`: `nodes_that_cannot_work`
  runs before anything is saved, with no lock held: duplicates ("<host>:<port>
  is listed twice.") and nodes answering a different known network
  ("<host>:<port> is on <nettype>, not <network>.") are refused with 400 and
  `fields: [{ key: "monero_node.<network>", message }]`, the same body shape
  as other refusals (`refused`). Unchanged networks aren't probed (D13).
- `crates/monokulo/src/engine_client.rs`: `NodeStatus` gains `in_cooldown`
  and `network`, both `#[serde(default)]`.
- `crates/scanner-test-support/src/bin/fake-monerod.rs`: `--nettype`
  (default `stagenet`), served by JSON-RPC `get_info` and `/get_info`.
- Acceptance: engine tests in `crates/scanner/src/http/tests.rs`
  (`a_stagenet_node_that_says_it_is_on_mainnet_is_refused_and_nothing_changes`,
  also covering a wrong fallback and that the rest of the request isn't
  saved; `nodes_that_do_not_answer_or_do_not_say_are_saved` for a node that
  doesn't answer, one reporting `fakechain` and one without `get_info`;
  `a_node_listed_twice_in_one_network_is_refused`;
  `status_says_which_network_each_node_is_on`), a parsing test in
  `daemon_rpc.rs` (`get_info_says_which_network_a_node_is_on`), and in
  monokulo `a_status_parses_with_and_without_each_nodes_network`.
  `real-4-crash.spec.js` (direct API save of the stagenet fake node) passes
  unchanged. `real-3` needed one change (decision D15).

### Step 5

- `crates/monokulo/src/admin_nodes.rs` (pure, unit-tested on its own):
  `parse_address` (host:port, `http://`/`https://` with https ticking TLS,
  `[IPv6]:port`, port required, 1-65535, no path; the host keeps an IPv6
  address's brackets, see decision D16), `format_address`, `NodeForm::from_form`
  (rows by network and index; blank rows dropped; the `node_action` applied;
  each row's address checked and a repeated address marked on the second
  row), `NodeAction` (`remove|up|down:<network>:<index>`), `rows_to_setting`
  (first row primary, the rest `fallbacks`; none clears the network) and
  `rows_from_setting` (with the engine's `host:port` label for matching
  `/status`; a fallback's own fallbacks are dropped, decision D17).
- View (`views/admin.rs`): `AdminNetworkFieldView` now holds `rows`
  (`NodeRowView`: the row, the engine's label, its `NodeStatusView`),
  `example_address`, `tenant_count` and `error`. `network_block` renders a
  `section.node-network` (heading, "Used by N stores.") or, with no nodes
  and no stores, a closed `details` "Add a node for <network>"; then the
  block's engine error, the fallbacks hint, the rows (`node_row`: legend
  Primary / Fallback N; Address, Use TLS, Accept a self-signed certificate,
  each name, help, control; the status line; Move up / Move down / Remove
  as `name="node_action"` submit buttons, not on the edges), and a blank
  "Add a node" row with self-signed ticked. "Add another" is rendered
  `js-only` for step 6. A hidden Save leads the form (decision D18). Field
  help is written on the page (decision D19). The textarea, its Example
  and the JSON error message are gone.
- Save (`http/admin_settings.rs`): `SplitForm` notes `node_*` fields;
  `save_tab` builds a `NodeForm` (button applied), refuses with every row
  kept when any row has an error ("Nothing was saved: some node addresses
  need fixing (marked below)."), else sends each submitted network as
  `monero_node`. An engine refusal's `fields` go on their network's block
  and the submitted rows are shown again. A save whose `changed` includes a
  `monero_node.*` key clears monokulo's `/status` cache.
- Status (`build_view_model`): on the Monero nodes tab `get_status_cached`
  fills each row's status by label (`attach_node_status`); a node reporting
  another known network shows "Wrong network". If `/status` fails, rows
  show no status.
- Acceptance: unit tests in `admin_nodes.rs` (step 5's first commit);
  HTTP tests `a_node_is_added_through_the_blank_row_and_ordered_by_its_buttons`
  (add through the blank row; up, down and remove, each one POST with
  `node_action`, the saved order read back from the engine and the page),
  `a_bad_address_is_shown_on_its_row_and_nothing_is_saved` (also with
  fixi: 422), `a_node_on_another_network_is_refused_on_its_block`,
  `clearing_a_network_stores_use_is_saved_and_says_so`,
  `a_saved_node_shows_its_status`; view tests
  `a_network_is_closed_until_it_has_nodes_or_stores`,
  `node_rows_are_named_in_order_with_their_buttons_and_a_blank_row_to_add_one`,
  `a_nodes_status_is_said_in_words`, `what_is_wrong_shows_where_it_is`.
- Playwright: `real-helpers.js` gains `fakeNodeAddress`, `fillNodes`,
  `saveNodes`, `nodeAddressBoxes`; every spec that saved node JSON on the
  page uses them (`real-4` still saves JSON through the API).

### Step 6

- `crates/monokulo/src/views/admin.rs`: `CONFIRM_CLEARED_NETWORK_SCRIPT`
  rewritten: on submit of `#settings-form` (capture, before fixi), for each
  `.node-network` with stores, counts rows whose address isn't blank, not
  counting the row a pressed Remove (`event.submitter`,
  `remove:<network>:<index>`) is for; asks when that leaves none where
  there were some (`defaultValue`). New `NODE_FORM_SCRIPT`: "Add another"
  clones the blank row with the next index (renaming `name`, `id`, `for`,
  `aria-describedby`), resets it and focuses its address; Use TLS shows or
  hides its row's self-signed field. All listeners are on the document,
  plus a pass on `fx:swapped`, so a swapped panel works.
- Checked: `node --check` on each inline script; the Playwright suite
  (the confirmation tests in `real-1` and `real-3`); step 7 adds tests for
  "Add another" and the TLS toggle.
