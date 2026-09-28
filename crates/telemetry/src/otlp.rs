//! Sending lines and spans to an OpenTelemetry collector (structured_logging.md
//! part 7), for operators who run their own stack (a Collector, Grafana,
//! Seq, the Aspire Dashboard).
//!
//! It exports what the log store keeps, the same rows, already redacted:
//! nothing leaves the process that the Logs page couldn't show. They go as
//! OTLP/HTTP protobuf (`POST {endpoint}/v1/logs` and `/v1/traces`), the
//! encoding every OTLP receiver accepts, in batches from one background
//! task. Like the store, sending never blocks the code that logs: when the
//! queue is full, records are dropped.
//!
//! The endpoint and headers are live settings (`logging.otlp_endpoint`,
//! `logging.otlp_headers`); changing them replaces the task.

use std::time::Duration;

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{span, status, ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;
use serde_json::{Map, Value};

use crate::store::{LogRow, Record, SpanRow};

/// Records waiting to be sent; more are dropped.
const QUEUE: usize = 10_000;
/// Most records in one request.
const BATCH: usize = 512;
const FLUSH_EVERY: Duration = Duration::from_secs(2);

/// Where to send, as the settings give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpConfig {
    /// The collector's base URL, such as `http://127.0.0.1:4318`.
    pub endpoint: String,
    /// Extra request headers (an API key), as `name=value` pairs.
    pub headers: Vec<(String, String)>,
}

impl OtlpConfig {
    /// From the settings: `None` when the endpoint is empty. Headers are
    /// `name=value` pairs separated by commas, as in
    /// `OTEL_EXPORTER_OTLP_HEADERS`.
    pub fn from_settings(endpoint: &str, headers: &str) -> Option<OtlpConfig> {
        let endpoint = endpoint.trim().trim_end_matches('/');
        if endpoint.is_empty() {
            return None;
        }
        let headers = headers
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .filter(|(name, _)| !name.is_empty())
            .collect();
        Some(OtlpConfig { endpoint: endpoint.to_string(), headers })
    }
}

/// The `check` for `logging.otlp_headers`.
#[allow(clippy::ptr_arg, reason = "a setting's `check` takes `&T`, and this setting is a `String`")]
pub fn check_headers(headers: &String) -> Result<(), String> {
    for pair in headers.split(',').filter(|p| !p.trim().is_empty()) {
        let Some((name, _)) = pair.split_once('=') else {
            return Err(format!("\"{}\" isn't name=value. Separate pairs with commas: authorization=Bearer abc,x-team=ops", pair.trim()));
        };
        let name = name.trim();
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(format!("\"{name}\" isn't a header name."));
        }
    }
    Ok(())
}

/// The `check` for `logging.otlp_endpoint`: empty (off) or an http(s) URL.
#[allow(clippy::ptr_arg, reason = "a setting's `check` takes `&T`, and this setting is a `String`")]
pub fn check_endpoint(endpoint: &String) -> Result<(), String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() || endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        Ok(())
    } else {
        Err("Enter the collector's http:// or https:// address, such as http://127.0.0.1:4318, or leave it empty.".into())
    }
}

fn text(value: impl Into<String>) -> Option<AnyValue> {
    Some(AnyValue { value: Some(any_value::Value::StringValue(value.into())) })
}

fn any(value: &Value) -> Option<AnyValue> {
    let value = match value {
        Value::String(s) => any_value::Value::StringValue(s.clone()),
        Value::Bool(b) => any_value::Value::BoolValue(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => any_value::Value::IntValue(i),
            None => any_value::Value::DoubleValue(n.as_f64().unwrap_or_default()),
        },
        Value::Null => return None,
        other => any_value::Value::StringValue(other.to_string()),
    };
    Some(AnyValue { value: Some(value) })
}

fn kv(key: String, value: Option<AnyValue>) -> KeyValue {
    KeyValue { key, value, ..Default::default() }
}

fn attributes(map: &Map<String, Value>) -> Vec<KeyValue> {
    map.iter().map(|(key, value)| kv(key.clone(), any(value))).collect()
}

