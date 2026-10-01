//! Structured logging for monokulo, the engine and the key-custody server
//! (structured_logging.md part 1).
//!
//! Each process calls [`init`] once, first thing in `main`. That installs a
//! `tracing` subscriber with:
//!
//! - a level filter that can be changed while running ([`Telemetry::apply`],
//!   driven by the `logging` settings section through [`LogReloadable`]);
//! - output on stderr, as one JSON object per line ([`Format::Json`], the
//!   default when stderr isn't a terminal, which is what journald and
//!   container runtimes collect) or as readable text ([`Format::Pretty`],
//!   the default at a terminal);
//! - redaction of secrets, client addresses and Monero addresses in every
//!   format ([`redact`]);
//! - OpenTelemetry trace and span ids on every span at `info` or above,
//!   whatever the level filter says, and on every line written inside one
//!   ([`trace`]).
//!
//! Development mode is a time limit, not a switch: `logging.dev_mode_until`
//! holds a Unix time, and until then the filter logs at `debug`. It ends by
//! itself, so it can't be left on in production by accident, and a restart
//! before then keeps it on until the same time.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

#[cfg(feature = "axum")]
pub mod http;
mod json;
pub mod otlp;
pub mod query;
pub mod redact;
pub mod store;
pub mod trace;

use std::io::IsTerminal;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use live_settings::{FieldError, Reloadable, Section, Warning};
use opentelemetry::trace::TracerProvider as _;
use parking_lot::Mutex;
use tracing::Subscriber;
use tracing_subscriber::field::MakeExt;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter, Layer, Registry};

/// The level when nothing else is set.
pub const DEFAULT_LEVEL: &str = "info";

/// Libraries that are unreadably chatty at `debug`; development mode keeps
/// them at `info` (or `warn`) unless the level setting names them.
const DEV_MODE_QUIET: &str =
    "hyper=info,hyper_util=info,h2=info,rustls=info,reqwest=info,tower=info,hickory_proto=warn,hickory_resolver=warn";

/// How lines are written to stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One JSON object per line.
    Json,
    /// Readable text, coloured at a terminal.
    Pretty,
}

impl Format {
    /// `<PREFIX>_LOG_FORMAT` (`json` or `pretty`) if set, otherwise pretty
    /// at a terminal and JSON everywhere else.
    pub fn from_env(env_prefix: &str) -> Format {
        match std::env::var(format!("{env_prefix}_LOG_FORMAT"))
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("json") => Format::Json,
            Some("pretty") => Format::Pretty,
            _ if std::io::stderr().is_terminal() => Format::Pretty,
            _ => Format::Json,
        }
    }
}

/// The logging settings, as the `logging` section of either process hands
/// them over. `Debug` redacts the collector headers: they carry its API key.
#[derive(Clone, PartialEq, Eq)]
pub struct LogConfig {
    /// A `tracing` filter: `info`, or `info,scanner::loops=debug`.
    pub level: String,
    /// Unix time until which development mode is on; 0 when off.
    pub dev_mode_until: u64,
    /// Days the log store keeps lines.
    pub retention_days: u64,
    /// Most megabytes the log store may use.
    pub max_mb: u64,
    /// An OpenTelemetry collector to send lines and spans to; empty for none.
    pub otlp_endpoint: String,
    /// Headers for it, as `name=value` pairs separated by commas.
    pub otlp_headers: String,
}

impl std::fmt::Debug for LogConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogConfig")
            .field("level", &self.level)
            .field("dev_mode_until", &self.dev_mode_until)
            .field("retention_days", &self.retention_days)
            .field("max_mb", &self.max_mb)
            .field("otlp_endpoint", &self.otlp_endpoint)
            .field(
                "otlp_headers",
                &if self.otlp_headers.is_empty() {
                    "(none)"
                } else {
                    "<redacted>"
                },
            )
            .finish()
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            level: DEFAULT_LEVEL.to_string(),
            dev_mode_until: 0,
            retention_days: store::DEFAULT_RETENTION_DAYS,
            max_mb: store::DEFAULT_MAX_MB,
            otlp_endpoint: String::new(),
            otlp_headers: String::new(),
        }
    }
}

