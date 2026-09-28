# Structured logging: work notes

Progress notes for `structured_logging.md`, kept so another agent can pick up
where the last one stopped. Newest state first in "Where things stand";
decisions and surprises below it.

Branch: `structured-logging` (branched from `admin-settings-v2` at c7c95dc,
because it builds on the `live-settings` crate from that work).

## How to resume

1. `git checkout structured-logging && git log --oneline admin-settings-v2..`
   shows what is done.
2. Read "Where things stand" below, then the plan section it points at.
3. Checks before every commit:
   `cargo test -p telemetry -p shared -p live-settings`,
   `cargo test -p scanner -p monokulo` (slow),
   `cargo clippy --workspace --all-targets`.

## Where things stand

| Task | State | Commit | Notes |
|---|---|---|---|
| Plan | done | 741b108 | `structured_logging.md` |
| 1.1 `crates/telemetry` | done | ace9ab7 | init, JSON and pretty output, reload handle |
| 1.2 move log calls to `tracing` | done for server code | ace9ab7 | see "What was left on println/eprintln" |
| 1.3 level and dev mode as live settings | done | ace9ab7, and the commit "development logging chosen by the hour" | admin page: "Logging" heading in both halves; `logging.dev_mode_until` is a select (Off / 1 / 4 / 24 hours / "On until <time> UTC") |
| 1.4 redaction | done | ace9ab7 | `crates/telemetry/src/redact.rs` |
| 2.1 HTTP spans | done | "request spans and trace propagation" | `telemetry::http::server` middleware, outermost on both routers |
| 2.2 background spans | done | same | webhook attempts (`info`), scan ticks, per-store scans, key-custody calls (`debug`) |
| 2.3 propagation | done | same | `tracing-opentelemetry`; `traceparent` monokulo to engine (`shared::http_cache::build_traced_client`) and engine to merchant webhooks; `trace_id`/`span_id` on JSON lines |
| 2.4 PHP plugin | done | same, and "plugin errors forwarded" | `traceparent` on both `wp_remote_post` calls, adopted from incoming webhooks, trace id on every log line. "Send errors to Monokulo" option: warnings and errors batched to `POST /pay/{pk}/logs` at the end of the PHP request, non-blocking |
| 2.5 browser | done except checkout toggle | same | `static/telemetry.js`, `POST /telemetry/client`, `<meta name="traceparent">`. Only on pages with nav; the checkout toggle comes with part 8 |
| 3.1 SQLite store | done | "log store" | `telemetry::store`: `<db stem>.logs.db` beside each main database; writer thread; spans via an SDK `SpanProcessor` |
| 3.2 retention | done | same | `logging.retention_days` (14) and `logging.max_mb` (500) in both processes, applied once a minute by the writer thread |
| 3.3 engine log API | done | same | `GET /api/v1/admin/logs`, `/trace/{id}`, `/histogram`, `/attributes`, instance admin token |
| 3.4 merged reader | done | same | `monokulo::logs` (`Sources`, `read`, `trace`, `histogram`, `attribute_names`) |
| filter language (5.1 row 1) | done early | same | `telemetry::query`, needed by 3.3 |
| 4.1 vendor fixi/ssexi | done | "fixi foundation" | fixi 0.9.4, ssexi at 78c2fb4 (no tags yet), `.LICENSE`/`.SOURCE` beside them |
| 4.2 `FxRequest` | done | same | `http::fx`: `FxRequest`, `Timezone`, `respond`, `invalid` |
| 4.3 glue | done | same | `static/fx-glue.js`: `fx-push-url`, `fx-replace`, `fx-debounce`, `fx-sse-reconnect`, `aria-busy`, 5xx/network banner, `data-fx-focus`, `X-Timezone` |
| 4.4 no meta refresh | done | same | status and order detail lost theirs and gained `views::reload_button`; `http::pay::tests::only_the_checkout_refreshes_by_itself` |
| 5 Logs page | done | "the Logs page" | `/dashboard/admin/logs` (+ `/tail`, `/trace/{id}`, `/export`, `/saved`). Rows 1-18 all built except row 5's lazy loading (properties are always inline) and row 18 (keyboard shortcuts, optional); row 13 is a list in "How to search", not a datalist |
| 6 order detail, status | done | "order detail and status pages stream" | ssexi streams `/dashboard/stores/{id}/orders/{order_id}/events` and `/status/events`, JSON-routed to `#order-live` / `#status-live`; `done` event ends the order stream |
| 6 admin settings | done | "settings pages save one section at a time" | `#monokulo-settings`, `#engine-settings`; engine section out of band when the engine connection changes |
| 6 store settings | done | same | six sections (`views::store_settings::StoreSection`); base currency also sends confirmations out of band |
| 6 orders list, store detail, invites | done | "orders list, store detail and invites" | orders search and paging swap `#orders-results` with `fx-push-url`; payment lookup swaps its card; embed warning Dismiss swaps in the one-line version; invites buttons and paging swap `#invites` |
| 6 connect | not converted | | its post leads to a different page ("Store connected"), like login and signup, so a swap buys nothing |
| 7 OTLP export and docs | done | "OTLP export" | `telemetry::otlp`, settings `logging.otlp_endpoint`/`logging.otlp_headers` (secret) in both processes; `docs/LOGGING.md` |
| 8 checkout embed | done | "checkout embed on fixi and ssexi" | `#checkout-stream` opens `/events?routed=true`: one ssexi JSON-routed message per changed `[data-live]` part, then `status`, then `done`; refund save through fixi on a `refund:save` trigger, JSON answer read in `fx:after`; noscript refresh and Auto Refresh toggle unchanged |
| 9 tests | done | "trace from a caller" | converted handlers have fixi and full-page tests; `only_the_checkout_refreshes_by_itself`; redaction; filter language; real browser: `real-5-logs` (JS on and off, trace across services, a plugin-style `traceparent` followed to the engine), `real-6-sections` |

