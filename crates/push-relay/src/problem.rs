//! The relay's errors, as `application/problem+json` (RFC 9457), each
//! `type` a section of the relay's page in the docs.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The media type of an error body.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// Where each problem is described: `{DOCS}#{slug}`.
const DOCS: &str = "https://docs.monokulo.com/push-relay/";

/// A kind of error the relay answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The body isn't what the route takes: not JSON, a field missing or
    /// unknown, a token that isn't one, a push without `TTL`, or a body
    /// the handle doesn't take.
    InvalidRequest,
    /// The registration names an app this relay doesn't serve.
    UnknownApp,
    /// The app is served, but not on that platform: this relay has no key
    /// for it.
    UnsupportedPlatform,
    /// No such handle: never made, expired, or its token was gone. A
    /// sender drops the subscription.
    UnknownHandle,
    /// The push isn't signed by the key the handle was registered with,
    /// or not for this relay, or its signature has expired.
    Unauthorized,
    /// The push is larger than a push may be, or than APNs or FCM carry
    /// once it is wrapped for them.
    PayloadTooLarge,
    /// Too many registrations from this address, or pushes from this
    /// sender or to this handle: wait `Retry-After` seconds.
    RateLimited,
    /// APNs or FCM is down, busy or limiting this relay: try again after
    /// `Retry-After` seconds.
    UpstreamUnavailable,
    /// APNs or FCM refused the push for a reason that is this relay's
    /// (its keys or settings): its log says which.
    UpstreamRefused,
    /// Something went wrong on the relay: try again in a moment.
    Internal,
}

impl Problem {
    pub const fn slug(self) -> &'static str {
        match self {
            Problem::InvalidRequest => "invalid-request",
            Problem::UnknownApp => "unknown-app",
            Problem::UnsupportedPlatform => "unsupported-platform",
            Problem::UnknownHandle => "unknown-handle",
            Problem::Unauthorized => "unauthorized",
            Problem::PayloadTooLarge => "payload-too-large",
            Problem::RateLimited => "rate-limited",
            Problem::UpstreamUnavailable => "upstream-unavailable",
            Problem::UpstreamRefused => "upstream-refused",
            Problem::Internal => "internal",
        }
    }

    pub const fn status(self) -> StatusCode {
        match self {
            Problem::InvalidRequest | Problem::UnknownApp | Problem::UnsupportedPlatform => {
                StatusCode::BAD_REQUEST
            }
            Problem::UnknownHandle => StatusCode::NOT_FOUND,
            Problem::Unauthorized => StatusCode::UNAUTHORIZED,
            Problem::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Problem::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Problem::UpstreamUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Problem::UpstreamRefused => StatusCode::BAD_GATEWAY,
            Problem::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Problem::InvalidRequest => "Invalid request",
            Problem::UnknownApp => "Unknown app",
            Problem::UnsupportedPlatform => "Platform not served",
            Problem::UnknownHandle => "Unknown handle",
            Problem::Unauthorized => "Not signed by the handle's sender",
            Problem::PayloadTooLarge => "Too large",
            Problem::RateLimited => "Too many requests",
            Problem::UpstreamUnavailable => "APNs or FCM unavailable",
            Problem::UpstreamRefused => "Refused by APNs or FCM",
            Problem::Internal => "Relay error",
        }
    }

    /// This problem, with what went wrong this time.
    pub fn detail(self, detail: impl Into<String>) -> ProblemDetails {
        ProblemDetails {
            problem: self,
            detail: detail.into(),
            retry_after: None,
        }
    }
}

/// An error response.
#[derive(Debug)]
pub struct ProblemDetails {
    problem: Problem,
    detail: String,
    retry_after: Option<u64>,
}

impl ProblemDetails {
    /// Seconds to wait, as `Retry-After` and the body's `retry_after`.
    pub fn retry_after(mut self, seconds: u64) -> Self {
        self.retry_after = Some(seconds);
        self
    }
}

#[derive(Serialize)]
struct Body<'a> {
    #[serde(rename = "type")]
    type_: String,
    title: &'static str,
    status: u16,
    detail: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after: Option<u64>,
}

impl IntoResponse for ProblemDetails {
    fn into_response(self) -> Response {
        let problem = self.problem;
        let body = Body {
            type_: format!("{DOCS}#{}", problem.slug()),
            title: problem.title(),
            status: problem.status().as_u16(),
            detail: &self.detail,
            retry_after: self.retry_after,
        };
        let mut response = (
            problem.status(),
            [(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON))],
            serde_json::to_string(&body).unwrap_or_default(),
        )
            .into_response();
        if let Some(seconds) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        if problem == Problem::Unauthorized {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("vapid"));
        }
        response
    }
}