/// The `check` for a level setting: refuses anything `tracing` can't parse
/// as a filter.
#[allow(
    clippy::ptr_arg,
    reason = "a setting's `check` takes `&T`, and this setting is a `String`"
)]
pub fn check_level(level: &String) -> Result<(), String> {
    if level.trim().is_empty() {
        return Err("Enter a level such as info, or leave the default.".to_string());
    }
    EnvFilter::builder().parse(level.trim()).map(|_| ()).map_err(|e| {
        format!("Not a log filter ({e}). Use a level (error, warn, info, debug, trace), optionally followed by target=level pairs, such as info,scanner::loops=debug.")
    })
}

/// The filter development mode uses: `debug`, the noisy libraries held back,
/// then any per-target directives from the level setting (which win).
fn dev_filter(level: &str) -> String {
    let targeted: Vec<&str> = level
        .split(',')
        .map(str::trim)
        .filter(|d| d.contains('='))
        .collect();
    let mut filter = format!("debug,{DEV_MODE_QUIET}");
    for directive in targeted {
        filter.push(',');
        filter.push_str(directive);
    }
    filter
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A Unix time as `2026-09-28 14:00 UTC`, for messages.
pub fn format_unix_utc(unix: u64) -> String {
    let Ok(at) = time::OffsetDateTime::from_unix_timestamp(i64::try_from(unix).unwrap_or(i64::MAX))
    else {
        return unix.to_string();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute()
    )
}

type Output = Box<dyn Layer<Registry> + Send + Sync>;

/// What is in effect right now, for the admin page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStatus {
    pub config: LogConfig,
    /// The filter actually in use: the level, or the development filter.
    pub effective_filter: String,
    /// Whether development mode is on right now.
    pub dev_mode: bool,
}

/// The running subscriber's controls.
pub struct Telemetry {
    service: &'static str,
    filter: reload::Handle<EnvFilter, Registry>,
    sink: Arc<store::StoreSink>,
    store: OnceLock<store::LogStore>,
    otlp_config: Mutex<Option<otlp::OtlpConfig>>,
    state: Mutex<LogStatus>,
    /// Bumped on every apply, so an expiry timer from an older apply does
    /// nothing.
    generation: AtomicU64,
}

static GLOBAL: OnceLock<Arc<Telemetry>> = OnceLock::new();

/// Installs the process-wide subscriber and returns its controls. `service`
/// names the process in every line (`scanner`, `monokulo`,
/// `key-custody-server`). `env_prefix` names its environment variables:
/// `<PREFIX>_LOG` sets the starting level (it is also the level setting's
/// variable, so it keeps winning after the settings load) and
/// `<PREFIX>_LOG_FORMAT` picks the format.
///
/// Calling it again (tests) returns the first process-wide controls.
pub fn init(service: &'static str, env_prefix: &str) -> Arc<Telemetry> {
    if let Some(existing) = GLOBAL.get() {
        return existing.clone();
    }
    let level = std::env::var(format!("{env_prefix}_LOG"))
        .ok()
        .filter(|l| check_level(l).is_ok())
        .unwrap_or_else(|| DEFAULT_LEVEL.to_string());
    let format = Format::from_env(env_prefix);
    let ansi = format == Format::Pretty && std::io::stderr().is_terminal();
    let (telemetry, subscriber) = build(service, format, ansi, &level, std::io::stderr);
    let telemetry = Arc::new(telemetry);
    // Fails only if something else installed a subscriber first; then the
    // first one keeps its place and these controls do nothing.
    let _ = subscriber.try_init();
    GLOBAL.get_or_init(|| telemetry).clone()
}

/// The process-wide controls, once [`init`] has run.
pub fn global() -> Option<Arc<Telemetry>> {
    GLOBAL.get().cloned()
}

