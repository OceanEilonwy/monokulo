//! `POST /dashboard/stores/{id}/pos/logs`: the POS's session timeline
//! (`pos-ui/src/timeline.ts`), sent in batches while the terminal runs.
//!
//! A POS runs for hours on a tablet that loses coverage, goes to the
//! background and gets reloaded by its browser. When something goes wrong,
//! what helps is the whole story of that session in order: when it went
//! offline and for how long, when it was hidden, when its live stream
//! dropped, which orders were created, backgrounded, brought back and
//! finished. Each event becomes one line with `source = "pos"`,
//! `store.id`, `pos.session`, `pos.seq` (its order in the session) and
//! `pos.client_ts` (the tablet's clock, in milliseconds): events recorded
//! offline arrive late, so the timeline is ordered by `pos.seq`, not by
//! when the lines were written (`http::logs_page::pos_timeline`).
//!
//! Only the store's owner can send (a signed-in merchant; the route is
//! ownership-checked like every other POS route), only while the store has
//! opted in to client logs (`403` otherwise, so the POS stops recording),
//! and at most `abuse.client_logs_per_min` batches a minute (`http::abuse`).
//! Texts are clipped; nothing the POS sends can choose its own field names.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::orders::load_owned_connection;
use super::{AppState, AuthedUser};

/// Most events one batch may carry.
pub const MAX_EVENTS: usize = 100;
/// The route's body limit.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_DETAIL: usize = 1000;

#[derive(Deserialize)]
struct PosLogs {
    session: String,
    events: Vec<PosEvent>,
}

#[derive(Deserialize)]
struct PosEvent {
    /// The event's place in the session, from 1.
    seq: u64,
    /// The tablet's clock when it happened, Unix milliseconds.
    t: i64,
    #[serde(default)]
    level: Option<String>,
    kind: String,
    #[serde(default)]
    order_id: Option<String>,
    /// Scalars only; anything else is left out.
    #[serde(default)]
    detail: Map<String, Value>,
}

