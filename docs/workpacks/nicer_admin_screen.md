# Work pack: a nicer admin settings screen (tabs, and a form for Monero nodes)

Files that go with this work pack, all in `docs/workpacks/`:
- `nicer_admin_screen.md`: this file, the spec.
- `nicer_admin_screen_progress.md`: the running work notes and the resume point. Create it when you start.
- `nicer_admin_screen_decisions.md`: the decision log. Create it when you make your first decision.

Read all three before doing anything. Section 5 explains how to keep them.

You are implementing a plan the project owner has reviewed and approved. Implement it **exactly**. Where it leaves a detail open, make the best engineering decision, record it in the decision log, and carry on. Don't stop to ask questions; nobody will be there to answer them.

Your work will be reviewed step by step against this document. Commits that mix steps, skip acceptance criteria, or quietly deviate from the plan will be sent back.

---

## 0. Working environment and rules

- **Repository and branch:**
  - Work in the git worktree and branch you were started in.
  - If you were started in the main checkout, create a worktree on a new branch `nicer-admin-screen` from `main` and work only there. Never `cd` into another checkout.
- **Git:**
  - Commit after each step: one or more commits per step, never one commit spanning two steps.
  - Message style matches the log: `area: summary` (e.g. `admin settings: ...`, `scanner: ...`, `e2e: ...`, `docs: ...`), then a wrapped plain-English body saying what changed and why.
  - End every commit message with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - Don't push, amend or rewrite existing commits.
  - Stage by explicit path. Never `git add -A`, `git add .` or `git commit -a`.
  - Never use bare `git stash` / `git stash pop`: the stash stack is shared with other sessions.
- **Shell:** the login shell is fish. Use `bash -c '...'` when you need bash syntax.
- **Crates you'll touch:**
  - `crates/monokulo`: the public web app. The admin settings page lives here.
  - `crates/engine`: **the engine** (also called "scanner"). It is private; only monokulo talks to it.
  - `crates/engine-test-support`: test doubles, including the `fake-monerod` binary the Playwright suite runs against.
  - `crates/live-settings`: the shared, typed settings library both processes use.
  - `e2e/browser`: the Playwright tests. The `real-*.spec.js` files run against the real binaries and cover the admin page.
- **Tests you must run and keep green before every commit:**
  - `cargo test --workspace`. At commit `1a6b27e` this gave 1162 passed, 0 failed, 22 ignored. Run it once before you start and record your own baseline in the progress notes.
  - `cargo clippy --workspace --all-targets`: add no new warnings in files you touch.
  - `node --check` on any JS file you edit.
  - When you touch HTML, CSS, JS or the admin page's behaviour, run the real-binaries Playwright suite from `e2e/browser`: `npx playwright test -c real-binaries.config.js`. See `e2e/browser/README.md` for setup. Use absolute paths or a subshell so your shell doesn't stay in another directory.
  - Don't run the stagenet suites (`*_stagenet*.rs`, `pos.spec.js`, `coverage-stagenet.config.js`). They need real funds and nodes, but they must still compile.
- **Code style:**
  - Match the surrounding code's naming, idiom and comment density. This codebase uses long doc comments that explain *why*. Write new ones in that spirit, plainly and without filler.
  - Views are `maud`, in `crates/monokulo/src/views/*.rs`.
  - Colours are defined only as roles in `crates/monokulo/src/views/theme.css` (a test enforces this). Shared components go in `views/site.css`.
  - Light mode never uses black. Buttons are neutral, with at most one orange primary per form.
- **Hard project rules (from the owner; breaking one fails the review):**
  1. **Progressive enhancement.** The admin page must work fully with JavaScript disabled: switching tabs, saving, and adding, removing and reordering nodes. JavaScript only enhances. It already does this through fixi (`static/fixi.js`, glue in `static/fx-glue.js`); keep using fixi.
     - All rendering and formatting happens in Rust. Never send raw data for JS to format.
     - Add no frontend frameworks or libraries (htmx was rejected).
     - No meta refresh on this page.
  2. **The engine is private.** Browsers only ever talk to monokulo. Monokulo talks to the engine through its admin API (`/api/v1/admin/...`, instance admin token) and `/status`.
  3. **Settings layout.** Each setting shows its name, then its help, then its control, then where its value came from. Key custody keeps a section per backend (`key_custody.<backend>_*` keys), hidden while that backend is off.
  4. **Every setting says what it is for.** Keep the help text and examples you move, and give every new control the same (admin_settings_v2.md goal 5).