/// Builds a subscriber writing to `writer`, without installing it.
pub fn build<W>(
    service: &'static str,
    format: Format,
    ansi: bool,
    level: &str,
    writer: W,
) -> (
    Telemetry,
    impl Subscriber + Send + Sync + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
)
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let filter = EnvFilter::builder()
        .parse(level)
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LEVEL));
    let (filter, filter_handle) = reload::Layer::new(filter);
    let sink = Arc::new(store::StoreSink::default());
    let output: Output = match format {
        Format::Json => Box::new(json::EventLayer::new(service, Some(writer), sink.clone())),
        Format::Pretty => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(ansi)
                .fmt_fields(tracing_subscriber::fmt::format::debug_fn(pretty_field).delimited(" "))
                .and_then(json::EventLayer::<W>::new(service, None, sink.clone())),
        ),
    };
    // The level filter applies to what is written out, not to span
    // creation, so trace ids exist even when a request's lines are
    // filtered out. OpenTelemetry sees spans at `info` and above, never
    // events (those are the log lines).
    let tracer = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_span_processor(store::StoreSpans::new(service, sink.clone()))
        .build()
        .tracer(service);
    let otel = tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_location(false)
        .with_threads(false)
        .with_filter(filter_fn(|metadata| {
            metadata.is_span() && *metadata.level() <= tracing::Level::INFO
        }));
    let subscriber = Registry::default()
        .with(output.with_filter(filter))
        .with(otel);
    let telemetry = Telemetry {
        service,
        filter: filter_handle,
        sink,
        store: OnceLock::new(),
        otlp_config: Mutex::new(None),
        state: Mutex::new(LogStatus {
            config: LogConfig {
                level: level.to_string(),
                ..LogConfig::default()
            },
            effective_filter: level.to_string(),
            dev_mode: false,
        }),
        generation: AtomicU64::new(0),
    };
    (telemetry, subscriber)
}

/// Field formatting for [`Format::Pretty`]: `name=value`, redacted, and
/// the message as plain text.
fn pretty_field(
    writer: &mut tracing_subscriber::fmt::format::Writer<'_>,
    field: &tracing::field::Field,
    value: &dyn std::fmt::Debug,
) -> std::fmt::Result {
    let raw = format!("{value:?}");
    if field.name() == "message" {
        write!(writer, "{}", redact::text(&raw))
    } else {
        write!(
            writer,
            "{}={}",
            field.name(),
            redact::field(field.name(), &raw)
        )
    }
}

