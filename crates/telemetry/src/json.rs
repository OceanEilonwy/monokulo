//! One JSON object per line on stderr, the production format.
//!
//! ```json
//! {"timestamp":"2026-09-28T12:00:00.123456Z","level":"WARN","service":"scanner","target":"scanner::loops",
//!  "trace_id":"4bf92f3577b34da6a3ce929d0e0e4736","span_id":"00f067aa0ba902b7","message":"scan tick failed","attributes":{"network":"Stagenet","error":"timeout"},"spans":["network loop"]}
//! ```
//!
//! `trace_id` and `span_id` (the OpenTelemetry log record's own fields) are
//! there when the line was written inside a span that has them.
//!
//! `attributes` holds the fields of every span the event is in, outermost
//! first, then the event's own; a field set closer to the event wins. Every
//! value goes through [`crate::redact`].

use std::io::Write;
use std::sync::OnceLock;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::dispatcher::WeakDispatch;
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::redact;

pub(crate) struct JsonLayer<W> {
    service: &'static str,
    make_writer: W,
    dispatch: SpanIds,
}

impl<W> JsonLayer<W> {
    pub(crate) fn new(service: &'static str, make_writer: W) -> Self {
        JsonLayer { service, make_writer, dispatch: SpanIds::default() }
    }
}

/// Finds a span's trace and span ids through the subscriber this layer is
/// part of. (`tracing_opentelemetry::get_otel_context` needs the
/// `Dispatch`, and `tracing::dispatcher::get_default` returns none while an
/// event is being dispatched.)
#[derive(Default)]
pub(crate) struct SpanIds(OnceLock<WeakDispatch>);

impl SpanIds {
    pub(crate) fn register(&self, dispatch: &Dispatch) {
        let _ = self.0.set(dispatch.downgrade());
    }

    /// The ids of the innermost span around `event` that has them. The
    /// spans are looked up in the registry itself, not through this
    /// layer's context, because the level filter hides spans from this
    /// layer that still carry ids (an `info` request span at level `warn`).
    pub(crate) fn find(&self, event: &Event<'_>) -> Option<(String, String)> {
        use opentelemetry::trace::TraceContextExt;
        let dispatch = self.0.get()?.upgrade()?;
        let start = match event.parent() {
            Some(parent) => parent.clone(),
            None if event.is_contextual() => dispatch.current_span().id()?.clone(),
            None => return None,
        };
        let registry = dispatch.downcast_ref::<tracing_subscriber::Registry>()?;
        // Collected first: the lookup below borrows each span's extensions.
        let spans: Vec<Id> = registry.span(&start)?.scope().map(|span| span.id()).collect();
        spans.iter().find_map(|id| {
            let context = tracing_opentelemetry::get_otel_context(id, &dispatch)?;
            let span = context.span();
            let ids = span.span_context();
            ids.is_valid().then(|| (ids.trace_id().to_string(), ids.span_id().to_string()))
        })
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
    fn on_register_dispatch(&self, dispatch: &Dispatch) {
        self.dispatch.register(dispatch);
    }

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
        // Before any extensions are borrowed below: the lookup takes them.
        let ids = self.dispatch.find(event);
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
        if let Some((trace_id, span_id)) = ids {
            push("trace_id", &Value::String(trace_id));
            push("span_id", &Value::String(span_id));
        }
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
