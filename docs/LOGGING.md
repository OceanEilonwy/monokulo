# Logging

Monokulo, the engine (`scanner`) and the key-custody server write structured
logs: every line has a level, a message and named properties (`store.id`,
`order.id`, `network`, `http.route` and so on), and every line written while
handling a request carries that request's OpenTelemetry trace id. A request
can be followed from the WooCommerce plugin, through monokulo, to the engine
and on to a merchant's webhook endpoint.

Secrets never reach any output, in any mode: fields named like secrets
(tokens, keys, passwords) are replaced by `[redacted]`, client IP addresses
keep only their network (IPv4 /24, IPv6 /48), Monero addresses are shortened
to their first 6 and last 4 characters, and `sk_…` store keys are removed.
This happens before a line is written anywhere: stderr, the log store, or an
OpenTelemetry collector.

## Where lines go

Out of the box, with nothing to configure:

- **stderr**, one JSON object per line when stderr isn't a terminal (what
  journald and container runtimes collect), readable text at a terminal.
  `MONOKULO_LOG_FORMAT`, `SCANNER_LOG_FORMAT` or `KEY_CUSTODY_LOG_FORMAT`
  (`json` or `pretty`) chooses explicitly.
- **A local log store**, `<database>.logs.db` next to each process's main
  database (`monokulo.logs.db`, `engine.logs.db`). It holds lines and spans
  for the Logs page, and keeps 14 days or 500 MB, whichever is less
  (`logging.retention_days`, `logging.max_mb`).

Optionally, **an OpenTelemetry collector** as well (below).

A JSON line looks like this:

```json
{"timestamp":"2026-09-28T12:00:00.123456Z","level":"WARN","service":"scanner","target":"scanner::webhook_delivery",
 "trace_id":"4bf92f3577b34da6a3ce929d0e0e4736","span_id":"00f067aa0ba902b7",
 "message":"webhook delivery failed; will retry",
 "attributes":{"webhook.id":"wh_1","order.id":"o_9","attempt":2,"http.response.status_code":502},
 "spans":["webhook delivery"]}
```

## Levels and development logging

Both are on the admin settings page's **Logging** tab (monokulo's and the
engine's, each under its own heading), and apply on save without a restart.

- `logging.level`: `info` by default. A `tracing` filter, so parts of a
  process can be louder: `info,scanner::loops=debug`. The environment
  variables `MONOKULO_LOG`, `SCANNER_LOG` and `KEY_CUSTODY_LOG` set it too and
  win over the saved value.
- `logging.dev_mode_until`: development logging, chosen as off or on for 1,
  4 or 24 hours. Until then the process logs at `debug` (chatty libraries
  stay at `info`), then goes back by itself. Redaction still applies.

## The Logs page

`/dashboard/admin/logs`, for admins (the **logs** link in the nav). It shows
monokulo's and the engine's lines together, newest first. Monokulo reads the
engine's store through the engine's admin API, with the engine admin token it
sends on every engine request (`MONOKULO_SCANNER_ADMIN_TOKEN`).

Search with a small filter language:

```text
level >= warn and store.id = 's_1'
order.id = 'o_9' or message contains 'timeout'
not (service = 'scanner') and has error
'payment'                      -- lines whose message contains it
```

Built-in names are `level`, `service`, `target`, `message`, `trace_id` and
`span_id`; any other name is a property of the line. Lines expand to show
their properties, each with **Find** and **Exclude** links; **Show the whole
trace** opens the request's spans as a waterfall. Searches can be saved and
downloaded as NDJSON or CSV. Order and store pages link to their lines.

Every request's lines say who it was for, once the server knows:

- `session.id`: the signed-in session, a short name derived from it that
  can't be used to find or present the session. Visitors who aren't signed
  in (a customer at the checkout) have none, and no cookie is set to give
  them one.
- `user.id`: the signed-in user. An opened line shows their email beside it.
- `store.id`: the store the request concerns: monokulo's id for its store
  pages and `/pay/{pk}/...` routes, the engine's own id for a request
  authenticated with a store's secret key. The two services number stores
  separately.

An opened line lists these first. Each line also has **Show trace** and
**Show session** buttons, so a request's trace, or every line of the
session it was part of, is one click away without opening it. For a POS
line with no signed-in session, Show session opens the POS session timeline.

The histogram above the search shows lines over the time range; a bar
narrows the search to its slice. **Refresh** in the title bar runs the
search again up to now. Without JavaScript the page is ordinary forms and
links. With it, searches update in place, **Older** appends the next page
and **Live**, beside Refresh, streams new lines as they are written until
**Pause**.

