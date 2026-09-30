//! The engine's log API, `GET /api/v1/admin/logs...` (structured_logging.md
//! 3.3): request and response shapes both ends share. Monokulo sends the
//! query text and the engine parses it itself, so nothing but text and
//! numbers crosses the wire.

use serde::{Deserialize, Serialize};

use super::{Cursor, LogQuery, LogRow};
use crate::query::{parse, ParseError};

/// Most lines one request returns.
pub const MAX_LIMIT: u32 = 500;

/// `GET /api/v1/admin/logs` query parameters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogsRequest {
    /// Filter language text (`crate::query`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// Unix nanoseconds, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<i64>,
    /// Unix nanoseconds, exclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<i64>,
    /// An encoded [`Cursor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl LogsRequest {
    /// The store query, or why the filter text doesn't parse. A cursor that
    /// doesn't parse is ignored (the first page).
    pub fn to_query(&self) -> Result<LogQuery, ParseError> {
        Ok(LogQuery {
            filter: parse(self.q.as_deref().unwrap_or(""))?,
            from: self.from,
            to: self.to,
            before: self.before.as_deref().and_then(Cursor::parse),
            after: self.after.as_deref().and_then(Cursor::parse),
            limit: self.limit.unwrap_or(100).clamp(1, MAX_LIMIT),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogsResponse {
    pub rows: Vec<LogRow>,
}

/// `GET /api/v1/admin/logs/histogram` query parameters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistogramRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    pub from: i64,
    pub to: i64,
    pub buckets: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistogramResponse {
    pub counts: Vec<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttributesResponse {
    pub names: Vec<String>,
}

/// A 400 answer: the filter text doesn't parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryErrorResponse {
    pub error: String,
    pub start: usize,
    pub end: usize,
}

impl From<ParseError> for QueryErrorResponse {
    fn from(e: ParseError) -> Self {
        QueryErrorResponse {
            error: e.message,
            start: e.start,
            end: e.end,
        }
    }
}

impl From<QueryErrorResponse> for ParseError {
    fn from(e: QueryErrorResponse) -> Self {
        ParseError {
            message: e.error,
            start: e.start,
            end: e.end,
        }
    }
}

/// A trace id as the API accepts it: 32 lowercase hex characters.
pub fn is_trace_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