---

## 1. Background: where things stand

### The page today

- **Route:** `GET /dashboard/admin/settings` renders the page and `POST /dashboard/admin/settings` saves monokulo's half (`crates/monokulo/src/http/mod.rs` around line 171, handlers in `crates/monokulo/src/http/admin_settings.rs`: `page`, `save_monokulo`). `POST /dashboard/admin/scanner-settings` (`save_scanner`) saves the engine's half.
- **View:** `crates/monokulo/src/views/admin.rs`. `admin_settings_page` renders two sections, one after the other:
  - `monokulo_section`: one form holding monokulo's fields. General fields come first, then "Abuse protection" (`is_abuse_field`), then "Logging" (`is_logging_field`).
  - `engine_section`: one form. "Monero nodes" comes first, as a `<textarea name="monero_node_<network>">` of raw JSON per network with an "Example" disclosure. Then the groups "Key custody", "Payments", "Server", "Webhooks", "Logging" and "Other", chosen by key prefix (`in_group`). Key custody includes `custody_backend_sections`.
- **fixi:** each form posts with `fx-action` and swaps its own section back (`fx-target="#monokulo-settings"` / `#engine-settings`). A monokulo save also sends the engine section back with `data-fx-oob`, because changing the engine connection changes it. fixi builds the body with `new FormData(form, evt.submitter)`, so a clicked submit button's `name`/`value` is sent (`static/fixi.js` line 14). The row buttons in step 5 rely on this.
- **View model:** `AdminSettingsViewModel` has `saved_section: Option<SettingsSection>` (`Monokulo` / `Engine`), which decides where banners show, plus `monokulo_fields`, `scanner_fields` and `scanner_networks` (`AdminNetworkFieldView`: `network`, `value_json`, `description`, `example`, `tenant_count`), and flags for whether the engine is configured and reachable.
- **Inline scripts** at the end of the page: `CONFIRM_CLEARED_NETWORK_SCRIPT` (confirms before a save that empties a network stores use; it reads the textarea's `data-tenant-count`) and `CUSTODY_BACKENDS_SCRIPT` (shows or hides a backend's section as its checkbox changes).
- **Banners:**
  - Restart needed: `server.bind` and `server.worker_threads` apply only on a restart.
  - Red: stores use a network that no longer has a reachable node.
  - Error: `engine.url` doesn't answer.
  - Environment variable overrides.

  All come from `banners(data)`, `notices`, and `scanner_save_notices` in the handler.

### Every setting on the page

Monokulo's (from its registry, `monokulo_fields`):
`signup.mode`, `engine.url`, `engine.admin_token`, `exchange_rate.coingecko_enabled`, `exchange_rate.coingecko_base_url`, `exchange_rate.cache_seconds`, `http_cache.max_mb`, `abuse.soft_per_min`, `abuse.hard_per_min`, `abuse.signed_in_per_min`, `abuse.client_logs_per_min`, `abuse.challenge_bits`, `abuse.under_attack`, `abuse.trusted_proxies`, `abuse.onion_listener`, `abuse.stream_cap`, `rate_limit.per_store_key_per_min`, `logging.level`, `logging.dev_mode_until`, `logging.retention_days`, `logging.max_mb`, `logging.otlp_endpoint`, `logging.otlp_headers`.