fn hex_bytes(hex: Option<&str>) -> Vec<u8> {
    let Some(hex) = hex else { return Vec::new() };
    (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
}

fn resource(service: &str) -> Option<Resource> {
    Some(Resource { attributes: vec![kv("service.name".into(), text(service))], ..Default::default() })
}

fn scope() -> Option<InstrumentationScope> {
    Some(InstrumentationScope { name: "mokulo".into(), ..Default::default() })
}

fn nanos(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}

pub(crate) fn log_record(row: &LogRow) -> LogRecord {
    let mut attrs = attributes(&row.attributes);
    attrs.push(kv("code.namespace".into(), text(row.target.clone())));
    LogRecord {
        time_unix_nano: nanos(row.ts),
        observed_time_unix_nano: nanos(row.ts),
        severity_number: i32::try_from(row.level).unwrap_or(0),
        severity_text: row.severity().upper().to_string(),
        body: text(row.message.clone()),
        attributes: attrs,
        trace_id: hex_bytes(row.trace_id.as_deref()),
        span_id: hex_bytes(row.span_id.as_deref()),
        ..Default::default()
    }
}

pub(crate) fn span(row: &SpanRow) -> Span {
    let kind = match row.kind.as_str() {
        "server" => span::SpanKind::Server,
        "client" => span::SpanKind::Client,
        "producer" => span::SpanKind::Producer,
        "consumer" => span::SpanKind::Consumer,
        _ => span::SpanKind::Internal,
    };
    let code = match row.status.as_str() {
        "ok" => status::StatusCode::Ok,
        "error" => status::StatusCode::Error,
        _ => status::StatusCode::Unset,
    };
    Span {
        trace_id: hex_bytes(Some(&row.trace_id)),
        span_id: hex_bytes(Some(&row.span_id)),
        parent_span_id: hex_bytes(row.parent_span_id.as_deref()),
        name: row.attributes.get("otel.name").and_then(Value::as_str).unwrap_or(&row.name).to_string(),
        kind: kind as i32,
        start_time_unix_nano: nanos(row.start),
        end_time_unix_nano: nanos(row.end),
        attributes: attributes(&row.attributes),
        status: Some(Status { code: code as i32, ..Default::default() }),
        ..Default::default()
    }
}

/// The two requests for one batch, grouped by service.
pub(crate) fn requests(batch: &[Record]) -> (Option<ExportLogsServiceRequest>, Option<ExportTraceServiceRequest>) {
    let mut logs: Vec<(String, Vec<LogRecord>)> = Vec::new();
    let mut spans: Vec<(String, Vec<Span>)> = Vec::new();
    for record in batch {
        match record {
            Record::Log(row) => match logs.iter_mut().find(|(s, _)| *s == row.service) {
                Some((_, list)) => list.push(log_record(row)),
                None => logs.push((row.service.clone(), vec![log_record(row)])),
            },
            Record::Span(row) => match spans.iter_mut().find(|(s, _)| *s == row.service) {
                Some((_, list)) => list.push(span(row)),
                None => spans.push((row.service.clone(), vec![span(row)])),
            },
        }
    }
    let logs = (!logs.is_empty()).then(|| ExportLogsServiceRequest {
        resource_logs: logs
            .into_iter()
            .map(|(service, log_records)| ResourceLogs {
                resource: resource(&service),
                scope_logs: vec![ScopeLogs { scope: scope(), log_records, ..Default::default() }],
                ..Default::default()
            })
            .collect(),
    });
    let spans = (!spans.is_empty()).then(|| ExportTraceServiceRequest {
        resource_spans: spans
            .into_iter()
            .map(|(service, spans)| ResourceSpans {
                resource: resource(&service),
                scope_spans: vec![ScopeSpans { scope: scope(), spans, ..Default::default() }],
                ..Default::default()
            })
            .collect(),
    });
    (logs, spans)
}

/// A running exporter: records sent to it go out in batches until it's
/// dropped.
pub(crate) struct Exporter {
    sender: tokio::sync::mpsc::Sender<Record>,
}

impl Exporter {
    /// Starts sending to `config` on the current tokio runtime; `None`
    /// without one.
    pub(crate) fn start(config: OtlpConfig) -> Option<Exporter> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let (sender, receiver) = tokio::sync::mpsc::channel(QUEUE);
        runtime.spawn(run(config, receiver));
        Some(Exporter { sender })
    }

    /// Queues a record; false when the queue is full.
    pub(crate) fn offer(&self, record: Record) -> bool {
        self.sender.try_send(record).is_ok()
    }
}

async fn run(config: OtlpConfig, mut receiver: tokio::sync::mpsc::Receiver<Record>) {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(10)).build() {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(error = %e, "OTLP export not started");
            return;
        }
    };
    let mut batch = Vec::with_capacity(BATCH);
    let mut last_failure: Option<std::time::Instant> = None;
    loop {
        let open = match tokio::time::timeout(FLUSH_EVERY, receiver.recv_many(&mut batch, BATCH)).await {
            Ok(0) => false,
            Ok(_) | Err(_) => true,
        };
        if !batch.is_empty() {
            let (logs, spans) = requests(&batch);
            batch.clear();
            let mut result = Ok(());
            if let Some(logs) = logs {
                result = result.and(post(&client, &config, "/v1/logs", logs.encode_to_vec()).await);
            }
            if let Some(spans) = spans {
                result = result.and(post(&client, &config, "/v1/traces", spans.encode_to_vec()).await);
            }
            // At most one line a minute about it: the line itself is exported too.
            if let Err(e) = result {
                if last_failure.is_none_or(|at| at.elapsed() >= Duration::from_secs(60)) {
                    last_failure = Some(std::time::Instant::now());
                    tracing::warn!(error = %e, endpoint = %config.endpoint, "OTLP export failed; records were dropped");
                }
            }
        }
        if !open {
            return;
        }
    }
}

async fn post(client: &reqwest::Client, config: &OtlpConfig, path: &str, body: Vec<u8>) -> Result<(), String> {
    let mut request = client.post(format!("{}{path}", config.endpoint)).header("content-type", "application/x-protobuf").body(body);
    for (name, value) in &config.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("the collector answered {}", response.status()))
    }
}
