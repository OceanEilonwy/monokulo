//! Examples from real exchanges. Under test every JSON answer the API and
//! the handshakes give, the middleware's problems too, is checked against
//! the OpenAPI document (`check_layer`): a problem it doesn't list for the
//! route fails the test. The recording
//! (`super::recording`, in a test build or with the `test-support`
//! feature) makes one exchange of each kind against a real engine, checks
//! each the same way and keeps them, with their ids and times made stable:
//! the published OpenAPI document's and the docs' examples, recorded afresh
//! each time they're generated. So an example is always something the API
//! really said.

/// One request and its answer, as the reference keeps it
/// (`api_reference::Exchange`, which the docs build reads).
pub use api_reference::Exchange;

/// Checks each JSON answer against the OpenAPI document
/// (`super::openapi::check_response`). Around the API's routes under test.
#[cfg(test)]
pub async fn check_layer(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    super::recording::exchange(request, next).await.0
}