The engine's (fetched from `GET /api/v1/admin/settings`, `crates/engine/src/http/instance_admin.rs` `get_settings`):
`monero_node.mainnet|stagenet|testnet` (JSON, `engine_settings::NETWORKS`), `key_custody.enabled_backends`, `key_custody.default_backend`, `key_custody.socket_path` (and any other `key_custody.<backend>_*`), `payment.confirmations_required`, `payment.order_expiry_minutes`, `payment.reorg_check_depth`, `payment.mempool_poll_interval_ms`, `payment.expired_order_grace_period_minutes`, `payment.scan_chunk_memory_budget_mb`, `server.bind`, `server.worker_threads`, `server.rate_limit_per_token_per_min`, `server.max_body_bytes`, `webhooks.allow_private_urls`, `webhooks.delivery_timeout_ms`, `webhooks.max_attempts`, `logging.level`, `logging.dev_mode_until`, `logging.retention_days`, `logging.max_mb`, `logging.otlp_endpoint`, `logging.otlp_headers`.

Re-check both lists against the code when you start; the registries are the source of truth. The HTTP tests in `admin_settings.rs` (around lines 720–830) post every one of them and are a good cross-check.

### Monero nodes today

- **Shape:** `crates/engine/src/settings.rs`, `MoneroNodeSetting { host, port, ssl (default false), accept_self_signed_certs (default true), fallbacks: Vec<MoneroNodeSetting> }`. Fallbacks never nest deeper than one level. The primary plus its fallbacks is simply an ordered list.
- **Save path:** monokulo's `save_scanner` parses each `monero_node_<network>` textarea as JSON (empty means clear) and forwards `{ scalars, monero_node }` to the engine's `POST /api/v1/admin/settings` (`update_settings`). The engine saves, rebuilds its node clients live, then probes the saved networks' nodes with `get_height` (3s timeout each). It warns (`unserved_networks`) when a network stores use has no reachable node. That warning becomes the red banner.
- **Node labels:** the engine labels each node `"{host}:{port}"` (`engine_settings.rs` around line 665).
- **Live status:** the engine's unauthenticated `GET /status` (`crates/engine/src/http/status_page.rs`) probes every node live (5s timeout) and returns `networks[].nodes[]` as `NodeStatus { label, is_active, in_cooldown, height, error }`. Monokulo reads it through `http::status_page::get_status_cached` (10s cache).
- **The engine never asks a node which network it is on.** It only calls `get_height`. Monerod's `get_info` (JSON-RPC method, and plain `/get_info`) returns `nettype` (`"mainnet"`, `"stagenet"`, `"testnet"`, `"fakechain"`). Nothing in the engine calls it yet. `DaemonClient` has many implementations, including test doubles in `crates/engine-test-support/src/lib.rs` and `daemon_fallback.rs` tests.
- **`fake-monerod`** (`crates/engine-test-support/src/bin/fake-monerod.rs`) serves `/get_height`, `/json_rpc` (`get_block`, and not much else), the pool endpoints, and `/fake/online|offline`. It has no `get_info`.

### Playwright coverage of this page

- `real-1-settings.spec.js`: a node saved on a fresh instance applies straight away (fills `textarea[name="monero_node_stagenet"]`).
- `real-3-admin-page.spec.js`: every visible `.setting-field` has help; the node example opens; nothing scrolls sideways at phone and desktop widths; the confirmation when clearing a network in use (reads `data-tenant-count` off the textarea); banners in both themes.
- `real-6-sections.spec.js`: saving the engine half keeps unsaved edits in the monokulo half.
- `real-helpers.js`: `saveEngineSettings(page, fields)` fills fields by `name` and clicks "Save engine settings". `fakeNodeJson()` gives the fake node as JSON. Most specs call these.
- `real-8-theme.spec.js`: captures dashboard pages for the coverage gallery with `captureCoverageStage`, in both themes.
- `real-4-crash.spec.js` posts `monero_node` straight to the engine API. That doesn't change.

All of these need updating for tabs and the node form (step 7).

---

## 2. The agreed design

### Decisions already made (don't revisit)

