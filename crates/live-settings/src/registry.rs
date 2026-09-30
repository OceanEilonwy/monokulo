//! The one owner of every section in a process: boot, save and describe.

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::join_all;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;

use crate::section::{BootPolicy, FieldError, Live, Reloadable, Section, Warning};
use crate::setting::{AnySetting, Applies, Env, Problem, SettingSource, Snapshot};
use crate::store::{SettingsStore, StoreError};
use crate::value::SettingKind;

/// Changes submitted in one save: a key and its new raw value, or `None`
/// to delete the stored value so the setting goes back to its default.
pub type Changes = Vec<(String, Option<String>)>;

/// What an accepted save did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SaveReport {
    /// Keys whose stored value changed. A submitted value equal to the one
    /// already stored is not a change.
    pub changed: Vec<&'static str>,
    /// Restart-only keys whose effective value now differs from the one
    /// the process started with.
    pub restart_required: Vec<&'static str>,
    /// Warnings from the reloadables that were prepared.
    pub warnings: Vec<Warning>,
    /// Changed keys whose environment variable still wins, so the saved
    /// value has no effect until it is unset.
    pub env_overridden: Vec<&'static str>,
}

/// Why a save was refused. Nothing was stored or applied.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("there is no setting called {0:?}")]
    UnknownKey(String),
    /// A value is invalid, a combination is invalid, or a reloadable
    /// couldn't prepare it.
    #[error("{}", join_errors(.0))]
    Invalid(Vec<FieldError>),
    #[error("the settings store failed: {0}")]
    Store(#[from] StoreError),
    #[error("settings can't be saved before the registry has booted")]
    NotBooted,
    /// An `install` panicked. The values were already stored.
    #[error("applying the saved settings failed: {0}")]
    Install(String),
}

