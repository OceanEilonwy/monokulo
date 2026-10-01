# Admin settings v2: apply in place, explain every setting, per-store key custody

## Why

On a fresh start, `scripts/dev-run.sh` starts the engine with an empty
database, then saves the stagenet node through `POST /api/v1/admin/settings`.
The admin page shows that node, because it reads the saved value back. But
the engine built its node clients once at boot
(`crates/engine/src/main.rs`, `build_daemon_clients`), so
`AppState.daemons` and `AppState.configured_networks` stay empty until the
next restart. The status page says no nodes are configured, and connecting a
stagenet store is refused (`crates/engine/src/http/admin.rs`,
`configured_networks.contains`).

Node settings are not the only case. Most engine settings, and several
monokulo settings, are read once at boot and passed into loops or clients
by value. Saving them on the admin page persists them without any visible
effect, and nothing on the page says so.

Separately, key custody is one instance-wide choice today
(`key_custody.backend`). It should be a per-store choice, with the admin
page deciding which backends are available and which one new stores get.

## Goals

1. Saving a setting on the admin page, for either the engine or monokulo,
   takes effect straight away in the running process.
2. A setting that must apply only on a restart (`server.worker_threads`
   and `server.bind`) can still be saved from the page. After the save, a warning banner at the top of the page names each
   saved setting that needs a restart.
3. A value that cannot be applied at all (a port already in use, a
   key-custody socket with nothing listening) is refused at save time, with
   nothing persisted.
4. Saves that are accepted but leave something broken are reported, not
   hidden:
   - Monero nodes: when a network used by stores has no reachable node
     after a save, the admin page shows a red banner, and every affected
     merchant sees an error alert on every page except the POS app.
   - `engine.url`: when the new URL doesn't answer, the value is saved
     anyway and the page shows an error banner.
5. Every setting on the admin page says what it is for, what kind of value
   it takes, and gives an example where one helps.
6. Each store chooses its own key custody backend from the ones the admin
   has enabled. Switching a store to another backend means entering its
   keys again. Keys are never moved between backends.
7. The scanning engine keeps detecting payments through key-custody
   sidecar failures, database errors, node failures, and thousands of
   stores, fairly, and always recovers by itself (part 7).

## Decisions recorded

These came from review of the first draft and are settled.

| # | Question | Decision |
|---|---|---|
| D1 | `server.worker_threads` can't change while running | Keep it. Wire it up so it is actually used at boot. Saving it (or any other restart-only setting) shows a warning banner after the POST naming the settings that need a restart. |
| D2 | Clearing (or breaking) a network's nodes while stores use it | Allow it. After the POST, a red banner says "N stores use the <network> network, which no longer has any reachable nodes." With JS, a confirmation dialog appears before submitting a save that clears a network in use. Every affected merchant sees an error alert at the top of every page except the POS app. |
| D3 | Key custody | Chosen per store. The admin page sets which backends are enabled and which is the default. Changing a store's backend means providing its keys again; there is no migration of key material between backends. The engine looks up each tenant's backend whenever it needs key material. |
| D4 | `engine.url` that doesn't answer | Save it anyway. After the POST, show an error banner at the top of the page. |
| D5 | Settings that no longer apply | Remove them. See task 1.5. |
| D6 | Scanning during key custody changes | A store may be skipped for a while, but no payment is ever lost, no other store is held up, and recovery is always automatic. See "Scanning safety" in part 5 and task 5.0. |
| D8 | `server.bind` | Restart-only, like `server.worker_threads`. Moving the engine's listen address live, and rewriting monokulo's `engine.url` to follow it, is too much risk for a setting that rarely changes. See 2.7 and 3.6. |
| D9 | Engine robustness | The engine must tolerate and recover from key-custody sidecar errors, database errors and node failures, and stay fair across thousands of stores. See part 7. |
| D10 | Scan window per store | Scan only indices of orders open or closed within the grace period (by a new `closed_at` time). Late payments to older orders are handled by the merchant with payment lookup, and the merchant-facing text says so. See 7.3. |
| D7 | Reloadable config | One shared, typed library (`crates/live-settings`) used by both processes. See part 1. |

## Current state of every setting

Where each value is read today. "Boot" means it is read once and never
again; those are the ones this work changes.

### Engine (`crates/engine/src/settings.rs`)

| Setting | Read today | After this work |
|---|---|---|
| `monero_node.<network>` | Boot (`build_daemon_clients`) | Live (2.1) |
| `key_custody.backend` | Boot (`build_key_custody`) | Removed, replaced by per-store custody (part 5) |
| `key_custody.socket_path` | Boot | Live, as the socket backend's own setting (5.2) |
| `payment.confirmations_required` | Only `local_admin::bootstrap_wallet` | Live; also the default for API-created tenants (2.9) |
| `payment.order_expiry_minutes` | Only `local_admin::bootstrap_wallet` | Same (2.9) |
| `payment.reorg_check_depth` | Boot, passed into `run_scanner_loop` | Live (2.3) |
| `payment.mempool_poll_interval_ms` | Boot, loop sleep and `scan_poll_interval_secs` | Live (2.3) |
| `payment.expired_order_grace_period_minutes` | Boot, loop and `AppState` | Live (2.3) |
| `payment.scan_chunk_memory_budget_mb` | Per use (`scanner.rs`) | Already live |
| `server.bind` | Boot, `TcpListener::bind` | Read at boot; restart required (D8, 2.7) |
| `server.worker_threads` | Never read | Read at boot; restart required (D1, 2.8) |
| `server.rate_limit_per_token_per_min` | Boot, `RateLimiter::new` | Live (2.5) |
| `server.max_body_bytes` | Boot, `RequestBodyLimitLayer` | Live (2.6) |
| `webhooks.allow_private_urls` | Boot, passed into `run_webhook_delivery_loop` | Live (2.4) |
| `webhooks.delivery_timeout_ms` | Boot, same | Live (2.4) |
| `webhooks.max_attempts` | Boot, same | Live (2.4) |
| (new) `key_custody.enabled_backends` | | Live (5.2) |
| (new) `key_custody.default_backend` | | Live (5.2) |

### Monokulo (`crates/monokulo/src/settings.rs`)

| Setting | Read today | After this work |
|---|---|---|
| `signup.mode` | Per request | Already live |
| `engine.url` | Boot, `EngineClient::with_cache_limit` | Live (3.2) |
| `engine.admin_token` | Per request (admin settings page) | Already live |
| `exchange_rate.coingecko_enabled` | Boot, `ExchangeRateProviders::build` | Live (3.3) |
| `exchange_rate.coingecko_base_url` | Boot, same | Live (3.3) |
| `exchange_rate.cache_seconds` | Boot, same | Live (3.3) |
| `http_cache.max_mb` | Boot, `EngineClient`'s HTTP cache | Live (3.2) |
| `public_url` | Per request | Already live |
| `rate_limit.per_store_key_per_min` | Reloaded on save (`AbuseProtection::reload`) | Already live |
| `abuse.*` except `abuse.onion_listener` | Reloaded on save | Already live |
| `abuse.onion_listener` | Boot, `OnionListener::bind` | Live (3.4) |

`MONOKULO_ENCRYPTION_KEY` stays an environment variable read at boot and is
not an admin setting. The reasons are in `crates/monokulo/src/settings.rs`.
Monokulo's own listen address (`127.0.0.1:8081`, `main.rs`) is hardcoded
and not a setting, so it is out of scope.

An environment variable still overrides a saved value. When one does, the
save is stored but the effective value doesn't change. The page already
says this in general; after this work the save response also lists which
of the submitted settings are overridden by the environment (task 4.5).

## Task tree

Each task gives its **Aim**, how to **Verify** it, and a suggested
**Approach**. Parts are ordered so each builds on the ones before it.
Part 5 (per-store key custody) needs part 1, part 2, and from parts 3 and
4 only the alert plumbing (3.7), the engine-reported metadata (4.2) and
the banner and confirmation mechanism (4.4 to 4.6). Task 5.0 needs only
the existing scanner and can start straight away.

Part 7 (engine resilience and scale) is independent of the settings work.
Suggested order: 7.1 (panics) first, since it's small and the worst
failure today; then 5.0 and 7.4 together, since both reshape
`run_scan_tick`; then 7.2 and 7.3; the rest in any order. 7.12's load and
chaos suite should exist before 5.0 and 7.4 merge, so they're measured
against it.

---

### 1. Shared foundations: the `live-settings` library

Today each crate has its own `settings.rs` with the same shape: a
`ScalarSetting { key, env_var, default }` of strings, read with
`settings::get::<T>(store, &SETTING)`. The caller picks `T`, so nothing
stops two call sites reading the same key as different types (a test,
`every_scalar_settings_own_default_parses_as_the_type_boot_code_actually_requests_it_as`,
exists only to catch that). Nothing says which settings a loop or client
depends on, so nothing can tell it to rebuild when one of them changes;
that is the root of the original bug.

Parts 1.1 to 1.4 replace this with one library used by both processes.

#### Design

A new workspace crate, `crates/live-settings`. It knows nothing about
Monero, HTTP handlers or either database; each process plugs in its own
storage. The pieces, from the bottom up:

```rust
/// One setting, typed. The value type is fixed at declaration, so every
/// reader gets the same type and parsing lives in one place.
pub struct Setting<T: SettingValue> {
    pub key: &'static str,
    pub env_var: &'static str,
    pub default: fn() -> T,
    /// Extra rule on top of the type, checked at parse time, e.g.
    /// `Some(|v| range(v, 1, 10_000))`. Bounds live here, not in the type,
    /// because stable Rust can't take bounds of a generic `T` as const
    /// generics.
    pub check: Option<fn(&T) -> Result<(), String>>,
    pub description: &'static str,
    pub example: Option<&'static str>,
    pub applies: Applies,            // Live | Restart
}

/// How a value type parses, prints and describes itself on the admin
/// page. Implemented for the primitives, and by newtypes that carry
/// their own rules: `BindAddr`, `HttpUrl`, `CommaList<T>`, `Json<T>` (for
/// `monero_node.<network>`), `Secret` (never echoed back in full, never
/// logged), and one small enum per choice setting (`SignupMode`,
/// `CustodyBackend`), whose variants are the choices.
pub trait SettingValue: Sized + Clone + PartialEq + Send + Sync + 'static {
    fn parse(raw: &str) -> Result<Self, String>;
    fn render(&self) -> String;
    fn kind() -> SettingKind;        // drives the admin page control
}

/// A group of settings that one runtime piece depends on, as a plain
/// typed struct: `ScanConfig { reorg_check_depth: u64, poll_interval:
/// Duration, grace_period: Duration }`. Built from a snapshot of every
/// setting, and able to reject combinations (`hard > soft`).
pub trait Section: Clone + PartialEq + Send + Sync + 'static {
    const NAME: &'static str;
    fn keys() -> &'static [&'static dyn AnySetting];
    fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>>;
}

/// A value a reader holds. Cheap to clone, never blocks, never held
/// across an `.await` by accident: `load()` hands back an `Arc<T>`.
pub struct Live<T>(Arc<ArcSwap<T>>);
impl<T> Live<T> {
    pub fn load(&self) -> Arc<T>;
    /// For loops that sleep. Subscribe once, before the loop, and keep the
    /// receiver: a fresh receiver per iteration can miss a change made
    /// between `load()` and subscribing.
    pub fn subscribe(&self) -> watch::Receiver<()>;
}

/// Runtime state built from a section that needs work to swap in: node
/// clients, a listener, a key-custody backend, the engine client.
#[async_trait]
pub trait Reloadable: Send + Sync + 'static {
    type Config: Section;
    type Prepared: Send;
    /// Build the new state without touching the running one. Errors
    /// refuse the save. Warnings are returned, not raised.
    async fn prepare(&self, new: &Self::Config, old: &Self::Config)
        -> Result<(Self::Prepared, Vec<Warning>), FieldError>;
    /// Swap it in. Must not fail. Async, because some installs have to
    /// finish real work (registering tenants in a newly enabled custody
    /// backend) before the save returns; the registry awaits it while
    /// still holding the save mutex, so a later save can't overlap it.
    async fn install(&self, prepared: Self::Prepared);
    /// What boot does when `prepare` fails for this piece: `Exit` (a
    /// listener that can't bind), or `StartDegraded` (a custody backend or
    /// node that isn't reachable yet: start without it and keep retrying).
    fn boot_policy(&self) -> BootPolicy;
}

