# Structured logging, a built-in Logs page, and fixi across monokulo's pages

## Why

Nothing in the workspace uses structured logging. Across the crates there are
about 170 `eprintln!`/`println!` calls and no `tracing` call sites; the only
logging helper is `shared::log::throttled` (`crates/shared/src/log.rs`), which
also writes plain lines to stderr. The WooCommerce plugin logs through
`wc_get_logger()` (`class-wc-gateway-monokulo.php`), and the browser JS in
`crates/monokulo/static/` reports nothing at all. A request that crosses the
plugin, monokulo and the engine can't be followed, logs can't be filtered by
store or order, and there's nowhere for an admin to read them except
`journalctl`.

Separately, every dashboard and admin page is a full page reload per form
post, and the pages that stay current (order detail, status) do it with a
`<meta http-equiv="refresh">`. That loses scroll position and unsaved edits in
other sections, and flashes the page. The Logs page needs partial updates and
live streaming anyway, so the same tools (fixi and ssexi) are adopted across
the server-rendered pages at the same time.

## Goals

1. Logs are structured: every event has named fields, so it can be searched,
   filtered, and grouped by request (`trace_id`), order, store and session.
2. Admins can explore logs from a Logs page built into monokulo, or from any
   other tool through OpenTelemetry (OTLP) export.
3. There is a sensible production default that needs no setup (JSON on
   stderr plus a local log store), a runtime "development mode" toggle for
   verbose logging, and a configurable export destination.
4. Community crates do as much of the work as possible (`tracing`,
   `tracing-subscriber`, `tower-http`, `opentelemetry*`, `chumsky`).
5. Logs from the PHP plugin and the browser JS join the same traces.
6. The setup follows OpenTelemetry practice: W3C `traceparent` propagation,
   the OTel log data model and semantic conventions for field names.
7. Server-rendered pages work fully without JavaScript and are enhanced with
   fixi (partial swaps) and ssexi (server-sent events) when it is available.

## Decisions recorded