fn join_errors(errors: &[FieldError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Why `Registry::build` refused. Each of these is a mistake in the code
/// declaring the settings, so each crate's unit test that calls `build()`
/// catches it, apart from `Store`.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("the settings store couldn't be read: {0}")]
    Store(StoreError),
    #[error("the setting {0} is declared twice")]
    DuplicateKey(&'static str),
    #[error("the setting {key} is declared wrongly: {message}")]
    BadDeclaration { key: &'static str, message: String },
    #[error("section {section} reads {key}, which isn't declared")]
    UndeclaredKey {
        section: &'static str,
        key: &'static str,
    },
    #[error("these settings belong to no section, so nothing would ever read them: {0:?}")]
    Orphaned(Vec<&'static str>),
    #[error("section {0} is registered twice")]
    DuplicateSection(&'static str),
    #[error("section {0} has no settings")]
    EmptySection(&'static str),
    #[error("section {0} mixes live and restart-only settings")]
    MixedApplies(&'static str),
}

/// What boot did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BootReport {
    pub warnings: Vec<Warning>,
    /// Sections whose `prepare` failed with `BootPolicy::StartDegraded`,
    /// and why. They were not installed.
    pub degraded: Vec<(&'static str, FieldError)>,
}

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// A section with `BootPolicy::Exit` couldn't prepare.
    #[error("{section} couldn't start: {error}")]
    Exit {
        section: &'static str,
        error: FieldError,
    },
    #[error("the registry has already booted")]
    AlreadyBooted,
    #[error("applying the settings at boot failed: {0}")]
    Install(String),
}

/// One setting as the admin page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingView {
    pub key: &'static str,
    pub env_var: &'static str,
    /// The effective value; masked for secrets.
    pub value: String,
    /// The default; masked for secrets.
    pub default: String,
    pub source: SettingSource,
    pub description: &'static str,
    pub kind: SettingKind,
    pub example: Option<&'static str>,
    pub applies: Applies,
    /// A restart-only setting whose effective value (environment included)
    /// differs from the one the process started with.
    pub pending_restart: bool,
    /// Why the value in effect isn't the one that was set, if it isn't.
    pub problem: Option<Problem>,
}

/// A section registered with the registry, whatever its type.
///
/// Save and boot run in phases (stage, prepare, commit), and each entry
/// keeps its own staged and prepared values between them, typed, so
/// nothing needs downcasting. Only one save or boot runs at a time (the
/// save mutex), so one slot per entry is enough.
#[async_trait]
trait Entry: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn keys(&self) -> &'static [&'static dyn AnySetting];
    fn restart_only(&self) -> bool;
    fn boot_policy(&self) -> BootPolicy;
    /// Builds the section from `snapshot` and keeps it staged. `Ok(true)`
    /// if it differs from the current value.
    fn stage(&self, snapshot: &Snapshot) -> Result<bool, Vec<FieldError>>;
    /// Stages the current value, for boot.
    fn stage_current(&self);
    async fn prepare_staged(&self, boot: bool) -> Result<Vec<Warning>, FieldError>;
    /// Installs what was prepared, then publishes the staged value.
    async fn commit(&self);
    /// Drops anything staged or prepared.
    fn discard(&self);
}

/// A plain section: a reloadable with nothing to prepare.
struct Plain<S>(PhantomData<fn() -> S>);

#[async_trait]
impl<S: Section> Reloadable for Plain<S> {
    type Config = S;
    type Prepared = ();

    async fn prepare(&self, _new: &S, _old: &S) -> Result<((), Vec<Warning>), FieldError> {
        Ok(((), Vec::new()))
    }

    async fn install(&self, _prepared: ()) {}

    fn boot_policy(&self) -> BootPolicy {
        BootPolicy::StartDegraded
    }
}

struct SectionEntry<R: Reloadable> {
    reloadable: R,
    live: Live<R::Config>,
    defaults: R::Config,
    restart_only: bool,
    staged: Mutex<Option<R::Config>>,
    prepared: Mutex<Option<R::Prepared>>,
}

#[async_trait]
impl<R: Reloadable> Entry for SectionEntry<R> {
    fn name(&self) -> &'static str {
        R::Config::NAME
    }

    fn keys(&self) -> &'static [&'static dyn AnySetting] {
        R::Config::keys()
    }

    fn restart_only(&self) -> bool {
        self.restart_only
    }

    fn boot_policy(&self) -> BootPolicy {
        self.reloadable.boot_policy()
    }

    fn stage(&self, snapshot: &Snapshot) -> Result<bool, Vec<FieldError>> {
        let value = R::Config::from_snapshot(snapshot)?;
        let differs = *self.live.load() != value;
        *self.staged.lock() = Some(value);
        Ok(differs)
    }

    fn stage_current(&self) {
        let current = (*self.live.load()).clone();
        *self.staged.lock() = Some(current);
    }

    async fn prepare_staged(&self, boot: bool) -> Result<Vec<Warning>, FieldError> {
        let new = (*self.staged.lock()).clone();
        let Some(new) = new else {
            return Ok(Vec::new());
        };
        let old = if boot {
            self.defaults.clone()
        } else {
            (*self.live.load()).clone()
        };
        let (prepared, warnings) = self.reloadable.prepare(&new, &old).await?;
        *self.prepared.lock() = Some(prepared);
        Ok(warnings)
    }

    async fn commit(&self) {
        let prepared = self.prepared.lock().take();
        let staged = self.staged.lock().take();
        if let Some(prepared) = prepared {
            self.reloadable.install(prepared).await;
        }
        if let Some(value) = staged {
            if *self.live.load() != value {
                self.live.publish(value);
            }
        }
    }

    fn discard(&self) {
        let prepared = self.prepared.lock().take();
        let staged = self.staged.lock().take();
        // Dropped here, outside the locks: dropping a prepared value may
        // do real work (closing a listener).
        drop((prepared, staged));
    }
}

/// Discards every staged and prepared value unless disarmed, so a refused
/// save, or one whose caller went away mid-prepare, releases what it built.
struct Staging {
    entries: Vec<Arc<dyn Entry>>,
    armed: bool,
}

impl Staging {
    fn new(entries: Vec<Arc<dyn Entry>>) -> Self {
        Staging {
            entries,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.armed {
            for entry in &self.entries {
                entry.discard();
            }
        }
    }
}

/// Builds a [`Registry`]. Sections are registered here, and each hands
/// back the `Live` value its readers keep.
pub struct RegistryBuilder {
    store: Arc<dyn SettingsStore>,
    env: Env,
    declared: Vec<&'static dyn AnySetting>,
    snapshot: Snapshot,
    read_error: Option<StoreError>,
    entries: Vec<Arc<dyn Entry>>,
    section_problems: HashMap<&'static str, Vec<FieldError>>,
}

impl RegistryBuilder {
    /// Registers a plain section: readers get a `Live<S>`, updated on every
    /// save that changes it.
    pub fn section<S: Section>(&mut self) -> Live<S> {
        self.register(Plain::<S>(PhantomData))
    }

    /// Registers a section with runtime state behind it. `boot` prepares
    /// and installs it; each save that changes it prepares and installs it
    /// again.
    pub fn reloadable<R: Reloadable>(&mut self, reloadable: R) -> Live<R::Config> {
        self.register(reloadable)
    }

    fn register<R: Reloadable>(&mut self, reloadable: R) -> Live<R::Config> {
        let (value, problem) = section_value::<R::Config>(&self.snapshot);
        if let Some(errors) = problem {
            self.section_problems.insert(R::Config::NAME, errors);
        }
        let keys = R::Config::keys();
        let restart_only = !keys.is_empty() && keys.iter().all(|k| k.applies() == Applies::Restart);
        let live = Live::new(value);
        self.entries.push(Arc::new(SectionEntry {
            reloadable,
            live: live.clone(),
            defaults: defaults_of::<R::Config>(),
            restart_only,
            staged: Mutex::new(None),
            prepared: Mutex::new(None),
        }));
        live
    }

    /// Checks the declarations and sections, and logs any value that is
    /// set but invalid. Nothing is prepared or installed until `boot`.
    pub fn build(self) -> Result<Registry, BuildError> {
        if let Some(e) = self.read_error {
            return Err(BuildError::Store(e));
        }
        let mut by_key: HashMap<&'static str, &'static dyn AnySetting> = HashMap::new();
        for setting in &self.declared {
            if by_key.insert(setting.key(), *setting).is_some() {
                return Err(BuildError::DuplicateKey(setting.key()));
            }
            setting
                .check_declaration()
                .map_err(|message| BuildError::BadDeclaration {
                    key: setting.key(),
                    message,
                })?;
        }

        let mut names = HashSet::new();
        let mut covered = HashSet::new();
        for entry in &self.entries {
            let section = entry.name();
            if !names.insert(section) {
                return Err(BuildError::DuplicateSection(section));
            }
            let keys = entry.keys();
            if keys.is_empty() {
                return Err(BuildError::EmptySection(section));
            }
            for key in keys {
                if !by_key.contains_key(key.key()) {
                    return Err(BuildError::UndeclaredKey {
                        section,
                        key: key.key(),
                    });
                }
                covered.insert(key.key());
            }
            let restart = keys
                .iter()
                .filter(|k| k.applies() == Applies::Restart)
                .count();
            if restart != 0 && restart != keys.len() {
                return Err(BuildError::MixedApplies(section));
            }
        }
        let orphaned: Vec<_> = self
            .declared
            .iter()
            .map(|s| s.key())
            .filter(|k| !covered.contains(k))
            .collect();
        if !orphaned.is_empty() {
            return Err(BuildError::Orphaned(orphaned));
        }

        let stored = self.snapshot.stored().clone();
        let mut boot_effective = HashMap::new();
        for setting in &self.declared {
            let view =
                setting.resolve_view(stored.get(setting.key()).map(String::as_str), &self.env);
            if let Some(problem) = &view.problem {
                tracing::warn!(setting = setting.key(), "settings: {}", problem.message);
            }
            if setting.applies() == Applies::Restart {
                boot_effective.insert(setting.key(), view.stored_form);
            }
        }
        for (section, errors) in &self.section_problems {
            tracing::warn!(section = %section, reasons = %join_errors(errors), "settings: section is using its defaults");
        }

        Ok(Registry {
            inner: Arc::new(Inner {
                store: self.store,
                env: self.env,
                declared: self.declared,
                by_key,
                entries: self.entries,
                stored: RwLock::new(stored),
                boot_effective,
                section_problems: RwLock::new(self.section_problems),
                save_lock: Arc::new(tokio::sync::Mutex::new(false)),
            }),
        })
    }
}

/// The one owner of every section in a process. Cheap to clone.
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn SettingsStore>,
    env: Env,
    declared: Vec<&'static dyn AnySetting>,
    by_key: HashMap<&'static str, &'static dyn AnySetting>,
    entries: Vec<Arc<dyn Entry>>,
    /// The stored values as of the last read or accepted save, so
    /// `describe` needs no I/O and can't fail.
    stored: RwLock<HashMap<String, String>>,
    /// Each restart-only setting's effective stored form when this registry
    /// was built, environment included.
    boot_effective: HashMap<&'static str, String>,
    /// Sections whose `from_snapshot` rejected the stored values at build,
    /// so they run on defaults. Cleared by a save that fixes them.
    section_problems: RwLock<HashMap<&'static str, Vec<FieldError>>>,
    /// Held for the whole of a boot or save, installs included. The flag
    /// says whether boot has run.
    save_lock: Arc<tokio::sync::Mutex<bool>>,
}

impl Registry {
    /// Starts a registry over `store`, resolving against the real process
    /// environment. `declared` is every setting the process has (each
    /// module's `ALL`); `build` refuses one that no section reads.
    ///
    /// The store is read once here, so each section's `Live` value exists
    /// as soon as it is registered.
    pub fn builder(
        store: Arc<dyn SettingsStore>,
        declared: &[&'static dyn AnySetting],
    ) -> RegistryBuilder {
        Registry::builder_with_env(store, declared, Env::process())
    }

    /// As [`Registry::builder`], resolving against `env` instead of the
    /// process environment.
    pub fn builder_with_env(
        store: Arc<dyn SettingsStore>,
        declared: &[&'static dyn AnySetting],
        env: Env,
    ) -> RegistryBuilder {
        let (stored, read_error) = match store.read_all() {
            Ok(stored) => (stored, None),
            Err(e) => (HashMap::new(), Some(e)),
        };
        RegistryBuilder {
            store,
            snapshot: Snapshot::new(stored, env.clone()),
            env,
            declared: declared.to_vec(),
            read_error,
            entries: Vec::new(),
            section_problems: HashMap::new(),
        }
    }

    /// Prepares and installs every reloadable, restart-only ones included.
    /// A `prepare` that fails follows that reloadable's boot policy. Must
    /// run once, before any save.
    pub async fn boot(&self) -> Result<BootReport, BootError> {
        let lock = Arc::clone(&self.inner.save_lock).lock_owned().await;
        if *lock {
            return Err(BootError::AlreadyBooted);
        }
        let entries = self.inner.entries.clone();
        let staging = Staging::new(entries.clone());
        for entry in &entries {
            entry.stage_current();
        }
        let results = join_all(entries.iter().map(|entry| entry.prepare_staged(true))).await;

        let mut report = BootReport::default();
        let mut to_apply = Vec::new();
        for (entry, result) in entries.iter().zip(results) {
            match result {
                Ok(warnings) => {
                    report.warnings.extend(warnings);
                    to_apply.push(Arc::clone(entry));
                }
                Err(error) => match entry.boot_policy() {
                    BootPolicy::Exit => {
                        return Err(BootError::Exit {
                            section: entry.name(),
                            error,
                        })
                    }
                    BootPolicy::StartDegraded => {
                        tracing::warn!(section = %entry.name(), error = %error, "settings: starting without it");
                        entry.discard();
                        report.degraded.push((entry.name(), error));
                    }
                },
            }
        }
        staging.disarm();

        // Spawned, so the installs finish even if the caller stops polling.
        tokio::spawn(async move {
            let mut lock = lock;
            for entry in &to_apply {
                entry.commit().await;
            }
            for entry in &entries {
                entry.discard();
            }
            *lock = true;
        })
        .await
        .map_err(|e| BootError::Install(e.to_string()))?;
        Ok(report)
    }

    /// Validates, prepares, persists and installs `changes`, under one
    /// mutex so saves never overlap:
    ///
    /// 1. Parse every value. Any failure refuses the save, naming each bad
    ///    field. Unknown keys are refused.
    /// 2. Rebuild every section that reads a changed key (cross-field
    ///    rules). Any failure refuses the save.
    /// 3. Prepare every changed live reloadable, all at once. Any failure
    ///    refuses the save and drops everything prepared.
    /// 4. Write every change in one transaction. A failure refuses the save
    ///    and drops everything prepared.
    /// 5. Install what was prepared, then publish the changed sections.
    ///    Restart-only sections are stored but not installed.
    ///
    /// Once step 4 has succeeded, step 5 runs to the end even if the
    /// caller stops waiting.
    pub async fn save(&self, changes: Changes) -> Result<SaveReport, SaveError> {
        let inner = &self.inner;
        let lock = Arc::clone(&inner.save_lock).lock_owned().await;
        if !*lock {
            return Err(SaveError::NotBooted);
        }

        // 1. Parse.
        let mut errors = Vec::new();
        let mut parsed: Vec<(&'static dyn AnySetting, Option<String>)> = Vec::new();
        let mut seen = HashSet::new();
        for (key, raw) in &changes {
            let Some(setting) = inner.by_key.get(key.as_str()).copied() else {
                return Err(SaveError::UnknownKey(key.clone()));
            };
            if !seen.insert(setting.key()) {
                errors.push(FieldError::new(
                    key,
                    "This setting was submitted more than once.",
                ));
                continue;
            }
            match raw {
                Some(raw) => match setting.normalise(raw) {
                    Ok(value) => parsed.push((setting, Some(value))),
                    Err(message) => errors.push(FieldError::new(key, message)),
                },
                None => parsed.push((setting, None)),
            }
        }
        if !errors.is_empty() {
            return Err(SaveError::Invalid(errors));
        }

        let old = inner.store.read_all()?;
        let mut new = old.clone();
        let mut writes: Vec<(&'static str, Option<String>)> = Vec::new();
        let mut changed: Vec<&'static dyn AnySetting> = Vec::new();
        for (setting, value) in parsed {
            let key = setting.key();
            let differs = match &value {
                Some(value) => old.get(key) != Some(value),
                None => old.contains_key(key),
            };
            if !differs {
                continue;
            }
            match &value {
                Some(value) => new.insert(key.to_string(), value.clone()),
                None => new.remove(key),
            };
            writes.push((key, value));
            changed.push(setting);
        }
        if changed.is_empty() {
            *inner.stored.write() = old;
            return Ok(SaveReport::default());
        }

        // 2. Rebuild the sections that read a changed key.
        let snapshot = Snapshot::new(new, inner.env.clone());
        let changed_keys: HashSet<&str> = changed.iter().map(|s| s.key()).collect();
        let touched: Vec<Arc<dyn Entry>> = inner
            .entries
            .iter()
            .filter(|entry| entry.keys().iter().any(|k| changed_keys.contains(k.key())))
            .cloned()
            .collect();
        let staging = Staging::new(touched.clone());
        let mut to_apply = Vec::new();
        for entry in &touched {
            match entry.stage(&snapshot) {
                Ok(true) if !entry.restart_only() => to_apply.push(Arc::clone(entry)),
                Ok(_) => {}
                Err(section_errors) => errors.extend(section_errors),
            }
        }
        if !errors.is_empty() {
            return Err(SaveError::Invalid(errors));
        }

        // 3. Prepare.
        let mut warnings = Vec::new();
        for result in join_all(to_apply.iter().map(|entry| entry.prepare_staged(false))).await {
            match result {
                Ok(w) => warnings.extend(w),
                Err(error) => errors.push(error),
            }
        }
        if !errors.is_empty() {
            return Err(SaveError::Invalid(errors));
        }

        // 4. Persist.
        inner.store.write_all(&writes)?;
        *inner.stored.write() = snapshot.stored().clone();
        {
            let mut problems = inner.section_problems.write();
            for entry in touched.iter().filter(|e| !e.restart_only()) {
                problems.remove(entry.name());
            }
        }

        // 5. Install and publish.
        staging.disarm();
        tokio::spawn(async move {
            for entry in &to_apply {
                entry.commit().await;
            }
            for entry in &touched {
                entry.discard();
            }
            drop(lock);
        })
        .await
        .map_err(|e| SaveError::Install(e.to_string()))?;

        // 6. Report.
        let env = snapshot.env();
        let mut report = SaveReport {
            warnings,
            ..SaveReport::default()
        };
        for setting in changed {
            let key = setting.key();
            report.changed.push(key);
            if env.get(setting.env_var()).is_some() {
                report.env_overridden.push(key);
            }
            if setting.applies() == Applies::Restart {
                let now = setting
                    .resolve_view(snapshot.stored().get(key).map(String::as_str), env)
                    .stored_form;
                if inner.boot_effective.get(key) != Some(&now) {
                    report.restart_required.push(key);
                }
            }
        }
        Ok(report)
    }

    /// Every declared setting, in declaration order, as the admin page
    /// shows it.
    pub fn describe(&self) -> Vec<SettingView> {
        let inner = &self.inner;
        let stored = inner.stored.read().clone();
        let section_problems = inner.section_problems.read().clone();
        inner
            .declared
            .iter()
            .map(|setting| {
                let key = setting.key();
                let view = setting.resolve_view(stored.get(key).map(String::as_str), &inner.env);
                let pending_restart = setting.applies() == Applies::Restart
                    && inner.boot_effective.get(key).is_some_and(|at_boot| *at_boot != view.stored_form);
                let problem = view.problem.or_else(|| {
                    section_problems.iter().find_map(|(section, errors)| {
                        errors.iter().find(|e| e.key == key).map(|e| Problem {
                            from_env: false,
                            message: format!("{} The {section} settings are using their defaults until this is fixed.", e.message),
                        })
                    })
                });
                SettingView {
                    key,
                    env_var: setting.env_var(),
                    value: view.shown,
                    default: setting.default_shown(),
                    source: view.source,
                    description: setting.description(),
                    kind: setting.kind(),
                    example: setting.example(),
                    applies: setting.applies(),
                    pending_restart,
                    problem,
                }
            })
            .collect()
    }

    /// Sections running on their defaults because the stored values broke
    /// a rule between fields, and why.
    pub fn section_problems(&self) -> Vec<(&'static str, Vec<FieldError>)> {
        let mut problems: Vec<_> = self
            .inner
            .section_problems
            .read()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        problems.sort_by_key(|(section, _)| *section);
        problems
    }
}

/// Reads one section straight from the store, resolving against the process
/// environment: for settings needed before an async runtime exists, such
/// as the runtime's own worker count. Returns the same value a registry
/// built from the same store would.
///
/// An unreadable store, or stored values the section rejects, give the
/// section's defaults (logged), as boot would.
pub fn read_sync<S: Section>(store: &dyn SettingsStore) -> S {
    read_sync_with_env(store, &Env::process())
}

/// As [`read_sync`], resolving against `env`.
pub fn read_sync_with_env<S: Section>(store: &dyn SettingsStore, env: &Env) -> S {
    let stored = store.read_all().unwrap_or_else(|e| {
        tracing::warn!(section = S::NAME, error = %e, "settings: couldn't read the settings store, so this section uses its defaults");
        HashMap::new()
    });
    let (value, problem) = section_value::<S>(&Snapshot::new(stored, env.clone()));
    if let Some(errors) = problem {
        tracing::warn!(section = S::NAME, reasons = %join_errors(&errors), "settings: section is using its defaults");
    }
    value
}

/// The section built from `snapshot`, or its defaults and the reason.
fn section_value<S: Section>(snapshot: &Snapshot) -> (S, Option<Vec<FieldError>>) {
    match S::from_snapshot(snapshot) {
        Ok(value) => (value, None),
        Err(errors) => (defaults_of::<S>(), Some(errors)),
    }
}

/// The section with every setting at its default.
///
/// # Panics
///
/// If the section rejects its own defaults. That is a bug in the section's
/// declaration, the same for every run, and there is no value left to fall
/// back to. Each crate's unit test that builds its registry hits this
/// first, so it can't reach a release.
fn defaults_of<S: Section>() -> S {
    match S::from_snapshot(&Snapshot::defaults()) {
        Ok(value) => value,
        Err(errors) => panic!(
            "section {} rejects its own defaults: {}",
            S::NAME,
            join_errors(&errors)
        ),
    }
}
