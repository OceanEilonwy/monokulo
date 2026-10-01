use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use super::*;

/// Everything the subscriber wrote, shared with the test.
#[derive(Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

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
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.0.lock().clone()).unwrap()
    }

    pub(crate) fn json_lines(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn clear(&self) {
        self.0.lock().clear();
    }
}

pub(crate) fn subscriber(
    format: Format,
    level: &str,
) -> (Arc<Telemetry>, Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let writer = capture.clone();
    let (telemetry, subscriber) = build("scanner", format, false, level, move || writer.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    (Arc::new(telemetry), capture, guard)
}

fn address() -> String {
    format!(
        "44AFFq{}wEP3A",
        "5kSiGBoZ4NMDwYtN18obc8"
            .repeat(4)
            .chars()
            .take(84)
            .collect::<String>()
    )
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
    assert_eq!(
        attributes["height"], 3_100_000,
        "recorded after the span started, and kept as a number"
    );
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
    let keys: Vec<usize> = [
        "\"timestamp\"",
        "\"level\"",
        "\"service\"",
        "\"target\"",
        "\"message\"",
        "\"attributes\"",
    ]
    .iter()
    .map(|k| text.find(k).unwrap())
    .collect();
    assert!(
        keys.windows(2).all(|w| w[0] < w[1]),
        "keys in a fixed, readable order: {text}"
    );
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
    assert!(
        !text.contains("deadbeef") && !text.contains(&secret),
        "{text}"
    );
}

#[test]
fn pretty_output_truncates_a_client_address_like_json_does() {
    let (_telemetry, capture, _guard) = subscriber(Format::Pretty, "info");
    tracing::info!(
        client.address = "203.0.113.77:4000",
        note = "plain",
        "request"
    );
    let text = capture.text();
    assert!(text.contains("client.address=203.0.113.0"), "{text}");
    assert!(
        text.contains("note=plain"),
        "a string field is not quoted: {text}"
    );
}

#[test]
fn applying_a_level_changes_what_is_written_straight_away() {
    let (telemetry, capture, _guard) = subscriber(Format::Json, "info");
    tracing::debug!("hidden");
    assert!(capture.text().is_empty());

    telemetry.apply(&LogConfig {
        level: "debug".into(),
        dev_mode_until: 0,
        ..LogConfig::default()
    });
    tracing::debug!("shown");
    assert_eq!(capture.json_lines()[0]["message"], "shown");
    assert_eq!(telemetry.status().effective_filter, "debug");

    capture.clear();
    telemetry.apply(&LogConfig {
        level: format!("warn,{}=debug", module_path!()),
        dev_mode_until: 0,
        ..LogConfig::default()
    });
    tracing::debug!("this target only");
    tracing::info!(target: "somewhere_else", "hidden");
    let lines = capture.json_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["message"], "this target only");
}

#[test]
fn a_filter_that_does_not_parse_leaves_the_old_one_in_place() {
    let (telemetry, capture, _guard) = subscriber(Format::Json, "info");
    telemetry.apply(&LogConfig {
        level: "info,===".into(),
        dev_mode_until: 0,
        ..LogConfig::default()
    });
    assert_eq!(telemetry.status().effective_filter, "info");
    tracing::debug!("still hidden");
    assert!(!capture.text().contains("still hidden"));
}

#[tokio::test(start_paused = true)]
async fn development_mode_logs_debug_until_its_time_then_turns_itself_off() {
    let (telemetry, capture, _guard) = subscriber(Format::Json, "info");
    let until = now_unix() + 600;
    telemetry.apply(&LogConfig {
        level: "info".into(),
        dev_mode_until: until,
        ..LogConfig::default()
    });
    let status = telemetry.status();
    assert!(status.dev_mode);
    assert!(
        status.effective_filter.starts_with("debug,hyper=info"),
        "{}",
        status.effective_filter
    );
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
    telemetry.apply(&LogConfig {
        level: "info".into(),
        dev_mode_until: now_unix() + 60,
        ..LogConfig::default()
    });
    telemetry.apply(&LogConfig {
        level: "info".into(),
        dev_mode_until: now_unix() + 3600,
        ..LogConfig::default()
    });
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert!(
        telemetry.status().dev_mode,
        "the first timer must not end the second window"
    );
}

#[test]
fn a_development_time_already_past_is_off() {
    let (telemetry, _capture, _guard) = subscriber(Format::Json, "info");
    telemetry.apply(&LogConfig {
        level: "info".into(),
        dev_mode_until: 1,
        ..LogConfig::default()
    });
    assert!(!telemetry.status().dev_mode);
    assert_eq!(telemetry.status().effective_filter, "info");
}

