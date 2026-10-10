# Proposal: the engine as a library inside monokulo

Status: proposal accepted (see "Decisions" at the end). Phases 1 to 5 are
built (see the "as built" sections at the end): monokulo runs the engine
inside it by default. Written 2026-10-03 against `1cc17f2` (origin/main).

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

  *As built (decided by the owner after phase 5):* the suite is not run
  twice. Tests reach the engine in-process, as monokulo does by default,
  through an engine started with `TestEngineConfig::embedded()` where its
  settings matter. HTTP is covered by a handful of tests, each named for it
  (`…_over_http…`):
  - `every_call_has_the_same_outcome_over_http_and_in_process`, the
    contract, which runs every client method over each transport;
  - what only a remote engine has: the HTTP response cache, its
    standalone-only settings (`server.bind`, `logging.*`) and `engine.url`
    on the admin page, and an engine that can't be reached.

  Placeholder clients for tests that never call the engine
  (`EngineClient::for_tests("http://127.0.0.1:1")`) aren't HTTP tests and
  keep their names. The stagenet e2e tests and the Playwright real stack
  run a separate engine process on purpose.
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

## Phase 1, as built

`engine::run` (`crates/engine/src/run.rs`) holds the engine as a library:

- `Engine::start(EngineConfig)` takes the options file path, the
  command-line/environment values and the database path. It opens
  storage, loads the settings, registers every store's wallet, and starts
  the network loop manager.
- `Engine::router()` returns the admin API; `Engine::bind_address()` returns
  `server.bind`, for the standalone binary.
- `Engine::shutdown(grace)` stops the loops and reports `Stopped::Cleanly`
  or `Stopped::TimedOut`.
- Start-up failures come back as a `StartError`: no token, a database that
  can't be opened, settings that don't load, the database worker or read
  pool. Nothing in the library ends the process.

`main.rs` is now the host: it reads the command line and options file,
starts logging, the runtime and resource sampling, calls `Engine::start`,
serves the router on `server.bind`, and on a signal finishes requests in
flight, shuts the engine down and flushes the logs. Its behaviour is the
same as before.

`shared::supervise::supervise_until` now returns its task, which ends only
once the stopped loop has been dropped (it awaits the aborted task). The
engine's two top-level loops run under it with the engine's stop signal.
The per-network loops stop with their manager, as before, because dropping
the manager drops their stop senders.

Tests (`run::tests`, and two in `shared::supervise`) start real engines from
files in a temporary directory and drive them through the router, as
monokulo will when embedded:
- an engine starts, serves `/status`, and shuts down cleanly;
- the router still refuses a request without the engine token;
- no token, or a database path that can't be opened, is a `StartError`;
- a setting saved through the first engine is in the options file and the
  database the second one starts from;
- a stopped supervisor ends only after its loop is dropped, including when
  its stop sender is dropped.

### Phase 1 decisions

Each line gives what was decided, the alternatives, and why.

- **`engine-test-support` keeps its own wiring.** The proposal said it would
  call `Engine::start`. Reading it, it shares almost nothing with production
  start-up beyond building `AppState`: it deliberately uses an in-memory
  store, fixed fake daemons, a fixed rate limit and its own short-interval
  tick loops instead of the production loops. Moving it onto
  `Engine::start` would change the engine every monokulo test runs against.
  It moves in phase 2 instead, where tests have to run over both transports
  anyway and can use a real `Engine` built from a temporary directory, as
  `run::tests` does. The duplication this proposal described was overstated.
- **Resource sampling stays with the host.** `shared::resources::start_sampling`
  samples the process, and a second call starts a second sampler. An
  embedded engine shares monokulo's process, so the host starts it, not
  `Engine::start`.
- **`Engine::shutdown` waits for the loops; dropping an engine doesn't.**
  An explicit shutdown with a grace period is what both hosts need. A
  `Drop` that stopped the loops would make a host that forgets to call
  `shutdown` behave differently from one that does.