/// A session id: what `timeline.ts` makes (`crypto.randomUUID()`).
pub fn is_session_id(text: &str) -> bool {
    (8..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Event kinds are dotted lowercase names (`order.created`).
fn kind(text: &str) -> String {
    let ok = !text.is_empty()
        && text.len() <= 40
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_');
    if ok {
        text.to_string()
    } else {
        "unknown".to_string()
    }
}

fn order_id(text: &str) -> Option<String> {
    let ok = !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then(|| text.to_string())
}

/// `key=value key=value`, keys in alphabetical order, clipped to `MAX_DETAIL`
/// characters. Keys are reduced to `[a-z0-9_]`, values have their control
/// characters replaced, and a value with a space is quoted.
pub fn detail_text(detail: &Map<String, Value>) -> String {
    let mut out = String::new();
    for (key, value) in detail {
        let key: String = key
            .chars()
            .take(32)
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let value = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        if key.is_empty() {
            continue;
        }
        let value: String = value
            .chars()
            .take(200)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let value = if value.contains(' ') || value.is_empty() {
            format!("{value:?}")
        } else {
            value
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&key);
        out.push('=');
        out.push_str(&value);
    }
    out.chars().take(MAX_DETAIL).collect()
}

pub async fn receive(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    body: Bytes,
) -> StatusCode {
    let row = match load_owned_connection(&state, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    let id = row.id.clone();
    if !state
        .db
        .read(move |db| db.client_logging(&id))
        .await
        .unwrap_or(false)
    {
        return StatusCode::FORBIDDEN;
    }
    // Parsed from the bytes: a beacon sent as the page hides may carry any
    // content type.
    let Ok(logs) = serde_json::from_slice::<PosLogs>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    if !is_session_id(&logs.session) {
        return StatusCode::BAD_REQUEST;
    }
    if logs.events.len() > MAX_EVENTS {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    let span = tracing::info_span!(parent: None, "pos report", source = "pos", store.id = %row.id, pos.session = %logs.session);
    let _entered = span.enter();
    for event in logs.events {
        let kind = kind(&event.kind);
        let order = event.order_id.as_deref().and_then(order_id);
        let detail = detail_text(&event.detail);
        let message = if detail.is_empty() {
            kind.clone()
        } else {
            format!("{kind} {detail}")
        };
        let detail = Some(detail).filter(|d| !d.is_empty());
        macro_rules! line {
            ($level:ident) => {
                tracing::$level!(
                    pos.seq = event.seq,
                    pos.client_ts = event.t,
                    pos.kind = %kind,
                    pos.detail = detail.as_deref(),
                    order.id = order.as_deref(),
                    "{message}"
                )
            };
        }
        match event.level.as_deref() {
            Some("error") => line!(error),
            Some("warn") => line!(warn),
            _ => line!(info),
        }
    }
    StatusCode::NO_CONTENT
}

/// [`detail_text`] read back into its pairs.
pub fn detail_pairs(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut rest = text.trim_start();
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].to_string();
        rest = &rest[eq + 1..];
        let value = if let Some(quoted) = rest.strip_prefix('"') {
            // A Rust debug string: ends at the first unescaped quote.
            let mut end = None;
            let mut escaped = false;
            for (i, c) in quoted.char_indices() {
                match c {
                    '\\' if !escaped => escaped = true,
                    '"' if !escaped => {
                        end = Some(i);
                        break;
                    }
                    _ => escaped = false,
                }
            }
            let end = end.unwrap_or(quoted.len());
            let value = quoted[..end].replace("\\\"", "\"").replace("\\\\", "\\");
            rest = quoted.get(end + 1..).unwrap_or("");
            value
        } else {
            let end = rest.find(' ').unwrap_or(rest.len());
            let value = rest[..end].to_string();
            rest = &rest[end..];
            value
        };
        pairs.push((key, value));
        rest = rest.trim_start();
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_kinds_and_order_ids_are_checked() {
        assert!(is_session_id("0f8e2c1a-3b4d-4e5f-9a6b-7c8d9e0f1a2b"));
        assert!(!is_session_id("short"));
        assert!(!is_session_id("has spaces in it here"));
        assert!(!is_session_id(&"a".repeat(65)));
        assert_eq!(kind("order.created"), "order.created");
        assert_eq!(kind("Order Created\n"), "unknown");
        assert_eq!(
            order_id("order_8f42a91c").as_deref(),
            Some("order_8f42a91c")
        );
        assert_eq!(order_id("order'; drop"), None);
    }

    #[test]
    fn details_are_flat_clipped_and_keep_no_control_characters() {
        let detail: Map<String, Value> = serde_json::from_str(
            r#"{"offline_ms": 42000, "persisted": true, "route": "/pos/orders", "error": "Failed to\nfetch", "nested": {"x": 1}, "Bad Key!": "v"}"#,
        )
        .unwrap();
        assert_eq!(
            detail_text(&detail),
            r#"bad_key_=v error="Failed to fetch" offline_ms=42000 persisted=true route=/pos/orders"#
        );
        assert_eq!(
            detail_pairs(&detail_text(&detail)),
            [
                ("bad_key_", "v"),
                ("error", "Failed to fetch"),
                ("offline_ms", "42000"),
                ("persisted", "true"),
                ("route", "/pos/orders")
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        let quoted: Map<String, Value> =
            serde_json::from_str(r#"{"message": "say \"hi\" now"}"#).unwrap();
        assert_eq!(
            detail_pairs(&detail_text(&quoted)),
            [("message".to_string(), "say \"hi\" now".to_string())]
        );
        let long: Map<String, Value> = (0..20)
            .map(|n| (format!("k{n}"), Value::String("x".repeat(200))))
            .collect();
        assert_eq!(detail_text(&long).chars().count(), MAX_DETAIL);
    }
}