#[test]
fn development_mode_keeps_the_targets_the_level_names() {
    assert_eq!(
        dev_filter("warn,scanner::loops=trace"),
        format!("debug,{DEV_MODE_QUIET},scanner::loops=trace")
    );
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
            Ok(Logging(LogConfig {
                level: snapshot.get(&LEVEL),
                dev_mode_until: snapshot.get(&DEV_MODE_UNTIL),
                ..LogConfig::default()
            }))
        }
    }

    /// The one test in this binary that installs the process-wide
    /// subscriber; the others use a thread-local one, which wins on their
    /// own threads.
    #[tokio::test]
    async fn a_saved_level_reaches_the_process_wide_subscriber() {
        let store = Arc::new(live_settings::MemoryStore::default());
        let mut builder = live_settings::Registry::builder_with_env(
            store,
            ALL,
            live_settings::Env::fixed::<&str, &str>([]),
        )
        .await;
        builder.reloadable(LogReloadable::<Logging>::default());
        let registry = builder.build().unwrap();
        let telemetry = init("telemetry-test", "TELEMETRY_TEST");
        registry.boot().await.unwrap();
        assert_eq!(telemetry.status().effective_filter, "info");

        registry
            .save(vec![(
                "logging.level".to_string(),
                Some("debug,hyper=warn".to_string()),
            )])
            .await
            .unwrap();
        assert_eq!(telemetry.status().config.level, "debug,hyper=warn");
        assert_eq!(telemetry.status().effective_filter, "debug,hyper=warn");
    }
}

#[test]
fn lines_inside_a_span_carry_its_trace_and_span_ids_even_when_the_span_is_not_logged() {
    let (_telemetry, capture, _guard) = subscriber(Format::Json, "warn");
    let request = tracing::info_span!("HTTP request");
    let (trace_id, span_id) = {
        let context = trace::span_context(&request).expect("an info span has ids at level warn");
        (
            context.trace_id().to_string(),
            context.span_id().to_string(),
        )
    };
    let child = request.in_scope(|| tracing::info_span!("engine call"));
    child.in_scope(|| tracing::warn!("slow"));
    request.in_scope(|| tracing::warn!("done"));
    tracing::warn!("outside");

    let lines = capture.json_lines();
    assert_eq!(lines.len(), 3, "{}", capture.text());
    assert_eq!(lines[0]["trace_id"], trace_id.as_str());
    assert_ne!(
        lines[0]["span_id"],
        span_id.as_str(),
        "the child span has its own id"
    );
    assert_eq!(lines[1]["trace_id"], trace_id.as_str());
    assert_eq!(lines[1]["span_id"], span_id.as_str());
    assert!(lines[2].get("trace_id").is_none());
    let text = capture.text();
    assert!(text.find("\"target\"").unwrap() < text.find("\"trace_id\"").unwrap());
    assert!(text.find("\"trace_id\"").unwrap() < text.find("\"message\"").unwrap());
}

#[test]
fn a_span_with_a_remote_parent_joins_the_callers_trace() {
    let (_telemetry, _capture, _guard) = subscriber(Format::Json, "info");
    let incoming = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let request = tracing::info_span!("HTTP request");
    assert!(trace::set_remote_parent(&request, incoming));
    let outgoing = trace::traceparent(&request).unwrap();
    assert!(
        outgoing.starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-"),
        "{outgoing}"
    );
    assert!(
        !outgoing.contains("00f067aa0ba902b7"),
        "our own span id, not the caller's: {outgoing}"
    );
    assert_eq!(
        request.in_scope(trace::current_trace_id).as_deref(),
        Some("4bf92f3577b34da6a3ce929d0e0e4736")
    );
}

#[test]
fn traceparent_values_are_checked() {
    let good = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let parsed = trace::parse_traceparent(good).unwrap();
    assert_eq!(trace::format_traceparent(&parsed), good);
    assert!(trace::parse_traceparent(
        "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00-extra"
    )
    .is_some());
    for bad in [
        "",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
        "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01",
    ] {
        assert!(trace::parse_traceparent(bad).is_none(), "{bad}");
    }
}

mod otlp_export {
    use axum::body::Bytes;
    use axum::routing::post;
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyValue;
    use prost::Message;

    use super::*;

    #[derive(Clone, Default)]
    struct Received {
        logs: Arc<Mutex<Vec<ExportLogsServiceRequest>>>,
        traces: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
        headers: Arc<Mutex<Vec<String>>>,
    }

