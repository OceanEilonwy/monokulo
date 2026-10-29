//! The Bell's errors, as `application/problem+json` (RFC 9457), each
//! `type` a section of the Bell's page in this repository's docs.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::pos::PointOfSale;

/// The media type of an error body.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// Where each problem is described: `{DOCS}#{slug}`.
const DOCS: &str = "https://github.com/OceanEilonwy/monokular/blob/main/docs/bell.md";

/// A kind of error the Bell answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The body isn't what the route takes: not JSON, a field missing or
    /// unknown, an endpoint that isn't https or is private, a key that
    /// isn't one.
    InvalidRequest,
    /// The point of sale refused the access token, or the location isn't
    /// its merchant's: `square-refused`, `clover-refused` (decision 46).
    Refused(PointOfSale),
    /// The point of sale didn't answer, or couldn't: try again later.
    /// `square-unavailable`, `clover-unavailable`.
    Unavailable(PointOfSale),
    /// No such registration: never made, unregistered, or deleted after
    /// its push service said the subscription was gone.
    UnknownRegistration,
    /// The location has as many registrations as it may.
    TooManyRegistrations,
    /// The endpoint is registered for another merchant, and the keys given
    /// aren't its own.
    EndpointRegistered,
    /// A webhook its point of sale didn't sign, or authenticate, for this
    /// Bell.
    InvalidSignature(PointOfSale),
    /// The body is more than the route reads.
    PayloadTooLarge,
    /// Too many registrations from this address: wait `Retry-After`
    /// seconds.
    RateLimited,
    /// Something went wrong on the Bell: try again in a moment.
    Internal,
}

impl Problem {
    pub const fn slug(self) -> &'static str {
        match self {
            Problem::InvalidRequest => "invalid-request",
            Problem::Refused(PointOfSale::Square) => "square-refused",
            Problem::Refused(PointOfSale::Clover) => "clover-refused",
            Problem::Unavailable(PointOfSale::Square) => "square-unavailable",
            Problem::Unavailable(PointOfSale::Clover) => "clover-unavailable",
            Problem::UnknownRegistration => "unknown-registration",
            Problem::TooManyRegistrations => "too-many-registrations",
            Problem::EndpointRegistered => "endpoint-registered",
            Problem::InvalidSignature(_) => "invalid-signature",
            Problem::PayloadTooLarge => "payload-too-large",
            Problem::RateLimited => "rate-limited",
            Problem::Internal => "internal",
        }
    }

    pub const fn status(self) -> StatusCode {
        match self {
            Problem::InvalidRequest => StatusCode::BAD_REQUEST,
            Problem::Refused(_) | Problem::InvalidSignature(_) => StatusCode::UNAUTHORIZED,
            Problem::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Problem::UnknownRegistration => StatusCode::NOT_FOUND,
            Problem::TooManyRegistrations | Problem::EndpointRegistered => StatusCode::CONFLICT,
            Problem::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Problem::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Problem::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Problem::InvalidRequest => "Invalid request",
            Problem::Refused(PointOfSale::Square) => "Refused by Square",
            Problem::Refused(PointOfSale::Clover) => "Refused by Clover",
            Problem::Unavailable(PointOfSale::Square) => "Square unavailable",
            Problem::Unavailable(PointOfSale::Clover) => "Clover unavailable",
            Problem::UnknownRegistration => "Unknown registration",
            Problem::TooManyRegistrations => "Too many registrations",
            Problem::EndpointRegistered => "Registered elsewhere",
            Problem::InvalidSignature(PointOfSale::Square) => "Not signed by Square",
            Problem::InvalidSignature(PointOfSale::Clover) => "Not from Clover",
            Problem::PayloadTooLarge => "Too large",
            Problem::RateLimited => "Too many requests",
            Problem::Internal => "Bell error",
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
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Square's slugs are as they were; each point of sale has its own, so
    /// a till matches `{pos}-refused` for its own.
    #[test]
    fn the_point_of_sale_problems_are_named_for_it() {
        for pos in PointOfSale::ALL {
            assert_eq!(Problem::Refused(pos).slug(), format!("{pos}-refused"));
            assert_eq!(
                Problem::Unavailable(pos).slug(),
                format!("{pos}-unavailable")
            );
            assert_eq!(Problem::InvalidSignature(pos).slug(), "invalid-signature");
            assert!(Problem::Refused(pos).title().contains(pos.title()));
            assert!(Problem::Unavailable(pos).title().contains(pos.title()));
        }
    }
}
