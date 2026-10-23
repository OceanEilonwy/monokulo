//! Examples from real exchanges. Under test every JSON answer the API gives
//! is checked against the OpenAPI document (`check_layer`). The recording
//! (`super::recording`, in a test build or with the `test-support`
//! feature) makes one exchange of each kind against a real engine, checks
//! each the same way and keeps them, with their ids and times made stable:
//! the published OpenAPI document's and the docs' examples, recorded afresh
//! each time they're generated. So an example is always something the API
//! really said.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One request and its answer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Exchange {
    pub method: String,
    /// The route's template.
    pub route: String,
    /// The path asked for.
    pub path: String,
    /// The key it carried: `pk`, `sk`, `ak`, or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
    pub status: u16,
    /// The answer's JSON; for a stream, the events read from it, each
    /// `{"event", "data"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}

/// Checks each JSON answer against the OpenAPI document
/// (`super::openapi::check_response`). Around the API's routes under test.
#[cfg(test)]
pub async fn check_layer(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    super::recording::exchange(request, next).await.0
}
