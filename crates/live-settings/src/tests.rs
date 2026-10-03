//! The library on its own: an in-memory store, test sections, and
//! reloadables that count what they prepare, install and drop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Barrier};

use crate::*;

settings! {
    DEPTH: u64 {
        key: "scan.depth",
        default: 20,
        check: range(1, 10_000),
        description: "How many recent blocks are checked again.",
        example: "20",
    },
    POLL_MS: u64 {
        description: "How long to wait between polls.",
        default: 1000,
        key: "scan.poll_ms",
    },
    SOFT: u32 { key: "limits.soft", default: 60, description: "Soft limit." },
    HARD: u32 { key: "limits.hard", default: 300, description: "Hard limit." },
    NODE_A: HttpUrl {
        key: "node.a",
        default: parsed_default("http://a.example"),
        description: "Node A.",
        example: "https://node.example:18081",
    },
    NODE_B: HttpUrl { key: "node.b", default: parsed_default("http://b.example"), description: "Node B." },
    WORKERS: usize {
        key: "server.workers",
        default: 2,
        check: range(1, 256),
        description: "Worker threads.",
        applies: Restart,
    },
    BIND: BindAddr {
        key: "server.bind",
        default: parsed_default("127.0.0.1:8443"),
        description: "Listen address.",
        applies: Restart,
    },
    TOKEN: Secret { key: "engine.token", env: "TEST_TOKEN", default: Secret::default(), description: "Admin token.", sources: [Env], applies: Restart },
}

#[derive(Debug, Clone, PartialEq)]
struct Scan {
    depth: u64,
    poll_ms: u64,
}

