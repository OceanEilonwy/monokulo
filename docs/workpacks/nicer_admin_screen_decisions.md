# Decisions: nicer admin screen

Decisions made while implementing `nicer_admin_screen.md`, where the plan
left something open or had to be adapted. Numbered in the order they were
made.

## D1 (step 1): `public_url` goes on General

- **Decision:** monokulo's `public_url` (this instance's public address,
  added after the work pack's setting list was written) is placed on the
  General tab, next to `signup.mode` and the engine connection.
- **Alternatives:** Server (it is about how the instance is reached), or
  Other.
- **Why:** it is one of the first things an operator sets up and plugins
  can't connect until it is set, which is the same "set this first" job the
  General tab has. Server holds process tuning (bind, threads, memory), not
  public identity. Other is meant to stay empty.

## D2 (step 1): the newer exchange-rate providers follow `exchange_rate.*`

- **Decision:** `exchange_rate.coinmarketcap_*` and `exchange_rate.haveno_*`
  (landed on main after the plan was written) go under Payments >
  "Exchange rates" with the rest of `exchange_rate.*`, by prefix.
- **Alternatives:** none worth considering; the plan places the whole
  `exchange_rate.*` family there.
- **Why:** the plan's table says `exchange_rate.*`. The unit test lists
  every key explicitly so a future addition has to be placed on purpose.

## D3 (step 2): a save's banners survive the 303 in a one-time flash

- **Decision:** a successful save without JavaScript redirects (303) to
  `?tab=<id>&saved=<token>`. The save's success line and notices (restart
  needed, a network stores use left with no node, environment overrides)
  are kept in a small in-memory map in `http/admin_settings.rs`
  (`FLASHES`), keyed by a random 128-bit token, shown once by the page it
  redirects to, and dropped after 10 minutes (at most 64 kept).
- **Alternatives:** no banners after the redirect (loses the red "no
  reachable node" warning, which the plan relies on as the no-JavaScript
  warning); encoding the notices in the query string (long, ugly URLs and
  free text from the engine in them); a new `AppState` field (touches every
  place that builds an `AppState`, including other crates and examples);
  keeping the old "render the page on success" (the plan asks for a 303).
- **Why:** it keeps post/redirect/get and the plan's no-JS warning, stays
  entirely in Rust, and needs nothing but the page. A restart between the
  post and the redirect only loses a banner, never a setting.

## D4 (step 2): one success line for every tab

- **Decision:** a save says "Settings saved and applied." whichever process
  owned what was saved, replacing "Monokulo settings saved and applied." and
  "Engine settings saved and applied.". The Playwright specs that waited for
  the old text were updated to the new one in the same commit so the suite
  stays green.
- **Alternatives:** keep both lines and show whichever applied, or both on a
  mixed tab.
- **Why:** a tab is one form with one Save button (T3); which process holds a
  setting is not something the admin should need to know. The per-half
  notices (for example "take effect after the engine restarts") still say
  which process they are about.

## D5 (step 2): `saved_section` stays until the page has tabs

- **Decision:** step 2 adds `saved_tab` to the view model but keeps
  `SettingsSection` / `saved_section` so the two-form page still knows which
  section to put its banners in. The `/dashboard/admin/scanner-settings`
  route now uses the same combined handler. Step 3 removes both along with
  the two-section layout.
- **Alternatives:** building the tabbed panel in step 2.
- **Why:** step 2 is the save and step 3 the page; this keeps each commit
  working and each step reviewable on its own.

## D6 (step 2): the test engine can apply saved nodes

- **Decision:** `scanner_test_support::TestEngineConfig::with_live_nodes()`
  wires the engine's real `NodesReloadable` into the test engine, so a saved
  node gets a real RPC client and the settings API probes it, as in
  production.
- **Alternatives:** asserting the unserved-network banner only in
  Playwright.
- **Why:** step 2's acceptance asks for an HTTP test showing the unserved
  network notice still appears, and step 5 needs saved nodes to be probed in
  HTTP tests too. The harness's default (fixed fakes) is unchanged.