| # | Question | Decision |
|---|---|---|
| D1 | Where logs are stored and viewed | Proposal P1: each process writes to its own local SQLite log store (`logs.db`, separate from the main database), and monokulo has a built-in Logs page. OTLP export is optional, for operators who run their own stack (Grafana, Seq, SigNoz and so on). |
| D2 | Which viewer UI | Build it in maud, copying Seq's UX: one search bar with a filter language, expandable events with find/exclude links, saved searches, a histogram strip, a trace waterfall. No third-party viewer is embedded (Seq is closed source; Aspire Dashboard and Jaeger UI don't fit server rendering). |
| D3 | Progressive enhancement library | fixi (https://github.com/bigskysoftware/fixi) and ssexi (https://github.com/bigskysoftware/ssexi), both 0BSD, vendored into `static/`. Used on the Logs page, on the existing server-rendered pages listed in part 6, and on the checkout embed (part 8). Never in the POS app, which stays a separate JS-only app. |
| D4 | Live updates without JavaScript | Dropped everywhere except the payment embed. Without JS, every page other than the checkout embed is a point-in-time view with a Reload button; no `<meta http-equiv="refresh">`. The checkout embed keeps its `<noscript>` meta refresh and its Auto Refresh ON/OFF toggle, because it must live-update for Tor Browser users with JS off. The abuse challenge page keeps its `<noscript>` 10-second continue refresh, since it is a step in the embed's payment flow, not a live update. |
| D5 | Dev tooling | .NET Aspire Dashboard (a standalone OTLP viewer) is documented as an optional companion for `scripts/dev-run.sh`. Seq and Grafana are documented as OTLP destinations. None is bundled. |
| D6 | How monokulo gets the engine's logs | Monokulo pulls them through a new engine admin API route (`GET /api/v1/admin/logs`). This keeps the current boundary (the engine is private and only answers monokulo); the engine never pushes to monokulo. See 3.3. |
| D7 | Retention of `logs.db` | 14 days or 500 MB, whichever comes first; both live settings. See 3.2. |
| D8 | Browser telemetry | Off by default on the checkout embed (privacy on a Monero payment page), on by default for admin and dashboard pages. See 2.5. |
| D9 | Checkout embed | Converted to fixi and ssexi as well (part 8). Its `<noscript>` meta refresh stays (D4). |

## Task tree

### 1. Telemetry foundation

#### 1.1 `crates/telemetry`

A new workspace crate that monokulo, the engine and the key-custody service
call once at boot: `telemetry::init(service_name, &config) -> TelemetryHandle`.

It builds a `tracing_subscriber::Registry` with these layers:

- an `EnvFilter` wrapped in `tracing_subscriber::reload`, so the level can
  change at runtime (1.3);
- stderr output: JSON lines in production (journald keeps them), pretty
  ANSI output with span open/close events in development mode;
- the redaction layer (1.4), placed before every layer that writes out;
- the local log store exporter (part 3), behind the `log-store` feature;
- OTLP export (part 7), behind the `otlp` feature.

`MONOKULO_LOG` (or `RUST_LOG`) overrides the filter at boot for operators.

Cargo features: `log-store` (default on), `otlp` (default off), and on
monokulo `log-viewer` (default on).

#### 1.2 Move every log call onto `tracing`

Crate by crate, replace `eprintln!` in server code with `tracing` events that
carry fields, not formatted strings:
`tracing::warn!(network = ?network, error = %e, "scan tick failed")`.
CLI output meant for the person at the terminal (`scanner/src/main.rs`
subcommands, `cli-wallet`) stays as `println!`.

`shared::log::throttled` keeps its behaviour (one message per key per
minute) but emits a `tracing` event with a `suppressed` field in place of the
"(and N more like this ...)" suffix.

Field names follow OTel semantic conventions where one exists
(`http.request.method`, `http.response.status_code`, `server.address`,
`error.type`), plus project attributes: `store.id`, `order.id`, `network`,
`session.id` (hashed), `custody.backend`.

#### 1.3 Development mode and level as live settings

New `logging` section in both processes' `live-settings` registries:

- `logging.level` (default `info`);
- `logging.dev_mode_until` (a Unix time, default 0 = off). Until then the
  level is `debug` (noisy libraries held at `info`). It ends by itself, so it
  can't be left on in production by accident, and a restart keeps the same
  end time. The admin page offers Off / on for 1, 4 or 24 hours and shows
  when it ends. The output format doesn't change with it: JSON or pretty is
  chosen once at start (`<PREFIX>_LOG_FORMAT`, else pretty at a terminal),
  so whatever parses the logs keeps working. (Changed from the first draft's
  on/off switch plus duration; see the work notes.)

Both apply on save through the reload handle, with no restart, and follow
the admin page's existing "applies on save" rules.

#### 1.4 Redaction

Secrets, view keys, spend keys, admin and store tokens, full Monero
addresses and client IP addresses never reach any output, in any mode,
including development mode:

- secret values are held as `secrecy::SecretString`, which can't be printed;
- a redaction layer drops fields whose names are on a deny list and replaces
  addresses and IPs with a per-install keyed hash (so they can still be
  grouped, but not read);
- a test logs every deny-listed field name through the real subscriber and
  asserts none of the values appears in any output.

### 2. Spans and trace propagation

#### 2.1 HTTP spans

A small axum middleware, `telemetry::http::server`, outermost on both
routers (built instead of `tower-http`'s `TraceLayer`, which would have
needed the same custom span and finishing code). Span fields use the OTel
HTTP conventions. The request id is the `trace_id`: the caller's when a
`traceparent` arrives, otherwise a new one, returned in a `traceresponse`
header.

#### 2.2 Background work

One span per scanner tick (`network`), per store scan (`store.id`), per
webhook delivery attempt (`store.id`, `order.id`, attempt number), per rescan
job and per key-custody call. Events inside them inherit those fields, which
is what makes "everything about order X" work.

#### 2.3 Propagation across processes

`tracing-opentelemetry` connects `tracing` spans to OTel context. W3C
`traceparent` is injected on:

- monokulo to engine (`engine_client.rs`);
- engine to merchant webhooks (so a merchant can match our trace to theirs);

and extracted on every incoming HTTP request.

#### 2.4 PHP plugin

- Keep `wc_get_logger()` so WooCommerce admins still see plugin logs where
  they expect them.
- Add a small `traceparent` generator (random trace and span ids, about 20
  lines, no SDK) and send the header on every request to monokulo. Plugin
  log lines include the trace id.
- Optional plugin setting "Forward errors to monokulo": warn and error
  entries are sent in batches to `POST /api/v1/telemetry/logs`, authenticated
  with the store's API key, tagged `source=woocommerce`.
- The OTel PHP SDK is not used: its Composer dependencies conflict with
  other WordPress plugins.

#### 2.5 Browser JS

- `static/telemetry.js` (about 60 lines, no build step) reports `error` and
  `unhandledrejection` events, SSE disconnects, QR failures and failed fixi
  requests (`fx:error`) with `navigator.sendBeacon` to `/telemetry/client`
  on the same origin.
- Every page renders `<meta name="traceparent">` for its own request, and
  `telemetry.js` sends it back, so browser events join the trace of the
  request that rendered the page.
- `/telemetry/client` is rate limited through the `abuse` module, caps the
  body size, and always tags entries `source=browser`; the client can't
  choose the service name or level above `warn`.
- Off by default on the checkout embed, on by default elsewhere (D8).

### 3. Local log store

#### 3.1 SQLite exporter

An OTel `LogExporter` and `SpanExporter` backed by `rusqlite`, writing to
`logs.db` next to each process's main database. Because it is just another
OTel exporter, the stored data uses the OTel data model and nothing is lost
when an operator also exports over OTLP.

Schema (one table for log records, one for spans):
`ts, severity, service, target, body, trace_id, span_id, attrs` (JSON), with
generated, indexed columns for `store_id` and `order_id`, and an FTS5 index
on `body`.

Writes go through a bounded channel to one writer task that inserts in
batches. The log path never blocks on SQLite: when the channel is full,
records are dropped and a `dropped` counter is logged once a minute.

#### 3.2 Retention

A background loop deletes by age and by size (D7), both live settings.

#### 3.3 Engine logs through the admin API

`GET /api/v1/admin/logs` and `GET /api/v1/admin/logs/trace/{trace_id}` on the
engine, admin token only. They take the compiled filter (4.x) and a cursor,
and return rows in the same shape monokulo reads locally.

#### 3.4 Reading from both stores

A `LogReader` trait with a local SQLite implementation and an engine
implementation. The Logs page merges both as two sorted streams, with a
cursor that covers both sources. If the engine can't be reached, the page
shows a banner and monokulo's own logs still appear.

### 4. fixi foundation

#### 4.1 Vendor fixi and ssexi

`static/fixi.js` and `static/ssexi.js`, pinned, each with `.LICENSE` and
`.SOURCE` files (as for `jsQR.js`), served from the same origin. No CDN: the
pages must work offline and over Tor.

#### 4.2 One handler, two responses

- An `FxRequest` extractor that is true when the request carries fixi's
  `FX-Request` header.
- A response helper for form posts: for a fixi request it returns the
  section's HTML fragment (with its saved or error state), otherwise it
  returns the redirect as today (`dashboard::redirect_302`).
- This matters because `fetch` follows redirects by itself: a fixi post
  that got the usual redirect would swap a whole page into one section.
  Every converted handler has a test for both paths.
- Fragments come from the same maud functions that render the full page;
  where a section isn't its own function yet, it becomes one first.
- Check how fixi treats 4xx responses before relying on status codes;
  validation errors render inside the form fragment.

#### 4.3 Glue script

`static/fx-glue.js` covers what fixi leaves out on purpose:

- history: `history.pushState` on `fx:after` for views that use the URL
  (logs, orders list), and a full reload on `popstate`;
- request indicators: `aria-busy` set on `fx:before`, cleared on
  `fx:finally`;
- errors: a banner on `fx:error`;
- request queueing: aborting the previous request and debouncing, in
  `fx:config`, where needed;
- focus: after a swap, focus moves to the swapped section's heading or first
  error, and saved and error messages sit in an `aria-live` region;
- timezone: an `X-Timezone` header from `Intl`, so Rust can format times in
  the browser's zone. Formatting always stays in Rust.

#### 4.4 No meta refresh outside the embed (D4)

Remove `<meta http-equiv="refresh">` from every page except the checkout
embed and the abuse challenge page. Pages that used it get a Reload button
for no-JS users. A test renders every page and asserts that only those two
contain a meta refresh (generalising the dashboard's existing
`never_shows_a_meta_refresh`).

### 5. Logs page

`/admin/logs`, admin only, behind the `log-viewer` feature.

#### 5.1 Features

| # | Feature | No JS | JS (fixi / ssexi) | Difficulty |
|---|---|---|---|---|
| 1 | Search bar with filter language (`order.id = 'x' and level >= warn`) | GET form with a submit button, full reload. The query lives in the URL, so it can be shared and bookmarked. A bad query shows the error above the results. | fixi on the form, `fx-target` is the results area. The handler returns only that fragment for `FX-Request` (4.2). | High. Parser (`chumsky`), AST to parameterised SQL over `json_extract`, error spans. The most security-sensitive part: SQL is never built from query text. |
| 2 | Quick filters (level, service, time presets) | Selects and radios in the same form; they apply when Submit is pressed. | `fx-trigger="change"` submits immediately. | Low |
| 3 | Custom time range | `datetime-local` inputs. Times shown in UTC or the admin's saved timezone. | Glue sends `X-Timezone`; Rust formats in that zone. | Low |
| 4 | Results list and paging | "Older" and "Newer" links with a keyset cursor (`ts, id`), not OFFSET. | "Load more" button with `fx-swap="outerHTML"` on itself; the server returns the next rows and a new button. | Low–Medium |
| 5 | Expand an event to see its properties | `<details>`/`<summary>` with properties rendered inline; pages capped at about 100 rows. | `fx-trigger="toggle"` on `<details>` loads the properties on first open. | Low |
| 6 | Find / exclude property links | Plain links; the server writes the extra clause into the query in the URL. | Same links with `fx-action` and the results target. | Medium (needs an AST-to-text printer) |
| 7 | Histogram strip | Server-rendered SVG or CSS bars; each bar links to a zoomed-in time range. | Swapped together with the results (one wrapper, since fixi swaps one target). | Medium |
| 8 | Live tail | No. A Refresh button re-runs the current query up to now. Nothing reloads by itself. | ssexi: the tail endpoint streams rendered rows, `fx-swap="afterbegin"` into the table body. `sseReconnect` with `Last-Event-ID` set to the last row id, so there are no gaps. `ssePauseOnHidden`. Pause button calls `cfg.sse.close()`. Glue caps the rows kept in the page. | Medium–High. New-row notifications from the exporter (`tokio::sync::broadcast`), filtered against the compiled query; engine rows polled through 3.3. |
| 9 | Trace waterfall for one `trace_id` | Separate page `/admin/logs/trace/{id}`: CSS bars with left and width percentages computed in Rust, nesting by indent. | Opens in a side panel through fixi. | Medium |
| 10 | Order / store / session view | Prefilled query links from the order and store pages. | Same. | Low |
| 11 | Merging engine logs | Server side (3.4); banner when the engine can't be reached. | Same. | Medium–High |
| 12 | Saved searches | POST form then redirect; list of links. | fixi POST, swap the list. | Low–Medium |
| 13 | Field-name help and autocomplete | `<datalist>` of recently seen property names; grammar help in `<details>`. | Same. | Low |
| 14 | Export (NDJSON / CSV) | Download link with the same query and `?format=ndjson`. | Same plain link. | Low |
| 15 | Search as you type | No; Enter or Submit. | Optional: `fx-trigger="input"`, debounced, previous request aborted in `fx:config`. | Low–Medium |
| 16 | Back/forward and shareable URL after partial swaps | Normal navigation. | Glue `pushState` on `fx:after`, reload on `popstate`. Required, or JS mode would work worse than no-JS mode. | Low |
| 17 | Loading and error states | The browser's own. | Glue: `aria-busy` and an error banner. | Low |
| 18 | Keyboard navigation (j/k, `/` to focus search) | No. | Glue only; optional. | Low |

#### 5.2 Build order

1, 4, 5 and 16 first (the core, working both ways), then 2, 3, 6 and 10,
then 11 and 9, then 7, 12, 13 and 14, then 8.

### 6. Converting the existing pages to fixi

Every page below keeps working without JS as a point-in-time view (D4).

| Page | Today | No JS after | JS after |
|---|---|---|---|
| Admin settings (`views/admin.rs`, `http/admin_settings.rs`, 16 forms) | Each Save reloads the page: scroll jumps to the top, unsaved edits in other sections are lost. | Unchanged (post, redirect, reload). | fixi posts one section and swaps back that section with its saved or error state and any banners (restart-needed, unreachable node) inline. Scroll and other sections stay as they were. |
| Store settings (`views/store_settings.rs`, `http/orders.rs` update handlers, 17 forms) | Same as admin settings. | Unchanged. | Same section-swap pattern, including key storage moves and confirmation thresholds. |
| Order detail (`views/orders.rs`, `meta_refresh_secs: 15`) | Whole page reloads every 15 seconds, with or without JS. | Meta refresh removed; a Reload button. | ssexi on the order's existing event stream (the one the checkout uses), swapping the status, confirmations and payment fragments. |
| Status page (`views/status.rs`, 30-second meta refresh) | Whole page reloads every 30 seconds. | Meta refresh removed; a Reload button. | ssexi stream of the status fragment. |
| Orders list | GET form and links, full reload. | Unchanged. | Same pattern as the Logs results list (5.1 rows 1, 4, 16). |
| Connect, store detail, embed domains, invites, webhooks | Posts with full reloads. | Unchanged. | Section swaps (4.2). |
| Login, signup, create order | Post then redirect to another page. | Unchanged. | Not converted; these are page changes anyway. |
| Challenge page, `monokulo-client.js`, POS app | — | Unchanged. | Not converted. The challenge page is proof-of-work code, `monokulo-client.js` runs on merchants' own pages, and the POS app is separate (D3). |

Order: admin settings and store settings first (biggest UX gain), then order
detail and status, then the rest, then the checkout embed (part 8).

### 7. OTLP export and docs

- `otlp` feature: `opentelemetry-otlp` with the HTTP/protobuf transport over
  `reqwest` (already a dependency), so no gRPC stack. Settings:
  `telemetry.otlp.endpoint`, `telemetry.otlp.headers`, and whether the local
  store stays on alongside it.
- Docs: sending to an OpenTelemetry Collector, then Grafana (Loki and
  Tempo) or Seq; running the Aspire Dashboard with `scripts/dev-run.sh`.
- Before the docs depend on them, check current Seq OTLP trace support, the
  Aspire Dashboard's OTLP/HTTP port, and each product's licence.

### 8. Checkout embed (D9)

- The `<noscript>` meta refresh and the Auto Refresh ON/OFF toggle stay
  exactly as they are (D4).
- With JS, the hand-written EventSource code in `static/checkout.js` is
  replaced by ssexi, using its JSON event routing
  (`event: {"target":"#id","swap":"outerHTML"}`) with one message per
  `[data-live]` fragment in place of one template. The server stream changes
  shape to match.
- The refund-address form posts through fixi (the no-JS Save button stays
  in `<noscript>`); the debounce and the saved/error display that drive
  auto-save stay in `checkout.js`, triggering the fixi request.
- QR scanning stays hand-written.
- Stream behaviour to keep: reconnect with `Last-Event-ID`, stop when the
  order reaches a terminal status (a named `status` event, handled on
  `fx:sse:status`), close on `pagehide`, reload on a back-forward cache
  `pageshow`.
- The embed and its embedder still know nothing about each other.

### 9. Tests

- Every converted handler: one test for the full-page path and one for the
  fixi fragment path.
- The meta-refresh rule (4.4).
- Redaction (1.4).
- Filter language: parser round trips, SQL injection attempts, errors with
  positions.
- End to end in a real browser (existing Playwright setup): the Logs page
  with JS on and off; one request followed from the WooCommerce plugin
  through monokulo to the engine on the trace waterfall; a settings section
  saved without reloading the page.