impl Telemetry {
    pub fn service(&self) -> &'static str {
        self.service
    }

    /// Opens the log store at `path` (`logs.db` next to the process's main
    /// database) and starts storing lines, including those logged since
    /// start-up. Once per process; a second call returns the open store.
    pub fn open_store(&self, path: &std::path::Path) -> Result<store::LogStore, store::StoreError> {
        if let Some(open) = self.store.get() {
            return Ok(open.clone());
        }
        let opened = store::LogStore::open(path, self.sink.clone())?;
        let config = self.state.lock().config.clone();
        opened.set_limits(config.retention_days, config.max_mb);
        Ok(self.store.get_or_init(|| opened).clone())
    }

    /// [`Self::open_store`] beside the process's main database, logging
    /// (not failing) when it can't be opened: logs still go to stderr.
    pub fn open_store_beside(&self, database: &std::path::Path) -> Option<store::LogStore> {
        let path = store::path_beside(database);
        match self.open_store(&path) {
            Ok(store) => Some(store),
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "log store not opened; logs go to stderr only");
                None
            }
        }
    }

    /// Starts, replaces or stops OTLP export. Only when it changed: an
    /// unchanged config keeps the running exporter and its queue.
    pub fn set_otlp(&self, config: Option<otlp::OtlpConfig>) {
        let mut current = self.otlp_config.lock();
        if *current == config {
            return;
        }
        let exporter = config.clone().and_then(otlp::Exporter::start);
        if config.is_some() && exporter.is_none() {
            // No tokio runtime to send from; try again on the next apply.
            return;
        }
        *self.sink.otlp.write() = exporter;
        if let Some(config) = &config {
            tracing::info!(endpoint = %config.endpoint, "sending lines and spans to an OpenTelemetry collector");
        } else if current.is_some() {
            tracing::info!("stopped sending to the OpenTelemetry collector");
        }
        *current = config;
    }

    /// Waits, up to `timeout`, until every line and span logged so far is
    /// stored and, with OTLP export on, sent: the last thing a process's
    /// `main` does, so the lines saying why it stopped aren't lost with it.
    /// False if the time ran out first.
    pub async fn flush(&self, timeout: Duration) -> bool {
        let stored = self.sink.queued();
        let exported = self.sink.otlp.read().as_ref().map(|exporter| {
            let progress = exporter.progress();
            let target = progress.queued();
            (progress, target)
        });
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let done = self.sink.finished() >= stored
                && exported
                    .as_ref()
                    .is_none_or(|(progress, target)| progress.finished() >= *target);
            if done {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The log store, once [`Self::open_store`] has run.
    pub fn store(&self) -> Option<store::LogStore> {
        self.store.get().cloned()
    }

    /// What is in effect now.
    pub fn status(&self) -> LogStatus {
        self.state.lock().clone()
    }

    /// Applies `config`: the level now, or development mode until its
    /// time, after which the level comes back by itself (when there is a
    /// tokio runtime to time it; a Tokio-less caller has to apply again).
    pub fn apply(self: &Arc<Self>, config: &LogConfig) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let now = now_unix();
        let dev_mode = config.dev_mode_until > now;
        self.set(config, dev_mode);
        if dev_mode {
            tracing::info!(until = %format_unix_utc(config.dev_mode_until), "development logging is on");
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return;
            };
            let this = Arc::clone(self);
            let config = config.clone();
            let wait = Duration::from_secs(config.dev_mode_until - now);
            runtime.spawn(async move {
                tokio::time::sleep(wait).await;
                if this.generation.load(Ordering::SeqCst) == generation {
                    this.set(&config, false);
                    tracing::info!(level = %config.level, "development logging has ended");
                }
            });
        }
    }

    fn set(&self, config: &LogConfig, dev_mode: bool) {
        if let Some(store) = self.store.get() {
            store.set_limits(config.retention_days, config.max_mb);
        }
        self.set_otlp(otlp::OtlpConfig::from_settings(
            &config.otlp_endpoint,
            &config.otlp_headers,
        ));
        let effective = if dev_mode {
            dev_filter(&config.level)
        } else {
            config.level.clone()
        };
        let filter = match EnvFilter::builder().parse(&effective) {
            Ok(filter) => filter,
            Err(e) => {
                tracing::warn!(filter = %effective, error = %e, "log filter not applied");
                return;
            }
        };
        if let Err(e) = self.filter.reload(filter) {
            tracing::warn!(error = %e, "log filter not applied");
            return;
        }
        *self.state.lock() = LogStatus {
            config: config.clone(),
            effective_filter: effective,
            dev_mode,
        };
    }
}

/// Applies a process's `logging` section to the process-wide subscriber.
/// Both processes register one; `C` is their own section type, since each
/// declares the settings under its own environment variable names.
pub struct LogReloadable<C>(PhantomData<fn() -> C>);

impl<C> Default for LogReloadable<C> {
    fn default() -> Self {
        LogReloadable(PhantomData)
    }
}

#[live_settings::async_trait]
impl<C> Reloadable for LogReloadable<C>
where
    C: Section + AsRef<LogConfig>,
{
    type Config = C;
    type Prepared = LogConfig;

    async fn prepare(&self, new: &C, _old: &C) -> Result<(LogConfig, Vec<Warning>), FieldError> {
        Ok((new.as_ref().clone(), Vec::new()))
    }

    async fn install(&self, config: LogConfig) {
        if let Some(telemetry) = global() {
            telemetry.apply(&config);
        }
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

#[cfg(test)]
mod tests;
