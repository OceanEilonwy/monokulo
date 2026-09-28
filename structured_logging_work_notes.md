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
| 2.4 PHP plugin | done except error forwarding | same | `traceparent` on both `wp_remote_post` calls, adopted from incoming webhooks, trace id on every log line. "Forward errors to monokulo" waits for part 3's store |
| 2.5 browser | done except checkout toggle | same | `static/telemetry.js`, `POST /telemetry/client`, `<meta name="traceparent">`. Only on pages with nav; the checkout toggle comes with part 8 |
| 3.x onwards | not started | | |

### Next

Part 3, the local log store: see the plan. Notes for it:

- Spans already come out of `tracing-opentelemetry` into an
  `opentelemetry_sdk` tracer provider built in `telemetry::build` with no
  processors. The SQLite span exporter is a `SpanProcessor`/`SpanExporter`
  added to that builder.
- Log lines: the store should be a `tracing` layer beside the JSON layer
  (sharing its per-layer level filter, e.g. `json.and_then(store)` under
  one `with_filter`), reusing `json::JsonVisitor` and `json::SpanIds` so
  redaction and trace ids are identical.

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
- **PHP**: one trace per PHP request (a static on the gateway class);
  `traceparent` gets a new span id per call; the trace id is appended to
  each log message as `[trace <id>]`, because WooCommerce's file handler
  drops the context array.

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
