# Proposal: the engine as a library inside monokulo

Status: proposal accepted (see "Decisions" at the end), nothing implemented
yet. Written 2026-10-03 against `1cc17f2` (origin/main).

## Summary

Make the engine a library that monokulo can start inside its own process,
and keep the separate engine binary for deployments that want it apart.
Monokulo talks to the engine through one client, `EngineClient`, with two
transports, chosen in configuration:

- **`embedded`** (the new default): monokulo starts the engine in-process
  and calls the engine's own admin API router directly, as a function
  call on an `axum::Router`. There is no socket, no port and no token in the
  environment. One binary, one process, one options file.
- **`remote`** (today's behaviour): monokulo calls a separately running
  `monokulo-engine` over HTTP, with the engine token. This stays for
  deployments that keep the engine on another host, such as the SEV-SNP
  setup with its key custody service.

The engine's HTTP API stays the single contract in both modes: the same
handlers, authentication, validation, rate limits and event streams run
either way. Only the bytes skip the network. That is the main decision in
this proposal, and the alternatives are below.

For the options file: in embedded mode the engine's settings move into
monokulo's options file under an `[engine.*]` set of tables, and
`engine.toml` is only used by the standalone engine. Settings that only
make sense for a standalone server (its listen address, its token, its own
logging) don't apply when embedded, and the registry says so.

## Why

- **One thing to install and run.** The design's own goal is "one config
  file, one binary" (`docs/DESIGN.md` §2). Today a deployment runs two
  processes with a shared secret between them. On the Flint 2 that means
  two procd instances, two binaries (17.7 + 14.5 MB), a shared token, two
  options files and two log stores.
- **No HTTP hop.** Every page that shows orders makes one or more calls to
  the engine: a TCP connection on loopback, request and response headers,
  and a trip through the HTTP stack each way. In-process those become
  function calls. The JSON stays, for now (see "Transport").
- **Smaller.** The two binaries share most of their dependencies (tokio,
  axum, hyper, rustls/aws-lc, SQLite, monero). One binary should come out
  around 22 to 25 MB stripped, against 32 MB for the pair today. That is an
  estimate, to measure in phase 1.
- **Less to get wrong.** No engine token to generate, store and keep equal
  in two places, and no engine listener that could be bound to the wrong
  address.

What is lost, and stays available through `remote`:

- **Process isolation.** Today a compromise of monokulo, the public-facing
  process, doesn't by itself read the engine's memory, where the view keys
  live once unsealed. Embedded, they share an address space. For a single
  owner on a router that is a fair trade. For a hosted, hardened deployment
  it isn't, which is why `remote` stays and why the key custody backends
  (`socket`, out of process) work in both modes.
- **Separate crash domains.** Panics are already caught and the task
  restarted (`shared::supervise`), but an abort or running out of memory now
  takes down both. With RandomX's 256 MiB cache in the engine, that is worth
  knowing on a 1 GB router.

## What the code looks like today

- `crates/engine/src/main.rs` (393 lines) wires everything up by hand:
  options file, telemetry, runtime, token, store, settings, key custody
  router, tenant registration, database worker and read pool, `AppState`,
  the webhook and network loops under `supervise`, then `axum::serve` on
  `server.bind`, and graceful shutdown. Boot failures call
  `std::process::exit`.
- `crates/engine-test-support/src/lib.rs` repeats much of the same wiring
  to stand up a test engine on `127.0.0.1:0`.
- `crates/monokulo/src/engine_client.rs` is a reqwest client: about 25 typed
  methods (`create_tenant`, `list_orders_page`, `create_order`,
  `get_status`, the log queries...) that build a request, send it with the
  token header and the trace context, and parse JSON. A few places step
  outside the typed methods:
  - `request()` returns a raw `reqwest_middleware::RequestBuilder`, used 4
    times: the admin settings page's get, save and reload of the engine's
    settings, and one `DELETE` on the status page;
  - `open_order_events()` returns a `reqwest::Response`, whose body
    `crate::live` reads as a server-sent event stream.
- The wire types are copied field for field on each side (`TenantView`,
  `OrderView`...), on purpose, since "the two crates talk over HTTP as
  separate services, not by linking against each other".
- `engine` has an optional dependency on `monokulo` (the `e2e` feature, for
  the e2e harness binary and stagenet tests).
- `store_connections.engine_url` records `EngineClient::base_url()` when a
  store connects, but nothing reads it back.

## Design

### 1. `engine::Engine`: the engine as a library

A new module, `engine::run`, holds what `main.rs` does today, minus the
listener, the process exits and the signal handling:

```rust
/// Everything the engine needs to start, decided by its host: the
/// standalone binary (from its command line and engine.toml) or monokulo
/// (from its own options file).
pub struct EngineConfig {
    /// Where the engine's settings are read from and saved to.
    pub options: live_settings::OptionsSource,
    /// Command-line and environment values, which win over the file.
    pub env: live_settings::Env,
    pub database_path: PathBuf,
    /// The token every API call must carry. Embedded, monokulo makes a
    /// random one at start and never stores it.
    pub token: shared::auth::EngineToken,
    /// Embedded or standalone: which settings apply (see "Settings").
    pub host: Host,
}

pub struct Engine { /* AppState, loop handles, shutdown token, runtime */ }

impl Engine {
    /// Opens storage, loads settings, registers tenants' wallets, starts
    /// the scan and webhook loops. Errors are returned, never exit().
    pub async fn start(config: EngineConfig) -> Result<Engine, StartError>;

    /// The admin API, exactly as the standalone binary serves it.
    pub fn router(&self) -> axum::Router;

    /// Stops the loops and waits for work in flight, up to `grace`.
    pub async fn shutdown(self, grace: Duration);
}
```

Then:

- **`monokulo-engine`'s `main.rs`** shrinks to: parse the command line, read
  `engine.toml`, start telemetry, build the runtime, `Engine::start`, serve
  `engine.router()` on `server.bind`, and shut down on a signal. Its
  behaviour doesn't change.
- **`engine-test-support`** calls `Engine::start` too, instead of repeating
  the wiring. Test engines then boot the way production engines do.
- **The loops stop when the `Engine` is dropped or shut down**, through a
  cancellation token passed to `supervise`, instead of only ending with the
  process. Embedded, monokulo has to be able to stop the engine without
  exiting.
- **Process-wide statics get an owner.** A library must not assume it owns
  the process. The audit found:
  - `key_custody::plain::SCAN_SLOTS`, sized from `available_parallelism()`
    the first time a scan runs. It becomes a field sized from the engine's
    own thread count (see "Threads and CPUs").
  - `shared::password::SLOTS`, `shared::supervise::RESTARTS`,
    `shared::log::SEEN` and `shared::resources::SAMPLER` are already safe to
    share between two services in one process. They should still be
    checked for any assumption that one process is one service. For
    example, `RESTARTS` is keyed by loop name, and the names don't collide.

### 2. One client, two transports

`EngineClient` keeps its typed methods and gains a transport underneath:

```rust
enum Transport {
    /// Today's: reqwest, with the HTTP cache and the token header.
    Remote { base_url: String, http: ClientWithMiddleware },
    /// The engine's router, called in-process.
    Embedded { router: axum::Router },
}

impl EngineTarget {
    /// The one place a request reaches the engine, either way.
    async fn send(&self, request: http::Request<Body>) -> Result<http::Response<Body>, EngineClientError>;
}
```

- **Embedded `send`** sets the token header and the trace context (the same
  W3C `traceparent` the reqwest middleware sets today), then calls
  `router.clone().oneshot(request)` under the same `ENGINE_CALL_TIMEOUT`. The
  engine's middleware runs as if the request had come over TCP: token
  check, rate limit, body limit and request span. One difference is that
  there is no peer `SocketAddr`. The router is called with a fixed loopback
  `ConnectInfo`, so the handlers that read it see `127.0.0.1`.
- **No reqwest types leave `engine_client.rs`:**
  - the 4 raw `request()` callers get typed methods
    (`get_engine_settings`, `save_engine_settings`,
    `reload_engine_options`, and the status page's delete);
  - `open_order_events()` returns a stream of bytes (`BoxStream<Bytes>`)
    instead of a `reqwest::Response`, so `crate::live` parses the same
    event stream from either transport.
- **The HTTP cache** (`shared::http_cache`) only applies to `Remote`. It
  caches nothing today, because no engine response is marked cacheable.
- **`base_url()`** becomes `describe()`, returning `embedded` or the URL,
  for logs and the status page. `store_connections.engine_url` is written
  and never read, so this proposal removes the column rather than filling
  it with `embedded`.

**Why the router in-process, and not a typed Rust API?** The alternatives:

| | In-process router (proposed) | A typed `EngineApi` trait, two implementations | A Unix socket |
|---|---|---|---|
| Engine code that runs | the same handlers either way | new direct implementation beside the HTTP handlers | the same handlers |
| Auth, validation, rate limits | one place | two places, which can drift | one place |
| Size of the change | the transport, 4 call sites, the event stream | extract a service layer from every handler, share the wire types in a new crate, move every call site | small |
| One process, one binary | yes | yes | no |
| Cost per call | JSON in and out, no network | none | a socket hop |

The typed trait is the "real library" answer, and it does save the JSON.
But the handlers hold the logic today, so it means splitting every handler
into a service function and an HTTP adapter, plus a second implementation
of the client that tests have to cover twice. JSON for one order is
microseconds, against the milliseconds of the SQLite work behind it.
Proposal: ship the router transport, measure, and extract a typed path
only for a call that shows up in a profile. If that happens, it becomes a
third arm of `Transport` for that call alone.

### 3. Choosing the mode

Monokulo's registry gains one setting, and two existing ones become
remote-only:

| Key | Values | Applies |
|---|---|---|
| `engine.mode` | `embedded` (default) or `remote` | at restart |
| `engine.url` | the remote engine's URL; an error if set in `embedded` mode | at restart |
| `engine.token` (secret, `MONOKULO_ENGINE_TOKEN`) | required in `remote`, refused in `embedded` | at restart |

The checks follow the existing rule that invalid values stop the process.
`remote` without `engine.url` or the token stops at start. So does
`embedded` with `engine.url` set, with a message saying the setting does
nothing in embedded mode rather than leaving it to be ignored.

Monokulo gets a cargo feature, `embedded-engine`, on by default. It is
what pulls in the `engine` crate. A build without it is remote-only and
refuses `engine.mode = "embedded"` at start.

For that, the `engine -> monokulo` optional dependency (the `e2e` feature)
has to go, or Cargo sees a cycle. The e2e harness binary and the two
stagenet tests move into their own crate, `e2e-harness`, which depends on
both. Embedded, the harness also gets simpler: it no longer needs to start
two servers.

### 4. Settings and `engine.toml`

This is the part that needs the most care, because the settings rules are
strict: one registry per service, the options file for configuration, the
database for runtime switches, the environment for secrets, the command
line and environment locking a field, and invalid values stopping the
process.

**Remote mode doesn't change.** The engine reads `engine.toml` (or
`--options`), the admin page edits it through `/api/v1/admin/settings`, and
"Reload options file" reloads it.

**Embedded: one file.** The engine's settings live in monokulo's options
file, under tables prefixed `engine.`:

```toml
# monokulo.toml
[server]
bind = "192.168.1.1:8081"

[payment]                      # monokulo's own [payment]-prefixed keys, if any

[engine.payment]
confirmations_required = 10
mempool_poll_interval_ms = 2000

[engine.monero_node]
mainnet = '{"url": "http://192.168.1.10:18081"}'

[engine.proof_of_work]
mainnet = true
```

How it works:

- **`live_settings::OptionsSource`** is a handle to one options file,
  shared by everything that reads or writes it, with a scope: the whole
  file, or one table and everything under it. The engine gets
  `source.scoped("engine")`. It reads and writes `engine.payment.*` as
  `payment.*`, so the engine's registry, keys and validation don't change.
  Monokulo's registry gets the rest of the file, with the `engine` table
  set aside for the engine.
- **One writer.** Both services save into the same file, so the source
  holds one lock around each read-modify-write (it already edits in place
  with `toml_edit`, keeping comments and layout). Two handles to the same
  path would race. One shared handle can't.
- **Unknown keys still stop the process.** A typo under `[engine.payment]`
  is reported by the engine's registry, by its full key. In `remote` mode,
  an `[engine.*]` table in monokulo's file is itself an error: it would do
  nothing, and silently ignoring it is the one thing the settings rules
  forbid.
- **"Reload options file"** reloads the one file. The admin page's button
  already reloads monokulo and calls the engine's reload. In embedded mode
  both read the same file, and nothing else changes.
- **The engine's database stays its own file** (`engine.db`, default:
  beside `monokulo.db`). The schemas, migrations and write workers are
  separate today, and merging them gains nothing here. Its runtime
  switches stay in its own `settings` table.