impl Section for Scan {
    const NAME: &'static str = "scan";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&DEPTH, &POLL_MS]
    }
    fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(Scan {
            depth: s.get(&DEPTH),
            poll_ms: s.get(&POLL_MS),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Limits {
    soft: u32,
    hard: u32,
}

impl Section for Limits {
    const NAME: &'static str = "limits";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&SOFT, &HARD]
    }
    fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let (soft, hard) = (s.get(&SOFT), s.get(&HARD));
        if soft > hard {
            return Err(vec![FieldError::new(
                SOFT.key,
                "The soft limit can't be above the hard limit.",
            )]);
        }
        Ok(Limits { soft, hard })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct NodeA {
    url: HttpUrl,
}

impl Section for NodeA {
    const NAME: &'static str = "node_a";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&NODE_A]
    }
    fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(NodeA {
            url: s.get(&NODE_A),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct NodeB {
    url: HttpUrl,
}

impl Section for NodeB {
    const NAME: &'static str = "node_b";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&NODE_B]
    }
    fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(NodeB {
            url: s.get(&NODE_B),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Runtime {
    workers: usize,
    bind: BindAddr,
}

impl Section for Runtime {
    const NAME: &'static str = "runtime";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&WORKERS, &BIND]
    }
    fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(Runtime {
            workers: s.get(&WORKERS),
            bind: s.get(&BIND),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Auth {
    token: Secret,
}

impl Section for Auth {
    const NAME: &'static str = "auth";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&TOKEN]
    }
    fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(Auth {
            token: s.get(&TOKEN),
        })
    }
}

/// Holds up a probe's `prepare` or `install` once armed: it reports that it
/// got there, then waits for the test to meet it at the barrier.
struct Gate {
    armed: AtomicBool,
    entered: mpsc::UnboundedSender<()>,
    release: Barrier,
}

impl Gate {
    fn new() -> (Arc<Gate>, mpsc::UnboundedReceiver<()>) {
        let (entered, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Gate {
                armed: AtomicBool::new(false),
                entered,
                release: Barrier::new(2),
            }),
            rx,
        )
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    async fn pass(&self) {
        if self.armed.load(Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            self.release.wait().await;
        }
    }
}

#[derive(Default)]
struct Counters {
    prepared: AtomicUsize,
    dropped: AtomicUsize,
    installed: AtomicUsize,
    in_prepare: AtomicUsize,
    max_in_prepare: AtomicUsize,
    installed_values: parking_lot::Mutex<Vec<String>>,
}

impl Counters {
    fn prepared(&self) -> usize {
        self.prepared.load(Ordering::SeqCst)
    }
    fn dropped(&self) -> usize {
        self.dropped.load(Ordering::SeqCst)
    }
    fn installed(&self) -> usize {
        self.installed.load(Ordering::SeqCst)
    }
}

/// A prepared value that counts its own drops, so a test can see that a
/// refused save released everything it prepared.
struct Token<C> {
    counters: Arc<Counters>,
    value: C,
}

impl<C> Drop for Token<C> {
    fn drop(&mut self) {
        self.counters.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct Probe<C> {
    counters: Arc<Counters>,
    fail: fn(&C) -> bool,
    policy: BootPolicy,
    prepare_gate: Option<Arc<Gate>>,
    install_gate: Option<Arc<Gate>>,
}

impl<C> Probe<C> {
    fn new() -> Self {
        Probe {
            counters: Arc::new(Counters::default()),
            fail: |_| false,
            policy: BootPolicy::Exit,
            prepare_gate: None,
            install_gate: None,
        }
    }
}

#[async_trait]
impl<C: Section + std::fmt::Debug> Reloadable for Probe<C> {
    type Config = C;
    type Prepared = Token<C>;

    async fn prepare(&self, new: &C, _old: &C) -> Result<(Token<C>, Vec<Warning>), FieldError> {
        let now = self.counters.in_prepare.fetch_add(1, Ordering::SeqCst) + 1;
        self.counters
            .max_in_prepare
            .fetch_max(now, Ordering::SeqCst);
        if let Some(gate) = &self.prepare_gate {
            gate.pass().await;
        }
        self.counters.in_prepare.fetch_sub(1, Ordering::SeqCst);
        if (self.fail)(new) {
            return Err(FieldError::new(
                C::keys()[0].key(),
                "Nothing answers at that address.",
            ));
        }
        self.counters.prepared.fetch_add(1, Ordering::SeqCst);
        let token = Token {
            counters: Arc::clone(&self.counters),
            value: new.clone(),
        };
        Ok((
            token,
            vec![Warning::for_key(
                C::keys()[0].key(),
                format!("{} prepared", C::NAME),
            )],
        ))
    }

    async fn install(&self, prepared: Token<C>) {
        if let Some(gate) = &self.install_gate {
            gate.pass().await;
        }
        self.counters.installed.fetch_add(1, Ordering::SeqCst);
        self.counters
            .installed_values
            .lock()
            .push(format!("{:?}", prepared.value));
    }

    fn boot_policy(&self) -> BootPolicy {
        self.policy
    }
}

/// A `MemoryStore` whose writes can be made to fail.
#[derive(Default)]
struct TestStore {
    inner: MemoryStore,
    fail_writes: AtomicBool,
    fail_reads: AtomicBool,
    writes: AtomicUsize,
}

impl TestStore {
    fn with(values: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(TestStore {
            inner: MemoryStore::with(values.iter().copied()),
            ..TestStore::default()
        })
    }
    fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }
    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SettingsStore for TestStore {
    async fn read_all(&self) -> Result<HashMap<String, String>, StoreError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(StoreError::new("database is locked"));
        }
        self.inner.read_all().await
    }

    async fn write_all(
        &self,
        changes: Vec<(&'static str, Option<String>)>,
    ) -> Result<(), StoreError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(StoreError::new("disk full"));
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.write_all(changes).await
    }
}

struct Harness {
    registry: Registry,
    scan: Live<Scan>,
    limits: Live<Limits>,
    node_a: Live<NodeA>,
    node_b: Live<NodeB>,
    runtime: Live<Runtime>,
    auth: Live<Auth>,
    a: Arc<Counters>,
    b: Arc<Counters>,
    rt: Arc<Counters>,
}

async fn build(store: &Arc<TestStore>, env: Env, a: Probe<NodeA>, b: Probe<NodeB>) -> Harness {
    let (a_counters, b_counters) = (Arc::clone(&a.counters), Arc::clone(&b.counters));
    let rt_probe = Probe::<Runtime>::new();
    let rt_counters = Arc::clone(&rt_probe.counters);
    let mut builder =
        Registry::builder_with_env(Arc::clone(store) as Arc<dyn SettingsStore>, ALL, env).await;
    let scan = builder.section::<Scan>();
    let limits = builder.section::<Limits>();
    let node_a = builder.reloadable(a);
    let node_b = builder.reloadable(b);
    let runtime = builder.reloadable(rt_probe);
    let auth = builder.section::<Auth>();
    let registry = builder.build().unwrap();
    Harness {
        registry,
        scan,
        limits,
        node_a,
        node_b,
        runtime,
        auth,
        a: a_counters,
        b: b_counters,
        rt: rt_counters,
    }
}

async fn booted(store: &Arc<TestStore>, env: Env) -> Harness {
    booted_with(store, env, Probe::new(), Probe::new()).await
}

async fn booted_with(
    store: &Arc<TestStore>,
    env: Env,
    a: Probe<NodeA>,
    b: Probe<NodeB>,
) -> Harness {
    let h = build(store, env, a, b).await;
    h.registry.boot().await.unwrap();
    h
}

fn no_env() -> Env {
    Env::fixed(Vec::<(String, String)>::new())
}

/// No environment, and `values` given on the command line, by key.
fn options(values: &[(&str, &str)]) -> Env {
    no_env().with_cli(
        values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn change(key: &str, value: &str) -> (String, Option<String>) {
    (key.to_string(), Some(value.to_string()))
}

fn view<'a>(views: &'a [SettingView], key: &str) -> &'a SettingView {
    views.iter().find(|v| v.key == key).unwrap()
}

fn url(raw: &str) -> HttpUrl {
    HttpUrl::parse(raw).unwrap()
}

fn invalid_keys(err: &SaveError) -> Vec<String> {
    match err {
        SaveError::Invalid(errors) => errors.iter().map(|e| e.key.clone()).collect(),
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[tokio::test]
async fn a_value_resolves_the_command_line_over_stored_over_default_and_reports_its_source() {
    let store = TestStore::with(&[("scan.depth", "30"), ("scan.poll_ms", "500")]);
    let h = booted(&store, options(&[("scan.poll_ms", "250")])).await;

    assert_eq!(
        *h.scan.load(),
        Scan {
            depth: 30,
            poll_ms: 250
        }
    );
    assert_eq!(h.limits.load().soft, 60);

    let views = h.registry.describe();
    assert_eq!(view(&views, "scan.depth").source, SettingSource::Toml);
    assert_eq!(view(&views, "scan.depth").value, "30");
    assert_eq!(view(&views, "scan.poll_ms").source, SettingSource::Cli);
    assert_eq!(view(&views, "scan.poll_ms").value, "250");
    assert_eq!(view(&views, "limits.soft").source, SettingSource::Default);
    assert_eq!(view(&views, "limits.soft").value, "60");

    let snapshot = Snapshot::new(
        store.inner.read_all().await.unwrap(),
        options(&[("scan.poll_ms", "250")]),
    );
    assert_eq!(snapshot.source(&POLL_MS), SettingSource::Cli);
    assert_eq!(snapshot.source(&DEPTH), SettingSource::Toml);
    assert_eq!(snapshot.source(&SOFT), SettingSource::Default);
}

#[tokio::test]
async fn a_blank_environment_variable_is_unset() {
    let store = TestStore::with(&[]);
    let h = booted(&store, Env::fixed([("TEST_TOKEN", "  ")])).await;
    assert_eq!(h.auth.load().token.expose(), "");
    assert_eq!(
        view(&h.registry.describe(), "engine.token").source,
        SettingSource::Default
    );
}

#[tokio::test]
async fn describe_reports_metadata_and_masks_secrets() {
    let store = TestStore::with(&[]);
    let h = booted(&store, Env::fixed([("TEST_TOKEN", "hunter2")])).await;
    assert_eq!(h.auth.load().token.expose(), "hunter2");

    let views = h.registry.describe();
    assert_eq!(
        views.len(),
        ALL.len(),
        "every declared setting is described"
    );
    let token = view(&views, "engine.token");
    assert_eq!(token.value, MASK);
    assert_eq!(token.kind, SettingKind::Secret);
    assert!(
        !serde_json::to_string(&views).unwrap().contains("hunter2"),
        "the secret never leaves describe"
    );

    let depth = view(&views, "scan.depth");
    assert_eq!(depth.env_var, "");
    assert_eq!(depth.cli_flag, "scan-depth");
    assert_eq!(
        depth.description,
        "How many recent blocks are checked again."
    );
    assert_eq!(depth.example, Some("20"));
    assert_eq!(depth.default, "20");
    assert_eq!(depth.applies, Applies::Live);
    assert_eq!(
        depth.kind,
        SettingKind::Integer {
            min: Some(1),
            max: Some(10_000)
        }
    );
    assert_eq!(
        serde_json::to_value(&depth.kind).unwrap(),
        serde_json::json!({ "type": "integer", "min": 1, "max": 10000 })
    );
    assert_eq!(view(&views, "server.workers").applies, Applies::Restart);
}

#[tokio::test]
async fn a_save_with_one_bad_value_changes_nothing_including_the_valid_values() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    let changed = h.scan.subscribe();

    let err = h
        .registry
        .save(vec![
            change("scan.depth", "50"),
            change("scan.poll_ms", "soon"),
            change("node.a", "http://new-a.example"),
        ])
        .await
        .unwrap_err();
    assert_eq!(invalid_keys(&err), ["scan.poll_ms"]);
    assert!(
        err.to_string().contains("whole number"),
        "the message says what to enter: {err}"
    );

    assert_eq!(store.writes(), 0);
    assert_eq!(store.get("scan.depth"), None);
    assert_eq!(
        *h.scan.load(),
        Scan {
            depth: 20,
            poll_ms: 1000
        }
    );
    assert_eq!(h.a.prepared(), 1, "only boot prepared node A");
    assert!(!changed.has_changed().unwrap());

    let err = h
        .registry
        .save(vec![change("scan.depth", "0")])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("from 1 to 10000"), "{err}");
}

#[tokio::test]
async fn unknown_keys_are_refused() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    let err = h
        .registry
        .save(vec![change("scan.depth", "50"), change("scan.nope", "1")])
        .await
        .unwrap_err();
    assert!(
        matches!(err, SaveError::UnknownKey(ref key) if key == "scan.nope"),
        "{err:?}"
    );
    assert_eq!(store.writes(), 0);
}

#[tokio::test]
async fn a_key_submitted_twice_is_refused() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    let err = h
        .registry
        .save(vec![change("scan.depth", "50"), change("scan.depth", "60")])
        .await
        .unwrap_err();
    assert_eq!(invalid_keys(&err), ["scan.depth"]);
    assert_eq!(store.writes(), 0);
}

#[tokio::test]
async fn a_cross_field_rule_refuses_the_save() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    let err = h
        .registry
        .save(vec![change("limits.soft", "500")])
        .await
        .unwrap_err();
    assert_eq!(invalid_keys(&err), ["limits.soft"]);
    assert_eq!(store.writes(), 0);
    assert_eq!(
        *h.limits.load(),
        Limits {
            soft: 60,
            hard: 300
        }
    );

    // Raising both together is fine.
    h.registry
        .save(vec![
            change("limits.soft", "500"),
            change("limits.hard", "900"),
        ])
        .await
        .unwrap();
    assert_eq!(
        *h.limits.load(),
        Limits {
            soft: 500,
            hard: 900
        }
    );
}

#[tokio::test]
async fn a_failing_prepare_refuses_the_save_writes_nothing_and_drops_what_was_prepared() {
    let store = TestStore::with(&[]);
    let mut b = Probe::<NodeB>::new();
    b.fail = |c| c.url.as_str().contains("unreachable");
    let h = booted_with(&store, no_env(), Probe::new(), b).await;
    assert_eq!((h.a.prepared(), h.a.installed()), (1, 1));

    let err = h
        .registry
        .save(vec![
            change("node.a", "http://new-a.example"),
            change("node.b", "http://unreachable.example"),
        ])
        .await
        .unwrap_err();
    assert_eq!(invalid_keys(&err), ["node.b"]);

    assert_eq!(store.writes(), 0);
    assert_eq!(store.get("node.a"), None);
    assert_eq!(h.a.prepared(), 2, "node A was prepared for the save");
    assert_eq!(
        h.a.dropped(),
        h.a.prepared(),
        "and what it prepared was dropped"
    );
    assert_eq!(h.a.installed(), 1, "but not installed");
    assert_eq!(h.node_a.load().url, url("http://a.example"));
    assert_eq!(h.node_b.load().url, url("http://b.example"));
}

#[tokio::test]
async fn a_failing_write_installs_nothing() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    let changed = h.node_a.subscribe();
    store.fail_writes.store(true, Ordering::SeqCst);

    let err = h
        .registry
        .save(vec![
            change("node.a", "http://new-a.example"),
            change("scan.depth", "50"),
        ])
        .await
        .unwrap_err();
    assert!(matches!(err, SaveError::Store(_)), "{err:?}");

    assert_eq!(h.a.installed(), 1, "only the boot install");
    assert_eq!(
        h.a.dropped(),
        h.a.prepared(),
        "the prepared node client was dropped"
    );
    assert_eq!(h.node_a.load().url, url("http://a.example"));
    assert_eq!(h.scan.load().depth, 20);
    assert!(!changed.has_changed().unwrap());
    assert_eq!(view(&h.registry.describe(), "scan.depth").value, "20");
}

#[tokio::test]
async fn a_successful_save_installs_and_readers_see_the_new_value() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    let mut scan_changed = h.scan.subscribe();
    let mut node_changed = h.node_a.subscribe();

    let report = h
        .registry
        .save(vec![
            change("scan.depth", " 50 "),
            change("node.a", "https://new-a.example/"),
        ])
        .await
        .unwrap();
    assert_eq!(report.changed, ["scan.depth", "node.a"]);
    assert!(report.restart_required.is_empty());
    assert!(report.env_overridden.is_empty());
    assert_eq!(
        report.warnings,
        [Warning::for_key("node.a", "node_a prepared")]
    );

    assert_eq!(
        store.get("scan.depth").as_deref(),
        Some("50"),
        "stored in its normal form"
    );
    assert_eq!(
        store.get("node.a").as_deref(),
        Some("https://new-a.example")
    );
    assert_eq!(h.scan.load().depth, 50);
    assert_eq!(h.node_a.load().url, url("https://new-a.example"));
    tokio::time::timeout(Duration::from_secs(1), scan_changed.changed())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), node_changed.changed())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(h.a.installed(), 2);
    assert!(h
        .a
        .installed_values
        .lock()
        .last()
        .unwrap()
        .contains("new-a.example"));
    assert_eq!(
        h.b.prepared(),
        1,
        "an untouched reloadable isn't prepared again"
    );
    assert_eq!(h.rt.prepared(), 1);

    // The same values again: nothing changes, nothing is prepared.
    let report = h
        .registry
        .save(vec![change("scan.depth", "50")])
        .await
        .unwrap();
    assert!(report.changed.is_empty());
    assert!(!scan_changed.has_changed().unwrap());
    assert_eq!(h.a.prepared(), 2);
}

#[tokio::test]
async fn deleting_a_stored_value_goes_back_to_the_default() {
    let store = TestStore::with(&[("scan.depth", "50")]);
    let h = booted(&store, no_env()).await;
    assert_eq!(h.scan.load().depth, 50);
    let report = h
        .registry
        .save(vec![("scan.depth".to_string(), None)])
        .await
        .unwrap();
    assert_eq!(report.changed, ["scan.depth"]);
    assert_eq!(store.get("scan.depth"), None);
    assert_eq!(h.scan.load().depth, 20);
    assert_eq!(
        view(&h.registry.describe(), "scan.depth").source,
        SettingSource::Default
    );
}

