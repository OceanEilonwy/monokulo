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
        env: "TEST_SCAN_DEPTH",
        default: 20,
        check: range(1, 10_000),
        description: "How many recent blocks are checked again.",
        example: "20",
    },
    POLL_MS: u64 {
        description: "How long to wait between polls.",
        default: 1000,
        env: "TEST_SCAN_POLL_MS",
        key: "scan.poll_ms",
    },
    SOFT: u32 { key: "limits.soft", env: "TEST_LIMITS_SOFT", default: 60, description: "Soft limit." },
    HARD: u32 { key: "limits.hard", env: "TEST_LIMITS_HARD", default: 300, description: "Hard limit." },
    NODE_A: HttpUrl {
        key: "node.a",
        env: "TEST_NODE_A",
        default: parsed_default("http://a.example"),
        description: "Node A.",
        example: "https://node.example:18081",
    },
    NODE_B: HttpUrl { key: "node.b", env: "TEST_NODE_B", default: parsed_default("http://b.example"), description: "Node B." },
    WORKERS: usize {
        key: "server.workers",
        env: "TEST_WORKERS",
        default: 2,
        check: range(1, 256),
        description: "Worker threads.",
        applies: Restart,
    },
    BIND: BindAddr {
        key: "server.bind",
        env: "TEST_BIND",
        default: parsed_default("127.0.0.1:8443"),
        description: "Listen address.",
        applies: Restart,
    },
    TOKEN: Secret { key: "engine.token", env: "TEST_TOKEN", default: Secret::default(), description: "Admin token." },
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
async fn a_value_resolves_env_over_stored_over_default_and_reports_its_source() {
    let store = TestStore::with(&[("scan.depth", "30"), ("scan.poll_ms", "500")]);
    let h = booted(&store, Env::fixed([("TEST_SCAN_POLL_MS", "250")])).await;

    assert_eq!(
        *h.scan.load(),
        Scan {
            depth: 30,
            poll_ms: 250
        }
    );
    assert_eq!(h.limits.load().soft, 60);

    let views = h.registry.describe();
    assert_eq!(view(&views, "scan.depth").source, SettingSource::Database);
    assert_eq!(view(&views, "scan.depth").value, "30");
    assert_eq!(view(&views, "scan.poll_ms").source, SettingSource::Env);
    assert_eq!(view(&views, "scan.poll_ms").value, "250");
    assert_eq!(view(&views, "limits.soft").source, SettingSource::Default);
    assert_eq!(view(&views, "limits.soft").value, "60");

    let snapshot = Snapshot::new(
        store.inner.read_all().await.unwrap(),
        Env::fixed([("TEST_SCAN_POLL_MS", "250")]),
    );
    assert_eq!(snapshot.source(&POLL_MS), SettingSource::Env);
    assert_eq!(snapshot.source(&DEPTH), SettingSource::Database);
    assert_eq!(snapshot.source(&SOFT), SettingSource::Default);
}

#[tokio::test]
async fn a_blank_environment_variable_does_not_hide_a_stored_value() {
    let store = TestStore::with(&[("scan.depth", "30")]);
    let h = booted(&store, Env::fixed([("TEST_SCAN_DEPTH", "  ")])).await;
    assert_eq!(h.scan.load().depth, 30);
}