- **Statics audit:**
  - `key_custody::plain::SCAN_SLOTS` is one semaphore per process, sized by
    `available_parallelism()`. Two engines in one process (only in tests)
    share it, and production has one. It moves into the engine in phase 4,
    where its size comes from `engine.cpus`.
  - `shared::password::SLOTS`, `shared::supervise::RESTARTS` and
    `shared::log::SEEN` are safe to share between two services in one
    process. Loop names don't collide. The throttled-log keys didn't collide
    either (monokulo's only key is `client-logs-dropped:`), but nothing
    namespaced them. Fixed with phase 2: `shared::throttled!` now keeps
    each key under the crate that logs it (`engine/tick-failed:Mainnet`),
    from the call site's `module_path!()`.
  - `shared::resources`' `SAMPLER`, `MACHINE` and `HOST` describe the
    process and the machine, so sharing them is right.

## Phase 2, as built

**The client** (`crates/monokulo/src/engine_client.rs`). `EngineClient` has
two transports:
- `Transport::Remote`: reqwest through `shared::http_cache`, as before;
- `Transport::Embedded`: the engine's router, called with
  `tower::ServiceExt::oneshot`.

How the two share one path:
- Every method builds one `Call` (method, path and query, the store's
  secret, a JSON body) and sends it through `EngineTarget::send`, or
  `EngineTarget::stream` for the order-event stream.
- Embedded, the call carries the same headers the remote client sends (the
  engine token, the store's bearer secret, `traceparent`) and a loopback
  `ConnectInfo`. It gets the same 35 s limit, applied with
  `tokio::time::timeout`.
- `EngineClient::embedded(router, token)` makes an embedded client, and
  `EngineClient::embedded_for_tests(router)` its test twin.

The rest of the client changed with it:
- `request()` and its four raw callers are gone. The admin settings page and
  the status page use `get_settings`, `save_settings`, `reload_options` and
  `take_new_anchor`. These return an `EngineReply` (status and body), which
  the pages read as before.
- `open_order_events` returns an `EventStream` of bytes. `crate::live` reads
  it the same way from either transport.
- `base_url()` is now `location()`: the URL, or `embedded`.
- `EngineClientError` lost `InvalidUrl` (nothing produced it any more) and
  gained:
  - `Unreadable`, for a success whose body doesn't parse;
  - `Embedded`, for the in-process engine not answering in time;
  - `NotAdminRoute`.

**The e2e harness moved to its own crate** (`crates/e2e-harness`): the
`e2e-harness` binary and the `e2e_stagenet` and `e2e_dashboard_stagenet`
tests. Run them with `cargo build -p e2e-harness --features e2e --bin
e2e-harness` and `cargo test -p e2e-harness --features e2e --test ...`. The
POS Playwright suite and the docs use the new commands.

**Monokulo depends on the engine** behind its `embedded-engine` feature, on by
default. `cargo check -p monokulo --no-default-features` builds a
remote-only monokulo.

**`engine-test-support`'s `TestEngineHandle::router()`** hands out the same
engine's router, so one test engine can be reached either way.

Tests:
- **Contract.** `engine_client::contract_tests` runs every client method
  against a fresh real engine over each transport: tenant, confirmations,
  orders (idempotent creation, list, page, by ids, detail, unknown and
  malformed ids, refund address), webhooks, payment lookup, status, the
  settings get, save and reload, a new anchor, the log API, the event
  stream, a wrong engine token, and a deleted tenant. It requires the two
  transcripts to be identical, and requires the outcomes the API promises.
  Removing the engine token from the embedded transport fails both contract
  tests, which shows they can catch a difference.
- **Live updates.** A change to a watched order arrives over the embedded
  event stream.
- **Route guard.** A call to a non-admin route is refused before anything
  is sent.

### Phase 2 decisions

- **The engine-wide private-route check moved from a source scan to the
  code.** The old test read `engine_client.rs` for `format!("{}/...")` URLs,
  which no longer exist. `EngineTarget::allowed` now refuses any path that
  isn't `/api/v1/admin/...` or `/status`, at the one place every call
  passes, for both transports, and the test calls it. The check is wired in,
  not just tested.
- **Monokulo's other tests stay on the remote transport for now.** The
  contract tests cover every client method over both transports. Switching
  the whole suite to embedded belongs with phase 3, when embedded becomes
  what `main.rs` runs by default. Then the suite will test what production
  runs.
- **`EngineClient::embedded` has no production caller until phase 3.** It is
  this phase's deliverable. Phase 3 wires it into `main.rs` with
  `engine.mode`, `[engine.*]` in `monokulo.toml` and its own runtime.
- **`set_refund_address` now checks its order id** with `path_id`, like
  every other id that goes into an engine path. Before, it went into the
  path unchecked.
- **The e2e crate's dependencies are all optional, behind `e2e`.** If they
  weren't, `cargo build --workspace` would unify `engine/test-support` and
  `monokulo/test-support` into an ordinary build, through Cargo's feature
  unification.

## Phase 3, as built

**monokulo runs the engine inside it by default.**

- **`engine.mode`.** It is `embedded` (the default) or `remote`.
  `settings::engine_mode` checks the mode against everything that only the
  other mode uses, so no such value is set and silently ignored:
  - `engine.url` and `MONOKULO_ENGINE_TOKEN` stop an embedded monokulo;
  - a remote engine needs its token, which is checked as the engine checks
    it.

  `engine.token` is no longer a required setting.
- **The options file.** Both services use one file, read through
  `live_settings::OptionsFile` handles that share it:
  - monokulo's handle `leaving("engine")` skips the `[engine.*]` tables;
  - the engine's handle `scoped("engine")` reads and writes them under its
    own key names;
  - with a remote engine, monokulo reads the file `with_hint`, so
    `[engine.*]` tables are refused with the reason.

  Both handles share what was last read and written, so neither refuses the
  other's save as "changed since it was loaded". `main` reads the file
  twice, because the mode itself is in it: once leniently to learn the
  mode, then strictly for a remote engine.
- **Command line and `--init`.** Every embedded engine setting is a
  `--engine-…` option (`cli::with_nested_settings`), and `monokulo --init`
  writes them under `[engine.*]` (`render_init_nested`).
- **Standalone-only settings.** `engine::engine_settings::STANDALONE_ONLY`
  is `server.bind`, `server.token` and `logging.*`. An embedded engine
  refuses any of them, whether given in the file, on the command line or in
  the environment. It refuses them before anything is opened: `prepare` in
  monokulo's `main` checks, and so does `Engine::start`. Its settings API
  leaves them out, refuses saving them, and names monokulo's `--engine-…`
  options in its "set with" hints.
- **Starting and stopping.** `main` is synchronous now. It reads the file,
  starts logging and checks the mode. It then builds the engine's runtime
  from `server.worker_threads`, with threads named `engine-worker`, and
  monokulo's runtime. Inside `run`:
  1. monokulo opens its database and the process's one log store;
  2. it starts the engine on the engine's runtime, with a token made for
     this run (`shared::auth::generate_engine_token`) and `engine.db`
     beside `monokulo.db`, unless the engine's `database.path` says
     otherwise;
  3. the embedded `EngineClient` answers each call on the engine's runtime.

  On SIGTERM, requests in flight finish, then the engine's loops stop, then
  the logs are flushed.
- **Logs.** `telemetry::Telemetry::host("engine", &["engine"], "engine-")`
  names a line `engine` when its target is in the engine's crate, or when
  it is logged on an `engine-` thread. Both services' lines go to
  `monokulo.logs.db`, and the Logs page reads the engine's lines from there
  (`EngineSource::Local`) instead of through the engine's log API.
- **The admin page.** It locks `engine.url` while the engine is embedded,
  and refuses a form that sends it anyway. Saved, it would stop monokulo at
  its next start.
- **Deployment.** `compose.yaml` runs one service, and shows the
  two-container setup commented out. The README and Dockerfile describe the
  embedded default.

Tests:
- **The real binary** (`crates/monokulo/tests/options_file.rs`):
  - it starts with only the encryption key, the engine's database beside
    monokulo's;
  - `--init` lists `[engine.payment]` and no standalone-only table;
  - each wrong combination of mode and settings stops it before anything
    opens.
- **live-settings:** seven tests for the shared file, nested options and
  nested `--init`.
- **The engine:** an embedded engine starts on its host's token, keeps its
  settings in `[engine.*]`, and refuses standalone-only settings, named by
  where each was given.
- **telemetry:** lines are named by crate and by thread.
- **monokulo:**
  - `engine_mode` in every combination;
  - the `--engine-…` options;
  - the admin page's lock on `engine.url`;
  - the contract test, which now also runs on a runtime of the engine's
    own, as `main` does.
- **monokulo's real-engine tests** now reach the engine in-process (24
  places).

A live run of the real binary showed:
- one process, with 3 `engine-worker` threads;
- `/status` answered through the engine in-process;
- engine lines tagged `engine` in the log and in `monokulo.logs.db`;
- SIGTERM ending with "the engine stopped" and exit 0.

### Phase 3 decisions

- **The engine's own runtime came in phase 3, not phase 4.** Without it,
  `server.worker_threads` would do nothing while embedded, which is exactly
  what the settings rules forbid. Phase 4 keeps `engine.cpus`,
  `engine.nice`, sizing the scan slots from them, and splitting the
  resources chart by thread.
- **Fewer standalone-only settings than the proposal listed.**
  `server.rate_limit_per_token_per_min` and `server.max_body_bytes` still
  act on in-process calls (the router's middleware runs either way), so
  they stay. Only `server.bind`, `server.token` and `logging.*` don't apply
  when embedded.
- **The engine's registry still requires `server.token`.** When embedded,
  the token monokulo made is handed to it as that setting
  (`Env::with_var`), after the standalone-settings check has looked at the
  real environment. That keeps one registry declaration for both hosts.
- **One log store, not two.** The proposal kept an `engine.logs.db` for the
  embedded engine. A process has one `telemetry` store, and every line
  already carries its service, so both services share `monokulo.logs.db`
  and the Logs page reads it once. Spans (traces) are still named after the
  process: only events are named per service.
- **Who still runs the engine as a separate process.** `scripts/dev-run.sh`
  starts monokulo with `--engine-mode remote`, and the Playwright real
  stack writes `mode = "remote"`. Both run the engine as a process of its
  own on purpose (the real-stack suite kills and restarts it). The
  admin-settings tests, and the monokulo inside the WooCommerce mock, also
  stay on the remote transport, so remote mode stays exercised end to end
  beyond the contract test.
- **`store_connections.engine_url`** records `embedded` for stores
  connected to an embedded engine. Nothing reads it; it goes in phase 5.
- **The OpenWrt package (PR #36)** still runs two binaries. It moves to the
  single binary in phase 5, as planned.


## Phase 4, as built

**The engine's threads run on chosen CPUs, at a chosen niceness.**

- **Two settings, `server.cpus` and `server.nice`.** `server.cpus` is a CPU
  list as `taskset` takes it (`2,3`, `1-3`), empty for all CPUs.
  `server.nice` is 0 to 19. Embedded they are `engine.server.cpus` and
  `engine.server.nice` (`--engine-server-cpus`, `--engine-server-nice`);
  standalone, `server.cpus` and `server.nice` in `engine.toml`. They are not
  standalone-only: they mean the same thing in either host.
- **`engine::threads::ThreadPlan`** builds the engine's runtime for both
  binaries: `server.worker_threads` workers, every thread (workers and the
  blocking pool where scans run) named `engine-worker`. Each thread pins and
  renices itself as it starts (`on_thread_start`): `sched_setaffinity` and
  `setpriority` on its own thread id, which on Linux act per thread. Inside
  monokulo only the engine's threads are touched; monokulo's own keep every
  CPU and normal priority.
- **A plan the machine can't take stops the engine at start.**
  `ThreadPlan::check` applies it on a throwaway thread first, so a CPU that
  doesn't exist (or a nice value the user may not set) is reported before
  any database opens, and monokulo exits 1 with the reason. Elsewhere than
  Linux, asking for either setting is refused the same way.
- **Scan slots are sized by the engine** (`key_custody::size_scan_slots`,
  called by `Engine::start`): one per CPU in `server.cpus`, or, without a
  list, one per CPU the process may use. They used to come from
  `available_parallelism()` on whichever thread first scanned, which
  inside monokulo would have depended on that thread's affinity.
- **Every engine thread is named `engine-…`**: `engine-worker`, `engine-db`,
  `engine-db-read-N` and `engine-randomx-<network>` (they were
  `scanner-db…` and `randomx-…`). `engine::threads::THREAD_PREFIX` is what
  logging (`Telemetry::host`) and the resources sampler use to tell the
  engine's threads apart.
- **The Resources panel in one process.** `shared::resources::Sampler::
  host_threads(prefix)` adds up the CPU of the matching threads from
  `/proc/self/task/*/stat`. `ResourceReport::hosted()` and
  `without_hosted()` split one process's report in two. With the engine
  embedded the CPU chart stacks engine over monokulo as before, but memory
  (which the two share) is drawn once, as "monokulo (with engine)". With a
  remote engine the panel is unchanged.

Tests: `ThreadPlan` parsing and checking, and a runtime whose threads are
read back from `/proc/thread-self` (affinity and nice); scan slots sized
from the plan; the per-thread CPU split in `shared::resources`; the
Resources view in one-process form; the new settings on the admin page's
tab list and save coverage.

A live run of the real binary with `--engine-server-cpus 2-3
--engine-server-nice 10` showed every `engine-worker`, `engine-db` and
`engine-db-read` thread on CPUs 2-3 at nice 10, monokulo's threads on all
CPUs at nice 0, and `scan_slots=2` in the log. `--engine-server-cpus 99`
exited 1 before any database was opened.

### Phase 4 decisions

- **Per-thread, not per-process.** `taskset` or procd's `nice` on the whole
  process would also have slowed monokulo's pages. That was the reason for
  the engine's own runtime (decision 3).
- **The settings are named `server.cpus` and `server.nice`,** beside
  `server.worker_threads`, rather than the proposal's top-level
  `engine.cpus` and `engine.nice`: embedded, every engine setting is already
  under `[engine.*]`, so `engine.server.cpus` reads the same and the
  standalone engine uses the same name.
- **A bad value stops the process; it isn't ignored.** This follows the
  rule that every setting is applied or refused. The OpenWrt init script,
  which takes the list from the user in LuCI, checks it with `taskset`
  first and drops a list the router can't honour, with a log line, so a
  typo there doesn't keep the service down.
- **Linux only.** macOS has no per-thread affinity of this kind; there the
  settings must be left empty.

## Phase 5, as built

**The Flint 2 package ships one binary, and the dead store column is gone.**

- **`store_connections.moneropay_endpoint` dropped** (migration 0029). Each
  store recorded the engine URL it was connected through (`embedded` since
  phase 3), but nothing read it. `create_store_connection` lost the
  parameter, and `EngineClient::location()`, left with only test callers,
  went too.
- **The OpenWrt package** (stacked on this branch, replacing PR #36's
  two-binary form):
  - one binary, `monokulo`, built with the engine in it (and its `zmq`
    feature); the static-executable check still guards it;
  - one procd instance; no engine port, no engine token. The secrets file
    holds only `MONOKULO_ENCRYPTION_KEY`;
  - UCI's `engine_cpus` and `engine_nice` are passed as
    `--engine-server-cpus` and `--engine-server-nice`. The `taskset` wrapper
    and procd's `nice` are gone, so monokulo's pages keep normal priority on
    every core;
  - the engine's database is `engine.db` in the data folder, given
    explicitly (`--engine-database-path`);
  - LuCI shows one status line ("the engine runs inside it") and no engine
    port; the landing page and README describe one program.
- **compose.yaml** already ran one service from phase 3.

The package is 10.5 MB, against about 15 MB with two binaries. In an
OpenWrt 25.12.5 container (x86-64, with a stand-in binary, since the real
one is aarch64), it installed from the signed repository and procd ran one
instance as `monokulo` with the engine options above. A CPU list the
machine lacks, damaged secrets, a foreign or in-memory data folder and
`enabled 0` each behaved as in PR #36, and the LuCI page showed the one
status line. The real binary (x86-64) took the same arguments, an empty CPU
list included: the engine's threads ran at nice 10 and `engine.db` went
into the data folder.

### Phase 5 decisions

- **A stacked PR, not a force-push to #36.** The OpenWrt work is
  cherry-picked onto this branch and changed there, so #36 stays as
  reviewed; it can be closed in favour of the stacked PR.
- **Secrets files from the two-binary package still work**: the init script
  reads only `MONOKULO_ENCRYPTION_KEY` and ignores a leftover
  `ENGINE_TOKEN` line. Nothing is deployed yet, so no migration was written
  for it.
- **The proposal's LuCI "Engine" line from `/status/summary`** was not
  built. LuCI talks only to procd (the engine is private, and monokulo's
  admin page already shows the engine's state), so the one status line
  says the engine runs inside monokulo.