#[tokio::test]
async fn a_restart_setting_is_stored_not_installed_and_pending_until_a_new_registry() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    assert_eq!(
        (h.rt.prepared(), h.rt.installed()),
        (1, 1),
        "boot installs restart-only sections"
    );
    let changed = h.runtime.subscribe();

    let report = h
        .registry
        .save(vec![change("server.workers", "8")])
        .await
        .unwrap();
    assert_eq!(report.changed, ["server.workers"]);
    assert_eq!(report.restart_required, ["server.workers"]);

    assert_eq!(store.get("server.workers").as_deref(), Some("8"));
    assert_eq!(h.runtime.load().workers, 2, "the running value stays");
    assert_eq!((h.rt.prepared(), h.rt.installed()), (1, 1));
    assert!(!changed.has_changed().unwrap());
    let views = h.registry.describe();
    assert!(view(&views, "server.workers").pending_restart);
    assert_eq!(view(&views, "server.workers").value, "8");
    assert!(!view(&views, "server.bind").pending_restart);
    assert!(!view(&views, "scan.depth").pending_restart);

    // Putting the boot value back clears it.
    let report = h
        .registry
        .save(vec![change("server.workers", "2")])
        .await
        .unwrap();
    assert!(report.restart_required.is_empty());
    assert!(!view(&h.registry.describe(), "server.workers").pending_restart);
    h.registry
        .save(vec![change("server.workers", "8")])
        .await
        .unwrap();

    // A new registry from the same store starts with the saved value.
    let restarted = booted(&store, no_env()).await;
    assert_eq!(restarted.runtime.load().workers, 8);
    assert!(!view(&restarted.registry.describe(), "server.workers").pending_restart);
}

#[tokio::test]
async fn a_setting_given_on_the_command_line_is_locked_and_a_save_of_it_refused() {
    let store = TestStore::with(&[]);
    let h = booted(&store, options(&[("server.workers", "4")])).await;
    assert_eq!(h.runtime.load().workers, 4);
    let error = h
        .registry
        .save(vec![
            change("server.workers", "8"),
            change("scan.poll_ms", "10"),
        ])
        .await
        .unwrap_err();
    assert_eq!(invalid_keys(&error), ["server.workers"]);
    assert_eq!(store.get("scan.poll_ms"), None, "nothing saved");
    let workers = view(&h.registry.describe(), "server.workers").clone();
    assert_eq!(workers.source, SettingSource::Cli);
    assert!(workers.locked.is_some());
}

/// A value that is set but can't be used stops the start, every one named
/// with where it came from: nothing runs on a default nobody chose.
#[tokio::test]
async fn at_boot_any_invalid_value_stops_the_start() {
    let store = TestStore::with(&[
        ("scan.depth", "zero"),
        ("scan.poll_ms", "250"),
        ("limits.soft", "70"),
    ]);
    let built = Registry::builder_with_env(
        Arc::clone(&store) as Arc<dyn SettingsStore>,
        ALL,
        options(&[("limits.hard", "lots")]),
    )
    .await;
    let mut builder = built;
    builder.section::<Scan>();
    builder.section::<Limits>();
    builder.section::<NodeA>();
    builder.section::<NodeB>();
    builder.section::<Runtime>();
    builder.section::<Auth>();
    let Err(BuildError::Invalid(problems)) = builder.build() else {
        panic!("expected the start refused");
    };
    assert_eq!(
        problems,
        [
            "scan.depth: In the options file, it is invalid. Enter a whole number from 1 to 10000.",
            "limits.hard: --limits-hard is set to an invalid value. Enter a whole number from 0 to 4294967295.",
        ]
    );
}

/// Values that break a rule between them stop the start too.
#[tokio::test]
async fn at_boot_a_section_whose_values_break_a_rule_stops_the_start() {
    let store = TestStore::with(&[("limits.soft", "500"), ("limits.hard", "100")]);
    let mut builder =
        Registry::builder_with_env(Arc::clone(&store) as Arc<dyn SettingsStore>, ALL, no_env())
            .await;
    builder.section::<Scan>();
    builder.section::<Limits>();
    builder.section::<NodeA>();
    builder.section::<NodeB>();
    builder.section::<Runtime>();
    builder.section::<Auth>();
    let Err(BuildError::Invalid(problems)) = builder.build() else {
        panic!("expected the start refused");
    };
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].starts_with("the limits settings: "),
        "{problems:?}"
    );
}