| # | Question | Decision |
|---|---|---|
| T1 | How settings are grouped | By job, into the seven tabs below, not by which process owns a setting. |
| T2 | Tab mechanics | Each tab is a real URL, `/dashboard/admin/settings?tab=<id>`, rendered on the server. The tab bar is plain links. fixi swaps just the panel. No CSS `:target` tabs (they break the redirect after a save). An unknown or missing `tab` shows General. |
| T3 | Tabs holding both processes' settings | One Save button per tab. One handler splits the submitted form by owner and saves each half through the existing paths (monokulo's registry; the engine's `POST /api/v1/admin/settings`). |
| T4 | Page-wide banners | Always above the tab bar, on every tab. |
| T5 | Marking a tab | A tab's label carries a marker while something in it needs attention: a network stores use with no reachable node (Monero nodes), or a saved setting waiting for a restart (whichever tab holds it). A failed save shows the tab holding the error. |
| T6 | Engine unreachable or not configured | Engine-only tabs (Monero nodes, Key custody) show the existing "Could not reach the configured engine: ..." / "Set `engine.url` and `engine.admin_token`..." message. Mixed tabs still show and save their monokulo fields, with that message where the engine fields would be. |
| T7 | Node editing | A form of plain fields, one row per node. The raw JSON textarea and its Example disclosure are removed from the page. The engine's API keeps accepting JSON (`MoneroNodeSetting`); only the page changes. |
| T8 | Node row actions without JavaScript | Remove, Move up and Move down are submit buttons of the tab's form (`name="node_action"`, e.g. `value="remove:stagenet:2"`). Pressing one applies that change to the submitted rows and saves, exactly like Save. No client-side list state. |
| T9 | A node on the wrong network | Refused at save time with nothing saved: it can never work. The engine enforces this, so its API is protected too. A node that doesn't answer is still saved (admin_settings_v2.md D2), with the existing red banner if that leaves a network stores use with no reachable node. |
| T10 | Where a node's status comes from | The engine's `/status` via monokulo's `get_status_cached`, extended with each node's network. Rows match by label (`host:port`). A successful node save clears the cache so the new rows get fresh results. |

### The tabs

| Tab | `?tab=` | Settings | Owner |
|---|---|---|---|
| General | `general` (default) | `signup.mode`, `engine.url`, `engine.admin_token` | monokulo |
| Monero nodes | `nodes` | `monero_node.<network>`, as the node form | engine |
| Payments | `payments` | `payment.confirmations_required`, `payment.order_expiry_minutes`, `payment.expired_order_grace_period_minutes`, `payment.reorg_check_depth`, `payment.mempool_poll_interval_ms`; then a "Webhooks" heading with `webhooks.*` | engine |
| | | "Exchange rates" heading with `exchange_rate.*` | monokulo |
| Key custody | `custody` | `key_custody.enabled_backends`, `key_custody.default_backend`, then `custody_backend_sections` | engine |
| Abuse protection | `abuse` | the existing explanatory paragraph, `abuse.*`, `rate_limit.per_store_key_per_min` | monokulo |
| Server | `server` | `server.bind`, `server.worker_threads`, `server.max_body_bytes`, `server.rate_limit_per_token_per_min`, `payment.scan_chunk_memory_budget_mb` | engine |
| | | `http_cache.max_mb` | monokulo |
| Logging | `logging` | "Monokulo" subsection with monokulo's `logging.*`, then "Engine" subsection with the engine's | both |

General comes first because the engine tabs are empty until the engine connection works. The explanatory line about environment variables ("Saved settings apply straight away. An environment variable, where set, always wins...") shows on every tab, above its fields.

Engine keys that fit no tab (a setting added later) go to a final "Other" tab, shown only when it has something in it. A test makes sure every setting known today has a named tab, so "Other" stays empty.

### The Monero nodes tab

One block per network, in the order mainnet, stagenet, testnet. A network with no nodes and no stores starts as a closed `<details>` whose summary reads "Add a node for <network>". The others are open blocks headed "<Network>", with the existing "Used by N stores." line.

A block is an ordered list of node rows. Row 1 is labelled "Primary"; the rest are "Fallback 1", "Fallback 2" and so on, with a hint: "Fallbacks are tried in order when the one before fails." Each row:

