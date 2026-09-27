//! Sections, the values readers hold, and the runtime state behind them.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use serde::Serialize;
use tokio::sync::watch;

use crate::setting::{AnySetting, Snapshot};

/// A reason one field can't be saved or applied, shown next to that field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub key: String,
    pub message: String,
}

impl FieldError {
    pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        FieldError { key: key.into(), message: message.into() }
    }
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

impl std::error::Error for FieldError {}

/// Something a save accepted but the admin should know about, like a
/// node that doesn't answer yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    /// The setting it concerns, if it concerns one.
    pub key: Option<String>,
    pub message: String,
}

impl Warning {
    pub fn new(message: impl Into<String>) -> Self {
        Warning { key: None, message: message.into() }
    }

    pub fn for_key(key: impl Into<String>, message: impl Into<String>) -> Self {
        Warning { key: Some(key.into()), message: message.into() }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.key {
            Some(key) => write!(f, "{key}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// A group of settings that one runtime piece depends on, as a plain typed
/// struct. Built from a snapshot of every setting, and able to reject
/// combinations of values (`soft <= hard`) that each look fine on their
/// own.
///
/// Every key of a section must be `Applies::Live`, or every key must be
/// `Applies::Restart`; `Registry::build` refuses a mix, since a section is
/// either swapped in on save or not.
pub trait Section: Clone + PartialEq + Send + Sync + 'static {
    /// A name for messages and logs.
    const NAME: &'static str;

    /// The settings this section reads.
    fn keys() -> &'static [&'static dyn AnySetting];

    /// Builds the section, or says which fields are wrong together. Read
    /// values with `snapshot.get(&SETTING)`; each one is already valid on
    /// its own.
    ///
    /// Must accept every setting at its default: the registry falls back to
    /// the all-defaults section when this fails, and treats defaults this
    /// rejects as a bug (see `Registry::build`).
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>>;
}

/// A section's current value, as readers hold it. Cheap to clone and never
/// blocks for long: `load()` hands back an `Arc<T>`, so no lock is ever
/// held across an `.await` by a reader.
pub struct Live<T> {
    inner: Arc<LiveInner<T>>,
}

struct LiveInner<T> {
    // A parking_lot lock around an `Arc`, held only to clone or replace
    // the `Arc`. The workspace doesn't use arc-swap, and at settings'
    // write rate this costs nothing.
    value: RwLock<Arc<T>>,
    changed: watch::Sender<()>,
}

impl<T> Clone for Live<T> {
    fn clone(&self) -> Self {
        Live { inner: Arc::clone(&self.inner) }
    }
}

impl<T: fmt::Debug> fmt::Debug for Live<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Live").field(&*self.load()).finish()
    }
}

impl<T> Live<T> {
    /// A value that never changes unless a registry publishes to it. Handy
    /// for handing fixed settings to code under test.
    pub fn new(value: T) -> Self {
        let (changed, _) = watch::channel(());
        Live { inner: Arc::new(LiveInner { value: RwLock::new(Arc::new(value)), changed }) }
    }

    /// The current value.
    pub fn load(&self) -> Arc<T> {
        Arc::clone(&self.inner.value.read())
    }

    /// Fires after each change. For loops that sleep: subscribe once,
    /// before the loop, and keep the receiver. A fresh receiver per
    /// iteration can miss a change made between `load()` and subscribing.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.inner.changed.subscribe()
    }

    pub(crate) fn publish(&self, value: T) {
        *self.inner.value.write() = Arc::new(value);
        self.inner.changed.send_replace(());
    }
}

/// What boot does when a reloadable's `prepare` fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootPolicy {
    /// Refuse to start: a listener that can't bind, for instance.
    Exit,
    /// Start without it: a node or custody backend that isn't reachable
    /// yet. The piece is expected to keep retrying on its own.
    StartDegraded,
}

/// Runtime state built from a section that needs work to swap in: node
/// clients, a listener, a custody backend, the engine client.
///
/// `install` only takes what `prepare` produced, so nothing can be
/// installed that wasn't prepared.
#[async_trait]
pub trait Reloadable: Send + Sync + 'static {
    type Config: Section;
    /// The new state, built but not yet in use. Dropping it must release
    /// whatever it holds (a bound listener closes, for instance), because
    /// a refused save drops everything it prepared.
    type Prepared: Send;

    /// Builds the new state without touching the running one. An error
    /// refuses the save; warnings are reported and the save goes ahead.
    ///
    /// At boot nothing is installed yet and `old` is the section with every
    /// setting at its default, so an implementation must not skip building
    /// just because `new == old`.
    async fn prepare(&self, new: &Self::Config, old: &Self::Config) -> Result<(Self::Prepared, Vec<Warning>), FieldError>;

    /// Swaps the prepared state in. Must not fail. The registry awaits it
    /// while still holding the save mutex, so the next save can't overlap
    /// it, and finishes it even if the caller of `save` goes away.
    async fn install(&self, prepared: Self::Prepared);

    fn boot_policy(&self) -> BootPolicy;
}