/// Where values live. Implemented by the engine's `Store` and monokulo's
/// `Db`, each over its own `settings` table. `write_all` is one SQLite
/// transaction.
pub trait SettingsStore: Send + Sync {
    fn read_all(&self) -> Result<HashMap<String, String>, StoreError>;
    fn write_all(&self, changes: &[(&str, Option<String>)]) -> Result<(), StoreError>;
}

/// The one owner of every section in a process.
pub struct Registry { .. }
impl Registry {
    pub fn builder(store: Arc<dyn SettingsStore>) -> RegistryBuilder;
    /// Register a plain section: readers get a `Live<S>`.
    pub fn section<S: Section>(&mut self) -> Live<S>;
    /// Register a section with runtime state behind it.
    pub fn reloadable<R: Reloadable>(&mut self, r: R) -> Live<R::Config>;
    /// Validate, prepare, persist, install. See "Save" below.
    pub async fn save(&self, changes: Changes) -> Result<SaveReport, SaveError>;
    /// Every setting's value, source, metadata and pending-restart flag,
    /// for the admin page and the engine's settings API.
    pub fn describe(&self) -> Vec<SettingView>;
}
```

**Declaring settings.** A macro keeps each declaration to a few lines and
builds `ALL`, the list the admin page and the "every setting is used"
test read:

```rust
live_settings::settings! {
    REORG_CHECK_DEPTH: u64 {
        key: "payment.reorg_check_depth",
        env: "ENGINE_PAYMENT_REORG_CHECK_DEPTH",
        default: 20,
        check: range(1, 10_000),   // keep today's bounds exactly
        description: "How many recent blocks are checked again on every scan for a chain reorganisation.",
        example: "20",
    },
    WORKER_THREADS: usize {
        check: range(1, 256),
        key: "server.worker_threads",
        ..,
        applies: Restart,
    },
}
```

**Save.** `Registry::save` does, in order, under one async mutex so saves
never interleave:

1. Parse every submitted raw value with its `SettingValue::parse`. Any
   failure refuses the save and names the field.
2. Build a snapshot of old values plus the changes, and rebuild every
   section that includes a changed key with `Section::from_snapshot`
   (this is where cross-field rules run). Any failure refuses the save.
3. For each changed section that is `Reloadable`, call `prepare`, all in
   parallel. Any error refuses the save; everything prepared so far is
   dropped, which must release it (a bound listener closes on drop).
4. `SettingsStore::write_all` in one transaction. A failure refuses the
   save and drops everything prepared.
5. `install` each prepared piece (awaited, still under the save mutex),
   then publish each changed plain section to its `Live<T>`. Sections
   marked `Restart` are persisted but not installed.
6. Return a `SaveReport`: which keys changed, `restart_required`, the
   warnings from `prepare`, and `env_overridden` (keys whose environment
   variable still wins, so the saved value has no effect).

**Boot** is the same path from an empty "old" snapshot: read everything,
build every section, prepare and install every reloadable.
- A value that doesn't parse or check falls back to its default *for that
  key only*, and is logged and reported on the admin page as "invalid
  value, using default". The rest of its section keeps its valid values.
  (Today's `resolve_parsed` already works per key; the library keeps
  that.) If the invalid value comes from an environment variable, the
  page says so, since the admin can't fix it from the page.
- If a section's cross-field rule still fails, the section falls back to
  defaults and the page shows why.
- A reloadable whose `prepare` fails follows its `boot_policy`.

Restart-only sections need to be readable before the async runtime exists
(`server.worker_threads` sizes the runtime), so the library also offers
`live_settings::read_sync::<S>(&dyn SettingsStore) -> S`, a plain
synchronous read of one section. The registry remembers each restart-only
setting's *effective* value at boot (environment included), and
`describe()` reports `pending_restart` when the current effective value
differs from it.

**Type safety this buys.**
- A setting's type is part of its declaration, so a reader can't pick the
  wrong one.
- Code outside the library can only reach a value through a `Live<S>` of
  its section. There is no string-keyed getter in the public API, so a
  new boot-time read that never reloads can't be written by accident.
- Every setting must belong to at least one section. The registry checks
  this when `build()` runs, and a unit test in each crate calls `build()`,
  so an orphaned setting fails `cargo test`.
- A `Reloadable` can't install without having prepared, because
  `install` only takes the `Prepared` value `prepare` produced.
- Validation, the admin page's control and the example all come from
  `SettingValue`, so they can't disagree.

#### 1.1 Build the `live-settings` crate

- **Aim:** The library above exists, is documented, and is fully tested
  on its own, with no dependency on either process.
- **Verify:** Unit tests in the crate, with an in-memory `SettingsStore`
  and test sections:
  - A value is resolved env over stored over default, and reported with
    its source.
  - A save with one bad value changes nothing, including the values that
    were valid.
  - A cross-field rule in `from_snapshot` refuses the save.
  - A failing `prepare` refuses the save, nothing is written, and the
    other sections' prepared values are dropped (a drop counter in the
    test double shows it).
  - A failing `write_all` installs nothing.
  - A successful save installs, and a `Live<T>` reader sees the new value
    on its next `load()`, and its `changed()` receiver fires.
  - A `Restart` setting is persisted, not installed, and appears in
    `restart_required` and as `pending_restart` until a new registry is
    built from the store. If an environment variable overrides it, saving
    doesn't set `pending_restart`, because the effective value won't
    change.
  - At boot, one invalid stored value falls back to its own default while
    its siblings in the same section keep their stored values.
  - `install` is awaited under the save mutex: a second save that starts
    while the first one's install is still running waits for it.
  - `read_sync` returns the same section value the registry would.
  - An env-overridden key appears in `env_overridden`.
  - `build()` fails if a declared setting is in no section.
  - Two concurrent saves run one after the other (check with a `prepare`
    that waits on a barrier).
  - Each `SettingValue` newtype: parse and render round-trip, and bad
    input is refused with a readable message.
- **Approach:** New crate `crates/live-settings`, depending on `arc-swap`,
  `tokio` (sync only), `async-trait` and `serde`. Move
  `resolve_parsed`/`resolve_raw` and `SettingSource` from
  `crates/shared/src/settings.rs` into it, and delete them from `shared`
  once both processes use the new crate.

#### 1.2 Move the engine onto it

- **Aim:** Every engine setting is declared with `live_settings::settings!`
  and read through a section. The old `settings::get` and `ScalarSetting`
  are gone.
- **Verify:**
  - Unit test: `Registry::build()` succeeds with the engine's real
    sections, so no setting is orphaned.
  - `GET` and `POST /api/v1/admin/settings` tests in
    `crates/engine/src/http/tests.rs` pass unchanged, apart from the new
    fields added in 4.2.
  - `grep -rn "settings::get" crates/engine/src` finds nothing outside
    the library.
- **Approach:** Sections: `NodeConfig` (every `monero_node.<network>`),
  `CustodyConfig` (part 5), `ScanConfig`, `WebhookConfig`,
  `RateLimitConfig`, `BodyLimitConfig`, `BindConfig`, `RuntimeConfig`
  (worker threads and bind address, both `Restart`), `TenantDefaults`. The `Store` implements
  `SettingsStore`. `instance_admin.rs`'s `update_settings` becomes a thin
  wrapper around `Registry::save`, and its `validate_scalar` rules move
  into the `SettingValue` types and `from_snapshot`. The `Reloadable`
  implementations are parts 2 and 5.

#### 1.3 Move monokulo onto it

- **Aim:** The same for monokulo. `crate::settings::get` and `help()` are
  gone.
- **Verify:** As 1.2, for monokulo's sections and
  `crates/monokulo/src/http/admin_settings.rs` tests.
- **Approach:** Sections: `SignupConfig`, `EngineConnection` (URL, admin
  token, HTTP cache size), `ExchangeRateConfig` (reuse the struct in
  `exchange_rate_config.rs` as the section), `PublicUrl`, `AbuseConfig`
  (reuse the struct in `abuse/mod.rs`), `OnionListenerConfig`. `Db`
  implements `SettingsStore`. `save_monokulo` becomes a thin wrapper
  around `Registry::save`, and `validate_monokulo_scalar` moves into the
  types (the soft/hard rule into `AbuseConfig::from_snapshot`).
  `MONOKULO_ENCRYPTION_KEY` stays outside the library, as now.

#### 1.4 Boot through the registry

- **Aim:** Both `main.rs` files become: open the store, build the
  registry (which prepares and installs everything), start serving. There
  is no second, boot-only way of reading a setting.
- **Verify:** The existing tests that start each app through its router
  builder pass, and the end-to-end tests in part 6 start real binaries on
  empty databases.
- **Approach:** Move `build_daemon_clients`, `build_key_custody`,
  `register_all_tenants` and the loop setup out of
  `crates/engine/src/main.rs` into `Reloadable` implementations in the
  library crate of each process. Several other places build an
  engine `AppState` by hand and copy the boot wiring:
  `crates/engine/src/bin/e2e_harness.rs`, `crates/engine-test-support/src/lib.rs`,
  and the stagenet end-to-end tests (`e2e_stagenet.rs`,
  `e2e_dashboard_stagenet.rs`). Switch them all to the registry, so none
  can drift from the real boot.

#### 1.5 Remove settings that no longer apply

- **Aim:** No setting on the admin page does nothing, and none describes a
  model that no longer exists.
- **Verify:**
  - Every setting belongs to a section: covered by `Registry::build()`'s
    orphan check, run in each crate's tests (1.2, 1.3).
  - Test: `POST /api/v1/admin/settings` with a key that isn't a declared
    setting (for example `key_custody.backend` after 5.1) is refused with
    `400` and nothing is stored. Today unknown keys fall through
    `validate_scalar`'s `_ => Ok(())` and are persisted.
- **Approach:**
  - `key_custody.backend` is removed in task 5.1, together with the
    conversion of its value, not here: removing it before per-store
    custody exists would leave the engine with no custody setting.
  - Remove the dead rescan branches: `rescan.default_lookback_days` and
    `rescan.max_lookback_days` in monokulo's `validate_monokulo_scalar`,
    and `payment.default_rescan_lookback_days` and
    `payment.max_rescan_lookback_days` in the engine's `validate_scalar`
    (`crates/engine/src/http/instance_admin.rs`). The rescan feature was
    dropped (`migrations/0012_drop_order_rescans.sql`).
  - With the library, only declared settings can be saved, which closes
    the unknown-key hole.
  - Keep `server.worker_threads` (D1) and wire it up (2.8).
  - Keep `payment.confirmations_required` and
    `payment.order_expiry_minutes`, and make them apply (2.9).

---

### 2. Engine applies its settings in place

#### 2.1 Monero nodes (the reported bug)

- **Aim:** Saving `monero_node.<network>` updates the running engine at
  once. The status page lists the network, a store can connect on it, and
  the scanner starts scanning it. Clearing a network stops scanning it and
  removes it from the status page's node list.
- **Verify:**
  - Integration test in `crates/engine/src/http/tests.rs`: build the app
    with no nodes, then `POST /api/v1/admin/settings` a stagenet node
    pointing at a local HTTP fake daemon (see the approach). Creating a stagenet tenant now
    succeeds, where before the save it was refused, and `GET /status`
    reports stagenet. Clearing it with `null` makes tenant creation refuse
    again.
  - Unit test: the scanner loop's per-tick snapshot picks up a network
    added between ticks (drive one tick with the fake daemon and check the
    network's scanned height, and the tenant's cursor from 5.0 once that
    lands, move).
  - Unit test: replacing a node's host swaps the `FallbackDaemonClient`,
    and the old one is not used on the next tick.
- **Approach:**
  - Replace `configured_networks: Arc<HashSet<Network>>` and
    `daemons: Arc<HashMap<..>>` in `AppState` (`crates/engine/src/http/mod.rs`)
    with one shared `DaemonSet` handle. Derive the configured networks
    from its keys, so there is one source.
  - Keep `strict_tls` in `AppState` so rebuilt clients honour it.
  - `run_scanner_loop` and `run_double_spend_revalidation_loop` take the
    handle and snapshot it at the top of each tick.
  - The status page (`http/status_page.rs`), tenant creation and
    `lookup_payment` (`http/admin.rs`) read the snapshot per request.
  - Building node clients must not panic: `build_daemon_clients` panics
    today if a client can't be built (`main.rs`). In prepare, a node that
    can't be built refuses the save with the reason.
  - Test support: a saved `monero_node` is a host and port, so the engine
    builds a real `RpcDaemonClient` over HTTP. The in-process
    `FakeDaemonClient` used by `http/tests.rs` can't answer that. Move the
    small HTTP replay server from `crates/engine/tests/daemon_rpc_replay.rs`
    into `crates/engine-test-support` as a reusable fake daemon that
    serves canned heights, blocks and mempool on a local port. Tests for
    2.1, 2.2 and part 6 use it.
  - When a network is removed, drop its `scanner_status` entry so the
    status page doesn't show stale scanner health.

#### 2.2 Networks with stores but no reachable nodes (D2, engine side)

- **Aim:** The engine can say, at any moment and as part of any node save,
  which networks have active tenants but no reachable node, and how many
  tenants each has. "No reachable node" covers both a network with no
  node configured and one whose nodes all fail.
- **Verify:**
  - Integration test: with two stagenet tenants, clear stagenet. The
    settings response includes
    `warnings.unserved_networks = [{ "network": "stagenet", "tenants": 2 }]`,
    and `GET /status` includes the same list.
  - Integration test: set stagenet to a node address where nothing
    listens (a closed local port). The save succeeds, and the same warning
    appears.
  - Integration test: set it to the mock daemon. No warning.
  - `GET /api/v1/admin/settings` reports `tenant_count` per network, which
    the page's JS confirmation needs (4.4).
- **Approach:**
  - Add `Store::count_active_tenants_by_network()`.
  - During prepare, probe each changed network's nodes with the same
    short-timeout height call the status page uses (`NODE_HEIGHT_TIMEOUT`,
    `http/status_page.rs`), all in parallel. A network whose probe fails
    everywhere, or which has no nodes, and has tenants, goes into
    `SaveWarnings.unserved_networks`. The probe never blocks the save.
  - Add `unserved_networks` to `EngineStatusResponse`, computed from the
    same tenant counts plus the status page's existing per-node results,
    so the condition is visible after the save too, and clears by itself
    once a node answers.

#### 2.3 Scan-loop timing: reorg depth, poll interval, grace period

- **Aim:** `payment.reorg_check_depth`, `payment.mempool_poll_interval_ms`
  and `payment.expired_order_grace_period_minutes` take effect from the
  next scan tick after saving.
- **Verify:**
  - Unit test: a loop tick run after changing the grace period expires an
    order that the previous value would still have kept.
  - Unit test: the status page's "expected every Ns" figure follows a new
    poll interval without a restart.
- **Approach:** Put these three in a `ScanConfig` behind a shared handle.
  The loop reads it at the start of each tick and sleeps for the interval
  it read. `AppState` loses `scan_poll_interval_secs` and
  `expired_order_grace_period_seconds` in favour of reading `ScanConfig`.

#### 2.4 Webhook delivery settings

- **Aim:** `webhooks.allow_private_urls`, `webhooks.delivery_timeout_ms` and
  `webhooks.max_attempts` apply to the next delivery attempt after saving.
- **Verify:** Unit test around the delivery tick: with
  `allow_private_urls` false, a delivery to `127.0.0.1` is refused. Flip
  the setting through the shared config and the next attempt reaches the
  local test receiver.
- **Approach:** Pass a `WebhookConfig` handle into
  `run_webhook_delivery_loop` and read it per tick. The timeout is
  already set per request (`webhook_delivery.rs`), so a changed timeout
  needs no new client.

#### 2.5 Per-token rate limit

- **Aim:** `server.rate_limit_per_token_per_min` applies to the next
  request after saving.
- **Verify:** Unit test on `RateLimiter` (`crates/shared/src/rate_limit.rs`,
  also used by monokulo, so the change helps both): with a
  limit of 2, the 3rd request is refused. After `set_limit(5)`, requests
  up to the 5th pass inside the same window.
- **Approach:** Store the limit in an `AtomicU32` inside `RateLimiter` and
  add `set_limit`. Existing per-token counters stay as they are.

#### 2.6 Request body limit

- **Aim:** `server.max_body_bytes` applies to the next request after
  saving.
- **Verify:** Integration test: with a 100-byte limit, a 200-byte body gets
  `413`. Raise it to 1000 through the settings API, and the same request
  gets past the limit check.
- **Approach:** Replace `RequestBodyLimitLayer::new(max_body_bytes)` in
  `build_router` with a small middleware that reads the current limit from
  shared state, rejects on `Content-Length` over the limit, and wraps the
  body in `http_body_util::Limited` for bodies without one. `build_router`
  stops taking the number as an argument.

#### 2.7 Listen address (`server.bind`), restart-only (D8)

- **Aim:** `server.bind` is saved from the page and used at the next
  start. Saving it shows the restart banner, and the page says that
  monokulo's `engine.url` must be changed to match once the engine has
  restarted.
- **Verify:**
  - Integration test: saving `server.bind` persists it, the engine keeps
    serving on the old address, and the response lists it in
    `warnings.restart_required`. `GET /api/v1/admin/settings` reports
    `pending_restart: true` for it.
  - Unit test: saving an address that isn't private or loopback returns
    the existing public-address warning as a save warning.
  - Unit test: a value that doesn't parse as an address is refused.
- **Approach:** Declare it `Restart` in part 1. Validation stays as today
  (parse, and the private-address warning from `main.rs`). No listener
  swapping in the engine.

#### 2.8 Worker threads (D1)

- **Aim:** `server.worker_threads` is actually used, and saving it tells
  the admin a restart is needed.
- **Verify:**
  - Unit test: the runtime builder reads the stored value (test the
    function that turns settings into a `tokio::runtime::Builder`
    configuration, not the runtime itself).
  - Integration test: saving `server.worker_threads` returns it in
    `warnings.restart_required`, and `GET /api/v1/admin/settings` marks it
    `pending_restart: true` until the process restarts with that value.
- **Approach:** Replace `#[tokio::main]` in `crates/engine/src/main.rs`
  with a hand-built multi-thread runtime whose worker count comes from the
  setting. It has to be read before the runtime exists, so open the store
  synchronously first (the store is SQLite, no async needed). Record the
  value in effect at boot in `AppState`, and report `pending_restart` when
  the saved value differs from it.