- **The command line and environment.** Engine settings keep their
  environment variable names (`ENGINE_*`) in both modes, so documentation
  and deployments don't fork. On monokulo's command line they get an
  `--engine-` prefix (`--engine-payment-confirmations-required`), because
  `--server-bind` and `--database-path` already mean monokulo's. A field
  given either way is shown locked on the admin page, as today.

**Settings that don't apply when embedded.** Each engine setting
declaration gains `hosts: Standalone | Both` (default `Both`):

| Engine setting | Embedded |
|---|---|
| `server.bind` | n/a: no listener |
| `server.token` | n/a: monokulo makes a one-time token in memory |
| `server.max_body_bytes`, `server.rate_limit_per_token_per_min` | n/a: only monokulo calls it |
| `logging.*` (level, format, retention, OTLP) | n/a: one process, one logger, monokulo's settings |
| `server.worker_threads` | applies: sizes the engine's own runtime (below) |
| `database.path`, `database.read_connections` | apply |
| everything else (nodes, payments, key custody, proof of work...) | applies, unchanged |

A `Standalone` setting in embedded mode is hidden on the admin page, and
an error if it appears in the file or on the command line, with a message
saying it does nothing when embedded.

**Why not keep `engine.toml` beside `monokulo.toml`?** It is the smaller
change: the engine would read `engine.toml` exactly as now, from a path
monokulo names. But it keeps two files for one program, and the file's
`server.*` and `logging.*` settings would sit there doing nothing. One file
is what "one config file, one binary" asks for. This is a project that
isn't in production yet, so there is nothing to migrate: `monokulo --init`
writes the `[engine.*]` tables, commented, like the rest.

