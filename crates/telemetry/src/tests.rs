use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use super::*;

/// Everything the subscriber wrote, shared with the test.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().clone()).unwrap()
    }

    fn json_lines(&self) -> Vec<Value> {
        self.text().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn clear(&self) {
        self.0.lock().clear();
    }
}

fn subscriber(format: Format, level: &str) -> (Arc<Telemetry>, Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let writer = capture.clone();
    let (telemetry, subscriber) = build("scanner", format, false, level, move || writer.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    (Arc::new(telemetry), capture, guard)
}

fn address() -> String {
    format!("44AFFq{}wEP3A", "5kSiGBoZ4NMDwYtN18obc8".repeat(4).chars().take(84).collect::<String>())
}

#[test]
fn a_json_line_carries_the_event_and_every_enclosing_spans_fields_redacted() {
    let (_telemetry, capture, _guard) = subscriber(Format::Json, "info");
    let outer = tracing::info_span!("network loop", network = "stagenet", store.id = "t_1");
    let _outer = outer.enter();
    let inner = tracing::info_span!("scan", store.id = "t_2", height = tracing::field::Empty);
    inner.record("height", 3_100_000u64);
    let _inner = inner.enter();
    tracing::warn!(
        order.id = "o_9",
        secret_token = "sk_0123456789abcdef0123456789abcdef",
        client.address = "203.0.113.77:4000",
        attempts = 3,
        "payment to {} not seen",
        address()
    );

    let lines = capture.json_lines();
    assert_eq!(lines.len(), 1, "{}", capture.text());
    let line = &lines[0];
    assert_eq!(line["level"], "WARN");
    assert_eq!(line["service"], "scanner");
    assert_eq!(line["target"], module_path!());
    assert_eq!(line["message"], "payment to 44AFFq…EP3A not seen");
    assert!(line["timestamp"].as_str().unwrap().ends_with('Z'));
    let attributes = &line["attributes"];
    assert_eq!(attributes["network"], "stagenet");
    assert_eq!(attributes["store.id"], "t_2", "the inner span's value wins");
    assert_eq!(attributes["height"], 3_100_000, "recorded after the span started, and kept as a number");
    assert_eq!(attributes["order.id"], "o_9");
    assert_eq!(attributes["attempts"], 3);
    assert_eq!(attributes["secret_token"], redact::REDACTED);
    assert_eq!(attributes["client.address"], "203.0.113.0");
    assert_eq!(line["spans"], serde_json::json!(["network loop", "scan"]));
    assert!(!capture.text().contains("sk_0123"), "{}", capture.text());
}

#[test]
fn an_event_outside_any_span_has_empty_attributes_and_no_spans() {
    let (_telemetry, capture, _guard) = subscriber(Format::Json, "info");
    tracing::info!("started");
    let text = capture.text();
    let keys: Vec<usize> = ["\"timestamp\"", "\"level\"", "\"service\"", "\"target\"", "\"message\"", "\"attributes\""]
        .iter()
        .map(|k| text.find(k).unwrap())
        .collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "keys in a fixed, readable order: {text}");
    let line = &capture.json_lines()[0];
    assert_eq!(line["attributes"], serde_json::json!({}));
    assert!(line.get("spans").is_none());
}

#[test]
fn pretty_output_is_redacted_too() {
    let (_telemetry, capture, _guard) = subscriber(Format::Pretty, "info");
    let secret = format!("sk_{}", "cd".repeat(32));
    tracing::error!(view_key = "deadbeef", detail = %format!("token {secret}"), "sending to {}", address());
    let text = capture.text();
    assert!(text.contains("sending to 44AFFq…EP3A"), "{text}");
    assert!(text.contains("view_key=[redacted]"), "{text}");
    assert!(text.contains("detail=token sk_[redacted]"), "{text}");
    assert!(!text.contains("deadbeef") && !text.contains(&secret), "{text}");
}

#[test]
fn applying_a_level_changes_what_is_written_straight_away() {
    let (telemetry, capture, _guard) = subscriber(Format::Json, "info");
    tracing::debug!("hidden");
    assert!(capture.text().is_empty());

    telemetry.apply(&LogConfig { level: "debug".into(), dev_mode_until: 0 });
    tracing::debug!("shown");
    assert_eq!(capture.json_lines()[0]["message"], "shown");
    assert_eq!(telemetry.status().effective_filter, "debug");

    capture.clear();
    telemetry.apply(&LogConfig { level: format!("warn,{}=debug", module_path!()), dev_mode_until: 0 });
    tracing::debug!("this target only");
    tracing::info!(target: "somewhere_else", "hidden");
    let lines = capture.json_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["message"], "this target only");
}

#[test]
fn a_filter_that_does_not_parse_leaves_the_old_one_in_place() {
    let (telemetry, capture, _guard) = subscriber(Format::Json, "info");
    telemetry.apply(&LogConfig { level: "info,===".into(), dev_mode_until: 0 });
    assert_eq!(telemetry.status().effective_filter, "info");
    tracing::debug!("still hidden");
    assert!(!capture.text().contains("still hidden"));
}