## D7 (step 3): an engine setting that shares a key with monokulo is sent as `engine:<key>`

- **Decision:** both processes have `logging.level`, `logging.dev_mode_until`,
  `logging.retention_days`, `logging.max_mb`, `logging.otlp_endpoint` and
  `logging.otlp_headers`. On the Logging tab they share one form, so the
  engine's controls are named `engine:<key>` (and its "Clear it" box
  `clear:engine:<key>`); their ids follow. The save handler strips the
  prefix and sends the key to the engine. Only colliding keys get the
  prefix (`AdminScalarFieldView::name`, `engine_form_name`); every other
  engine field keeps its plain key as its name.
- **Alternatives:** prefix every engine field (changes every field name
  and every test and helper that fills by name, for no gain); separate
  forms on the Logging tab (breaks T3's one Save per tab).
- **Why:** without it the two `logging.level` boxes would be joined into one
  comma-separated value and both saved to monokulo. The HTTP test
  `every_scanner_setting_on_the_admin_page_saves_correctly` now posts the
  engine's logging settings as the page does and checks the engine got them.

## D8 (step 3): an empty engine secret keeps its value; its box clears it

- **Decision:** the engine's own `logging.otlp_headers` (a secret) now
  behaves like monokulo's on the page: left empty it is kept, and its
  "Clear it" box clears it. Monokulo asks the engine which of the submitted
  settings are secrets (one extra `GET /api/v1/admin/settings`, only when a
  submitted engine value is empty or a box is ticked).
- **Alternatives:** leave it as it was, where every engine save sent the
  empty box and so wiped the engine's saved headers (and a ticked "Clear it"
  was sent as an unknown setting and refused).
- **Why:** with both processes' logging settings on one tab, saving any
  logging setting would otherwise silently wipe the engine's OTLP headers.
  The setting's meaning is unchanged; only the page's handling of its empty
  box is fixed. Tested by `the_logging_tab_keeps_each_processs_secret_apart`.

## D9 (step 3): the "set the engine connection" message points at General

- **Decision:** the T6 message for an unconfigured engine now reads "Set
  `engine.url` and `engine.admin_token` on the General tab and save to
  manage this instance's engine settings from here", with General a link.
- **Alternatives:** keep "... above and save ...".
- **Why:** those fields are no longer above it; they are on another tab.
  The "Could not reach the configured engine: ..." message is unchanged.

## D10 (step 3): markers on other tabs use what monokulo already knows

- **Decision:** the Monero nodes marker comes from the engine's `/status`
  (stores reported as `no_reachable_node`, grouped by network), plus any
  network stores use that has no node saved at all. The Monero nodes tab
  itself waits for `get_status_cached` (10s cache); every other tab reads
  the cache without waiting (`known_unserved`), as the nav's status dot
  does, so opening, say, Logging never waits on node probes.
- **Alternatives:** await `/status` on every tab (each tab load could wait
  up to 5s per node while the engine probes); mark only after a save.
- **Why:** T5 wants the marker on every tab; this shows it on every tab as
  soon as monokulo has heard, without slowing tabs that don't need it.

## D11 (step 3): tab links use fx-glue's existing `fx-push-url`

- **Decision:** each tab link carries `fx-action`, `fx-target="#settings-panel"`
  and `fx-push-url`. fx-glue already pushes the URL after a successful GET
  swap and reloads the page on Back/Forward for entries it made (the Logs
  page's and Invites' pattern), so no new JavaScript was needed. A tab
  link's response is the panel with the tab bar and banners out of band;
  the new panel's heading gets focus.
- **Alternatives:** plain navigation.
- **Why:** the plan prefers the fixi swap when fx-glue has the pattern, and
  it does.

## D12 (step 3): the Playwright helpers follow the tabs from step 3 on