#### 2.9 Payment defaults

- **Aim:** `payment.confirmations_required` and
  `payment.order_expiry_minutes` are the defaults for any new tenant that
  doesn't give its own values, whether created by the bootstrap CLI or by
  `POST /api/v1/admin/tenants`. Today the API path ignores them and uses
  hardcoded values (`crates/engine/src/store.rs`, `unwrap_or(10)` and
  `unwrap_or(1800)`).
- **Verify:**
  - Integration test: save `payment.confirmations_required = 3`, create a
    tenant with no value, and the tenant has 3.
  - Unit test: `bootstrap_wallet` still picks up the current saved value.
- **Approach:** `create_tenant` in `http/admin.rs` fills missing values
  from the settings before calling `Store::create_tenant`. Remove both
  hardcoded fallbacks from the store. Descriptions (4.1) say this is only a
  default for new tenants and that each store's own thresholds in monokulo
  apply to its orders.

---

### 3. Monokulo applies its settings in place

#### 3.1 One apply step on save

- **Aim:** `save_monokulo` (`crates/monokulo/src/http/admin_settings.rs`)
  uses `Registry::save` (part 1) for every setting, not
  just the abuse ones.
- **Verify:** Unit test: a save that changes an abuse value and the
  exchange-rate cache together applies both, and a failing prepare applies
  neither.
- **Approach:** Replace the `AbuseProtection::reload` call at the end of
  `save_monokulo` with `Registry::save`. Its reloadables cover abuse
  (existing reload), the engine client (3.2), exchange rates (3.3) and the
  onion listener (3.4).

#### 3.2 Engine URL and HTTP cache size (D4)

- **Aim:** Saving `engine.url` or `http_cache.max_mb` makes the next engine
  call use the new address or cache. Live checkout streams reconnect to
  the new engine by themselves. If the new URL doesn't answer, it is still
  saved and applied, and the page shows an error banner after the POST.
- **Verify:**
  - Integration test with two mock engines: point at A, save B's URL, and
    the next store page load hits B.
  - Test: an open live-updates subscription on A ends when the URL
    changes, and a new subscription goes to B.
  - Test: save a URL where nothing listens. The response is `200`, the
    stored and live URL are the new one, and the page has an error banner
    containing the URL and the connection error.
- **Approach:** Hold `EngineClient` in `AppState` behind a shared handle.
  Handlers call `state.engine()` to get an `Arc<EngineClient>` snapshot.
  A changed URL or cache size builds a new client with a new `LiveHub`.
  The old hub won't drop by itself: every `OrderSubscription` holds an
  `Arc<LiveHub>` (`crates/monokulo/src/live.rs`), and its upstream task
  holds an `EngineClient` clone, which holds the hub too. So add
  `LiveHub::shutdown()`, which aborts its upstream tasks and closes its
  senders, and call it on install. Each browser's stream then ends, and
  the `EventSource` reconnect (`static/checkout.js`) lands on the new hub.
  Also clear `StatusCacheState` on install, so the status indicator and
  alerts don't show the old engine's state. The no-JS checkout already refreshes on its own. After install,
  call the new URL's `/status` with a short timeout. A failure goes into
  `SaveWarnings.engine_unreachable` and never undoes the save.

#### 3.3 Exchange-rate provider settings

- **Aim:** The three `exchange_rate.*` settings apply to the next rate
  lookup after saving.
- **Verify:** Unit test: with Coingecko disabled, a fiat order is refused
  with the existing "no provider" error. Enable it and point the base URL
  at a mock, and the next order is priced from the mock. Setting
  `cache_seconds` to 0 makes the next lookup fetch again.
- **Approach:** Keep `ExchangeRateProviders` behind a shared handle and
  rebuild it with `exchange_rate_config::parse` on save. `parse` already
  validates, so prepare calls it and refuses on error.

#### 3.4 Onion listener

- **Aim:** Saving `abuse.onion_listener` starts, moves or stops the onion
  listener at once. A bind failure refuses the save.
- **Verify:** Integration test: save `127.0.0.1:<free port>`, and a
  connection that sends a PROXY header is served. Clear it, and the port
  refuses connections. Save a port already held, and the save is refused.
- **Approach:** A small supervisor in monokulo that owns the onion
  listener task: prepare binds the new address (failure refuses the save),
  install starts serving on it and stops the old one, with a shutdown
  deadline (say 10 seconds) because live-update streams stay open. Drop "Read at
  startup" and "except the onion listener" from the abuse help text
  (`crates/monokulo/src/views/admin.rs`, the "Abuse protection" hint, and
  the `abuse.onion_listener` entry in `settings.rs`).

#### 3.5 Pin the settings that are already live

- **Aim:** `signup.mode`, `public_url` and `engine.admin_token` already
  apply at once. Tests keep it that way.
- **Verify:** One test each: save through `POST /dashboard/admin/settings`,
  then the next request behaves under the new value (signup open or
  closed, connect flow given the new public URL, admin page using the new
  token).
- **Approach:** Tests only.

#### 3.6 Engine listen address note

- **Aim:** An admin who saves a new engine `server.bind` is told what else
  to do: restart the engine, then set monokulo's `engine.url` to the new
  address. Until then monokulo keeps using the old URL, which still works,
  because the engine only moves when it restarts.
- **Verify:** View test: after saving `server.bind`, the restart banner
  includes the `engine.url` instruction with the new address filled in.
- **Approach:** Part of the banner rendering in 4.5. No automatic rewrite.

#### 3.7 Merchant alert for stores that can't be scanned (D2)

