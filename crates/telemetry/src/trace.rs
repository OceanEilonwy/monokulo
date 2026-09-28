//! Trace and span ids, and the W3C `traceparent` header that carries them
//! between processes (structured_logging.md 2.3).
//!
//! Ids come from OpenTelemetry: every `tracing` span at `info` or above gets
//! an OpenTelemetry span through `tracing-opentelemetry`, whatever the log
//! level, so a request keeps its trace id even when its lines are filtered
//! out. A span inherits its parent's trace id; a request span takes the
//! caller's through [`set_remote_parent`].

use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// The W3C header name.
pub const TRACEPARENT: &str = "traceparent";

/// Parses a `traceparent` value: `00-<32 hex trace id>-<16 hex span id>-<2
/// hex flags>`. Later versions are read the same way, ignoring anything
/// after the flags, as the spec asks. All-zero ids and version `ff` are
/// refused.
pub fn parse_traceparent(value: &str) -> Option<SpanContext> {
    let mut parts = value.trim().split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let flags = parts.next()?;
    let is_hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !is_hex(version, 2) || version == "ff" || !is_hex(trace_id, 32) || !is_hex(span_id, 16) || !is_hex(flags, 2) {
        return None;
    }
    if version == "00" && parts.next().is_some() {
        return None;
    }
    let trace_id = TraceId::from_hex(trace_id).ok()?;
    let span_id = SpanId::from_hex(span_id).ok()?;
    let flags = TraceFlags::new(u8::from_str_radix(flags, 16).ok()? & TraceFlags::SAMPLED.to_u8());
    let context = SpanContext::new(trace_id, span_id, flags, true, TraceState::default());
    context.is_valid().then_some(context)
}

/// Formats a span context as a `traceparent` value.
pub fn format_traceparent(context: &SpanContext) -> String {
    format!("00-{}-{}-{:02x}", context.trace_id(), context.span_id(), context.trace_flags().to_u8())
}

/// Makes `span` a child of the caller's span named by a `traceparent`
/// header value, so it joins the caller's trace. Call before the span is
/// first entered. Returns false (and leaves the span in a new trace) when
/// the value doesn't parse.
pub fn set_remote_parent(span: &Span, traceparent: &str) -> bool {
    let Some(remote) = parse_traceparent(traceparent) else { return false };
    let context = opentelemetry::Context::new().with_remote_span_context(remote);
    span.set_parent(context).is_ok()
}

/// `span`'s own trace context, when it has one (it doesn't when no
/// subscriber with telemetry is installed, as in most tests, or when the
/// span is disabled).
pub fn span_context(span: &Span) -> Option<SpanContext> {
    let context = span.context();
    let span_context = context.span().span_context().clone();
    span_context.is_valid().then_some(span_context)
}

/// The `traceparent` to send on an outgoing request made inside `span`.
pub fn traceparent(span: &Span) -> Option<String> {
    span_context(span).map(|c| format_traceparent(&c))
}

/// The `traceparent` for an outgoing request made now.
pub fn current_traceparent() -> Option<String> {
    traceparent(&Span::current())
}

/// The trace id of the current span, as 32 hex characters.
pub fn current_trace_id() -> Option<String> {
    span_context(&Span::current()).map(|c| c.trace_id().to_string())
}