#[tokio::test(start_paused = true)]
async fn development_mode_logs_debug_until_its_time_then_turns_itself_off() {
    let (telemetry, capture, _guard) = subscriber(Format::Json, "info");
    let until = now_unix() + 600;
    telemetry.apply(&LogConfig { level: "info".into(), dev_mode_until: until });
    let status = telemetry.status();
    assert!(status.dev_mode);
    assert!(status.effective_filter.starts_with("debug,hyper=info"), "{}", status.effective_filter);
    tracing::debug!("during");
    assert!(capture.text().contains("during"));
    assert!(capture.text().contains("development logging is on"));

    tokio::time::sleep(Duration::from_secs(601)).await;
    assert!(!telemetry.status().dev_mode);
    assert_eq!(telemetry.status().effective_filter, "info");
    assert!(capture.text().contains("development logging has ended"));
    capture.clear();
    tracing::debug!("after");
    assert!(capture.text().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_later_apply_cancels_the_earlier_expiry() {
    let (telemetry, _capture, _guard) = subscriber(Format::Json, "info");
    telemetry.apply(&LogConfig { level: "info".into(), dev_mode_until: now_unix() + 60 });
    telemetry.apply(&LogConfig { level: "info".into(), dev_mode_until: now_unix() + 3600 });
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert!(telemetry.status().dev_mode, "the first timer must not end the second window");
}

#[test]
fn a_development_time_already_past_is_off() {
    let (telemetry, _capture, _guard) = subscriber(Format::Json, "info");
    telemetry.apply(&LogConfig { level: "info".into(), dev_mode_until: 1 });
    assert!(!telemetry.status().dev_mode);
    assert_eq!(telemetry.status().effective_filter, "info");
}

#[test]
fn development_mode_keeps_the_targets_the_level_names() {
    assert_eq!(dev_filter("warn,scanner::loops=trace"), format!("debug,{DEV_MODE_QUIET},scanner::loops=trace"));
    assert_eq!(dev_filter("info"), format!("debug,{DEV_MODE_QUIET}"));
}

#[test]
fn check_level_accepts_filters_and_refuses_the_rest() {
    for ok in ["info", "debug", "warn,scanner::loops=debug", " info "] {
        assert!(check_level(&ok.to_string()).is_ok(), "{ok}");
    }
    for bad in ["", "  ", "info,===", "scanner=loud"] {
        assert!(check_level(&bad.to_string()).is_err(), "{bad}");
    }
}

#[test]
fn unix_times_read_as_utc() {
    assert_eq!(format_unix_utc(1_790_000_000), "2026-09-21 14:13 UTC");
}

mod through_settings {
    use live_settings::{settings, AnySetting, FieldError, Section, Snapshot};

    use super::*;

    settings! {
        LEVEL: String {
            key: "logging.level",
            env: "TELEMETRY_TEST_LOG",
            default: DEFAULT_LEVEL.to_string(),
            check: check_level,
            description: "level",
        },
        DEV_MODE_UNTIL: u64 {
            key: "logging.dev_mode_until",
            env: "TELEMETRY_TEST_LOGGING_DEV_MODE_UNTIL",
            default: 0,
            description: "until",
        },
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Logging(LogConfig);

    impl AsRef<LogConfig> for Logging {
        fn as_ref(&self) -> &LogConfig {
            &self.0
        }
    }

    impl Section for Logging {
        const NAME: &'static str = "logging";
        fn keys() -> &'static [&'static dyn AnySetting] {
            &[&LEVEL, &DEV_MODE_UNTIL]
        }
        fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
            Ok(Logging(LogConfig { level: snapshot.get(&LEVEL), dev_mode_until: snapshot.get(&DEV_MODE_UNTIL) }))
        }
    }

    /// The one test in this binary that installs the process-wide
    /// subscriber; the others use a thread-local one, which wins on their
    /// own threads.
    #[tokio::test]
    async fn a_saved_level_reaches_the_process_wide_subscriber() {
        let store = Arc::new(live_settings::MemoryStore::default());
        let mut builder = live_settings::Registry::builder_with_env(store, ALL, live_settings::Env::fixed::<&str, &str>([]));
        builder.reloadable(LogReloadable::<Logging>::default());
        let registry = builder.build().unwrap();
        let telemetry = init("telemetry-test", "TELEMETRY_TEST");
        registry.boot().await.unwrap();
        assert_eq!(telemetry.status().effective_filter, "info");

        registry.save(vec![("logging.level".to_string(), Some("debug,hyper=warn".to_string()))]).await.unwrap();
        assert_eq!(telemetry.status().config.level, "debug,hyper=warn");
        assert_eq!(telemetry.status().effective_filter, "debug,hyper=warn");
    }
}