| Field | Control | Behaviour |
|---|---|---|
| Address | text input | `host:port`, e.g. `node.example.com:18081`. Also accepts `http://host:port` and `https://host:port`, where `https://` ticks TLS on save. IPv6 as `[::1]:18081`. The port is always required, with or without a scheme. |
| Use TLS | checkbox | `ssl` |
| Accept a self-signed certificate | checkbox, ticked by default | `accept_self_signed_certs`. With JavaScript it shows only while Use TLS is ticked. Without JavaScript it's always shown and ignored when TLS is off. |
| Status | text | Rendered in Rust from `/status`: "Reachable, height 1,234,567", "Not reachable: <error>", or "Wrong network: this node is on mainnet". Also "In use" when the node is active, "Resting after failures" when it's in cooldown, and nothing for a node with no status yet. |
| Move up / Move down | submit buttons | Not shown on the first or last row respectively. |
| Remove | submit button | |

Help text, the example and the existing field explanations (host and port, TLS, self-signed, fallbacks) move onto these fields as help, following the settings layout rule.

Below the rows there's always one blank row headed "Add a node", so adding works with no JavaScript: fill it in and press Save. A blank row (empty address) is ignored on save. With JavaScript, an "Add another" button appends another blank row, and focus moves to its address.

Form field names, per network `<n>` and row index `<i>` starting at 0: `node_<n>_<i>_address`, `node_<n>_<i>_ssl`, `node_<n>_<i>_self_signed`. The rows' order in the form is the saved order. The four row fields cover every field of `MoneroNodeSetting` today. If you find a saved node field the form doesn't cover, don't drop it silently on save: log a decision on how you handled it.

On save, monokulo builds each network's list into `MoneroNodeSetting` JSON (first row primary, the rest `fallbacks`) and sends it as `monero_node.<network>`. An empty list clears the network. Only networks whose rows were submitted are sent.

**Errors:**
- Address parse errors (missing port, port out of range, empty host, bad IPv6 brackets) are found in monokulo and shown under the row's address, with every submitted value kept. Nothing is saved when any row has an error.
- Two rows with the same address in one network: refused, shown on the second one.
- The engine refuses a wrong-network node (T9) with a field error on `monero_node.<network>`. Monokulo shows it at the top of that network's block. The engine's message names the node: "node.example.com:18081 is on mainnet, not stagenet."

**Confirmation:** removing (or blanking) the last node of a network stores use still asks first with JavaScript (`CONFIRM_CLEARED_NETWORK_SCRIPT`, rewritten to count rows). Without JavaScript the red banner after the save is the warning, as today.

---

## 3. The steps (implement in this order)

Each step lists what to do and its acceptance criteria.

### Step 1: One map from setting to tab

- In `crates/monokulo/src/views/admin.rs`, add a `SettingsTab` enum (General, Nodes, Payments, Custody, Abuse, Server, Logging, Other) with each tab's `?tab=` id and label. Add one function from a setting key and its owner (monokulo or engine) to its tab, and to its subsection heading where the tab has more than one (Payments: Webhooks and Exchange rates; Logging: Monokulo and Engine). The view (step 3) and the save handler (step 2) both use this function, so a setting can't show on the page and then be dropped on save.
- Replace `is_abuse_field`, `is_logging_field`, `group_of` and `in_group` with it once step 3 no longer needs them. Deleting them may wait until step 3.

**Acceptance:**
- A unit test lists every setting key from both registries today (section 1) and asserts each maps to a named tab (not Other) as in the table in section 2.
- A unit test asserts an unknown engine key maps to Other.

### Step 2: One save for a whole tab

- `POST /dashboard/admin/settings` takes a hidden `tab` field and any mix of monokulo and engine keys. It splits them by owner: monokulo's keys are the ones its registry knows, everything else goes to the engine, and `node_*` / `node_action` fields go to the node form parser in step 5. Then it saves each half through the existing code:
  - Refactor `save_monokulo` and `save_scanner` into functions that return a result rather than a response.
  - When both halves have something to save, save monokulo's first. If it's refused, don't send the engine's half, and show the error.
  - Merge both halves' success, errors and notices into one result for the page.