    fn string(value: &Option<opentelemetry_proto::tonic::common::v1::AnyValue>) -> String {
        match value.as_ref().and_then(|v| v.value.as_ref()) {
            Some(AnyValue::StringValue(s)) => s.clone(),
            other => format!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn lines_and_spans_reach_the_collector_redacted_and_in_their_trace() {
        let received = Received::default();
        let (logs, traces, headers) = (received.clone(), received.clone(), received.clone());
        let app = axum::Router::new()
            .route(
                "/v1/logs",
                post(move |h: axum::http::HeaderMap, body: Bytes| async move {
                    headers.headers.lock().push(
                        h.get("x-team")
                            .map(|v| v.to_str().unwrap().to_string())
                            .unwrap_or_default(),
                    );
                    logs.logs
                        .lock()
                        .push(ExportLogsServiceRequest::decode(body).unwrap());
                }),
            )
            .route(
                "/v1/traces",
                post(move |body: Bytes| async move {
                    traces
                        .traces
                        .lock()
                        .push(ExportTraceServiceRequest::decode(body).unwrap())
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (telemetry, _capture, _guard) = subscriber(Format::Json, "info");
        telemetry.apply(&LogConfig {
            otlp_endpoint: endpoint.clone(),
            otlp_headers: "x-team=ops".into(),
            ..LogConfig::default()
        });
        let request = tracing::info_span!("otlp test request", view_key = "abcdef");
        let trace_id = trace::span_context(&request).unwrap().trace_id();
        request.in_scope(|| {
            tracing::warn!(
                order.id = "o_otlp",
                secret_token = "sk_1234",
                "otlp test line"
            )
        });
        drop(request);

        // `flush` returns once every record offered so far has been posted.
        assert!(
            telemetry.flush(Duration::from_secs(10)).await,
            "the export didn't finish"
        );
        assert!(!received.logs.lock().is_empty() && !received.traces.lock().is_empty());
        let logs = received.logs.lock().clone();
        let resource = &logs[0].resource_logs[0];
        let service = &resource.resource.as_ref().unwrap().attributes[0];
        assert_eq!(
            (service.key.as_str(), string(&service.value)),
            ("service.name", "scanner".to_string())
        );
        let record = resource.scope_logs[0]
            .log_records
            .iter()
            .find(|r| string(&r.body) == "otlp test line")
            .unwrap();
        assert_eq!(record.severity_number, 13);
        assert_eq!(record.trace_id, trace_id.to_bytes().to_vec());
        let attr = |name: &str| {
            string(
                &record
                    .attributes
                    .iter()
                    .find(|a| a.key == name)
                    .unwrap()
                    .value,
            )
        };
        assert_eq!(attr("order.id"), "o_otlp");
        assert_eq!(attr("secret_token"), redact::REDACTED);
        assert_eq!(received.headers.lock()[0], "ops");

        let traces = received.traces.lock().clone();
        let span = &traces[0].resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(span.trace_id, trace_id.to_bytes().to_vec());
        let key = span
            .attributes
            .iter()
            .find(|a| a.key == "view_key")
            .unwrap();
        assert_eq!(
            string(&key.value),
            redact::REDACTED,
            "span fields are redacted too"
        );

        telemetry.apply(&LogConfig::default());
        assert!(
            telemetry.sink.otlp.read().is_none(),
            "an empty endpoint stops it"
        );
    }

    #[test]
    fn settings_are_checked() {
        assert!(otlp::check_endpoint(&"".to_string()).is_ok());
        assert!(otlp::check_endpoint(&"http://127.0.0.1:4318".to_string()).is_ok());
        assert!(otlp::check_endpoint(&"127.0.0.1:4318".to_string()).is_err());
        // Credentials belong in the (secret) headers, never the address.
        assert!(otlp::check_endpoint(&"https://user:key@collector.example".to_string()).is_err());
        assert!(otlp::check_endpoint(&"https://collector.example/v1?x=a@b".to_string()).is_ok());
        let headers = |raw: &str| otlp::check_headers(&live_settings::Secret::new(raw));
        for fine in [
            "",
            "authorization=Bearer x, x-team=ops",
            // A value may hold `=` (base64 padding); an empty pair is skipped.
            "authorization=Basic dXNlcjpwYXNz==,,x_team=ops",
        ] {
            assert_eq!(headers(fine), Ok(()), "{fine:?}");
        }
        // What an operator pastes by mistake is the key itself: it is
        // refused, and the message names the pair, not what was typed.
        for (wrong, pair) in [
            ("sk-live-abc123", "Pair 1 isn't name=value"),
            ("x-team=ops,sk-live-abc123", "Pair 2 isn't name=value"),
            ("=sk-live-abc123", "Pair 1's name isn't a header name"),
            (
                "api key=sk-live-abc123",
                "Pair 1's name isn't a header name",
            ),
        ] {
            let error = headers(wrong).unwrap_err();
            assert!(error.contains(pair), "{wrong:?}: {error}");
            assert!(!error.contains("sk-live-abc123"), "{error}");
        }
        let config = otlp::OtlpConfig::from_settings("http://c:4318/", "a=1, b = 2").unwrap();
        assert_eq!(config.endpoint, "http://c:4318");
        assert_eq!(
            config.headers,
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
    }
}