- **Aim:** While any of a merchant's stores can't be scanned, because its
  network has no reachable node (D2) or its key storage is disabled or
  down (5.7), every page the merchant sees shows a red alert at the top,
  except the POS app. The alert names the store and why, and goes away by
  itself once the store is scanned and caught up again. It works without
  JavaScript. It is only ever shown to the signed-in owner of the store.
- **Verify:**
  - Integration test: a user with a stagenet store, engine status mock
    reporting that store's tenant as unserved. The dashboard, store page,
    orders page, settings pages and the checkout share page they open
    while signed in all contain the alert with the store name. `GET` on
    the POS page does not.
  - Test: a user whose stores are all served sees no alert.
  - Test: once the mock reports the tenant served again, the alert is gone
    on the next page load (after the status cache refreshes).
  - Test: a signed-out visitor, and a signed-in user who doesn't own the
    store, never see it, including on that store's checkout pages. The
    embedded checkout never shows it.
  - Playwright (on the `coverage_fixture` setup, see 4.7): the alert
    renders at phone and desktop widths without overlapping the nav.
- **Approach:**
  - The engine's `/status` reports `unserved_tenants`: for each tenant
    that can't currently be scanned, its public key, network and reason
    (`no_reachable_node`, `custody_disabled`, `custody_unavailable`,
    `catching_up` with blocks behind). This covers D2 and 5.7 with one
    list, and monokulo already knows each store's tenant public key
    (`store_connections.tenant_public_key`), so no new monokulo column or
    backfill is needed.
  - Monokulo's status cache (`http/status_page.rs`, `StatusCacheState`)
    already polls the engine's `/status`. Keep the list from it.
  - Add `alerts: Vec<Alert>` to `PageChrome` (`views/mod.rs`) and fill it
    in `page_chrome` (`http/mod.rs`) for a signed-in user: their stores
    whose tenant is in the list. Render alerts under the nav in both
    `layout` and `layout_with_head` (the checkout share page uses the
    latter), with the existing `.error` style. `views/pos.rs` uses
    `layout_bare_with_head`, and the embedded checkout has its own
    layout, so neither renders them; add tests that pin both.

---

### 4. Admin page: explanations, warnings and inputs

#### 4.1 Write the descriptions

- **Aim:** Every setting has plain-language text saying what it controls,
  what kind of value it takes, its limits, and an example where the format
  isn't obvious. Suggested content:

  | Setting | Kind | Example |
  |---|---|---|
  | `monero_node.<network>` | JSON: host, port, ssl, accept_self_signed_certs, fallbacks | see 4.3 |
  | `key_custody.enabled_backends` | one or more of `plain`, `socket` | `plain,socket` |
  | `key_custody.default_backend` | one of the enabled backends | `plain` |
  | `key_custody.socket_path` | Unix socket path of a running key-custody-server; required when `socket` is enabled | `/run/key-custody/sock` |
  | `payment.confirmations_required` | whole number; default for new stores | `10` |
  | `payment.order_expiry_minutes` | whole number of minutes; default for new stores | `30` |
  | `payment.reorg_check_depth` | blocks re-checked for reorgs | `20` |
  | `payment.mempool_poll_interval_ms` | milliseconds between scans | `1000` |
  | `payment.expired_order_grace_period_minutes` | minutes a late payment is still matched | `360` |
  | `payment.scan_chunk_memory_budget_mb` | MB per scan chunk | `8` |
  | `server.bind` | address:port, private or loopback; needs a restart | `127.0.0.1:8443` |
  | `server.worker_threads` | whole number of threads; needs a restart | `2` |
  | `server.rate_limit_per_token_per_min` | requests a minute per API token | `120` |
  | `server.max_body_bytes` | bytes | `8192` |
  | `webhooks.allow_private_urls` | `true`/`false` | `false` |
  | `webhooks.delivery_timeout_ms` | milliseconds | `5000` |
  | `webhooks.max_attempts` | whole number | `8` |
  | `signup.mode` | `public` or `invite_only` | `invite_only` |
  | `engine.url` | http(s) URL of the engine | `http://127.0.0.1:8443` |
  | `engine.admin_token` | the engine's instance admin token | none (secret) |
  | `exchange_rate.coingecko_enabled` | `true`/`false` | `true` |
  | `exchange_rate.coingecko_base_url` | https URL | `https://api.coingecko.com` |
  | `exchange_rate.cache_seconds` | seconds a rate is reused | `30` |
  | `http_cache.max_mb` | MB for monokulo's engine response cache | `16` |
  | `public_url`, `rate_limit.*`, `abuse.*` | existing help text, reworded where needed | existing |

- **Verify:** Part 1's metadata tests, plus a person reading the wording
  on the rendered page.
- **Approach:** Write them into the `scalar_settings!` declarations from
  part 1. One to three sentences each, in the style of the existing
  `abuse.*` help.

#### 4.2 Engine reports its own metadata

- **Aim:** The engine owns its settings' descriptions. Monokulo renders
  what the engine reports, so a newer engine's settings are described
  correctly without a monokulo change.
- **Verify:**
  - Test in `crates/engine/src/http/tests.rs`: `GET /api/v1/admin/settings`
    returns `description`, `kind` (with range or choices), `example`,
    `applies` and `pending_restart` for every scalar, and a description,
    example and `tenant_count` for each `monero_node` network.
  - Test in `admin_settings.rs`: a mock engine response with those fields
    renders them. A response without them (an older engine) still renders
    the fields, with no help text.
- **Approach:** Add the fields to the engine's settings view in
  `instance_admin.rs` and to `RemoteScalarSetting` in monokulo with
  `#[serde(default)]`. `fetch_scanner_settings` fills
  `AdminScalarFieldView.help` and the new kind, example and restart fields
  instead of `None`.

#### 4.3 Monero node field

- **Aim:** Each network's node field explains its JSON shape, shows a full
  example with a fallback, and says that fallbacks can't nest.
- **Verify:** View test in `crates/monokulo/src/views/admin.rs`: the page
  contains the example JSON for each network's field, and each field name
  (`host`, `port`, `ssl`, `accept_self_signed_certs`, `fallbacks`) with its
  meaning.
- **Approach:** Under each textarea, render a short field reference and a
  `<details>` block with an example:

  ```json
  {
    "host": "node.monerodevs.org",
    "port": 38089,
    "ssl": false,
    "accept_self_signed_certs": true,
    "fallbacks": [
      { "host": "node2.monerodevs.org", "port": 38089, "ssl": false }
    ]
  }
  ```

  Note that `ssl` defaults to false and `accept_self_signed_certs` to
  true, and that fallbacks are tried in order when the primary fails. Use
  a mainnet node for the mainnet field's example. Show "Used by N stores"
  next to each network, from `tenant_count`.

#### 4.4 Confirm before clearing a network in use (D2, JS)

- **Aim:** With JavaScript, submitting the engine settings form with a
  node field emptied for a network that stores use asks for confirmation
  first, naming the network and the number of stores. Cancelling keeps the
  page as it was. Without JavaScript the form submits straight away and
  the red banner (4.5) reports the result.
- **Verify:**
  - Playwright (on the `coverage_fixture` setup, see 4.7): empty the stagenet field
    where `tenant_count` is 2, click save, and a dialog mentions
    "stagenet" and "2 stores". Dismiss it and nothing is posted. Accept it
    and the POST is sent.
  - Playwright: emptying a network with 0 stores posts with no dialog.
  - Existing no-JS save tests still pass.
- **Approach:** Render `data-network` and `data-tenant-count` on each node
  textarea. A small inline script on the admin page listens for the form's
  `submit` event, checks for emptied fields with a count above 0, and
  calls `confirm()`. The same page already renders inline scripts the way
  `views/mod.rs` does for the status indicator.

#### 4.5 Banners after a save (D1, D2, D4)

- **Aim:** After any POST, the page shows the outcome at the top:
  - Success: "Engine settings saved and applied." or "Monokulo settings
    saved and applied."
  - Warning (amber): "Saved. These settings take effect after the engine
    restarts: server.worker_threads." (or `server.bind`, with the
    `engine.url` instruction from 3.6). Only for restart-only settings whose
    value changed.
  - Error (red): "2 stores use the stagenet network, which no longer has
    any reachable nodes." One line per network.
  - Error (red): "Saved, but the engine at <url> didn't answer: <error>."
  - Note: "Saved, but <key> is set by the environment variable <VAR>, so
    the saved value won't be used while it is set."

  Each field whose saved value is waiting for a restart also shows a
  "restart needed" note next to it on every later load, not only straight
  after the save.
- **Verify:**
  - View tests: each warning in `SaveWarnings` renders its banner with the
    right style class, and more than one can show at once.
  - Handler tests: saving `server.worker_threads` shows the restart
    banner. Saving a different value for an ordinary setting doesn't.
  - Playwright (on the `coverage_fixture` setup): the banners render at phone and desktop
    width, and the red ones use the error colour tokens in both themes.
- **Approach:** Extend `AdminSettingsViewModel` with a `warnings` list
  built from the engine's save response (`save_scanner`) or monokulo's own
  `SaveWarnings` (`save_monokulo`). Render it where `error` and `success`
  render today. Add a `.warning` style to `views/head.html` next to the
  existing `.error` and `.success`.

#### 4.6 Inputs that match the value kind

- **Aim:** Fields use the right control: numbers with min and max, a
  select for choices and booleans, checkboxes for a list of choices
  (`key_custody.enabled_backends`), a URL input for URLs, and a password
  input for `engine.admin_token`. It all still works without JavaScript.
- **Verify:**
  - View test: an integer setting renders `type="number"` with the
    declared `min` and `max`. `signup.mode` renders a `select` with both
    options and the current one selected. `enabled_backends` renders one
    checkbox per backend.
  - Existing no-JS save tests still pass. The form still posts the same
    field names, and a list setting posts as a comma-separated value.
- **Approach:** In `scalar_field` (`views/admin.rs`), switch on the kind
  from part 1 or 4.2. Server-side validation stays the real check.

#### 4.7 Page layout and wording

- **Aim:** The page reads well at phone and desktop width, descriptions
  sit under their field, the source (environment, saved, default) stays
  visible, and no text says a setting is "read at startup" except the
  restart-only settings.