### 5. Threads and CPUs

Recommendation: the engine gets **its own Tokio runtime**, started by
`Engine::start` on threads named `engine-*`, with `server.worker_threads`
workers and its own blocking pool for scans. It does not share monokulo's
runtime.

- **Isolation where it matters.** A catch-up scan, or RandomX hashing, can't
  starve monokulo's request handling. This was the real benefit of two
  processes, and it can be kept.
- **The CPU limits move into settings.** The engine gets two new settings,
  `engine.cpus` (a CPU list) and `engine.nice`. Each engine thread pins and
  renices itself as it starts, through `on_thread_start`, using
  `sched_setaffinity` and `setpriority` on its own thread id; on Linux both
  are per thread. `SCAN_SLOTS` is then sized from `engine.cpus` (or the
  thread count) explicitly, rather than from `available_parallelism()`. That
  function reads the calling thread's affinity, which would be fragile once
  the engine shares a process.
  - On the Flint 2 this replaces the `taskset` wrapper and the procd `nice`
    parameter with two lines in the options file.
  - In standalone mode the same settings work too, so the wrapper goes
    there as well.

The cheaper alternative is to spawn the engine's tasks on monokulo's
runtime. It has fewer threads, but a catch-up can slow every page, and
pinning would have to be per process again.

### 6. Logging, resources and shutdown