- **Decision:** step 3 already rewrites `saveEngineSettings` in
  `real-helpers.js` to open the tab holding the given fields (several tabs
  are saved in turn) and press "Save", adds `settingsTabOf`,
  `openSettingsTab`, `fillSettings` and `SETTINGS_TABS`, and moves
  `real-1`, `real-3` and `real-6` onto the tabbed page, so the suite stays
  green at every commit. `real-6`'s "keeps edits in the other half" part is
  dropped (there is no other half on the page any more) and replaced by the
  check the plan asks for: a fixi save leaves one tab bar with the same
  links and one banners area. The red/yellow banner test saves the
  restart-only setting and the node on their own tabs, one after the
  other. The new tests step 7 lists are still written in step 7.
- **Alternatives:** leave the suite red until step 7.
- **Why:** the work pack asks for the suite to pass before every commit
  that touches the page.

## D13 (step 4): only changed networks are probed; the probe trusts the node's own TLS choice

- **Decision:** `update_settings` asks a network's nodes for `get_info` only
  when that network's submitted value differs from what the engine runs
  with now (`state.settings.nodes`). The probe client is built from the
  node's own `ssl` / `accept_self_signed_certs`; the engine's
  `--strict-tls` flag isn't in `AppState`, so it isn't applied to this one
  question. One message per network is kept (the first wrong node in saved
  order), and a duplicate is reported before any probing of that network.
- **Alternatives:** probe every submitted network on every save (the
  Monero nodes tab always submits all three, so every save could wait up to
  3s on a node that doesn't answer); thread `strict_tls` into `AppState`.
- **Why:** an unchanged node was already accepted, so asking again can't
  change the outcome and only slows the save. The probe only reads which
  network a node says it's on; with `--strict-tls` the real client would
  refuse a self-signed node anyway, and `/status` shows that.

## D14 (step 4): `/status` reports the node's raw nettype, `fakechain` included

- **Decision:** `NodeStatus.network` is whatever the node's `get_info`
  said (`mainnet`, `stagenet`, `testnet` or `fakechain`), or `null` when
  it didn't answer or didn't say. Only a known network different from the
  block's is treated as wrong, by the engine's save check and by the page.
- **Alternatives:** report only the three known networks.
- **Why:** the plan says "the node's reported nettype"; keeping
  `fakechain` visible is more honest, and both readers already ignore it.

## D15 (step 4): the Playwright spec that gave testnet the stagenet fake node

- **Decision:** `real-3`'s "clearing a network stores use" test saved the
  one fake node (which says it's on stagenet) as testnet's node too. That
  is now refused, correctly. The test gives testnet `127.0.0.1:9` instead,
  a node that doesn't answer, which is still saved (D2).
- **Alternatives:** start a second fake node for testnet.
- **Why:** the test only needs testnet to have some node and no stores;
  step 7 adds the wrong-network Playwright test with a second fake node.

## D16 (step 5): an IPv6 node is saved with its brackets in `host`

- **Decision:** `[::1]:18081` is saved as `host: "[::1]", port: 18081`.
  A saved bare IPv6 host (possible through the API) shows as `[::1]:18081`
  in the address box.
- **Alternatives:** save `host: "::1"` and change the engine to add
  brackets when it builds its URL and label.
- **Why:** the engine builds its RPC URL as `{scheme}://{host}:{port}`
  and its `/status` label as `{host}:{port}`; with the brackets in `host`
  both are right without touching the engine, and the label matches the
  address the admin typed, so the row's status is found.

## D17 (step 5): a fallback's own fallbacks are not carried by the form

- **Decision:** `MoneroNodeSetting` lets a fallback have a `fallbacks` list
  of its own, but the engine never reads it (`build_daemon_client` only
  walks the primary's list, one level). The form shows the primary and its
  fallbacks and, on save, writes every fallback with `fallbacks: []`, so
  any nested list saved through the API is dropped at the next save from
  the page.
- **Alternatives:** flatten nested fallbacks into the list (would start
  using nodes the engine ignores today, changing behaviour); keep them in a
  hidden field (a page field nobody can see or edit).
- **Why:** they have no effect now, and the four row fields cover every
  field of `MoneroNodeSetting` that does. Nothing that works stops working.