// The paused clock makes each sleep below return only once every task is
// blocked, so the assertion sees the second save parked, not unscheduled.
#[tokio::test(start_paused = true)]
async fn install_is_awaited_under_the_save_mutex() {
    let store = TestStore::with(&[]);
    let (gate, mut entered) = Gate::new();
    let mut a = Probe::<NodeA>::new();
    a.install_gate = Some(Arc::clone(&gate));
    let h = booted_with(&store, no_env(), a, Probe::new()).await;
    gate.arm();

    let registry = h.registry.clone();
    let first = tokio::spawn(async move {
        registry
            .save(vec![change("node.a", "http://new-a.example")])
            .await
    });
    entered.recv().await.unwrap();

    let registry = h.registry.clone();
    let second = tokio::spawn(async move { registry.save(vec![change("scan.depth", "77")]).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !second.is_finished(),
        "the second save waits for the first one's install"
    );
    assert_eq!(h.scan.load().depth, 20);

    gate.release.wait().await;
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(h.scan.load().depth, 77);
    assert_eq!(h.a.installed(), 2);
}

#[tokio::test]
async fn an_install_finishes_even_if_the_caller_stops_waiting() {
    let store = TestStore::with(&[]);
    let (gate, mut entered) = Gate::new();
    let mut a = Probe::<NodeA>::new();
    a.install_gate = Some(Arc::clone(&gate));
    let h = booted_with(&store, no_env(), a, Probe::new()).await;
    gate.arm();

    let registry = h.registry.clone();
    let save = tokio::spawn(async move {
        registry
            .save(vec![change("node.a", "http://new-a.example")])
            .await
    });
    entered.recv().await.unwrap();
    save.abort();
    gate.release.wait().await;

    // The next save queues behind the install, so once it returns the
    // install has finished.
    h.registry
        .save(vec![change("scan.depth", "5")])
        .await
        .unwrap();
    assert_eq!(h.a.installed(), 2);
    assert_eq!(h.node_a.load().url, url("http://new-a.example"));
}

// Paused clock: the timeout elapses only once every task is blocked.
#[tokio::test(start_paused = true)]
async fn two_concurrent_saves_run_one_after_the_other() {
    let store = TestStore::with(&[]);
    let (gate, mut entered) = Gate::new();
    let mut a = Probe::<NodeA>::new();
    a.prepare_gate = Some(Arc::clone(&gate));
    let h = booted_with(&store, no_env(), a, Probe::new()).await;
    gate.arm();

    let registry = h.registry.clone();
    let first = tokio::spawn(async move {
        registry
            .save(vec![change("node.a", "http://one.example")])
            .await
    });
    let registry = h.registry.clone();
    let second = tokio::spawn(async move {
        registry
            .save(vec![change("node.a", "http://two.example")])
            .await
    });

    entered.recv().await.unwrap();
    // Were the saves not serialised, the second prepare would arrive now.
    let early = tokio::time::timeout(Duration::from_millis(150), entered.recv()).await;
    assert!(
        early.is_err(),
        "the second save must not prepare while the first is still running"
    );

    gate.release.wait().await;
    entered.recv().await.unwrap();
    gate.release.wait().await;
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    assert_eq!(h.a.max_in_prepare.load(Ordering::SeqCst), 1);
    assert_eq!(h.a.installed(), 3);
    let installed = h.a.installed_values.lock().clone();
    assert_eq!(
        h.node_a.load().url.as_str(),
        if installed[2].contains("two") {
            "http://two.example"
        } else {
            "http://one.example"
        }
    );
}

#[tokio::test]
async fn read_sync_returns_the_same_section_value_the_registry_would() {
    let store = TestStore::with(&[
        ("scan.depth", "30"),
        ("scan.poll_ms", "250"),
        ("limits.soft", "50"),
        ("limits.hard", "100"),
        ("server.workers", "8"),
    ]);
    let env = options(&[("server.bind", "0.0.0.0:9000")]);
    let h = build(&store, env.clone(), Probe::new(), Probe::new()).await;

    assert_eq!(
        read_sync_with_env::<Scan>(store.read_all().await, &env),
        *h.scan.load()
    );
    assert_eq!(
        read_sync_with_env::<Limits>(store.read_all().await, &env),
        *h.limits.load()
    );
    let runtime = read_sync_with_env::<Runtime>(store.read_all().await, &env);
    assert_eq!(runtime, *h.runtime.load());
    assert_eq!(runtime.workers, 8);
    assert_eq!(runtime.bind.render(), "0.0.0.0:9000");

    store.fail_reads.store(true, Ordering::SeqCst);
    assert_eq!(
        read_sync_with_env::<Scan>(store.read_all().await, &env),
        Scan {
            depth: 20,
            poll_ms: 1000
        }
    );
}

#[tokio::test]
async fn build_fails_if_a_declared_setting_is_in_no_section() {
    let store: Arc<dyn SettingsStore> = Arc::new(MemoryStore::new());
    let mut builder = Registry::builder_with_env(store, ALL, no_env()).await;
    builder.section::<Scan>();
    builder.section::<Limits>();
    match builder.build() {
        Err(BuildError::Orphaned(keys)) => {
            assert_eq!(
                keys,
                [
                    "node.a",
                    "node.b",
                    "server.workers",
                    "server.bind",
                    "engine.token"
                ]
            )
        }
        other => panic!("expected Orphaned, got {:?}", other.err()),
    }
}

#[tokio::test]
async fn build_fails_on_mistakes_in_the_declarations() {
    let store: Arc<dyn SettingsStore> = Arc::new(MemoryStore::new());

    // A section reading a setting that isn't declared.
    let mut builder = Registry::builder_with_env(Arc::clone(&store), &[&DEPTH], no_env()).await;
    builder.section::<Scan>();
    assert!(matches!(
        builder.build(),
        Err(BuildError::UndeclaredKey {
            section: "scan",
            key: "scan.poll_ms"
        })
    ));

    // The same section twice.
    let mut builder =
        Registry::builder_with_env(Arc::clone(&store), &[&DEPTH, &POLL_MS], no_env()).await;
    builder.section::<Scan>();
    builder.section::<Scan>();
    assert!(matches!(
        builder.build(),
        Err(BuildError::DuplicateSection("scan"))
    ));

    // A setting declared twice.
    let mut builder =
        Registry::builder_with_env(Arc::clone(&store), &[&DEPTH, &POLL_MS, &DEPTH], no_env()).await;
    builder.section::<Scan>();
    assert!(matches!(
        builder.build(),
        Err(BuildError::DuplicateKey("scan.depth"))
    ));

    // A section mixing live and restart-only settings.
    #[derive(Debug, Clone, PartialEq)]
    struct Mixed;
    impl Section for Mixed {
        const NAME: &'static str = "mixed";
        fn keys() -> &'static [&'static dyn AnySetting] {
            &[&DEPTH, &WORKERS]
        }
        fn from_snapshot(_: &Snapshot) -> Result<Self, Vec<FieldError>> {
            Ok(Mixed)
        }
    }
    let mut builder =
        Registry::builder_with_env(Arc::clone(&store), &[&DEPTH, &WORKERS], no_env()).await;
    builder.section::<Mixed>();
    assert!(matches!(
        builder.build(),
        Err(BuildError::MixedApplies("mixed"))
    ));

    // A default its own range rejects, and an invalid example.
    const OUT_OF_RANGE: Setting<u32> = Setting {
        key: "bad.default",
        env_var: "",
        default: || 0,
        check: None,
        bounds: range(1, 10),
        description: "",
        example: None,
        applies: Applies::Live,
        sources: Sources::CONFIG,
        editable: true,
        required: false,
    };
    const BAD_EXAMPLE: Setting<u32> = Setting {
        key: "bad.example",
        example: Some("ten"),
        default: || 1,
        ..OUT_OF_RANGE
    };
    #[derive(Debug, Clone, PartialEq)]
    struct Bad;
    impl Section for Bad {
        const NAME: &'static str = "bad";
        fn keys() -> &'static [&'static dyn AnySetting] {
            &[&OUT_OF_RANGE, &BAD_EXAMPLE]
        }
        fn from_snapshot(_: &Snapshot) -> Result<Self, Vec<FieldError>> {
            Ok(Bad)
        }
    }
    for (declared, key, reason) in [
        (
            [&OUT_OF_RANGE as &'static dyn AnySetting, &BAD_EXAMPLE],
            "bad.default",
            "the default is rejected",
        ),
        (
            [&BAD_EXAMPLE as &'static dyn AnySetting, &OUT_OF_RANGE],
            "bad.example",
            "the example \"ten\" is invalid",
        ),
    ] {
        let mut builder = Registry::builder_with_env(Arc::clone(&store), &declared, no_env()).await;
        builder.section::<Bad>();
        match builder.build() {
            Err(BuildError::BadDeclaration { key: k, message }) => {
                assert_eq!(k, key);
                assert!(message.contains(reason), "{message}");
            }
            other => panic!("expected BadDeclaration, got {:?}", other.err()),
        }
    }
}

#[tokio::test]
async fn build_fails_if_the_store_cannot_be_read() {
    let store = TestStore::with(&[]);
    store.fail_reads.store(true, Ordering::SeqCst);
    let mut builder = Registry::builder_with_env(
        store as Arc<dyn SettingsStore>,
        &[&DEPTH, &POLL_MS],
        no_env(),
    )
    .await;
    builder.section::<Scan>();
    assert!(matches!(builder.build(), Err(BuildError::Store(_))));
}

#[tokio::test]
async fn boot_follows_each_reloadables_boot_policy() {
    // Exit: boot fails, and what others prepared is dropped.
    let store = TestStore::with(&[("node.a", "http://unreachable.example")]);
    let mut a = Probe::<NodeA>::new();
    a.fail = |c| c.url.as_str().contains("unreachable");
    let h = build(&store, no_env(), a, Probe::new()).await;
    let err = h.registry.boot().await.unwrap_err();
    assert!(
        matches!(
            err,
            BootError::Exit {
                section: "node_a",
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(h.b.prepared(), 1);
    assert_eq!(h.b.dropped(), 1);
    assert_eq!(h.b.installed(), 0);
    assert!(matches!(
        h.registry.save(vec![change("scan.depth", "5")]).await,
        Err(SaveError::NotBooted)
    ));

    // StartDegraded: boot goes on without it.
    let mut a = Probe::<NodeA>::new();
    a.fail = |c| c.url.as_str().contains("unreachable");
    a.policy = BootPolicy::StartDegraded;
    let h = build(&store, no_env(), a, Probe::new()).await;
    let report = h.registry.boot().await.unwrap();
    assert_eq!(report.degraded.len(), 1);
    assert_eq!(report.degraded[0].0, "node_a");
    assert_eq!(h.a.installed(), 0);
    assert_eq!(h.b.installed(), 1);
    assert_eq!(h.rt.installed(), 1);

    // A later save that fixes it installs it.
    h.registry
        .save(vec![change("node.a", "http://a2.example")])
        .await
        .unwrap();
    assert_eq!(h.a.installed(), 1);

    assert!(matches!(
        h.registry.boot().await,
        Err(BootError::AlreadyBooted)
    ));
}

#[tokio::test]
async fn a_save_before_boot_is_refused() {
    let store = TestStore::with(&[]);
    let h = build(&store, no_env(), Probe::new(), Probe::new()).await;
    assert!(matches!(
        h.registry.save(vec![change("scan.depth", "5")]).await,
        Err(SaveError::NotBooted)
    ));
    assert_eq!(store.writes(), 0);
}

#[test]
fn integers_and_bools_parse_render_and_refuse_bad_input() {
    for (raw, want) in [("0", 0u16), (" 443 ", 443), ("65535", 65535)] {
        let v = u16::parse(raw).unwrap();
        assert_eq!(v, want);
        assert_eq!(u16::parse(&v.to_stored()).unwrap(), v);
    }
    assert_eq!(
        u16::parse("65536").unwrap_err(),
        "Enter a whole number from 0 to 65535."
    );
    assert_eq!(
        u32::parse("-1").unwrap_err(),
        "Enter a whole number from 0 to 4294967295."
    );
    assert_eq!(
        u64::parse("1.5").unwrap_err(),
        "Enter a whole number, 0 or more."
    );
    assert_eq!(
        usize::parse("").unwrap_err(),
        "Enter a whole number, 0 or more."
    );
    assert_eq!(i64::parse("-30").unwrap(), -30);
    assert_eq!(i64::parse("x").unwrap_err(), "Enter a whole number.");

    assert!(bool::parse(" TRUE ").unwrap());
    assert!(!bool::parse("false").unwrap());
    assert_eq!(bool::parse("yes").unwrap_err(), "Enter true or false.");
    assert_eq!(bool::kind(), SettingKind::Bool);

    assert_eq!(String::parse("  hi  ").unwrap(), "hi");
    assert_eq!(
        PathBuf::parse(" /run/custody.sock ").unwrap(),
        PathBuf::from("/run/custody.sock")
    );
    assert_eq!(PathBuf::parse(" ").unwrap_err(), "Enter a path.");
    assert_eq!(<Option<PathBuf>>::parse(" ").unwrap(), None);
}

#[test]
fn secret_masks_itself_everywhere_but_the_store() {
    let secret = Secret::parse(" s3cret\n").unwrap();
    assert_eq!(secret.expose(), "s3cret");
    assert_eq!(secret.render(), MASK);
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert_eq!(secret.to_stored(), "s3cret");
    assert_eq!(Secret::parse(&secret.to_stored()).unwrap(), secret);
    assert_eq!(
        Secret::default().render(),
        "",
        "an unset secret shows as unset"
    );
    assert_eq!(Some(secret.clone()).render(), MASK);
    assert_eq!(Some(secret).to_stored(), "s3cret");
}

#[test]
fn bind_addr_round_trips_and_refuses_an_address_without_a_port() {
    for raw in ["127.0.0.1:8443", "[::1]:18081", " 0.0.0.0:80 "] {
        let v = BindAddr::parse(raw).unwrap();
        assert_eq!(BindAddr::parse(&v.render()).unwrap(), v);
    }
    assert_eq!(
        BindAddr::parse("127.0.0.1").unwrap_err(),
        "Enter an IP address and port, like 127.0.0.1:8443 or [::1]:8443."
    );
    assert!(BindAddr::parse("localhost:80").is_err());
    assert_eq!(BindAddr::kind(), SettingKind::Address);
}

#[test]
fn http_url_round_trips_and_refuses_other_schemes() {
    let v = HttpUrl::parse(" https://pay.example.com/ ").unwrap();
    assert_eq!(v.render(), "https://pay.example.com");
    assert_eq!(HttpUrl::parse(&v.to_stored()).unwrap(), v);
    let with_path = HttpUrl::parse("http://127.0.0.1:8443/api/").unwrap();
    assert_eq!(with_path.render(), "http://127.0.0.1:8443/api/");
    assert_eq!(HttpUrl::parse(&with_path.to_stored()).unwrap(), with_path);

    let message =
        "Enter a full web address starting with http:// or https://, like https://example.com.";
    for bad in [
        "ftp://files.example",
        "pay.example.com",
        "",
        "http://",
        "mailto:a@b.example",
    ] {
        assert_eq!(HttpUrl::parse(bad).unwrap_err(), message, "{bad:?}");
    }
    assert_eq!(<Option<HttpUrl>>::parse("").unwrap(), None);
    assert_eq!(HttpUrl::kind(), SettingKind::Url);
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Backend {
    Plain,
    Socket,
}

impl SettingValue for Backend {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "plain" => Ok(Backend::Plain),
            "socket" => Ok(Backend::Socket),
            _ => Err("Choose plain or socket.".to_string()),
        }
    }
    fn render(&self) -> String {
        match self {
            Backend::Plain => "plain",
            Backend::Socket => "socket",
        }
        .to_string()
    }
    fn kind() -> SettingKind {
        SettingKind::Choice {
            choices: vec!["plain", "socket"],
        }
    }
}

#[test]
fn comma_list_round_trips_and_names_the_bad_item() {
    let v = <CommaList<u16>>::parse("1, 2,,3 ,").unwrap();
    assert_eq!(v, CommaList(vec![1, 2, 3]));
    assert_eq!(v.render(), "1, 2, 3");
    assert_eq!(<CommaList<u16>>::parse(&v.to_stored()).unwrap(), v);
    assert_eq!(<CommaList<u16>>::parse("").unwrap(), CommaList(vec![]));
    assert_eq!(
        <CommaList<u16>>::parse("1, x").unwrap_err(),
        "\"x\": Enter a whole number from 0 to 65535."
    );
    assert_eq!(<CommaList<u16>>::kind(), SettingKind::Text);

    let backends = <CommaList<Backend>>::parse("socket, plain").unwrap();
    assert_eq!(backends, CommaList(vec![Backend::Socket, Backend::Plain]));
    assert_eq!(
        <CommaList<Backend>>::kind(),
        SettingKind::ChoiceList {
            choices: vec!["plain", "socket"]
        }
    );
    assert_eq!(
        <CommaList<Backend>>::parse("plain, tpm").unwrap_err(),
        "\"tpm\": Choose plain or socket."
    );

    let secrets = <CommaList<Secret>>::parse("a, b").unwrap();
    assert_eq!(secrets.render(), format!("{MASK}, {MASK}"));
    assert_eq!(
        <CommaList<Secret>>::parse(&secrets.to_stored()).unwrap(),
        secrets
    );
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Node {
    host: String,
    port: u16,
    #[serde(default)]
    fallbacks: Vec<Node>,
}

#[test]
fn json_round_trips_and_refuses_invalid_documents() {
    let v = <Json<Node>>::parse(r#"{"host": "node.example", "port": 18081, "fallbacks": [{"host": "b.example", "port": 1}]}"#).unwrap();
    assert_eq!(v.0.fallbacks[0].host, "b.example");
    assert_eq!(<Json<Node>>::parse(&v.render()).unwrap(), v);
    assert_eq!(<Json<Node>>::parse(&v.to_stored()).unwrap(), v);

    let err = <Json<Node>>::parse(r#"{"host": "node.example"}"#).unwrap_err();
    assert!(
        err.starts_with("This isn't valid for this setting: missing field `port`"),
        "{err}"
    );
    let err = <Json<Node>>::parse("not json").unwrap_err();
    assert!(
        err.starts_with("This isn't valid for this setting:"),
        "{err}"
    );
    assert_eq!(<Json<Node>>::kind(), SettingKind::Json);

    let optional = <Option<Json<Node>>>::parse(" ").unwrap();
    assert_eq!(optional, None);
}

#[test]
fn a_setting_applies_its_range_and_check_on_top_of_its_type() {
    settings! {
        EVEN: u32 {
            key: "even",
            default: 2,
            check: |v: &u32| if v.is_multiple_of(2) { Ok(()) } else { Err("Enter an even number.".to_string()) },
            description: "An even number.",
        },
    }
    assert_eq!(ALL.len(), 1);
    assert_eq!(EVEN.parse("4").unwrap(), 4);
    assert_eq!(EVEN.parse("3").unwrap_err(), "Enter an even number.");
    assert_eq!(
        EVEN.parse("x").unwrap_err(),
        "Enter a whole number from 0 to 4294967295."
    );
    assert_eq!(
        EVEN.kind(),
        SettingKind::Integer {
            min: Some(0),
            max: Some(i64::from(u32::MAX))
        }
    );

    assert_eq!(DEPTH.parse("10000").unwrap(), 10_000);
    assert_eq!(
        DEPTH.parse("x").unwrap_err(),
        "Enter a whole number from 1 to 10000.",
        "the setting's own range, not the type's"
    );
    assert_eq!(
        DEPTH.parse("10001").unwrap_err(),
        "Enter a whole number from 1 to 10000."
    );
    assert_eq!(
        WORKERS.kind(),
        SettingKind::Integer {
            min: Some(1),
            max: Some(256)
        }
    );
    assert_eq!(WORKERS.applies, Applies::Restart);
    assert_eq!(POLL_MS.applies, Applies::Live);
    assert_eq!(POLL_MS.example, None);
}

#[tokio::test]
async fn a_secret_comes_from_the_environment_and_is_never_saved_or_shown() {
    let store = TestStore::with(&[]);
    let h = booted(&store, Env::fixed([("TEST_TOKEN", "hunter2")])).await;
    assert_eq!(h.auth.load().token.expose(), "hunter2");
    let error = h
        .registry
        .save(vec![change("engine.token", "other")])
        .await
        .unwrap_err();
    assert_eq!(invalid_keys(&error), ["engine.token"]);
    assert_eq!(store.get("engine.token"), None);
    assert_eq!(view(&h.registry.describe(), "engine.token").value, MASK);
}

#[test]
fn live_new_holds_a_fixed_value() {
    let live = Live::new(Scan {
        depth: 1,
        poll_ms: 2,
    });
    let rx = live.subscribe();
    assert_eq!(live.clone().load().depth, 1);
    assert!(!rx.has_changed().unwrap());
}

/// Where a value may come from: the options file, the database (runtime
/// switches), the command line and the environment (secrets), lowest first.
mod sources {
    use super::*;
    use crate::setting::private::Resolve;

    settings! {
        DB_PATH: String {
            key: "boot.db_path",
            default: "app.db".to_string(),
            description: "Where the database is. Kept beside its logs.",
            applies: Restart,
            editable: false,
        },
        KEY: Secret {
            key: "boot.key",
            env: "TEST_KEY",
            default: Secret::new(String::new()),
            check: |v: &Secret| if v.expose().len() == 4 { Ok(()) } else { Err("Enter 4 characters.".to_string()) },
            description: "The key that protects the store.",
            applies: Restart,
            sources: [Env],
            required: true,
        },
        LIMIT: u32 {
            key: "scan.limit_per_min",
            default: 10,
            check: range(1, 100),
            description: "Requests a minute.",
            example: "10",
        },
        ENABLED: CommaList<String> {
            key: "scan.networks",
            default: parsed_default("mainnet"),
            description: "Networks to scan.",
        },
        UNDER_ATTACK: bool {
            key: "abuse.under_attack",
            default: false,
            description: "Challenge every visitor.",
            sources: [Database],
        },
        PUBLIC: String {
            key: "public_url",
            default: String::new(),
            description: "Where this instance is.",
            example: "https://pay.example.com",
        },
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Boot {
        db_path: String,
    }

    impl Section for Boot {
        const NAME: &'static str = "boot";
        fn keys() -> &'static [&'static dyn AnySetting] {
            &[&DB_PATH, &KEY]
        }
        fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
            Ok(Boot {
                db_path: s.get(&DB_PATH),
            })
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Rate {
        limit: u32,
        networks: Vec<String>,
        under_attack: bool,
        public: String,
    }

    impl Section for Rate {
        const NAME: &'static str = "rate";
        fn keys() -> &'static [&'static dyn AnySetting] {
            &[&LIMIT, &ENABLED, &UNDER_ATTACK, &PUBLIC]
        }
        fn from_snapshot(s: &Snapshot) -> Result<Self, Vec<FieldError>> {
            Ok(Rate {
                limit: s.get(&LIMIT),
                networks: s.get(&ENABLED).0,
                under_attack: s.get(&UNDER_ATTACK),
                public: s.get(&PUBLIC),
            })
        }
    }

    fn cli(values: &[(&str, &str)]) -> HashMap<String, String> {
        values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn key_env() -> Env {
        Env::fixed([("TEST_KEY", "abcd")])
    }

    struct Setup {
        registry: Registry,
        rate: Live<Rate>,
        database: Arc<MemoryStore>,
    }

    async fn setup(file: OptionsFile, env: Env) -> Result<Setup, BuildError> {
        let database = Arc::new(MemoryStore::new());
        let store = LayeredStore::new(file, Arc::clone(&database) as Arc<dyn SettingsStore>, ALL);
        let mut builder = Registry::builder_with_env(Arc::new(store), ALL, env).await;
        let rate = builder.section::<Rate>();
        builder.section::<Boot>();
        let registry = builder.build()?;
        registry.boot().await.unwrap();
        Ok(Setup {
            registry,
            rate,
            database,
        })
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("live-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The options file under the command line under the environment; a
    /// runtime switch from the database; each reported with its source.
    #[tokio::test]
    async fn each_source_wins_over_the_ones_below_it() {
        let file = OptionsFile::in_memory(
            "public_url = \"https://file.example\"\n[scan]\nlimit_per_min = 20\nnetworks = [\"mainnet\", \"stagenet\"]\n",
        );
        let env = key_env().with_cli(cli(&[("scan.limit_per_min", "40")]));
        let s = setup(file, env).await.unwrap();
        assert_eq!(
            *s.rate.load(),
            Rate {
                limit: 40,
                networks: vec!["mainnet".into(), "stagenet".into()],
                under_attack: false,
                public: "https://file.example".into(),
            }
        );
        let views = s.registry.describe();
        assert_eq!(
            view(&views, "scan.limit_per_min").source,
            SettingSource::Cli
        );
        assert_eq!(view(&views, "scan.networks").source, SettingSource::Toml);
        assert_eq!(
            view(&views, "abuse.under_attack").source,
            SettingSource::Default
        );
        assert_eq!(view(&views, "boot.key").source, SettingSource::Env);
        assert_eq!(view(&views, "boot.key").value, MASK);

        // A runtime switch is saved to the database, not the file.
        s.registry
            .save(vec![change("abuse.under_attack", "true")])
            .await
            .unwrap();
        assert_eq!(
            s.database.get("abuse.under_attack").as_deref(),
            Some("true")
        );
        assert!(s.rate.load().under_attack);
        assert_eq!(
            view(&s.registry.describe(), "abuse.under_attack").source,
            SettingSource::Database
        );
    }

    /// What the command line or the environment sets, or what isn't stored
    /// or isn't the page's to change, is locked: described so, and refused
    /// by a save.
    #[tokio::test]
    async fn the_page_cannot_change_what_is_given_at_start() {
        let env = key_env().with_cli(cli(&[("scan.limit_per_min", "40")]));
        let s = setup(OptionsFile::in_memory(""), env).await.unwrap();
        let views = s.registry.describe();
        assert_eq!(
            view(&views, "scan.limit_per_min").locked.as_deref(),
            Some("This is set with --scan-limit-per-min when the process starts; remove it there to change it here.")
        );
        assert_eq!(
            view(&views, "boot.key").locked.as_deref(),
            Some("This is set with TEST_KEY when the process starts; change it there.")
        );
        assert!(view(&views, "boot.db_path").locked.is_some());
        assert_eq!(view(&views, "public_url").locked, None);
        for key in ["scan.limit_per_min", "boot.key", "boot.db_path"] {
            let error = s.registry.save(vec![change(key, "5")]).await.unwrap_err();
            assert_eq!(invalid_keys(&error), [key]);
        }
    }

    /// Read before any store exists: the environment or the default; a
    /// required one that is missing or invalid is an error to start on.
    #[test]
    fn a_secret_is_read_without_the_store_at_start() {
        assert_eq!(KEY.require(&key_env()).unwrap().expose(), "abcd");
        assert_eq!(
            KEY.require(&no_env()).unwrap_err(),
            "TEST_KEY must be set. The key that protects the store."
        );
        assert_eq!(
            KEY.require(&Env::fixed([("TEST_KEY", "abc")])).unwrap_err(),
            "TEST_KEY is set to an invalid value. Enter 4 characters."
        );
        let flagged = no_env().with_cli(cli(&[("boot.db_path", "/cli.db")]));
        assert_eq!(DB_PATH.require(&flagged).unwrap(), "/cli.db");
        assert_eq!(DB_PATH.require(&no_env()).unwrap(), "app.db");
    }

    /// Any value that is set but unusable stops the process at start, from
    /// whichever source, all of them named.
    #[tokio::test]
    async fn an_invalid_value_anywhere_stops_the_start() {
        let file = OptionsFile::in_memory("[scan]\nlimit_per_min = 500\n");
        let Err(BuildError::Store(e)) = setup(file, key_env()).await.map(|_| ()) else {
            panic!("expected the file refused");
        };
        assert!(
            e.0.contains("line 2: scan.limit_per_min: Enter a whole number from 1 to 100."),
            "{e}"
        );

        let missing = setup(OptionsFile::in_memory(""), no_env()).await;
        let Err(BuildError::Invalid(problems)) = missing.map(|_| ()) else {
            panic!("expected the missing secret refused");
        };
        assert_eq!(
            problems,
            ["boot.key: TEST_KEY must be set. The key that protects the store."]
        );
    }

    /// The file holds only what it may, each problem with its line.
    #[tokio::test]
    async fn the_options_file_refuses_what_it_may_not_hold() {
        let file = OptionsFile::in_memory(
            "[scan]\nlimit_per_minute = 5\n\n[boot]\nkey = \"abcd\"\n\n[abuse]\nunder_attack = true\n",
        );
        let Err(BuildError::Store(e)) = setup(file, key_env()).await.map(|_| ()) else {
            panic!("expected the file refused");
        };
        let message = e.0;
        assert!(
            message.contains("line 2: there is no setting called scan.limit_per_minute"),
            "{message}"
        );
        assert!(message.contains("line 5: boot.key can't be in the options file: it is a secret: set TEST_KEY in the environment"), "{message}");
        assert!(
            message.contains(
                "line 8: abuse.under_attack can't be in the options file: the admin page keeps it"
            ),
            "{message}"
        );
        let bad = OptionsFile::in_memory("[scan\n");
        let Err(BuildError::Store(e)) = setup(bad, key_env()).await.map(|_| ()) else {
            panic!("expected a syntax error");
        };
        assert!(e.0.contains("line 1"), "{e}");
    }

    /// A save writes only its keys, in place, keeping the file's comments;
    /// it refuses if the file changed on disk since it was read, and the
    /// reload that then applies those changes refuses an invalid one.
    #[tokio::test]
    async fn saves_write_the_file_in_place_and_reloads_apply_edits() {
        let dir = temp_dir("options");
        let path = dir.join("app.toml");
        std::fs::write(&path, "# Kept.\n[scan]\n# How fast.\nlimit_per_min = 20\n").unwrap();
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();
        assert_eq!(s.rate.load().limit, 20);
        let info = s.registry.options_file().unwrap();
        assert_eq!(info.path, path.display().to_string());
        assert!(info.exists && info.writable);

        s.registry
            .save(vec![
                change("scan.limit_per_min", "30"),
                change("public_url", "https://pay.example.com"),
                change("scan.networks", "mainnet,testnet"),
            ])
            .await
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "public_url = \"https://pay.example.com\"\n# Kept.\n[scan]\n# How fast.\nlimit_per_min = 30\nnetworks = \"mainnet,testnet\"\n"
        );
        assert_eq!(s.rate.load().limit, 30);

        // Edited by hand: a save refuses to overwrite it.
        std::fs::write(
            &path,
            text.replace("limit_per_min = 30", "limit_per_min = 50"),
        )
        .unwrap();
        let refused = s
            .registry
            .save(vec![change("public_url", "")])
            .await
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("has changed since it was loaded"),
            "{refused}"
        );
        assert_eq!(s.rate.load().limit, 30, "not applied until reloaded");

        let report = s.registry.reload().await.unwrap();
        assert_eq!(report.changed, ["scan.limit_per_min"]);
        assert_eq!(s.rate.load().limit, 50);

        std::fs::write(&path, "[scan]\nlimit_per_min = 0\n").unwrap();
        let refused = s.registry.reload().await.unwrap_err();
        assert!(
            refused.to_string().contains("line 2: scan.limit_per_min"),
            "{refused}"
        );
        assert_eq!(s.rate.load().limit, 50, "a bad file changes nothing");

        // Now it saves again: the reload refused, so the last good read stands
        // until the file is fixed.
        std::fs::write(&path, "[scan]\nlimit_per_min = 60\n").unwrap();
        s.registry.reload().await.unwrap();
        s.registry
            .save(vec![change("public_url", "https://x.example")])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "public_url = \"https://x.example\"\n[scan]\nlimit_per_min = 60\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A value saved empty is kept empty: the file records it, so a reload
    /// and the next start read back what was saved, not the default. (The
    /// key used to be dropped from the file while the process carried on
    /// with the empty value, until a restart quietly brought the default
    /// back.) Only taking a value away (`None`) removes the key.
    #[tokio::test]
    async fn a_value_saved_empty_is_kept_in_the_file_not_turned_back_into_the_default() {
        let dir = temp_dir("saved-empty");
        let path = dir.join("app.toml");
        std::fs::write(&path, "[scan]\nnetworks = \"mainnet,testnet\"\n").unwrap();
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();

        s.registry
            .save(vec![change("scan.networks", "")])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[scan]\nnetworks = \"\"\n"
        );
        assert!(s.rate.load().networks.is_empty());
        let saved = view(&s.registry.describe(), "scan.networks").clone();
        assert_eq!(
            (saved.value.as_str(), saved.source),
            ("", SettingSource::Toml)
        );

        let report = s.registry.reload().await.unwrap();
        assert!(
            report.changed.is_empty(),
            "the file says what the process has: {:?}",
            report.changed
        );

        let restarted = setup(OptionsFile::at(&path), key_env()).await.unwrap();
        assert!(
            restarted.rate.load().networks.is_empty(),
            "not the default, mainnet: {:?}",
            restarted.rate.load().networks
        );
        assert_eq!(
            view(&restarted.registry.describe(), "scan.networks").source,
            SettingSource::Toml
        );

        // Taking the value away is what brings the default back.
        restarted
            .registry
            .save(vec![("scan.networks".to_string(), None)])
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[scan]\n");
        assert_eq!(restarted.rate.load().networks, ["mainnet"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A save of values already in effect writes nothing: a form sends
    /// every field of its tab, and the ones left as they were (at their
    /// defaults, or at what the file already says) are not changes. (Every
    /// default used to be written into the file on any save of its tab,
    /// pinned there for good.)
    #[tokio::test]
    async fn a_save_of_the_values_already_in_effect_writes_nothing() {
        let dir = temp_dir("unchanged");
        let path = dir.join("app.toml");
        let text = "# Mine.\n[scan]\nlimit_per_min = 20\n";
        std::fs::write(&path, text).unwrap();
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();

        let unchanged = || {
            vec![
                change("scan.limit_per_min", "20"),
                change("scan.networks", "mainnet"),
                change("public_url", ""),
                change("abuse.under_attack", "false"),
            ]
        };
        let report = s.registry.save(unchanged()).await.unwrap();
        assert!(report.changed.is_empty(), "{:?}", report.changed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(
            s.database.read_all().await.unwrap().is_empty(),
            "nor into the database"
        );
        let views = s.registry.describe();
        assert_eq!(view(&views, "scan.networks").source, SettingSource::Default);
        assert_eq!(view(&views, "public_url").source, SettingSource::Default);

        // One field changed among the rest: only it is written.
        let mut one_changed = unchanged();
        one_changed[2] = change("public_url", "https://pay.example.com");
        let report = s.registry.save(one_changed).await.unwrap();
        assert_eq!(report.changed, ["public_url"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "public_url = \"https://pay.example.com\"\n# Mine.\n[scan]\nlimit_per_min = 20\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Sets a file's permission bits.
    #[cfg(unix)]
    fn chmod(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Whether permission bits bind this process: they don't for root,
    /// where the tests below that rely on them have nothing to show.
    #[cfg(unix)]
    fn permissions_apply(dir: &std::path::Path) -> bool {
        let probe = dir.join("probe");
        std::fs::write(&probe, "x").unwrap();
        chmod(&probe, 0o000);
        let applies = std::fs::read(&probe).is_err();
        chmod(&probe, 0o600);
        let _ = std::fs::remove_file(&probe);
        applies
    }

    /// No file yet: the process starts on the defaults, the page is told
    /// the file doesn't exist but can be made, and the first save makes it
    /// (and its directory).
    #[tokio::test]
    async fn a_missing_file_starts_on_the_defaults_and_the_first_save_creates_it() {
        let dir = temp_dir("missing");
        let path = dir.join("nested").join("app.toml");
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();
        assert_eq!(s.rate.load().limit, 10);
        assert_eq!(
            view(&s.registry.describe(), "scan.limit_per_min").source,
            SettingSource::Default
        );
        let info = s.registry.options_file().unwrap();
        assert!(!info.exists && info.writable, "{info:?}");

        s.registry
            .save(vec![change("scan.limit_per_min", "30")])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[scan]\nlimit_per_min = 30\n"
        );
        assert!(s.registry.options_file().unwrap().exists);
        assert_eq!(
            view(&s.registry.describe(), "scan.limit_per_min").source,
            SettingSource::Toml
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file the process can't read stops the start, saying so; once
    /// running, a reload of it or a save to it is refused and changes
    /// nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_file_stops_the_start_and_is_never_half_applied() {
        let dir = temp_dir("unreadable");
        if !permissions_apply(&dir) {
            return;
        }
        let path = dir.join("app.toml");
        std::fs::write(&path, "[scan]\nlimit_per_min = 20\n").unwrap();
        chmod(&path, 0o000);
        let Err(BuildError::Store(e)) = setup(OptionsFile::at(&path), key_env()).await.map(|_| ())
        else {
            panic!("expected the start refused");
        };
        assert!(
            e.0.contains(&format!("{} can't be read", path.display())),
            "{e}"
        );

        chmod(&path, 0o600);
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();
        chmod(&path, 0o000);
        let refused = s.registry.reload().await.unwrap_err().to_string();
        assert!(refused.contains("can't be read"), "{refused}");
        let refused = s
            .registry
            .save(vec![change("scan.limit_per_min", "30")])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("can't be written by this process"),
            "{refused}"
        );
        assert_eq!(s.rate.load().limit, 20, "nothing changed");
        chmod(&path, 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[scan]\nlimit_per_min = 20\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A read-only file, or a writable one in a directory that takes no new
    /// file (a save writes beside it and renames), is reported as not
    /// writable, and a save to it is refused - even one the page would not
    /// have sent - leaving the file, and its permissions, as they were.
    /// Runtime switches, in the database, still save.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_the_process_cannot_write_refuses_saves_and_stays_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("read-only");
        if !permissions_apply(&dir) {
            return;
        }
        let path = dir.join("app.toml");
        let text = "# Mine.\n[scan]\nlimit_per_min = 20\n";
        std::fs::write(&path, text).unwrap();
        chmod(&path, 0o444);
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();
        assert!(!s.registry.options_file().unwrap().writable);
        let refused = s
            .registry
            .save(vec![change("scan.limit_per_min", "30")])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(
                "can't be written by this process: change it by editing it, then reload it."
            ),
            "{refused}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o444,
            "still read-only"
        );
        assert_eq!(s.rate.load().limit, 20);
        s.registry
            .save(vec![change("abuse.under_attack", "true")])
            .await
            .unwrap();
        assert!(s.rate.load().under_attack);

        chmod(&path, 0o644);
        chmod(&dir, 0o555);
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();
        let writable = s.registry.options_file().unwrap().writable;
        let refused = s
            .registry
            .save(vec![change("scan.limit_per_min", "30")])
            .await;
        chmod(&dir, 0o755);
        assert!(!writable, "the directory takes no new file");
        assert!(refused.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from("app.toml")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file deleted while running: a save refuses to bring back a file
    /// someone removed; a reload takes the defaults, as a start would, and
    /// the next save creates it again.
    #[tokio::test]
    async fn a_file_deleted_while_running_is_reloaded_as_empty_not_silently_recreated() {
        let dir = temp_dir("deleted");
        let path = dir.join("app.toml");
        std::fs::write(&path, "[scan]\nlimit_per_min = 20\n").unwrap();
        let s = setup(OptionsFile::at(&path), key_env()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        let refused = s
            .registry
            .save(vec![change("public_url", "https://x.example")])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("has changed since it was loaded"),
            "{refused}"
        );
        assert!(!path.exists());

        let report = s.registry.reload().await.unwrap();
        assert_eq!(report.changed, ["scan.limit_per_min"]);
        assert_eq!(s.rate.load().limit, 10);
        s.registry
            .save(vec![change("scan.limit_per_min", "30")])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[scan]\nlimit_per_min = 30\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An options file linked from elsewhere (dotfiles) stays a link: a
    /// save writes the file it points to, keeping that file's permissions,
    /// and leaves nothing behind.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_save_writes_through_a_symlink_and_keeps_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("symlink");
        let real = dir.join("dotfiles").join("app.toml");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "[scan]\nlimit_per_min = 20\n").unwrap();
        chmod(&real, 0o600);
        let link = dir.join("app.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let s = setup(OptionsFile::at(&link), key_env()).await.unwrap();
        assert_eq!(s.rate.load().limit, 20);
        s.registry
            .save(vec![change("scan.limit_per_min", "30")])
            .await
            .unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "[scan]\nlimit_per_min = 30\n"
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let names = |d: &std::path::Path| -> Vec<std::ffi::OsString> {
            std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect()
        };
        assert_eq!(
            names(real.parent().unwrap()),
            [std::ffi::OsString::from("app.toml")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A database row for a setting the options file holds (left by an
    /// older release, or written by hand) means nothing: the file, or the
    /// default, wins. Runtime switches still come from the database.
    #[tokio::test]
    async fn a_database_row_for_a_setting_the_file_holds_is_ignored() {
        let database = Arc::new(MemoryStore::new());
        database
            .write_all(vec![
                ("scan.limit_per_min", Some("99".to_string())),
                ("public_url", Some("https://stale.example".to_string())),
                ("abuse.under_attack", Some("true".to_string())),
            ])
            .await
            .unwrap();
        let store = LayeredStore::new(
            OptionsFile::in_memory("[scan]\nlimit_per_min = 20\n"),
            Arc::clone(&database) as Arc<dyn SettingsStore>,
            ALL,
        );
        let mut builder = Registry::builder_with_env(Arc::new(store), ALL, key_env()).await;
        let rate = builder.section::<Rate>();
        builder.section::<Boot>();
        let registry = builder.build().unwrap();
        registry.boot().await.unwrap();
        let rate = rate.load();
        assert_eq!(rate.limit, 20);
        assert_eq!(rate.public, "");
        assert!(rate.under_attack);
        assert_eq!(
            view(&registry.describe(), "public_url").source,
            SettingSource::Default
        );
    }

    /// `--init` lists every setting the file may hold, commented out with
    /// its default, names the secrets' variables, leaves runtime switches
    /// out, reads back as an empty file would, and never overwrites one.
    #[test]
    fn init_writes_a_commented_file_that_changes_nothing() {
        let text = render_init("app", ALL);
        assert!(
            text.contains("# Options file for app, written by `app --init`."),
            "{text}"
        );
        assert!(text.contains("#   TEST_KEY (required)"), "{text}");
        assert!(text.contains("# Where this instance is.\n# Unset by default; for example:\n# public_url = \"https://pay.example.com\""), "{text}");
        assert!(text.contains("[scan]\n# Requests a minute.\n# A whole number from 1 to 100.\n# limit_per_min = 10\n"), "{text}");
        assert!(
            text.contains("\n\n# Networks to scan.\n# networks = \"mainnet\"\n"),
            "{text}"
        );
        assert!(
            !text.contains("\n\n\n"),
            "one blank line between settings: {text}"
        );
        assert!(text.contains("# Takes effect after a restart. The admin page doesn't change it.\n# db_path = \"app.db\""), "{text}");
        assert!(!text.contains("under_attack"), "{text}");
        assert!(
            text.find("public_url").unwrap() < text.find("[boot]").unwrap(),
            "{text}"
        );
        assert!(super::options_values_for_test(&text, ALL)
            .unwrap()
            .is_empty());

        let dir = temp_dir("init");
        let path = dir.join("nested").join("app.toml");
        write_init(&path, &text).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let again = write_init(&path, "other").unwrap_err();
        assert!(again.contains("already exists"), "{again}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Declarations that can't work are refused.
    #[test]
    fn declarations_keep_each_source_to_its_kind_of_setting() {
        assert_eq!(ALL.len(), 6);
        let with_env = Setting {
            env_var: "TEST_LIMIT",
            sources: Sources::of(&[Source::Env]),
            ..LIMIT
        };
        assert!(
            with_env.check_declaration().is_err(),
            "only a secret comes from the environment"
        );
        let both = Setting {
            sources: Sources::of(&[Source::Toml, Source::Database]),
            ..LIMIT
        };
        assert!(both.check_declaration().is_err(), "stored in one place");
        let required_stored = Setting {
            required: true,
            ..LIMIT
        };
        assert!(required_stored.check_declaration().is_err());
        let secret_option = Setting {
            sources: Sources::of(&[Source::Cli, Source::Env]),
            ..KEY
        };
        assert!(
            secret_option.check_declaration().is_err(),
            "the process list shows options"
        );
        let secret_file = Setting {
            sources: Sources::of(&[Source::Toml, Source::Env]),
            ..KEY
        };
        assert!(secret_file.check_declaration().is_err());
        assert!(
            KEY.check_declaration().is_ok(),
            "a required setting's default is a placeholder"
        );
    }

    /// Every setting that takes the command line is an option, with help
    /// from its declaration; a value is checked as it is parsed; the
    /// secrets' variables are listed after the options; `--options` and
    /// `--init` are there too.
    #[test]
    fn every_setting_is_a_command_line_option_with_help_from_its_declaration() {
        let command = crate::cli::with_settings(clap::Command::new("app"), ALL, "app.toml");
        let help = command.clone().render_long_help().to_string();
        for text in [
            "Scan settings:",
            "--scan-limit-per-min <NUMBER>",
            "[default: 10]",
            "[options file: scan.limit_per_min]",
            "--boot-db-path <TEXT>",
            "Options file:",
            "--options <PATH>",
            "--init",
            "TEST_KEY [required]\n          The key that protects the store.",
        ] {
            assert!(help.contains(text), "{text}: {help}");
        }
        assert!(!help.contains("--boot-key"), "{help}");
        assert!(
            !help.contains("--abuse-under-attack"),
            "a runtime switch isn't an option: {help}"
        );

        let matches = command
            .clone()
            .try_get_matches_from([
                "app",
                "--scan-limit-per-min",
                "50",
                "--options",
                "/etc/app.toml",
                "--init",
            ])
            .unwrap();
        let start = crate::cli::start(&matches, ALL, "app.toml");
        assert_eq!(start.env.cli("scan.limit_per_min").as_deref(), Some("50"));
        assert_eq!(start.options, std::path::PathBuf::from("/etc/app.toml"));
        assert!(start.init);
        let refused = command
            .try_get_matches_from(["app", "--scan-limit-per-min", "500"])
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("Enter a whole number from 1 to 100."),
            "{refused}"
        );
    }

    /// Two settings whose keys give the same option can't both be declared.
    #[tokio::test]
    async fn two_settings_with_one_option_are_refused() {
        settings! {
            DOTTED: u32 { key: "a.b_c", default: 1, description: "" },
            UNDERSCORED: u32 { key: "a.b.c", default: 1, description: "" },
        }
        let store: Arc<dyn SettingsStore> = Arc::new(MemoryStore::new());
        let builder = Registry::builder_with_env(store, ALL, no_env()).await;
        assert!(matches!(
            builder.build(),
            Err(BuildError::DuplicateFlag("a.b.c"))
        ));
    }
}

/// The options file's values, for a test that checks what a text holds.
fn options_values_for_test(
    text: &str,
    declared: &[&'static dyn AnySetting],
) -> Result<HashMap<String, String>, StoreError> {
    let file = OptionsFile::in_memory(text);
    let store = LayeredStore::new(file, Arc::new(MemoryStore::new()), declared);
    futures_util::FutureExt::now_or_never(store.read_all()).unwrap_or_else(|| Ok(HashMap::new()))
}

/// One options file for two services in one process (monokulo and an engine
/// embedded in it, docs/engine_as_library.md): the outer handle leaves the
/// `[engine.*]` tables alone, the engine's handle is that table.
mod nested_options {
    use super::*;

    settings! {
        // The outer service's: a value directly in `[engine]`.
        ENGINE_URL: String {
            key: "engine.url",
            default: String::new(),
            description: "Where the engine is.",
        },
        OUTER_BIND: BindAddr {
            key: "server.bind",
            default: parsed_default("127.0.0.1:8081"),
            description: "Listen address.",
            applies: Restart,
        },
    }

    /// The nested service's own settings, by their own keys.
    mod engine {
        settings! {
            CONFIRMATIONS: u32 {
                key: "payment.confirmations",
                default: 10,
                check: range(1, 100),
                description: "Confirmations a payment needs.",
            },
            NODE: String {
                key: "node.url",
                default: String::new(),
                description: "The node.",
            },
        }
    }

    const TEXT: &str = "\
[server]
bind = \"0.0.0.0:8081\"

[engine]
url = \"http://engine:8443\"

[engine.payment]
confirmations = 3

[engine.node]
url = \"http://node:18081\"
";

    fn read(
        file: &OptionsFile,
        declared: &[&'static dyn AnySetting],
    ) -> Result<HashMap<String, String>, String> {
        file.read(declared).map_err(|e| e.to_string())
    }

    /// A file of its own in the system's temporary directory.
    fn temp_file(tag: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "live-settings-nested-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("outer.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn each_handle_reads_only_its_own_keys() {
        let file = OptionsFile::in_memory(TEXT);
        let outer = read(&file.clone().leaving("engine"), ALL).unwrap();
        assert_eq!(
            outer,
            HashMap::from([
                ("server.bind".to_string(), "0.0.0.0:8081".to_string()),
                ("engine.url".to_string(), "http://engine:8443".to_string()),
            ])
        );
        let nested = read(&file.scoped("engine"), engine::ALL).unwrap();
        assert_eq!(
            nested,
            HashMap::from([
                ("payment.confirmations".to_string(), "3".to_string()),
                ("node.url".to_string(), "http://node:18081".to_string()),
            ])
        );
    }

    /// A mistake in the nested service's tables is reported by its reader,
    /// by the key as written in the file, with its line.
    #[test]
    fn a_mistake_in_a_nested_table_names_the_whole_key() {
        let file =
            OptionsFile::in_memory("[engine.payment]\nconfirmations = 0\nconfirmatons = 3\n");
        let problem = read(&file.scoped("engine"), engine::ALL).unwrap_err();
        assert!(
            problem.contains("line 2: engine.payment.confirmations: "),
            "{problem}"
        );
        assert!(
            problem.contains("line 3: there is no setting called engine.payment.confirmatons"),
            "{problem}"
        );
        // The outer reader leaves the table alone, mistakes and all.
        read(&file.leaving("engine"), ALL).unwrap();
    }

    /// Without the nested service (monokulo with a remote engine), its
    /// tables are refused, with why.
    #[test]
    fn nested_tables_with_nobody_to_read_them_are_refused_with_the_hint() {
        let file = OptionsFile::in_memory(TEXT).with_hint(
            "engine",
            "the engine's settings go here only when it is embedded",
        );
        let problem = read(&file, ALL).unwrap_err();
        assert!(
            problem.contains(
                "there is no setting called engine.payment.confirmations: the engine's settings go here only when it is embedded"
            ),
            "{problem}"
        );
        assert!(problem.contains("engine.node.url"), "{problem}");
        assert!(
            !problem.contains("engine.url:"),
            "the outer service's own key is fine: {problem}"
        );
    }

    /// Both services save into the one file, each keeping the other's
    /// keys, with no "changed since it was loaded" between them: they share
    /// what was last read and written.
    #[tokio::test]
    async fn saves_through_both_handles_keep_each_others_keys() {
        let path = temp_file("both", "# kept\n[server]\nbind = \"0.0.0.0:8081\"\n");
        let file = OptionsFile::at(&path);
        let outer = LayeredStore::new(
            file.clone().leaving("engine"),
            Arc::new(MemoryStore::new()),
            ALL,
        );
        let nested = LayeredStore::new(
            file.scoped("engine"),
            Arc::new(MemoryStore::new()),
            engine::ALL,
        );
        outer.read_all().await.unwrap();
        nested.read_all().await.unwrap();

        nested
            .write_all(vec![("payment.confirmations", Some("4".to_string()))])
            .await
            .unwrap();
        outer
            .write_all(vec![(
                "engine.url",
                Some("http://elsewhere:8443".to_string()),
            )])
            .await
            .unwrap();
        nested
            .write_all(vec![("node.url", Some("http://node:18081".to_string()))])
            .await
            .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# kept\n"), "{text}");
        assert!(text.contains("bind = \"0.0.0.0:8081\""), "{text}");
        assert!(text.contains("url = \"http://elsewhere:8443\""), "{text}");
        assert!(
            text.contains("[engine.payment]\nconfirmations = 4"),
            "{text}"
        );
        assert!(
            text.contains("[engine.node]\nurl = \"http://node:18081\""),
            "{text}"
        );
        assert_eq!(
            outer
                .read_all()
                .await
                .unwrap()
                .get("engine.url")
                .map(String::as_str),
            Some("http://elsewhere:8443")
        );
        assert_eq!(
            nested
                .read_all()
                .await
                .unwrap()
                .get("payment.confirmations")
                .map(String::as_str),
            Some("4")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A nested key saved into a file without the table gets the table's
    /// header, and no empty `[engine]` above it.
    #[tokio::test]
    async fn a_first_nested_save_writes_only_the_table_it_needs() {
        let path = temp_file("first", "");
        let file = OptionsFile::at(&path);
        let nested = LayeredStore::new(
            file.scoped("engine"),
            Arc::new(MemoryStore::new()),
            engine::ALL,
        );
        nested.read_all().await.unwrap();
        nested
            .write_all(vec![("payment.confirmations", Some("5".to_string()))])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[engine.payment]\nconfirmations = 5\n"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn nested_settings_get_their_own_options_and_keep_their_own_keys() {
        let command = cli::with_nested_settings(
            cli::with_settings(clap::Command::new("outer"), ALL, "outer.toml"),
            "engine",
            engine::ALL,
        );
        let matches = command
            .try_get_matches_from([
                "outer",
                "--server-bind",
                "0.0.0.0:9000",
                "--engine-payment-confirmations",
                "4",
            ])
            .unwrap();
        assert_eq!(
            cli::nested_values(&matches, "engine", engine::ALL),
            HashMap::from([("payment.confirmations".to_string(), "4".to_string())])
        );
        assert_eq!(
            cli::values(&matches, ALL),
            HashMap::from([("server.bind".to_string(), "0.0.0.0:9000".to_string())])
        );
        // Checked by the setting's own rules as it is parsed.
        let command = cli::with_nested_settings(clap::Command::new("outer"), "engine", engine::ALL);
        let refused = command
            .try_get_matches_from(["outer", "--engine-payment-confirmations", "0"])
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("--engine-payment-confirmations"),
            "{refused}"
        );
    }

    /// `--init` writes the nested service's settings under its table, and
    /// the file it writes reads back through both handles.
    #[test]
    fn init_lists_nested_settings_under_their_table() {
        let text = render_init_nested(
            "outer",
            ALL,
            "engine",
            "The engine's settings, when it runs inside outer.",
            engine::ALL,
        );
        assert!(text.contains("[server]\n"), "{text}");
        assert!(
            text.contains("# The engine's settings, when it runs inside outer."),
            "{text}"
        );
        assert!(text.contains("[engine.payment]\n"), "{text}");
        assert!(text.contains("# confirmations = 10\n"), "{text}");
        let uncommented = text.replace("# confirmations = 10", "confirmations = 7");
        let file = OptionsFile::in_memory(uncommented);
        read(&file.clone().leaving("engine"), ALL).unwrap();
        assert_eq!(
            read(&file.scoped("engine"), engine::ALL)
                .unwrap()
                .get("payment.confirmations")
                .map(String::as_str),
            Some("7")
        );
    }
}