- **Logging.** One process has one `tracing` subscriber. `telemetry::init`
  takes the service name today. Embedded, events from the engine's crates
  (target `engine::*`, plus anything under the engine's root span) are
  tagged `service = "engine"`, and the rest `monokulo`. Each service keeps
  its own log store (`engine.logs.db`, `monokulo.logs.db`), so the Logs page
  and the engine's log API don't change. They just answer in-process.
  Monokulo's `logging.*` settings govern both.
- **Resources** (the admin page's CPU and memory charts, engine stacked
  under monokulo). With one process, memory can't be split between
  services, so the chart shows one memory series labelled
  "monokulo (with engine)". CPU can still be split: per thread from
  `/proc/self/task/*/stat`, using the `engine-*` thread names. In remote
  mode the charts stay as they are.
- **Shutdown.** On SIGTERM monokulo stops accepting requests, finishes those
  in flight, calls `engine.shutdown(grace)`, then flushes telemetry once.
  Every engine step is already safe to interrupt, so the grace period is a
  courtesy, not a requirement.

### 7. Packaging

- **The Flint 2 package** ships one binary, `monokulo`. It has one procd
  instance and no engine token: `/etc/monokulo/secrets` holds only the
  encryption key. There is no `taskset` wrapper, because `engine.cpus` and
  `engine.nice` are set in the options file. The LuCI page's "Engine"
  status line reads monokulo's `/status/summary` instead of a second procd
  instance.
- **Docker** keeps building both binaries. `compose.yaml` becomes one
  service by default, and a commented second form shows `remote` with a
  separate engine container.
- **The SEV-SNP deployment** stays on `remote`, with the engine and key
  custody server on the hardened host.

## Testing

- **One contract, two transports.** Monokulo's integration tests that use a
  real engine (`engine-test-support`) run against both transports: the test
  engine hands out `EngineClient::embedded(&engine)` or an HTTP client to
  the same engine. Any behaviour difference between the modes fails a test.
  This is the main safety net for the "same handlers either way" claim.
- **Settings.** Real-file tests for the scoped options source:
  - saving from both services leaves both sets of keys intact;
  - comments and layout survive a save;
  - a typo under `[engine.*]` stops the process, naming the key;
  - `[engine.*]` in remote mode stops the process;
  - a `Standalone` setting set while embedded stops the process.
- **Lifecycle.** Start, shut down and start again in one process, with the
  same database. Also check that no loop survives `shutdown`.
- **Threads.** On Linux, check every `engine-*` thread's affinity and nice
  value after start, and that the scan slots equal the CPU count given.
- **e2e.** The stagenet harness switches to embedded. One remote run stays
  in CI so `remote` keeps working.

## Phases

| Phase | What | Size |
|---|---|---|
| 1 | `engine::Engine` (start, router, shutdown, cancellation into `supervise`); `main.rs` and `engine-test-support` on top of it; statics audited. No change in behaviour. | medium |
| 2 | Transport in `EngineClient`; typed methods for the 4 raw calls; byte-stream events; tests over both transports; e2e harness moved to its own crate; `embedded-engine` feature. | medium |
| 3 | `engine.mode`; `OptionsSource` with scopes and one writer; `hosts:` on engine settings; engine logging tagged in one subscriber; `monokulo --init` writes `[engine.*]`. | medium |
| 4 | Engine runtime with `engine.cpus` and `engine.nice`; resources chart for one process. | small |
| 5 | Packaging: Flint 2 package with one binary, compose with one service, docs. Remove `store_connections.engine_url`. | small |

Each phase leaves both binaries working. Phases 1 and 2 are worth doing even
if embedded mode were never shipped: they remove the duplicated boot
wiring and make the client's surface explicit.

## Decisions

Made by the project owner on 2026-10-03:

1. **Embedded is the default everywhere**, Docker included. `remote` is
   the opt-in for a split deployment, such as the SEV-SNP host.
2. **One options file.** In embedded mode the engine's settings live in
   `monokulo.toml` under `[engine.*]` tables, as in section 4. `engine.toml`
   is only read by the standalone engine.
3. **The engine gets its own thread pool**, as in section 5: its own Tokio
   runtime on `engine-*` threads, pinned and reniced by `engine.cpus` and
   `engine.nice`.