### Next

The plan is built. Open ends, none blocking:

- No test runs the real WooCommerce plugin against the real binaries; the
  trace test sends exactly what the plugin sends (its `traceparent`), and
  the plugin's side is covered by its PHPUnit tests.
- Logs page row 5 (lazy loading of properties) and row 18 (keyboard
  shortcuts) weren't built; properties are always inline.
- A store link on the Logs page finds monokulo's lines only: the engine
  logs a store by its tenant id, monokulo by its connection id.
- `connect` isn't converted to fixi (its post leads to another page).
- Browser reports from the checkout have no opt-in toggle; the checkout
  never loads `telemetry.js` (D8).

## Decisions and deviations from the plan

- **Development mode is a time, not a switch plus a duration.** The plan
  said `logging.dev_mode` (bool) plus `logging.dev_mode_minutes`. Built
  instead: one setting, `logging.dev_mode_until` (Unix seconds, 0 = off).
  Reasons: a restart during the window keeps the same end time (a bool would
  start a new hour, or need extra stored state); the admin page shows the
  true state without the process having to write its own setting back when
  the time runs out; and the expiry is just a timer in `Telemetry::apply`.
- **Development mode changes the level only, not the output format.** The
  plan said pretty output and span events in development mode. Switching a
  production process's stderr from JSON to text mid-run would break whatever
  parses its logs (journald JSON, a collector). Instead the format is chosen
  once at start: `<PREFIX>_LOG_FORMAT=json|pretty`, else pretty at a
  terminal and JSON otherwise. Span open/close events are not emitted; part
  2's HTTP spans will carry durations as fields instead.
- **Env variable names.** Level: `SCANNER_LOG`, `MONOKULO_LOG`,
  `KEY_CUSTODY_LOG` (the key-custody server has no settings, only the env
  variable). These are also the `logging.level` settings' env variables, so
  the variable is in effect from the first line and keeps winning after the
  settings load. `RUST_LOG` is not read.