- **Verify:**
  - Playwright on the `coverage_fixture` setup
    (`crates/monokulo/examples/coverage_fixture.rs`, which serves real
    monokulo pages over an in-process engine; `surface.spec.js` only tests
    hand-written HTML, so it doesn't fit): open the admin settings page at phone and
    desktop sizes. Every field's description is visible, the node example
    expands, and there is no horizontal scroll. Add the page's states
    (plain, after save with each banner) to the UI stages gallery.
  - View test: the only rendered mentions of "restart" are for
    `server.worker_threads` and `server.bind`.
- **Approach:** CSS in `views/head.html` for `.field-help`, reusing the
  existing form spacing tokens. Group engine settings under headings
  (Nodes, Key custody, Payments, Server, Webhooks), the way monokulo's
  abuse settings are grouped today.

---

### 5. Per-store key custody (D3)

#### Design

**Model.** Each tenant already records the backend that sealed its keys:
`tenants.key_custody_backend` next to `tenants.sealed_key_material`
(`crates/engine/migrations/0001_init.sql`). Today every tenant gets the
one instance-wide backend. After this work:

- The engine keeps a **custody registry**: one live `KeyCustody` instance
  per enabled backend, keyed by backend name (`plain`, `socket`).
- Each tenant's wallet is registered in the backend named by its own
  `key_custody_backend`, and the wallet-handle map records both:
  `tenant_id -> (backend, WalletHandle)`. A `WalletHandle` means nothing
  outside the backend that issued it, so the backend name always travels
  with it.
- Every place that uses key material resolves the tenant's backend first:
  tenant creation (`http/admin.rs` `create_tenant`), order creation
  (`http/orders.rs`, `derive_subaddress`), scanning (`run_scan_tick`,
  `scanner.rs`), payment lookup (`http/admin.rs` `lookup_payment`), tenant
  deletion (`delete_own_tenant`), boot registration
  (`register_all_tenants`), and the lazy re-registration in
  `resolve_wallet_handle` (`http/mod.rs`), which `create_order` and
  `lookup_payment` go through. That last one must register in the
  tenant's own backend, and must not register at all when that backend is
  disabled, or a disabled backend's store would quietly come back.
- Admin settings: `key_custody.enabled_backends` (which backends are
  loaded and offered to stores), `key_custody.default_backend` (the one
  new stores get unless they choose), and `key_custody.socket_path`
  (configuration of the `socket` backend). `default_backend` must be one of
  the enabled ones, and at least one must be enabled.
- **No migration of key material.** Moving a store to another backend
  means the merchant enters the store's private view key and public spend
  key again. The engine registers them in the new backend, checks they are
  the same wallet, then retires the old registration. Nothing ever reads
  keys out of one backend to put them in another. `KeyCustody` has no way
  to export keys, and that stays true.
- **Same wallet check.** The re-entered keys must be the same wallet as
  the tenant's stored `primary_address`. Compare decoded keys, not address
  strings: decode `primary_address`, and check its public spend key equals
  the submitted spend public key, its public view key equals the public
  key of the submitted private view key, and its network is the tenant's.
  String comparison isn't safe because `bootstrap_wallet` stores whatever
  address the operator typed (`local_admin.rs`) and older rows may hold a
  different address form. Otherwise the store's past and pending orders, which were given
  subaddresses of the old wallet, would stop matching. A mismatch is
  refused: "These keys belong to a different wallet from the one this
  store uses."
- **Disabling a backend that stores use.** Handled like a network with no
  reachable nodes (D2): allowed, with a red banner after the save ("N
  stores keep their keys in <backend>, which is no longer enabled") and a
  JS confirmation before submitting. Those tenants' wallets are removed
  from the registry, so they stop being scanned and can't take new
  orders. Their sealed key material stays in the database untouched, so
  re-enabling the backend brings them straight back. Affected merchants
  see an alert on every page except the POS app, telling them to switch
  the store to an enabled backend by entering its keys again.
- **Backend unavailable** (for example the socket server is down): the
  backend stays enabled, and its tenants fail operations with
  `KeyCustodyError::BackendUnavailable`. Two things in today's socket
  backend stop this from recovering, and both change (task 5.8):
  - `SocketKeyCustody` gives up for good after any error and never
    reconnects (`crates/key-custody-service/src/client.rs`), and the
    key-custody server keeps wallets only in memory. So the engine must
    reconnect with backoff and, on reconnecting, register that backend's
    tenants again from their sealed rows. Handles from the old connection
    are replaced. No keys need entering again.
  - At boot, an unreachable socket makes the engine `exit(1)`
    (`build_key_custody`, `main.rs`). Instead the engine starts, marks the
    backend unavailable, and keeps trying to connect; other backends'
    stores scan normally.
  The engine's `/status` reports each enabled backend's health, and
  affected merchants get the alert (3.7). With the per-store scan cursor
  (5.0), those stores catch up once the backend is back.

**Scanning safety.** Requirement: switching a store's backend, disabling
a backend, or a backend going down may make that store's payments show up
late, but must never lose one, never hold up any other store, and must
always recover by itself once the store's keys are usable again.

Two things in today's scanner break that requirement, and both have to be
fixed before per-store custody ships (task 5.0):

1. **One store's failure stalls the whole network.** The block scan in
   `run_scan_tick` (`crates/engine/src/scanner.rs`) keeps one scanned
   height per network. When `scan_transaction` fails for any tenant it
   does `break 'heights` and leaves the block unscanned for everyone.
   That is right for a one-tick blip. But a backend that stays down, for
   example a stopped socket server, would stop block scanning for every
   store on that network, including stores on healthy backends.
2. **A store left out of a tick misses those blocks for good.** The tick
   only scans tenants that have a wallet handle (`ranges` in
   `run_scan_tick` skips the rest). The network's scanned height still
   moves past those blocks, and nothing goes back for the skipped store.
   Today that already happens to a tenant whose keys failed to register at
   boot (`register_all_tenants` logs and carries on). With per-store
   custody it would also happen while a backend is disabled or down.

The fix is a **per-store scan cursor**:

- Each tenant gets `scanned_through_height` (new column, per tenant, on
  its own network). A tenant is "caught up" when its cursor equals the
  network's scanned height.
- The network's block scan scans every caught-up tenant whose keys are
  usable. When one tenant's scan fails, that tenant's cursor stays where
  it was and the scan carries on for the others. The network's height
  only stops moving for failures that affect everyone (fetching blocks,
  reading a block hash, a store write), as today.
- Any tenant behind the network height, because its backend was disabled,
  down, mid-switch, or its keys failed to register, is caught up by a
  separate step in each tick: fetch blocks from its cursor + 1, scan them
  for that tenant only, and move its cursor. Catch-up is capped per tick
  (reusing the `payment.scan_chunk_memory_budget_mb` chunking), so a long
  gap doesn't delay the live scan. A catch-up failure leaves the cursor
  where it is and retries next tick.
- **Orders must not expire while their store can't be scanned.** The
  status sweep expires orders on wall-clock time, per network
  (`non_terminal_order_ids` in `store.rs`, `derive_status` in
  `status.rs`). Without a change, a lagging store's unpaid orders would
  become `expired` and send `order.expired` webhooks, and a shop plugin
  may cancel them, even though catch-up later finds they were paid. So
  while a tenant's cursor is below the network height, `derive_status`
  doesn't move its orders to `Expired` (other transitions still happen).
  Once it has caught up, expiry applies as normal, with the grace period
  counted as usual.
- **Which orders catch-up matches.** Matching is by subaddress index, and
  `record_scan_match` doesn't look at order status. Until 7.3 lands, the
  live scan and catch-up both use the tenant's full index range
  (`0..next_minor_index`, the `ranges` in `run_scan_tick`). Once 7.3's
  scan window exists, catch-up uses the window as of the tenant's cursor
  (orders open at any point since the cursor's block time), never
  today's, so orders that closed during the gap are still matched.
- **Which tenants have cursors that move.** A tenant with no active orders
  isn't scanned today (`active_tenant_ids`), and nothing can be paid to
  it. Its cursor moves with the network height anyway, in the same
  transaction as `set_scanned_block`, so it never looks lagging. A new
  tenant's cursor starts at the network's scanned height. While a network
  has never been scanned (no seed yet), cursors are `NULL`, meaning
  "follows the network", and get the seed height when the network is
  seeded. Healthy tenants' cursors move in the same transaction as
  `set_scanned_block`, and never past a height that failed for that
  tenant.
- **Chain consistency during catch-up.** Catch-up fetches old blocks, and
  `FallbackDaemonClient` may send each request to a different node, which
  may be on a different fork. Where a `scanned_blocks` row exists for a
  height, catch-up checks the fetched block's hash against it and stops
  on a mismatch (the next reorg check sorts it out). Rows older than 4x
  the reorg depth are pruned (`scanner.rs`), so beyond that, catch-up
  relies on those blocks being deep enough to be final.
- **Shared fetches.** Lagging tenants at the same cursor, which is common
  when a backend that many stores use comes back, are caught up with one
  block fetch per range, not one per tenant.
- **Reorgs.** When `check_for_reorg_and_reconcile` finds a reorg at
  `reorg_point`, it deletes rows at and above it, so the network falls
  back to `reorg_point - 1`. Every tenant cursor at or above
  `reorg_point` is set to `reorg_point - 1`, so the replacement block at
  `reorg_point` itself is scanned for every tenant. (Clamping to
  `reorg_point` would skip that block and could lose a payment.)
- **Mempool.** A lagging tenant whose keys work (it is catching up after a
  backend came back) is included in the live mempool scan straight away,
  so 0-conf detection resumes before catch-up finishes. A tenant whose
  keys don't work misses mempool sightings during the gap. When it comes back, catch-up finds any
  payment already in a block, and the live mempool scan finds any still
  waiting. The order goes straight to its confirmation count; nothing is
  lost.
- **Mid-switch errors.** A tick that started before a switch may get
  `UnknownWallet` from the old backend. With per-store cursors that is an
  ordinary per-tenant failure: that store's cursor stays, others carry
  on, and the next tick (or catch-up) uses the new handle.
- **Order creation during a switch.** `create_order` resolves the handle
  and then calls `derive_subaddress` a little later (`http/orders.rs`). If
  the old registration is removed in between, that call returns
  `UnknownWallet`, which today becomes a `404`. So on `UnknownWallet`,
  order creation looks the tenant's handle up again and, if it changed,
  retries with the new one (at most 3 times, which covers switches in
  quick succession). Both handles are for the same wallet, as checked, so
  the order gets the same address either way. `lookup_payment` does the
  same. The same retry covers a socket reconnect replacing handles.
- **Status.** The engine's `/status` reports, per network, the tenants
  behind the network height and by how many blocks, so a stuck catch-up
  is visible, and the merchant alert (5.7) stays up until the store is
  caught up, not only until its keys are back.

**Security.** View keys travel only as they do today: merchant's browser
to monokulo to the engine, over the admin API, never stored by monokulo.
The re-entry form is a POST handled like the add-store form. The engine
never logs key material (`WalletMaterial`'s `Debug` already redacts it).
The same wallet check proves the merchant holds keys for the store's own
wallet, so a signed-in user can't bind someone else's wallet to their
store by switching backends.

#### 5.0 Per-store scan cursor (prerequisite)

- **Aim:** The scanner meets the scanning-safety requirement above: a
  store that can't be scanned for a while never loses a payment, never
  holds up other stores, and always catches up by itself.
- **Verify:** Unit tests in `crates/engine/src/scanner.rs` with the
  existing fake daemon and a key-custody wrapper that fails on demand for
  one tenant:
  - Tenant A's custody fails for 3 ticks while blocks arrive with
    payments for A and B. B's payments are recorded on time and the
    network height advances. A's cursor stays put. When A's custody
    recovers, the next ticks catch A up and record its payments, each
    exactly once.
  - An order of A whose expiry passes during the gap is not marked
    expired, and no `order.expired` webhook is sent, while A lags. If
    catch-up finds its payment it becomes paid. If not, it expires once A
    has caught up.
  - A new tenant created mid-run starts with its cursor at the network
    height (catch-up doesn't start from 0). A tenant with no active orders
    is never reported as lagging.
  - A reorg at `reorg_point` sets lagging and healthy cursors to
    `reorg_point - 1`, and a payment in the replacement block at
    `reorg_point` is recorded.
  - Catch-up that fetches a block whose hash differs from the stored
    `scanned_blocks` row stops without recording from it.
  - 100 tenants lagging at the same cursor cause one block fetch per
    range per tick, not 100.
  - A lagging tenant with working keys is matched in the mempool scan.
  - A tenant with no wallet handle at all (keys failed to register) is
    caught up once its handle appears.
  - A catch-up gap larger than one tick's budget is closed over several
    ticks, and the live scan for other tenants carries on meanwhile.
  - Existing tick tests still pass, including those in the `chain_scanned_to`
    family, which pin the reorg and "leave the block unscanned" rules.
  - `GET /status` reports a lagging tenant and how far behind it is.
  - Property-style test: random sequences of per-tenant failures,
    recoveries, blocks and reorgs; at the end, with every backend
    healthy and enough ticks run, every payment recorded by a run with no
    failures is present, recorded once, and every order has the same
    final status. (Not strict equality of all rows: timing fields such as
    first-seen times legitimately differ.)
- **Approach:** New engine migration adding
  `tenants.scanned_through_height`, set for existing tenants to the
  network's current scanned height (they are caught up today).
  `bump_scanned_heights_for_tenant` (the per-order coverage shown to
  merchants) uses the tenant's cursor instead of the network height, so an
  order never shows coverage its store doesn't have. Split
  `run_scan_tick`'s block loop so a per-tenant scan failure marks that
  tenant as lagging for the tick instead of `break 'heights`. Add a
  `catch_up_lagging_tenants` step after the live scan, grouping tenants by
  cursor. Make `derive_status` skip the `Expired` transition for a lagging
  tenant's orders. Update `check_for_reorg_and_reconcile` to clamp
  cursors to `reorg_point - 1`. This is a change to the payment-detection core, so it lands on
  its own, before any other part-5 task, and gets its own review.

#### 5.1 Engine: custody registry and per-tenant resolution

- **Aim:** The engine runs one `KeyCustody` per enabled backend and every
  key operation uses the tenant's own backend.
- **Verify:**
  - Unit test with two in-process backends (two `PlainKeyCustody`
    instances registered under different names): tenant A on one, tenant B
    on the other. Order creation for each derives the right address. A
    scan tick matches each tenant's payment only through its own backend
    (a counting wrapper like the one in `scanner.rs` tests shows zero
    calls to the wrong backend).
  - Unit test: boot registration puts each stored tenant in the backend
    its row names, and logs and skips a tenant whose backend isn't
    enabled.
  - Unit test: a tenant whose backend returns `UnknownWallet` mid-tick
    (the switch race) keeps its cursor, other tenants scan normally, and
    the next tick scans it with the new handle (builds on 5.0).
  - Migration tests. The old setting is converted from the value that was
    actually in effect (environment variable, then stored value, then
    default), because an operator may have set only
    `ENGINE_KEY_CUSTODY_BACKEND`:
    - Env `socket`, nothing stored: `enabled_backends = "socket"`,
      `default_backend = "socket"`.
    - Stored `socket`: the same.
    - Nothing anywhere: `plain` for both.
    - Every existing tenant row's `key_custody_backend` is set to that
      value. Today every tenant is registered in the one backend in
      effect, whatever its row says (rows created before WBS 2.1.3 say
      `plain` regardless). Both backends seal the same bytes (the
      key-custody server wraps `PlainKeyCustody`), so relabelling moves no
      key material.
    - The old `key_custody.backend` row is deleted, and a later save of
      that key is refused (1.5).
    - An invalid old value converts to `plain`, as `build_key_custody`
      treats it today.
    - The migration runs once. It records a marker (a settings row), so
      with `ENGINE_KEY_CUSTODY_BACKEND` still set, a later boot doesn't
      relabel tenant rows again and undo stores that were switched since.
  - Test: `create_order` racing a switch retries with the new handle and
    succeeds (drive it with a custody wrapper that removes the wallet
    between the lookup and `derive_subaddress`).
  - Test: `resolve_wallet_handle` for a tenant on a disabled backend
    returns an error and registers nothing.
- **Approach:**
  - Add `CustodyRegistry { backends: HashMap<String, Arc<dyn KeyCustody>> }`
    and `TenantWallets: HashMap<String, (String, WalletHandle)>` behind
    shared handles in `AppState`, replacing `key_custody`,
    `key_custody_backend` and `wallet_handles`.
  - Add a helper, `state.wallet_for(tenant_id) -> Option<(Arc<dyn KeyCustody>, WalletHandle)>`,
    and switch every call site listed in the design to it.
  - `run_scan_tick` takes the registry and the per-tenant pairs. Group
    tenants by backend inside the tick so each backend's table caching
    (see the `KeyCustody` trait docs) still works as before.
  - Add a one-time migration, run at boot before the registry is built,
    converting `key_custody.backend` as above and rewriting the tenant
    rows. Remove `key_custody.backend` from the settings declarations in
    the same change. If `ENGINE_KEY_CUSTODY_BACKEND` is still set after
    the migration has run, log a warning naming the settings that
    replaced it.
  - The bootstrap CLI (`--bootstrap-wallet`) gets
    `--key-custody-backend <name>`, defaulting to the default backend. It
    also checks the typed `--primary-address` against the keys and
    refuses a mismatch; today it stores the address unchecked, and a
    mistyped one would make the store impossible to switch later.
  - When `create_order` or `lookup_payment` still get `UnknownWallet`
    after the retries, return `503` (try again), not `404` as
    `http/mod.rs` maps it today.

#### 5.2 Engine: enabling backends from settings, live

- **Aim:** Saving `key_custody.enabled_backends`, `default_backend` or
  `socket_path` updates the registry at once, with the rules in the
  design.
- **Verify:**
  - Integration test (reuse
    `crates/key-custody-server/tests/socket_key_custody.rs`'s setup):
    enable `socket` with a running key-custody server. A tenant created
    with `socket` seals through it and its row says `socket`.
  - Test: enabling `socket` with a path where nothing listens is refused
    at save, and nothing is persisted.
  - Test: `default_backend` not among the enabled ones, or no backend
    enabled, is refused with a clear message.
  - Test: disabling a backend with 2 tenants on it succeeds, returns
    `warnings.unserved_custody = [{ "backend": "socket", "tenants": 2 }]`,
    stops scanning those tenants, and leaves their sealed rows unchanged.
    Re-enabling it registers them again and scanning resumes.
  - Test: changing `socket_path` to another running server re-registers
    that backend's tenants against the new server.
- **Approach:** Prepare builds a `KeyCustody` for each newly enabled
  backend (or a changed socket path) and makes one round trip to prove it
  answers: register a throwaway wallet made from a fixed test key, then
  remove it. Install swaps the registry, registers the tenants of newly
  enabled backends from their sealed rows, and removes the tenants of
  disabled backends from the handle map. `GET /api/v1/admin/settings`
  reports `tenant_count` per backend for the page's confirmation (4.4
  pattern).

#### 5.3 Engine API: choose a backend at creation, switch later

- **Aim:** Monokulo can ask which backends are available, create a tenant
  on a chosen backend, and move a tenant to another backend by providing
  its keys again.
- **Verify:**
  - Test: `GET /api/v1/key-custody` (instance-admin auth, like the
    settings API) returns the enabled backends with descriptions and the
    default.
  - Test: `POST /api/v1/admin/tenants` with `key_custody_backend` uses it. An
    unknown or disabled backend is refused with `400`. Omitting it uses
    the default.
  - Test: `PUT /api/v1/admin/tenant/key-custody` (tenant auth, next to the
    other `/api/v1/admin/tenant/...` routes) with
    `{ backend, view_key_hex, spend_pubkey_hex }` and the store's own keys
    moves the tenant. Its row has the new backend and new sealed bytes, it
    keeps its orders, and a payment to an existing order's subaddress is
    still matched after the switch.
  - Test: the same call with another wallet's keys is refused, and nothing
    changes.
  - Test: switching to the backend the tenant already uses, with correct
    keys, succeeds and changes nothing. (Recovering from a socket server
    that lost its state needs no keys; see 5.8.)
  - `TenantView` includes `key_custody_backend`.
- **Approach:** Add the endpoint in `http/admin.rs` next to
  `patch_own_tenant`. Steps: check the backend is enabled; run the
  same-wallet check (decoded keys and network against `primary_address`,
  as in the design); register the material in the new backend;
  seal; update the row's `key_custody_backend` and `sealed_key_material`
  in one statement (new `Store::update_tenant_key_custody`); swap the
  handle map entry; remove the old registration. If anything fails after
  registering in the new backend, remove that registration before
  returning, the same clean-up `create_tenant` already does.

#### 5.4 Monokulo: choose a backend when adding or connecting a store

- **Aim:** The add-store form (`http/dashboard.rs`) and the connect flow
  (`http/connect.rs`) offer a key storage choice when more than one
  backend is enabled, with the default preselected and a one-line
  description of each. With one backend enabled, no choice is shown.
- **Verify:**
  - Handler tests with a mock engine reporting two backends: the form
    shows a select with both and the default selected, and the chosen
    value is sent to `POST /api/v1/admin/tenants`.
  - With one backend: no select, and no `key_custody_backend` is sent.
  - Existing add-store and connect tests pass unchanged.
- **Approach:** Monokulo fetches `GET /api/v1/key-custody` through
  `EngineClient` using the engine admin token, caching it briefly like the
  status cache does. Descriptions come from the engine (4.2 pattern), so
  a new backend needs no monokulo change.

#### 5.5 Monokulo: switch a store's backend from its settings page

- **Aim:** The store settings page (`views/store_settings.rs`) has a "Key
  storage" section showing the current backend and, when another is
  enabled, a form to move the store: a backend select plus the private
  view key and public spend key fields. It says plainly that the keys
  must be entered again and must be this store's own wallet. It works
  without JavaScript.
- **Verify:**
  - Handler test: submitting with the right keys calls the engine's
    switch endpoint and shows "Key storage changed to <backend>".
  - Handler test: the engine's "different wallet" refusal is shown on the
    form, with the key fields cleared (never re-rendered with the view
    key in them).
  - View test: the section is hidden when only the store's own backend is
    enabled, and shows the "no longer enabled" state (5.7) when the
    store's backend has been disabled.
  - Playwright (on the `coverage_fixture` setup): the section at phone and desktop width.
- **Approach:** A new POST route under the store settings path, handled
  like the add-store form: validate hex shape in monokulo for a friendly
  error, then forward to the engine. The key fields use
  `autocomplete="off"`. Monokulo doesn't log or keep the submitted keys.

#### 5.6 Admin page: key custody section

- **Aim:** The admin page's key custody section has checkboxes for the
  enabled backends, a select for the default, the socket path, a short
  description of each backend, and "Used by N stores" per backend. With
  JavaScript, unticking a backend that stores use asks for confirmation,
  as in 4.4.
- **Verify:** View test for the controls and counts. Playwright for the
  confirmation, as 4.4. Save tests from 5.2 through the admin page.
- **Approach:** Same rendering and banner mechanism as parts 4.4 to 4.6.

#### 5.7 Merchant alert for stores whose backend is disabled or down

- **Aim:** A merchant whose store's key storage is disabled or unavailable
  sees a red alert at the top of every page except the POS app. It names
  the store and links to its "Key storage" section.
- **Verify:** Same shape as 3.7's tests, driven by the engine status
  reporting the backend as disabled or unhealthy for that store.
- **Approach:** The engine's `/status` reports, per backend, whether it is
  enabled and whether it answers, and puts each affected tenant in
  `unserved_tenants` with reason `custody_disabled` or
  `custody_unavailable` (3.7). For `custody_disabled`, the alert links to
  the store's "Key storage" section, since entering the keys under an
  enabled backend is the fix. For `custody_unavailable` it says the
  store's payments will show up once the key storage service is back.

#### 5.8 Socket backend recovers by itself

- **Aim:** A key-custody server that stops, restarts or loses its memory
  never needs an engine restart or keys entered again. The engine starts
  even when the socket is down.
- **Verify:** Integration tests using the setup in
  `crates/key-custody-server/tests/socket_key_custody.rs`:
  - Stop the server with socket tenants registered; scanning for them
    fails and they fall behind while plain tenants scan normally. Start
    it again; the engine reconnects, registers the socket tenants again
    from their sealed rows, and they catch up (5.0). Every payment made
    while it was down is recorded exactly once.
  - A socket-to-plain switch racing a reconnect: the tenant ends on plain,
    with no socket registration left behind (the server reports no wallet
    for it).
  - An `UnknownWallet` caused by the switch race doesn't trigger a
    reconnect or re-registration of other tenants (the canary is still
    known).
  - Start the engine with the socket down: it serves, plain tenants scan,
    `/status` shows the socket backend unavailable, and once the server
    starts the socket tenants come back.
- **Approach:** Wrap `SocketKeyCustody` in a reconnecting backend in the
  engine. On `BackendUnavailable`, reconnect with backoff, then register
  every tenant whose row names this backend from its sealed bytes, and
  replace their handles in `TenantWallets`. Only one reconnect runs at a
  time.
  - `UnknownWallet` alone is *not* a reason to reconnect: it's also the
    normal result of the switch race. To tell "the server lost its memory"
    apart, the engine registers a canary wallet (a fixed test key) on
    every connect, and asks for it when it sees `UnknownWallet`. Only an
    unknown canary triggers re-registration.
  - Every handle-map update is a per-tenant compare-and-swap on the
    backend name. A re-registration that finds the tenant's row now names
    a different backend (it was switched meanwhile) drops the handle it
    just made and removes that registration, so it can never overwrite a
    newer handle or leak a registration. Replace the
  `exit(1)` in `build_key_custody` with "start unavailable and keep
  trying". This relies on 5.0, so a store that couldn't be scanned while
  the socket was down loses nothing.

---

### 6. Tooling, docs and end-to-end proof

#### 6.0 A harness that runs the real binaries

- **Aim:** End-to-end tests can start the real `scanner` and `monokulo`
  binaries, with their real `main` and boot wiring, against empty
  databases and a local fake Monero daemon. Today nothing does:
  `coverage-real.config.js` runs `examples/coverage_fixture.rs`, which
  uses the in-process `engine-test-support` engine, and `global-setup.js`
  builds `e2e-harness`, which builds its own `AppState` and scan loop. The
  original bug lived in `main.rs`, so a test that skips `main` couldn't
  have caught it.
- **Verify:** A smoke spec starts both binaries through the harness and
  loads the status page. 6.3 is the regression test for the original bug:
  it fails against today's `main.rs`, which should be checked once when
  the harness is first written.
- **Approach:** A Playwright global setup that builds and spawns
  `target/debug/monokulo-engine` and `target/debug/monokulo` with temporary
  database paths and free ports, plus the fake daemon from 2.1 (moved into
  `engine-test-support`) as a small binary. Tear everything down after
  the run. It needs `MONOKULO_ENCRYPTION_KEY` and an engine admin token,
  both generated per run as `dev-run.sh` does.
  - Monokulo hardcodes its database path (`monokulo.db`) and port (8081)
    in `main.rs`. Add `MONOKULO_DB_PATH` and `MONOKULO_BIND` environment
    variables (boot-only, like `MONOKULO_ENCRYPTION_KEY`) so the harness
    can run it on a free port and a temporary database.
  - The fake daemon replays canned responses; it can't produce new
    payments to freshly created subaddresses. That would need a
    `get_blocks.bin` encoder (the `monero-epee` crate only decodes) plus
    building transactions, which is a project of its own. So the harness
    covers flows that need no new payment (6.3). Payment-dependent checks
    in 6.4, 5.0 and 5.8 run as Rust integration tests with the in-process
    `FakeDaemonClient`, which can serve any transaction.

#### 6.1 Dev script

- **Aim:** `scripts/dev-run.sh start` on a fresh checkout gives a working
  stagenet setup with no restart.
- **Verify:** Manual check, noted in the PR: remove `.dev-run/`, run
  `scripts/dev-run.sh start`. The status page shows stagenet healthy and
  connecting a stagenet store works.
- **Approach:** No change needed once 2.1 lands. Update the comment above
  `ensure_engine_settings`, and add
  `key_custody.enabled_backends = "plain"` to the settings it posts.

#### 6.2 Boot messages and docs

- **Aim:** The engine's "no Monero node is configured" boot warning and
  the docs describe settings as applied on save, and per-store key
  custody as the model.
- **Verify:** Read through. `grep -rni "restart" docs/ README.md` shows no
  stale advice about settings. `docs/DESIGN.md` §8.1's note on
  `tenants.key_custody_backend` describes the per-store model.
- **Approach:** Reword the warning in `crates/engine/src/main.rs`. Update
  `docs/DESIGN.md`, `README.md` and the `KeyCustody` module docs
  (`crates/shared/src/key_custody.rs`, `crates/engine/src/key_custody/mod.rs`),
  which describe one backend per process.

#### 6.3 End-to-end: fresh instance, configure from the page, use it

- **Aim:** Prove the reported scenario is fixed, and the new warnings
  work, through the real UI with real processes.
- **Verify:** Playwright test on the 6.0 harness (real engine and
  monokulo binaries, the local fake daemon rather than stagenet so it runs
  offline):
  1. Start both with empty databases.
  2. On the admin page, save a stagenet node pointing at the mock daemon.
  3. The status page shows stagenet with no restart.
  4. Connecting a stagenet store succeeds.
  5. Change `payment.mempool_poll_interval_ms`; the status page's
     expected-interval figure updates.
  6. Save `server.worker_threads`; the restart banner appears.
  7. Clear the stagenet node, accept the confirmation dialog; the red
     banner says 1 store is affected, and the merchant's dashboard shows
     the alert while the POS page doesn't.
  8. Restore the node; the alert goes away.
- **Approach:** A spec on the 6.0 harness.

#### 6.4 End-to-end: switch a store's key custody

- **Aim:** Prove a store can move between backends through the UI and
  keep working.
- **Verify:**
  - Playwright on the 6.0 harness, plus a real `key-custody-server`:
    enable `plain` and `socket`; add a store on `plain`; create an order;
    switch the store to `socket` with its keys; the order still shows and
    a new order gets an address. Stop the key-custody server: the
    merchant sees the alert. Start it again: the alert goes. Disable
    `socket`: the merchant sees the alert and the store page offers the
    switch back.
  - Rust integration test (in-process fake daemon, real key-custody
    server): a payment to the first order's subaddress after the switch is
    matched, and a payment made while the key-custody server was stopped
    is matched once it's back, with nobody doing anything (5.8).
- **Approach:** As 6.3, starting the key-custody server from the test
  setup the way `crates/key-custody-server/tests/socket_key_custody.rs`
  does.

---

### 7. Engine resilience and scale

The engine has to keep detecting payments through key-custody sidecar
failures, database errors, flaky or lying nodes, and hundreds to thousands
of stores, and be fair between stores while it does. A sweep of the
current code found the gaps below. Each is independent of the settings
work, and 7.1 to 7.3 matter even at today's scale.

Principles every task here follows:
- **Contain failures to the smallest unit.** One store, one backend, one
  node or one webhook endpoint failing must not hold up anything else.
- **Never lose, never double-count.** Progress markers only move past work
  that was recorded; recording is idempotent (the existing
  `UNIQUE(order_id, txid, output_index)` upsert).
- **Always recover without a restart or a human**, and make being
  degraded visible on `/status` and in merchant alerts while it lasts.
- **Bound everything:** time per call, work per tick, memory per store,
  queue length.

#### 7.1 A panic can't take the whole engine down

Done in commit 97817e1 (see the progress notes).

- **Aim:** No panic, anywhere, leaves the engine permanently broken.
- **Why:** The store is an `Arc<std::sync::Mutex<Store>>`
  (`store.rs`, `SharedStore`), and it and the other shared maps are
  locked with `.lock().unwrap()` about 200 times in non-test engine code.
  If anything panics while holding the store lock, the mutex is poisoned,
  and from then on every `.lock().unwrap()` panics: the supervisor
  (`shared/src/supervise.rs`) restarts the scan loop every 5 seconds, it
  panics straight away, and every HTTP handler fails, until someone
  restarts the process. There are also `.unwrap()`/`.expect()` calls in
  loop code paths (for example `serde_json::to_string(..).unwrap()` in
  `scan_transaction`, `expect("failed to list tenants at boot")` in
  `register_all_tenants`).
- **Verify:**
  - Unit test: a test that panics while holding the store lock, after
    which the scan tick, the webhook tick and an HTTP handler all still
    work.
  - `cargo clippy -p engine -- -D clippy::unwrap_used -D clippy::expect_used`
    passes for non-test code (allowed only on documented invariants with
    `#[allow]` and a comment).
- **Approach:** Switch shared locks to `parking_lot` (no poisoning), or a
  small wrapper that recovers the guard from a poisoned lock and logs it.
  Replace loop-path `unwrap`/`expect` with errors that the tick logs and
  retries. At boot, a failing `list_active_tenants` retries with backoff
  instead of panicking.

#### 7.2 Keep CPU-heavy scanning off the async runtime

- **Aim:** HTTP requests (including monokulo's checkout polling and order
  creation) stay fast while scanning is busy, at any number of stores.
- **Why:** Scanning is elliptic-curve work done inside async tasks
  (`scan_tx_outputs` in `PlainKeyCustody`), on a runtime with 2 worker
  threads by default. With many stores and a busy mempool, scan work
  occupies the workers and HTTP handlers queue behind it. `scanner.rs`'s
  own comment says to revisit this "if profiling ever shows contention".
- **Verify:** Load test (7.12): with 1,000 active stores and a 200-tx
  mempool, `GET /status` and order creation p99 latency stay under a set
  bound (say 200 ms) while ticks run.
- **Approach:** Run `PlainKeyCustody`'s scan work on a dedicated pool
  (`spawn_blocking` with a semaphore, or a `rayon` pool sized to spare
  cores), and scan independent (transaction, store) pairs in parallel
  within a tick. HTTP handlers never wait on that pool.
  - Parallel scans would contend inside `PlainKeyCustody` today: a scan
    holds the wallets map's read lock for its whole duration, and each
    wallet's table cache sits behind one mutex (`key_custody/plain.rs`).
    Take an `Arc` of the wallet's material and table under a brief lock,
    then scan without holding any lock, so registering or removing a
    wallet (tenant creation) never waits behind scans. (7.1 already moved
    these locks to `parking_lot`, whose `RwLock` doesn't starve writers.)

#### 7.3 Don't redo the same work every second, and cap each store's scan cost (D10)

- **Aim:** Work per tick grows with what's new, not with the size of the
  mempool times the number of stores, and no store's scan cost grows
  without bound over its lifetime.
- **Why:**
  - Every tick (1 second by default) fetches the whole mempool with full
    transaction bodies (`get_transaction_pool`, `daemon_rpc.rs`) and
    scans every transaction against every active store again. Under a
    mempool spam wave that is megabytes per second and
    stores × transactions scans per second.
  - Each scan clones the store's whole subaddress lookup table
    (`table.clone()` in `key_custody/plain.rs`). The table covers
    `0..next_minor_index`, which grows by one with every order a store
    ever creates.
  - There is a cliff: `MAX_SCAN_TABLE_ENTRIES = 1_000_000`
    (`key_custody/plain.rs`). A store whose `next_minor_index` passes it
    gets `ScanFailed` on every scan. Today that stalls block scanning for
    the whole network, for good; after 5.0 it would leave that store
    behind for ever.
- **The scan window (D10).** Each store is scanned only for the indices
  of its orders that are open, or were closed within the grace period
  (`payment.expired_order_grace_period_minutes`). This caps table size by
  the number of recent orders, not the store's age.
  - Orders need a close time to define "closed within the grace period".
    The orders table has none (only `updated_at`). Add `closed_at_utc`,
    set when an order first reaches a terminal status (`paid`,
    `overpaid`, `expired`), cleared if it leaves one (a reorg can move
    `paid` back to `confirming`). Backfill existing terminal orders with
    `updated_at`.
  - Window: status is non-terminal, or `closed_at >= now - grace`. This
    keeps a paid order in view during the grace period, so a second
    payment within it still turns the order `overpaid`. Today's
    `active_tenant_ids` drops paid orders at once, so this is a small
    improvement as well as a cap.
  - Catch-up (5.0) must not use today's window for old blocks. A store
    whose cursor sits at block time T is caught up against orders that
    were open at any point since T: non-terminal now, or
    `closed_at >= T - grace`. Orders that closed during the gap are
    included, so nothing paid during the gap is missed.
  - A payment to an order outside the window isn't detected
    automatically. `lookup_payment` (`http/admin.rs`) scans the store's
    full index range and records a match for any order regardless of
    status, re-deriving an expired order to paid, so the merchant can
    always recover it by entering the txid. The store page's "look up a
    payment" help text says when that's needed (payments sent after an
    order closed).
- **The scan API has to take a set of indices.** Today
  `KeyCustody::scan_tx_outputs` takes two contiguous ranges
  (`shared/src/key_custody.rs`), and so does the socket wire format
  (`ScanTxOutputsRequest`, `key-custody-service`). A window isn't
  contiguous.
  - Add `scan_tx_outputs_indices(handle, tx, indices: &IndexSet)` to the
    trait, where `IndexSet` carries the indices plus a generation number
    that changes when the set does.
  - Add the matching request to the wire protocol, and a protocol version
    exchanged on connect. An engine talking to an older sidecar falls
    back to the range call covering `min..=max` of the set (correct, just
    less efficient) and logs that the sidecar should be upgraded.
  - `PlainKeyCustody` keeps one table per wallet for the live window and
    updates it incrementally when the generation changes (adds new
    indices, drops closed ones) rather than rebuilding it. `lookup_payment`
    and catch-up build a temporary table for their own range and never
    replace the live one, so they can't throw it out of the cache.
  - The table is shared as an `Arc`, never cloned per scan.
- **Mempool:**
  - Fetch `get_transaction_pool_hashes` each tick and fetch bodies only
    for hashes not seen before.
  - Keep a per-network record of (txid, store, window generation) scanned.
    A pair counts as scanned only after the scan *succeeded*; a failed
    scan (a custody backend that was down) is retried next tick, so the
    store doesn't miss that transaction until it's mined. Entries are
    dropped when the txid leaves the pool. A store whose window changed
    rescans the pool only for indices that are new since its last scan.
- **Verify:**
  - Unit test: two ticks against an unchanged mempool make zero
    key-custody scan calls on the second tick, and the second tick fetches
    only pool hashes.
  - Unit test: a scan that fails for one store is retried on the next tick
    against the same pool, and the payment is recorded then.
  - Unit test: a payment to an open order's index matches. A second
    payment to a paid order within the grace period turns it `overpaid`.
    A payment to an order closed longer ago than the grace period is not
    matched by the scan, and is recorded by `lookup_payment`.
  - Unit test: catch-up after a gap longer than the grace period matches a
    payment to an order that was open during the gap and has closed since.
  - Unit test: a store with 1,200,000 orders of which 50 are open scans
    normally (no `ScanFailed`), and its table has about 50 entries.
  - Unit test: the table is updated incrementally when an order opens or
    closes (a counting test double shows no full rebuild), and a
    `lookup_payment` call doesn't evict the live table.
  - Protocol test: a new engine against a sidecar that only knows the
    range call still detects payments.
  - Benchmark: scanning one transaction costs the same for a store with a
    large window as for a small one (no per-call table copy).
- **Approach:** As above. Migration for `closed_at_utc` with the
  backfill; set and clear it in `recompute_order_status`. The window query
  replaces the `0..next_minor_index` range in `run_scan_tick`.

#### 7.4 Fair, parallel scanning across networks and stores

- **Aim:** No network, store or backend can delay another's payment
  detection beyond a set bound.
- **Why:**
  - Networks are scanned one after another in one loop (`run_scanner_loop`,
    `main.rs`, whose doc comment accepts that a slow node on one network
    delays the others).
  - Within a tick, stores are scanned one by one. A store on a slow
    backend (a socket sidecar that answers slowly but doesn't fail) holds
    up every store after it.
  - Catch-up (5.0) must not starve the live scan.
  - The double-spend revalidation loop (`run_double_spend_revalidation_loop`,
    `main.rs`) also walks networks one after another, and the reorg check
    makes one `get_block_hash` call per height in its window, so a slow
    node on one network delays those for the others too.
- **Verify:**
  - Test: a fake daemon that takes 10 s per call on stagenet doesn't
    delay mainnet ticks.
  - Test: one store whose custody calls take 2 s each doesn't delay
    detection for the other stores in the same tick beyond the per-store
    budget.
  - Test: with a large catch-up backlog, live-scan tick time stays within
    its budget and the backlog still shrinks every tick.
- **Approach:** One supervised loop per network, each with its own status
  entry, for both the scan loop and the double-spend revalidation loop. Within a tick, scan stores concurrently with a bounded pool,
  with a per-backend concurrency cap and a per-call deadline. A store
  whose calls hit the deadline is treated as a per-store failure (5.0)
  for this tick. Catch-up gets a fixed share of each tick's time, and
  lagging stores are served round-robin.

#### 7.5 Key-custody sidecar throughput and failure handling

- **Aim:** The socket backend scales to many stores, and its failures stay
  contained to its own stores.
- **Why:** `SocketKeyCustody` uses one connection behind a
  `tokio::sync::Mutex`, so every call from every store is serialised
  (`key-custody-service/src/client.rs`). With thousands of stores that
  one connection is the bottleneck. Reconnecting is covered by 5.8.
- **Verify:**
  - Test: with 8 concurrent callers, throughput scales above the
    single-connection rate, against a real `key-custody-server`.
  - Test: a call that exceeds the call timeout fails only that call,
    doesn't poison the other connections, and is reported as a per-store
    failure.
- **Approach:** A small pool of connections (size from a new
  `key_custody.socket_connections` setting), each used by one call at a
  time. A transport error on one connection replaces that connection
  only. The full re-registration of the backend's stores runs only when
  the canary wallet (5.8) is unknown. Keep the per-call timeout. The pool
  and 5.8's reconnect logic live in the same wrapper.
  - The sidecar (`key-custody-server`) already handles each connection in
    its own task, but behind it is one `PlainKeyCustody` with the same
    locks as the engine's. So the server-side locking change from 7.2
    applies there too, and the throughput test runs against a real
    server, not only one with an artificial delay.

#### 7.6 Node failures and inconsistent nodes

- **Aim:** Slow, dead or disagreeing nodes cost bounded time and never
  cause a wrong result.
- **Why:**
  - `FallbackDaemonClient` starts each call from the node that last
    answered and moves on when it fails, each attempt with a 15 s timeout
    (`daemon_rpc.rs`). When every node is dead, each call costs 15 s per
    node, and a tick makes many calls, so a tick can take minutes. There
    is no cooldown for a node that just failed.
  - Consecutive calls in one tick can go to different nodes, which may be
    at different heights or on different forks. Height from one node and
    blocks from another can disagree.
  - Response bodies have no size limit beyond the timeout.
- **Verify:**
  - Test: with the first node dead, after one failure it is skipped for a
    cooldown period, so later calls don't wait on it; after the cooldown
    it is tried again, and used again once it answers.
  - Test: one call's total time across fallbacks never exceeds its
    deadline.
  - Test: a tick pins to one node; if that node fails mid-tick, the tick
    ends and the next tick picks a node, rather than mixing answers
    from two nodes in one pass.
  - Test: a response over the size limit is refused as a node error.
- **Approach:** Per-node health with exponential cooldown in
  `daemon_fallback.rs`. An overall deadline per call. A "session" that
  pins one node for a tick's worth of calls. A response size limit (in
  line with `payment.scan_chunk_memory_budget_mb`).

#### 7.7 Database failures

- **Aim:** A failing or full database pauses progress without losing or
  corrupting anything, and everything resumes by itself once it's fixed.
- **Why:** Scan and webhook ticks already treat most store errors as
  "retry next tick", but this hasn't been tested against real database
  failures, and boot panics on some (7.1). One connection also serves
  both HTTP handlers and the scanner, so a long write blocks reads.
- **Verify:**
  - Tests using SQLite's `max_page_count` pragma to simulate a full disk
    mid-tick: nothing is marked scanned past a failed write, no payment is
    lost once space is freed, and HTTP handlers return `503` rather than
    panicking.
  - Test: a store write error while recording a match leaves the block
    unscanned for that tenant only (5.0), and it is recorded on the next
    tick.
  - Test: the engine boots and serves `/status` (reporting the database
    error) when a boot-time query fails, and recovers once it succeeds.
- **Approach:** Map store errors in HTTP handlers to `503`. Group each
  tick's writes for one block into one transaction (with 5.0's cursor
  writes). Consider a second read-only connection for HTTP reads (WAL mode
  already allows concurrent readers), if 7.12 shows contention.

#### 7.8 Webhook delivery that is fair and can't back up

- **Aim:** One store's slow or failing webhook endpoint can't delay
  another store's webhooks, and a backlog drains steadily.
- **Why:** `run_delivery_tick` (`webhook_delivery.rs`) takes the 50 oldest
  due deliveries and sends them one at a time, each with up to a 5 s
  timeout. A store with a slow endpoint and many events can fill the batch
  and hold up every other store for minutes. `now` is read once per
  batch, so retries scheduled late in a long batch are computed from a
  stale time.
- **Verify:**
  - Test: store A's endpoint takes 5 s per request and has 500 pending
    deliveries; store B's single delivery is sent within one tick.
  - Test: 50 stores with pending deliveries are all served within a
    bounded number of ticks (round-robin).
  - Test: a retry is scheduled relative to the time of its own attempt.
- **Approach:** Pick due deliveries round-robin across stores (at most a
  few per store per tick), send them concurrently with a global cap and a
  per-store cap of 1 or 2, and read the time per attempt.

#### 7.9 Loops that hang instead of failing

- **Aim:** A loop that stops making progress (an `.await` that never
  returns) is detected and restarted, not just a loop that panics.
- **Why:** The supervisor restarts a loop that panics or returns
  (`supervise.rs`), with a fixed 5 s delay. A tick stuck on an await with
  no timeout would stop scanning silently. `/status` shows staleness, but
  nothing acts on it.
- **Verify:**
  - Test: a tick that never completes is cancelled after its deadline,
    logged, and the next tick runs.
  - Test: a loop that panics repeatedly backs off (5 s, 10 s, 20 s, up to
    a cap), and the restart count shows on `/status`.
- **Approach:** Wrap each tick in `tokio::time::timeout` with a deadline
  derived from the poll interval and 7.4's budgets. Add backoff and a
  restart counter to `supervise`.

#### 7.10 HTTP handlers under pressure

- **Aim:** The engine's API degrades by refusing work, not by piling up.
- **Verify:** Test: with the store lock held by a slow operation, requests
  time out with `503` after the request deadline, and a flood beyond the
  concurrency limit gets `503` immediately rather than queueing without
  bound. Test: order-event streams open longer than the request deadline
  stay open, and don't count against the request concurrency limit.
- **Approach:** `tower` timeout and concurrency-limit layers in
  `build_router`, with limits from settings (live, part 1). The
  long-lived order-event stream (`order_events`,
  `/api/v1/admin/tenant/events`) is excluded from both: monokulo's
  `LiveHub` holds one open per watched store, so a request timeout would
  cut streams off and a shared concurrency limit would fill up with them.
  Streams get their own cap on open streams instead. Handlers that
  touch key custody use the 5.1 retry and return `503` when the backend is
  down.

#### 7.11 Crash safety and clean shutdown

- **Aim:** Killing the engine at any moment and starting it again never
  loses or double-counts a payment or a webhook.
- **Verify:**
  - Crash-injection test: run ticks with payments arriving, kill the
    engine task at random points (between recording a match and marking
    the block scanned, mid-webhook, mid-switch), restart from the same
    database, and check every payment is recorded once and every webhook
    is delivered at least once.
  - The same check with a real process: on the 6.0 harness, `kill -9` the
    engine binary during ticks and restart it on the same database.
    (Killing a task inside one process doesn't show what SQLite does when
    the process really dies.)
  - Test: on SIGTERM the engine stops accepting requests, lets the current
    tick finish (up to a deadline), and exits cleanly.
- **Approach:** Mostly verification; the idempotent recording and
  "mark scanned last" rules already aim at this. Add SIGTERM handling in
  `main.rs` with graceful shutdown of the listener and loops.

#### 7.12 Load and chaos test suite

- **Aim:** Evidence, repeatable in CI, that the engine meets the above at
  realistic scale.
- **Verify:** A test binary (or `#[ignore]` tests run in a separate CI
  job) that runs the real scan loop with the in-process fake daemon and a
  fake custody backend with configurable latency and failure rate:
  - Scale: 1,000 and 5,000 active stores, a 200-transaction mempool, a
    block every few seconds. Record tick time, time from payment to
    detection per store (max and p99), memory, and HTTP p99.
  - Chaos: random node errors, slow nodes, custody errors and slowness,
    database write failures, reorgs and process restarts.
  - Pass criteria: every payment detected exactly once; the worst
    store's detection delay stays within a set multiple of the median
    while nothing is failing for it; the engine recovers fully after each
    injected fault ends.
- **Approach:** Build on 5.0's property test and the existing
  `FakeDaemonClient`. It needs a generator for real payment transactions
  to many stores' subaddresses; reuse the transaction fixtures the scanner
  tests already build, and put the generator in `engine-test-support`.
  Keep the thresholds in the test so regressions fail CI.

#### 7.13 Seeing what's wrong

- **Aim:** An operator can tell from `/status` and the logs what is
  degraded, for which stores, and since when, without repeated identical
  log lines drowning everything.
- **Verify:** Test: `/status` reports per network the tick duration,
  lagging stores and blocks behind, per-backend custody health, per-node
  health and cooldown, webhook backlog per store, and loop restart counts.
  Test: a repeated identical error is logged once per interval with a
  count.
- **Approach:** Extend `EngineStatusResponse`. Move from `eprintln!` to
  `tracing` with store id, network and backend as fields, and rate-limit
  repeated errors.