## Browser, POS and plugin reports

Client logs from a store are **opt-in per store**: **Diagnostics** in the
store's settings, off by default. While it is off, that store's dashboard
pages and checkout don't load the report script, the POS records no session
timeline, and anything sent anyway is dropped on arrival; the WooCommerce
plugin's forwarded errors get `403` even with its own option on. Pages about
no store (the admin pages, the dashboard home) always report: they carry no
merchant or customer data.

- The site's own pages report JavaScript errors and failed requests to
  `POST /telemetry/client`, in the trace of the request that rendered the
  page. Reports are always `warn`, clipped, and limited per client by
  `abuse.client_logs_per_min` (a `429` drops them; never a challenge).
- The POS records a timeline of each session (`pos-ui/src/timeline.ts`):
  the tablet going offline and back (and for how long), the page hidden and
  shown, frozen and resumed, restored from the back/forward cache, the live
  stream dropping and reconnecting, every API request (route, status, time
  taken), orders charged (whether a note was given and its length, never
  its text), created, backgrounded, brought back, cancelled, changing
  status and finishing, refund addresses saved (never the address), screen
  changes and script errors. Events queue in `sessionStorage`, so nothing
  is lost offline or across a reload, and go in batches to
  `POST /dashboard/stores/{id}/pos/logs` (`source = pos`, `pos.session`,
  `pos.seq`, `pos.client_ts`, `pos.kind`, `pos.detail`, `order.id`). A
  line's "Show the POS session timeline" link, or "POS session" on a POS
  order's page, opens `/dashboard/admin/logs/pos/{session}`: the events in
  the tablet's order with gaps, offline and hidden periods, and late
  arrivals called out.
- The WooCommerce plugin sends a `traceparent` header on its requests and
  adds `[trace <id>]` to its own WooCommerce log lines, so the two can be
  matched. With **Send errors to Monokulo** on (off by default) and the
  store's Diagnostics on, its warnings and errors also go to monokulo's logs
  (`source = woocommerce`).

## Sending to an OpenTelemetry collector

Set `logging.otlp_endpoint` (and, if the collector needs an API key,
`logging.otlp_headers`) on the admin settings page's Logging tab, under both
Monokulo and Engine, or with the environment variables
`MONOKULO_LOGGING_OTLP_ENDPOINT`, `SCANNER_LOGGING_OTLP_ENDPOINT`,
`MONOKULO_LOGGING_OTLP_HEADERS` and `SCANNER_LOGGING_OTLP_HEADERS`.

Lines and spans are sent as OTLP over HTTP with protobuf, to
`{endpoint}/v1/logs` and `{endpoint}/v1/traces`, in batches every two
seconds. They are the same redacted rows the log store keeps. The local store
stays on as well. If the collector is down, records are dropped (and one
warning a minute says so); nothing waits for it.

Headers are `name=value` pairs separated by commas, as in
`OTEL_EXPORTER_OTLP_HEADERS`: `authorization=Bearer abc,x-team=ops`.

### An OpenTelemetry Collector (then Grafana, Loki, Tempo...)

Point the endpoint at the Collector's OTLP/HTTP receiver, usually
`http://<collector>:4318`, and route from there. This is the most flexible
setup for Grafana (Loki for logs, Tempo for traces) or any other backend.

### Seq

Seq accepts OTLP over HTTP/protobuf for logs and traces at
`/ingest/otlp/v1/logs` and `/ingest/otlp/v1/traces` on the same address as its
UI. Set the endpoint to `https://seq.example.com/ingest/otlp` and, with an API
key, the headers to `X-Seq-ApiKey=<key>`. Seq is commercial software with a
free individual licence; check its current terms.

### The .NET Aspire Dashboard, for development

A single container with a trace and log viewer, useful next to
`scripts/dev-run.sh`. It is MIT licensed.

```sh
docker run --rm -it -p 18888:18888 -p 4318:18890 mcr.microsoft.com/dotnet/aspire-dashboard:latest
export MONOKULO_LOGGING_OTLP_ENDPOINT=http://127.0.0.1:4318
export SCANNER_LOGGING_OTLP_ENDPOINT=http://127.0.0.1:4318
scripts/dev-run.sh
```

Open `http://localhost:18888` (the container prints a login link). Port 18890
inside the container is its OTLP/HTTP receiver.