- **Redaction is done where values are formatted**, not by a layer that
  removes fields (tracing layers can't change an event another layer sees).
  Both output formats call `telemetry::redact`. The log store (part 3) must
  reuse `json::JsonVisitor` or call `redact::field` the same way.
- **Redaction rules** (all in `redact.rs`, all unit tested): secret field
  names are replaced by `[redacted]`; client address fields keep only their
  network (IPv4 /24, IPv6 /48; loopback and private kept); Monero addresses
  in any string are shortened to 6 + 4 characters; `sk_<hex>` store keys in
  any string lose everything after `sk_`. Hex strings (txids, key images)
  are not touched: public data and needed for debugging. There is no keyed
  hashing, so nothing needs a per-install key.
- **Monokulo "connections" are logged as `store.id`** (a connection is a
  store in the UI), so one attribute name works across monokulo and the
  engine for the planned "everything about store X" view.
- **`shared::log::throttled` became the `shared::throttled!` macro**, which
  takes a key, a level and ordinary `tracing` fields, and adds
  `throttle_key` and `suppressed` fields. `shared` re-exports `tracing` for
  it.
- **`LogReloadable` applies to the process-wide subscriber** found through
  `telemetry::global()`, so test engines and test monokulo instances (which
  never call `telemetry::init`) register it and it does nothing. That kept
  every existing `EngineSettings::load*` / `MonokuloSettings::load` call
  site unchanged.

- **Trace ids come from `tracing-opentelemetry`**, with an SDK tracer
  provider that has no exporter yet. Its layer sees spans at `info` and
  above only, and never events; the output layer has the level filter as a
  per-layer filter. So an `info` request span has ids even at level
  `warn`. Hot-path spans (scan ticks, per-store scans, key-custody calls)
  are `debug`, so they never become stored traces and cost nothing at
  `info`.
- **The JSON layer looks up ids through the registry**, not its own
  layer context, because the level filter hides spans from that context
  (`json::SpanIds`, using the `Dispatch` saved in `on_register_dispatch`;
  `dispatcher::get_default` returns none inside an event).
- **No `tower-http` `TraceLayer`/`SetRequestIdLayer`.** One small axum
  middleware (`telemetry::http::server`, feature `axum`) does the span,
  the `traceparent` join and the finishing line. The request id is the
  trace id, returned in a W3C `traceresponse` header. `url.path` never
  includes the query string. Static files log at `debug`, 5xx at `warn`.
- **`client.address`**: the middleware records the TCP peer; monokulo's
  `http::abuse::anonymous_identity` records the real client over it when
  behind a trusted proxy. Onion clients have none.
- **Only calls to our own services carry `traceparent`**: monokulo to the
  engine, and engine webhooks (the merchant is our counterpart). Exchange
  rate APIs and other third parties don't.
- **Browser reports** are always `warn`, carry `browser.kind`, `url.path`,
  `detail`, and sit in a `browser report` span with `source = "browser"`
  that joins the page's trace. (`tracing::warn!(target: "browser", ...)`
  with dotted field names doesn't parse, so there is no `browser` target.)
- **Plugin errors go to `POST /pay/{pk}/logs`**, not the plan's
  `/api/v1/telemetry/logs`: the `/pay/{pk}` router already identifies
  the store, counts requests in the abuse limits and checks the store's
  secret key the way order creation does. At most 20 entries per request;
  lines carry `source = "woocommerce"` and `store.id`, and join the
  plugin request's trace.
- **PHP**: one trace per PHP request (a static on the gateway class);
  `traceparent` gets a new span id per call; the trace id is appended to
  each log message as `[trace <id>]`, because WooCommerce's file handler
  drops the context array.

- **The log store is a `tracing` layer, not an OTel `LogExporter`.**
  `json::EventLayer` captures each event once (fields, redaction, trace
  ids) and hands it to both the stderr writer and `store::StoreSink`, under
  the one level filter. Spans do go through OpenTelemetry: a
  `SpanProcessor` (`store::StoreSpans`) stores each finished span, with
  its attributes redacted (`store::redacted_attributes`), since
  `tracing-opentelemetry` copies span fields unredacted.
- **No FTS5 index.** Text search is `LIKE '%text%'` (case-insensitive for
  ASCII), which matches substrings the way a reader expects; FTS5 matches
  tokens. Fine for 14 days / 500 MB; revisit if searches get slow.
- **No `log-store`/`otlp`/`log-viewer` Cargo features yet.** The store is
  always built in; it costs nothing until `open_store` is called.
- **Hand-written parser instead of `chumsky`** for the filter language: the
  grammar is small, and error positions are simple to get right by hand.
  Printing (`Display`) round-trips through parsing (tested).
- **Store file name**: `path_beside(db)` gives `<stem>.logs.db`
  (`monokulo.logs.db`, `scanner.logs.db`), so two processes sharing a
  directory don't share a store. Lines logged before the store opens are
  held (up to 2,000) and stored when it does.
- **Paging** is keyset on `(ts, service, id)`, the same order in every
  store, so monokulo can merge its rows with the engine's and one cursor
  pages both. Encoded as `ts.id.service`.
- **Live tail** will use `LogStore::subscribe()` (a `watch` of the newest
  row id) and re-run the query with an `after` cursor, rather than
  evaluating the filter in memory.
- **The e2e targets** (`--features e2e`) didn't compile before this part
  (monokulo `AppState` gained `settings` in admin-settings-v2 and the
  harness wasn't updated). Fixed along with adding `log_store`.

- **fixi facts that shaped the glue**: fixi swaps any response, whatever
  its status (so a 422 fragment with errors just works, and the glue
  stops 5xx pages being swapped in); `fetch` follows redirects (hence
  `http::fx::respond`); fixi drops a new request while one is in flight
  unless told otherwise (`fx-replace`); view transitions are on by default
  (turned off in `fixiCfg`). An element can start its request as soon as
  it's processed with `fx-trigger="fx:inited"` (fixi fires `fx:inited`
  right after adding its listener): that's how a stream starts on load.
- **Scripts load on every page with nav** (`views::page_shell`), in the
  order glue, fixi, ssexi, all `defer`. Not on the bare layouts (checkout,
  challenge, POS); the checkout gets them in part 8.

- **Logs page URL is `/dashboard/admin/logs`**, not the plan's
  `/admin/logs`, like every other admin page. Links to it for an order
  (`order.id`) and a store (`store.id` = monokulo's connection id) are on
  those pages for admins (`views::logs_link`). The engine logs a store as
  its tenant id, so a store link finds monokulo's lines only; an order's
  id is the same in both.
- **Times**: UTC without JavaScript; with it, the glue sends the browser's
  zone (`X-Timezone`, and a `tz` cookie for full loads), and Rust formats
  with `jiff` using the system tz database.
- **Row 13 is not a `<datalist>`**: a datalist on the query box only
  matches the whole box's text, and headless Chrome left its popup open
  over the next page. Recent property names are listed in "How to search".
- **Live** re-runs the search with an `after` cursor whenever the local
  store's newest id changes, or every 2 s (the engine's lines are polled),
  and sends rendered rows as `event: {"target":"#log-rows","swap":"afterbegin"}`
  with the newest cursor as the event id (ssexi sends it back as
  `Last-Event-ID` on reconnect). The page script pauses it, stops it when a
  new search starts, and keeps at most 1,000 rows.
- **Real-browser tests**: `e2e/pos-playwright/tests/real-5-logs.spec.js`
  (`npx playwright test -c real-binaries.config.js`). With JavaScript off,
  headless Chrome hit-tests `<html>` for a while after a form submission,
  so that test follows the Refresh link's `href` rather than clicking it.

- **fixi posts urlencoded** (glue converts `FormData` to
  `URLSearchParams`): axum's `Form` refuses multipart, and without JS
  forms post urlencoded anyway. The glue swaps only 2xx and 422; other
  statuses show a banner (401/403 say "signed out").
- **Out-of-band swaps** (`data-fx-oob`, in the glue): a response can carry
  extra sections that replace the page's copies by id. Used when a save
  changes another section too.
- **Focus after a save** goes to a short status by the Save button
  (`.save-status`) with `preventScroll`, so the page doesn't jump; the
  fuller banners sit at the top of the section. Without JS, errors show at
  the top of the page, with a "Go to the form" link on store settings.
- **Confirm dialogs** (`onsubmit="return confirm(...)"`, and the admin
  page's cleared-network check, now a capture listener on the document)
  cancel fixi too: the glue drops a submit whose event was cancelled.
- **Store settings `saved()` re-reads the row**: the handler's row is
  from before the save.

- **OTLP isn't `opentelemetry-otlp`**: `telemetry::otlp` builds
  `opentelemetry-proto` messages (prost) from the same redacted rows the
  store keeps and POSTs them as OTLP/HTTP protobuf with reqwest. That way
  redaction happens in one place (the SDK exporters would get span fields
  and log records before our redaction), the endpoint can change live, and
  there's no gRPC stack. The local store always stays on. Seq's OTLP
  endpoint is `/ingest/otlp` (verified Sept 2026); the Aspire Dashboard's
  OTLP/HTTP port is 18890 inside its container.

- **Checkout stream**: `routed=true` is new; `fragments=true` stays for
  checkout pages already open with the old script. Per stream, the server
  remembers each part's last HTML and sends only changed parts. The glue
  attribute `fx-own-errors` stops its banner for the stream and the refund
  form (the checkout shows its own messages). A refused stream is retried
  from `checkout.js` with backoff by dispatching `fx:inited` again.
- **`var` hoisting bit once**: `checkout.js` is one function scope; a new
  `var stream` for the live stream silently replaced the camera's. The
  live stream is `liveStream`.

## What was left on println/eprintln, on purpose

- CLI output meant for the person at the terminal: the engine's one-off
  commands (`--rotate-secret`, `--show-tenant`, `--bootstrap-wallet`, the
  usage error) and the "generated a new instance admin token" line, which
  shows a secret once and must never go to a log.
- Test tools and harnesses: `crates/scanner/src/bin/e2e_harness.rs`,
  `crates/scanner-test-support` (including `fake-monerod`, whose
  `FAKE_MONEROD_READY` stdout line the Playwright setup waits for),
  `crates/cli-wallet`, `crates/snp-attest`, `crates/mock-woocommerce`,
  `xtask`, and `println!` inside tests.

## Things to know

- **Tests that assert on log lines need their own test binary with one
  global subscriber** (see `crates/scanner/tests/tracing.rs`). A
  thread-local `set_default` subscriber in the lib test binary loses lines
  at random: other tests on other threads hit the same callsites with no
  subscriber, and tracing's callsite interest cache races with them
  (`rebuild_interest_cache` doesn't fix it). The telemetry crate's own
  tests are fine because nothing else there logs through their callsites.
- **Running the PHP tests**: the wp-env containers mount another
  worktree's plugin directory. Copy this one in and run phpunit there:
  `docker exec C rm -rf /tmp/mk; docker exec C mkdir /tmp/mk; tar
  --exclude=vendor -cf - -C plugins/woocommerce . | docker exec -i C tar
  -xf - -C /tmp/mk; docker exec -w /tmp/mk C sh -c 'ln -s
  /var/www/html/wp-content/plugins/monokulo/vendor vendor;
  WP_TESTS_DIR=/wordpress-phpunit vendor/bin/phpunit'` with
  `C=wp-env-woocommerce-coverage-81799d41-tests-cli-1` (or whichever
  `*-tests-cli-1` container is running).

- Nothing in the repo parses the servers' stdout: the Playwright
  real-binaries setup waits by polling HTTP and the socket, so moving the
  "listening on" lines to `tracing` (stderr) broke nothing.
- Tests don't install a subscriber, so log output from code under test is
  no longer printed in failing test output (it used to be, via
  `eprintln!`). If that is missed, a test helper can call
  `telemetry::build(..)` and `tracing::subscriber::set_default`.