- Replace `SettingsSection` with the tab in the view model (`saved_tab`), so banners from a save show on that tab. For a field error, show the tab that holds the field.
- Without fixi: redirect (303) to `?tab=<id>` on success, or render the tab with the error, as today.
- With fixi: return the panel (step 3's `#settings-panel`), plus the banners area and the tab bar as `data-fx-oob`. The markers and page-wide banners can change after any save.
- Remove the `/dashboard/admin/scanner-settings` route once nothing posts to it (step 3), and move its HTTP tests onto the combined route.

**Acceptance:**
- HTTP tests show the following:
  - A form with only monokulo keys saves only monokulo.
  - A form with only engine keys saves only the engine.
  - A mixed form (Payments: `payment.confirmations_required` plus `exchange_rate.cache_seconds`) saves both, and both values read back.
  - An invalid monokulo value in a mixed form saves neither, and the error shows on that tab.
  - An engine refusal shows the engine's message verbatim.
  - Restart-needed and unserved-network notices still appear.
- The existing tests that post every monokulo setting and every engine setting (around lines 720–830) still pass against the combined route.

### Step 3: The tabbed page

- `GET /dashboard/admin/settings?tab=<id>` renders the following, in this order:
  - the breadcrumb and `h1`;
  - the page-wide banners (T4);
  - a `nav` tab bar of links, with `aria-current="page"` on the open tab and a marker on tabs needing attention (T5);
  - one `section id="settings-panel"` holding the open tab's single form (`method="post"`, `fx-action`, `fx-target="#settings-panel"`), its fields grouped by the step 1 map, and one "Save" button (the tab's primary button).
- The marker is text as well as colour, so it doesn't rely on colour alone (e.g. " (needs attention)" visually hidden, plus a dot). Its colour is a role in `theme.css`.
- Tab bar links carry `fx-action` so fixi swaps the panel and updates the URL with `history.pushState`, if fx-glue already has a pattern for that (the Logs page may). If it doesn't, plain navigation is acceptable; log which you did. Back and forward must work either way.
- Engine unreachable or not configured: T6.
- At phone width (320px and 390px), the tab bar must not make the page scroll sideways. Let it scroll horizontally inside itself, or wrap.
- Delete `monokulo_section`, `engine_section` and the two-form layout. The Monero nodes tab keeps the textarea for now; step 5 replaces it.

**Acceptance:**
- View unit tests:
  - each tab renders only its own settings;
  - General is the default and an unknown `tab` falls back to it;
  - `aria-current` is on the right link;
  - banners render above the tab bar on every tab;
  - the marker appears on Nodes when a network has no reachable node, and on the tab holding a setting with `pending_restart`;
  - T6 on each tab when the engine is unreachable.
- An HTTP test: `GET` of each tab returns 200.

### Step 4: The engine learns which network a node is on

- Add `get_info` to the engine's daemon client, returning at least `nettype`.
  - Give the trait a default implementation that returns "unknown", so test doubles don't all need changing.
  - Implement it for the real RPC client (`daemon_rpc.rs`), with the same timeouts and error handling as `get_height`.
  - An old or odd node that doesn't answer `get_info`, or reports a nettype the engine doesn't know (`fakechain`), counts as unknown, never as wrong.
