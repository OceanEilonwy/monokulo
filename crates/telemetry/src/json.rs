//! One JSON object per line on stderr, the production format.
//!
//! ```json
//! {"timestamp":"2026-09-28T12:00:00.123456Z","level":"WARN","service":"scanner","target":"scanner::loops",
//!  "message":"scan tick failed","attributes":{"network":"Stagenet","error":"timeout"},"spans":["network loop"]}
//! ```
//!
//! `attributes` holds the fields of every span the event is in, outermost
//! first, then the event's own; a field set closer to the event wins. Every
//! value goes through [`crate::redact`].

use std::io::Write;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::redact;

pub(crate) struct JsonLayer<W> {
    service: &'static str,
    make_writer: W,
}

impl<W> JsonLayer<W> {
    pub(crate) fn new(service: &'static str, make_writer: W) -> Self {
        JsonLayer { service, make_writer }
    }
}

/// A span's fields, redacted, kept in the span's extensions.
pub(crate) struct SpanFields(pub(crate) Map<String, Value>);

/// Collects fields as JSON values, redacted. The event's message goes to
/// `message` rather than into the map.
#[derive(Default)]
pub(crate) struct JsonVisitor {
    pub(crate) fields: Map<String, Value>,
    pub(crate) message: Option<String>,
}

impl JsonVisitor {
    fn put(&mut self, field: &Field, value: Value) {
        if redact::is_secret_name(field.name()) {
            self.fields.insert(field.name().to_string(), Value::String(redact::REDACTED.to_string()));
        } else {
            self.fields.insert(field.name().to_string(), value);
        }
    }

    fn put_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(redact::text(value).into_owned());
        } else {
            let value = redact::field(field.name(), value).into_owned();
            self.fields.insert(field.name().to_string(), Value::String(value));
        }
    }
}

impl Visit for JsonVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put_str(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put_str(field, &format!("{value:?}"));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put_str(field, &value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number));
    }
}

impl<S, W> Layer<S> for JsonLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = JsonVisitor::default();
        attrs.record(&mut visitor);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanFields(visitor.fields));
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = JsonVisitor::default();
        values.record(&mut visitor);
        let mut extensions = span.extensions_mut();
        match extensions.get_mut::<SpanFields>() {
            Some(fields) => fields.0.extend(visitor.fields),
            None => extensions.insert(SpanFields(visitor.fields)),
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut attributes = Map::new();
        let mut spans = Vec::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                spans.push(Value::String(span.name().to_string()));
                if let Some(fields) = span.extensions().get::<SpanFields>() {
                    attributes.extend(fields.0.clone());
                }
            }
        }
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);
        attributes.extend(visitor.fields);

        let metadata = event.metadata();
        // Written field by field, so every line has the same readable key
        // order (serde_json's map would sort them).
        let mut line = String::with_capacity(256);
        let mut push = |key: &str, value: &Value| {
            line.push(if line.is_empty() { '{' } else { ',' });
            line.push_str(&Value::String(key.to_string()).to_string());
            line.push(':');
            line.push_str(&value.to_string());
        };
        push("timestamp", &Value::String(crate::now_rfc3339()));
        push("level", &Value::String(metadata.level().to_string()));
        push("service", &Value::String(self.service.to_string()));
        push("target", &Value::String(metadata.target().to_string()));
        push("message", &Value::String(visitor.message.unwrap_or_default()));
        push("attributes", &Value::Object(attributes));
        if !spans.is_empty() {
            push("spans", &Value::Array(spans));
        }
        line.push_str("}\n");
        let bytes = line.into_bytes();
        // One write per line, so lines from different threads don't
        // interleave. A failed write to stderr has nowhere to be reported.
        let _ = self.make_writer.make_writer_for(metadata).write_all(&bytes);
    }
}
