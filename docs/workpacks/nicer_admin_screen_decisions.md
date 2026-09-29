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