- `NodeStatus` in `/status` gains `network: Option<String>` (the node's reported nettype). The status probe calls `get_info` alongside `get_height`, within the same timeout. Monokulo's copy of the DTO (`engine_client.rs`) gains the field as optional, so an older engine still parses.
- `update_settings` in `instance_admin.rs`:
  - Before saving, for each submitted network, parse each node and ask it for `get_info` (3s timeout each, all at once). If any node answers with a known nettype that isn't the network it's being saved for, refuse with 400 and a field error on `monero_node.<network>`: "<host>:<port> is on <nettype>, not <network>." Nothing is saved.
  - Refuse duplicate `host:port` within one network the same way ("<host>:<port> is listed twice.").
  - Unreachable nodes are saved as today.
  - Probing happens outside any lock, and the settings save must not stall on the probe beyond the timeout.
- `fake-monerod`: serve `get_info` (JSON-RPC and `/get_info`) with a `nettype` from a new `--nettype` argument, defaulting to `stagenet` (what the Playwright suite uses).

**Acceptance:**
- Engine tests:
  - saving a stagenet node that reports mainnet is refused, and the setting doesn't change;
  - a node that doesn't answer is saved;
  - a node that reports `fakechain`, or doesn't implement `get_info`, is saved;
  - duplicates are refused;
  - `/status` includes each node's network.
- A monokulo test: `EngineStatusResponse` parses both with and without the new field.
- The engine-side refusal also works through the API (`real-4-crash.spec.js`'s direct API path is unaffected for a correct node).

### Step 5: The node form

- **Pure conversion code** in a new monokulo module (e.g. `crates/monokulo/src/admin_nodes.rs`), unit-tested on its own:
  - parse an address (the rules in section 2);
  - turn submitted `node_<n>_<i>_*` fields into an ordered list of rows, with per-row errors;
  - apply a `node_action` (`remove:<n>:<i>`, `up:<n>:<i>`, `down:<n>:<i>`);
  - turn rows into `MoneroNodeSetting`-shaped JSON and saved JSON back into rows.
- **View:** replace `AdminNetworkFieldView.value_json` with rows (address, ssl, self_signed, plus the status matched from `/status` by label), and render the Monero nodes tab as in section 2. The Monero nodes tab's page load calls `get_status_cached`. If `/status` fails, rows show no status and the page still renders.
- **Save** (inside step 2's handler): parse rows, apply any `node_action`, and stop with per-row errors if any. Otherwise send `monero_node.<network>` for each submitted network, and show the engine's refusals on the network's block. After a successful save that changed nodes, clear the `/status` cache.
- **Remove from the page:** the textarea, its Example disclosure, and the JSON parse error message ("Monero node config for {network} is not valid JSON").

**Acceptance:**
- Unit tests:
  - addresses: `host:port`, `http://`, `https://` (ticks TLS), `[::1]:18081`, missing port, port 0 and 65536, empty host, a stray path;
  - rows to JSON and back, keeping order and all four fields, including three or more fallbacks;
  - every `node_action`, including at the edges (up on row 0, down on the last row, remove the only row);
  - a blank add-row is ignored;
  - duplicate addresses are caught.
- HTTP tests (no JS):
  - adding a node through the blank row;
  - removing, moving up and moving down (each a single POST with `node_action`), with the saved order read back;
  - a parse error re-renders with the submitted values and saves nothing;
  - a wrong-network node shows the engine's message on the block and saves nothing;
  - clearing a network stores use saves and shows the red banner.
- A view test: a network with no nodes and no stores is a closed `<details>`, and one in use is open with its store count.

### Step 6: JavaScript enhancements

Keep them in the page's inline scripts, like `CUSTODY_BACKENDS_SCRIPT`, or in `fx-glue.js` if they belong with fixi. No new libraries.
- "Add another" appends a blank row with the next index and focuses its address.
- Ticking or unticking Use TLS shows or hides that row's self-signed checkbox.
- Rewrite `CONFIRM_CLEARED_NETWORK_SCRIPT` to count non-blank rows per network (after a pending Remove) instead of reading the textarea. It asks before a save or Remove that would leave a network stores use with no node.
- Everything must work after fixi swaps the panel in. Bind with delegation, or on fixi's swap event, not once at load.

**Acceptance:** `node --check` passes on any JS file edited, and the Playwright tests in step 7 cover each behaviour.

### Step 7: Playwright

- Update `real-helpers.js`:
  - `saveEngineSettings` becomes a helper that opens the tab holding the given fields and clicks "Save", or split it into a per-tab helper;
  - add helpers to set a network's nodes through the form (fill rows, save);
  - replace `fakeNodeJson()` uses on the page with them.

  Keep direct API use (`real-4-crash.spec.js`) as it is.
- Update `real-1-settings.spec.js`, `real-3-admin-page.spec.js` and `real-6-sections.spec.js` to the tabbed page. `real-6`'s "keeps edits in the other half" test becomes: with fixi, saving one tab doesn't disturb the tab bar's links or the banners area. Drop the part that no longer applies, and log it.
- New tests, with JavaScript and with JavaScript disabled (`javaScriptEnabled: false`) where it says so:
  - Switching tabs by clicking the tab bar, both ways; Back returns to the previous tab.
  - Every tab: every visible `.setting-field` has help, and nothing scrolls sideways at 320px, 390px and 1280px.
  - Adding a node, moving it up to primary, and removing one, both ways. Read the saved order back from the page after a reload.
  - A wrong-network node: start a second `fake-monerod --nettype mainnet` (or make the existing fixture's nettype switchable) and check the refusal message shows and nothing changed.
  - With JS: Use TLS toggles the self-signed box; the confirmation before removing a network's last node while stores use it, and none when no store uses it.
  - The Nodes tab marker appears when stagenet's only node is offline (`/fake/offline`), and goes when it's back.
- Gallery: in `real-8-theme.spec.js` (or a new spec following it), capture every tab in light and dark at phone and desktop sizes with `captureCoverageStage`, grouped under the admin settings page.

**Acceptance:** `npx playwright test -c real-binaries.config.js` passes. Record the totals in the progress notes.

### Step 8: Docs and cleanup

- Update `docs/DESIGN.md` and `README.md` wherever they describe the admin settings page or editing `monero_node` JSON on it. `grep -n 'admin/settings\|monero_node'` finds them; `docs/WOOCOMMERCE_ROADMAP.md` and `docs/txid_lookup_and_scan_chunking_wbs.md` also mention these. Only change what's now wrong. The API still takes JSON, so say that where it's relevant.
- Remove dead code left behind: old section helpers, the `scanner-settings` route, `value_json`, and CSS for the textarea or example if nothing else uses it.
- Run `cargo clippy` and the full test commands one last time.

**Acceptance:** `grep` finds no `scanner-settings`, `monokulo_section`, `engine_section` or `monero_node_` form field names left in `crates/` or `e2e/`, except the engine's API and tests of it.

---

## 4. Out of scope

- Changing any setting's meaning, default, validation range or help text beyond moving it. The node fields' help is new; write it from the existing Example disclosure text.
- The Invites and Logs admin pages.
- Changing the engine's settings API shape, other than the new `network` field on `/status` nodes and the new refusals in step 4.
- Probing a node from the page before saving it (a "Test" button). The status column after saving is enough for now.

---

## 5. Progress notes, decisions and reporting

Both files live next to this one in `docs/workpacks/`. Commit them with the steps. **A new agent must be able to pick up from these files and `git log` alone.**

### Before you start (and whenever you resume)

1. Read this file in full.
2. Read `nicer_admin_screen_progress.md` and `nicer_admin_screen_decisions.md` if they exist.
3. Run `git log --oneline` and `git status`.
4. Continue from the progress notes' "Resume here". Don't redo finished steps.
5. If there is uncommitted work, check it against the notes. Finish it or revert it deliberately, and write down which.

### `nicer_admin_screen_progress.md`

Update it and commit it with every step, and at meaningful points inside a long step (step 5's pure module done, for example). A work-in-progress commit is fine when you must stop mid-step: prefix its subject with `WIP:` and say so in "Resume here". Keep this structure:
- **Status table:** one row per step (1–8), its status (`not started` / `in progress` / `done`), and commit SHAs.
- **Resume here:** the exact next action, any half-finished work and where it is (files, functions), and anything a newcomer needs that isn't obvious from the code.
- **Test status at last commit:** `cargo test --workspace` totals, clippy status for touched files, and Playwright real-binaries totals.
- **Notes per step:** what was done, where, how each acceptance criterion was checked, and known weaknesses.

Write it for someone with no context: plain and specific, no shorthand.

### `nicer_admin_screen_decisions.md`

Add an entry every time the plan leaves something open or you have to deviate. Number the entries and commit them with their step. Each entry gives the step, the decision, the alternatives considered, and why.

### Final report (your last message)

1. Each step: its commits (SHA and subject), what was done, and each acceptance criterion and how it was checked.
2. Test results: the exact `cargo test --workspace` totals, clippy status for touched files, and Playwright real-binaries totals.
3. Every decision from the decision log, one line each.
4. Anything not done, done differently from the plan, or known to be weak. Be explicit; don't bury it.

Work through all eight steps. If a step turns out much bigger than expected, still finish it; don't skip ahead. Report failures faithfully: if a test fails and you can't fix it, say so and include the output.