#[tokio::test]
async fn describe_reports_metadata_and_masks_secrets() {
    let store = TestStore::with(&[("engine.token", "hunter2")]);
    let h = booted(&store, no_env()).await;
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
    assert_eq!(depth.env_var, "TEST_SCAN_DEPTH");
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
async fn a_restart_setting_overridden_by_the_environment_is_not_pending_after_a_save() {
    let store = TestStore::with(&[]);
    let h = booted(&store, Env::fixed([("TEST_WORKERS", "4")])).await;
    assert_eq!(h.runtime.load().workers, 4);

    let report = h
        .registry
        .save(vec![change("server.workers", "8")])
        .await
        .unwrap();
    assert_eq!(report.changed, ["server.workers"]);
    assert!(
        report.restart_required.is_empty(),
        "the effective value won't change"
    );
    assert_eq!(report.env_overridden, ["server.workers"]);

    let workers = view(&h.registry.describe(), "server.workers").clone();
    assert!(!workers.pending_restart);
    assert_eq!(workers.value, "4");
    assert_eq!(workers.source, SettingSource::Env);
}

#[tokio::test]
async fn an_env_overridden_key_appears_in_env_overridden() {
    let store = TestStore::with(&[]);
    let h = booted(&store, Env::fixed([("TEST_SCAN_DEPTH", "99")])).await;
    let report = h
        .registry
        .save(vec![
            change("scan.depth", "50"),
            change("scan.poll_ms", "10"),
        ])
        .await
        .unwrap();
    assert_eq!(report.changed, ["scan.depth", "scan.poll_ms"]);
    assert_eq!(report.env_overridden, ["scan.depth"]);
    assert_eq!(
        store.get("scan.depth").as_deref(),
        Some("50"),
        "saved anyway"
    );
    assert_eq!(
        *h.scan.load(),
        Scan {
            depth: 99,
            poll_ms: 10
        }
    );
}

#[tokio::test]
async fn at_boot_an_invalid_value_falls_back_to_its_own_default_only() {
    let store = TestStore::with(&[
        ("scan.depth", "zero"),
        ("scan.poll_ms", "250"),
        ("limits.soft", "70"),
    ]);
    let h = booted(&store, Env::fixed([("TEST_LIMITS_HARD", "lots")])).await;

    assert_eq!(
        *h.scan.load(),
        Scan {
            depth: 20,
            poll_ms: 250
        },
        "poll_ms keeps its stored value"
    );
    assert_eq!(
        *h.limits.load(),
        Limits {
            soft: 70,
            hard: 300
        }
    );

    let views = h.registry.describe();
    let depth = view(&views, "scan.depth");
    assert_eq!(depth.source, SettingSource::Default);
    assert_eq!(depth.value, "20");
    let problem = depth.problem.as_ref().unwrap();
    assert!(!problem.from_env);
    assert!(
        problem.message.contains("saved value is invalid"),
        "{}",
        problem.message
    );
    assert_eq!(view(&views, "scan.poll_ms").problem, None);

    let hard = view(&views, "limits.hard").problem.as_ref().unwrap();
    assert!(
        hard.from_env,
        "the page must say the bad value is in the environment"
    );
    assert!(hard.message.contains("TEST_LIMITS_HARD"));
}

#[tokio::test]
async fn at_boot_a_section_whose_values_break_a_rule_uses_its_defaults_until_fixed() {
    let store = TestStore::with(&[("limits.soft", "500"), ("limits.hard", "100")]);
    let h = booted(&store, no_env()).await;
    assert_eq!(
        *h.limits.load(),
        Limits {
            soft: 60,
            hard: 300
        }
    );
    assert_eq!(h.registry.section_problems().len(), 1);
    let soft = view(&h.registry.describe(), "limits.soft").clone();
    assert!(soft
        .problem
        .unwrap()
        .message
        .contains("limits settings are using their defaults"));

    h.registry
        .save(vec![change("limits.hard", "1000")])
        .await
        .unwrap();
    assert_eq!(
        *h.limits.load(),
        Limits {
            soft: 500,
            hard: 1000
        }
    );
    assert!(h.registry.section_problems().is_empty());
    assert_eq!(view(&h.registry.describe(), "limits.soft").problem, None);
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
        ("scan.depth", "not a number"),
        ("scan.poll_ms", "250"),
        ("limits.soft", "500"),
        ("limits.hard", "100"),
        ("server.workers", "8"),
    ]);
    let env = Env::fixed([("TEST_BIND", "0.0.0.0:9000")]);
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
        env_var: "TEST_BAD_DEFAULT",
        default: || 0,
        check: None,
        bounds: range(1, 10),
        description: "",
        example: None,
        applies: Applies::Live,
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
            env: "TEST_EVEN",
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
async fn saved_secrets_are_applied_but_never_shown() {
    let store = TestStore::with(&[]);
    let h = booted(&store, no_env()).await;
    h.registry
        .save(vec![change("engine.token", "hunter2")])
        .await
        .unwrap();
    assert_eq!(h.auth.load().token.expose(), "hunter2");
    assert_eq!(store.get("engine.token").as_deref(), Some("hunter2"));
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
