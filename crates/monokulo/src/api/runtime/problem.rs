//! The errors every machine client gets, as `application/problem+json`
//! (RFC 9457): the API's, the plugin and app handshakes', and the JSON the
//! site's own scripts read (the checkout's refund address, the point of
//! sale, client logs). Only pages for people answer with HTML. Each kind
//! is a [`Problem`] variant, whose doc comment is its page in the docs and
//! whose `type` is that page (`https://docs.monokulo.com/problems/{slug}`),
//! so a response can't name a type that has no page. A response is a
//! [`ProblemDetails`]: the problem, what went wrong this time (`detail`), and
//! any extension members.

use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::Serialize;

use super::DOCS;
use crate::http::ApiError;

/// The media type of an error body.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// A kind of error the API answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, api_macros::Problem)]
pub enum Problem {
    /// The request carried no key, or one this Monokulo doesn't know: a
    /// mistyped key, a secret key that has since been replaced (the store's
    /// plugin was disconnected and connected again), or an app that was
    /// disconnected. Send `Authorization: Bearer <key>` with a current key.
    #[problem(status = 401, title = "No valid key")]
    Unauthorized,
    /// The key is valid but may not do this: each route says which keys it
    /// takes. A store's public key (`pk_`) only makes and reads orders;
    /// only an app's own key (`ak_`) reads or disconnects the app; only the
    /// store's secret key (`sk_`) sends a plugin's logs. A store that takes
    /// orders only from its verified websites also refuses an order made
    /// with its public key anywhere else.
    #[problem(status = 403, title = "Not allowed with this key")]
    Forbidden,
    /// There's no such thing here for this caller: an order the key's
    /// store doesn't have (an order of another store is not found either),
    /// a store or order that isn't the signed-in merchant's, a key bundle
    /// that has expired, or a plugin connect URL whose platform isn't a
    /// plugin's slug (a lowercase letter, then 1 to 31 lowercase letters,
    /// digits or hyphens).
    #[problem(status = 404, title = "Not found")]
    NotFound,
    /// The request isn't what the route takes: not JSON, a field missing or
    /// of the wrong type, or a value its rules refuse (`detail` says
    /// which). In the API, the form in `_templates` lists every field and
    /// its rules.
    #[problem(status = 400, title = "Invalid request")]
    InvalidRequest,
    /// The body was sent without `Content-Type: application/json`.
    #[problem(status = 415, title = "Send JSON")]
    UnsupportedMediaType,
    /// The order can't be priced: the currency is unknown, this Monokulo
    /// has no exchange rate for it right now, or the amount isn't one the
    /// currency can have.
    #[problem(status = 400, title = "The order can't be priced")]
    InvalidAmount,
    /// The `idempotency_key` was used before for a different order: a
    /// different amount, currency or reference.
    #[problem(status = 409, title = "Key used for another order")]
    IdempotencyConflict,
    /// The request body is larger than the route takes.
    #[problem(status = 413, title = "Request too large")]
    TooLarge,
    /// The store hasn't turned on client logs, so its plugin's reports are
    /// refused. The merchant turns them on in the store's settings.
    #[problem(status = 403, title = "Client logs are off for this store")]
    LogsOff,
    /// Too many requests from this client. Wait `retry_after` seconds (also
    /// in the `Retry-After` header) before trying again.
    #[problem(status = 429, title = "Too many requests")]
    RateLimited,
    /// Too many requests from this connection: solve the challenge in
    /// `challenge` (find a nonce such that SHA-256 of the challenge
    /// followed by the nonce starts with `difficulty` zero bits) and send
    /// the request again with `Monokulo-Proof: <challenge>.<nonce>`. The
    /// embed library does this by itself.
    #[problem(status = 429, title = "Solve a challenge to continue")]
    ChallengeRequired,
    /// This client has as many live update streams open for the store as
    /// it may. Close one first.
    #[problem(status = 429, title = "Too many open streams")]
    TooManyStreams,
    /// The payment engine is busy or can't be reached. Try again in a
    /// moment.
    #[problem(status = 503, title = "Payment engine unavailable")]
    EngineUnavailable,
    /// The store's settings can't be read right now. Try again in a
    /// moment.
    #[problem(status = 503, title = "Store unavailable")]
    StoreUnavailable,
    /// Something went wrong on this Monokulo. Its admin can find it in the
    /// logs; try again later.
    #[problem(status = 500, title = "Internal error")]
    Internal,
    /// The connect code is unknown, already used, or expired. A plugin's
    /// code (the `token` its return URL was given) lasts 10 minutes and an
    /// app's (the `token` in its connect link) as long as its connect page
    /// says; each works once. Start connecting again for a new one.
    #[problem(status = 401, title = "Connect code not valid")]
    InvalidConnectCode,
    /// This Monokulo's admin hasn't set its web address (`public_uri`) yet,
    /// so nothing can connect to it. The connect code was not spent: send
    /// it again once the admin has set one.
    #[problem(status = 503, title = "No public address yet")]
    NoPublicAddress,
    /// The request needs a signed-in merchant (the point of sale and the
    /// dashboard's own scripts call it), and its session is missing or has
    /// ended. Sign in again.
    #[problem(status = 401, title = "Not signed in")]
    SignedOut,
    /// Only this Monokulo's admin may do this.
    #[problem(status = 403, title = "Only the admin may do this")]
    AdminOnly,
    /// The refund address isn't a Monero address on the store's network,
    /// or is empty.
    #[problem(status = 400, title = "Not a refund address for this store")]
    InvalidRefundAddress,
    /// The order is settled (paid, overpaid or expired) and already has a
    /// refund address, which can no longer be changed: a refund may be on
    /// its way to it.
    #[problem(status = 409, title = "The refund address can't be changed")]
    RefundAddressLocked,
}

impl Problem {
    /// Its `type` URI: its page in the docs.
    pub fn type_uri(self) -> String {
        format!("{DOCS}/problems/{}", self.slug())
    }

    /// This problem, happening now for the reason `detail`.
    pub fn detail(self, detail: impl Into<String>) -> ProblemDetails {
        ProblemDetails {
            problem: self,
            detail: detail.into(),
            extensions: serde_json::Map::new(),
            headers: Vec::new(),
        }
    }
}

/// An error response: a [`Problem`] with what went wrong this time.
#[derive(Debug)]
pub struct ProblemDetails {
    pub problem: Problem,
    pub detail: String,
    extensions: serde_json::Map<String, serde_json::Value>,
    headers: Vec<(header::HeaderName, HeaderValue)>,
}

/// The body every error has. Extension members (`retry_after`,
/// `challenge`) come on top.
#[derive(Serialize, JsonSchema)]
#[schemars(rename = "Problem")]
pub struct ProblemBody {
    /// The problem's page in the docs.
    #[serde(rename = "type")]
    pub type_: String,
    /// A short summary, the same for every occurrence of the problem.
    pub title: String,
    /// The HTTP status.
    pub status: u16,
    /// What went wrong this time, for a person.
    pub detail: String,
}

impl ProblemDetails {
    /// Adds an extension member.
    pub fn with(mut self, name: &str, value: serde_json::Value) -> Self {
        self.extensions.insert(name.to_owned(), value);
        self
    }

    /// Adds a response header.
    pub fn header(mut self, name: header::HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }
}

impl IntoResponse for ProblemDetails {
    fn into_response(self) -> Response {
        let problem = self.problem;
        let body = ProblemBody {
            type_: problem.type_uri(),
            title: problem.title().to_owned(),
            status: problem.status().as_u16(),
            detail: self.detail,
        };
        let mut json = match serde_json::to_value(body) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        for (name, value) in self.extensions {
            json.entry(name).or_insert(value);
        }
        let mut response = (
            problem.status(),
            [(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON))],
            serde_json::Value::Object(json).to_string(),
        )
            .into_response();
        if problem == Problem::Unauthorized {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        for (name, value) in self.headers {
            response.headers_mut().insert(name, value);
        }
        response
    }
}

impl From<ApiError> for ProblemDetails {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::Unauthorized => Problem::SignedOut.detail("Sign in again."),
            ApiError::BadRequest(message) => Problem::InvalidRequest.detail(message),
            ApiError::NotFound => Problem::NotFound.detail("There's no such thing here."),
            ApiError::Forbidden => Problem::AdminOnly.detail("Only the admin may do this."),
            ApiError::EngineUnavailable => Problem::EngineUnavailable
                .detail("The payment engine is busy or unreachable. Try again in a moment."),
            ApiError::Internal => Problem::Internal
                .detail("Something went wrong on this Monokulo. Try again in a moment."),
        }
    }
}

/// No such order (or other resource) for this store.
pub fn not_found() -> ProblemDetails {
    Problem::NotFound.detail("There's no such order for this store.")
}

/// Something went wrong here.
pub fn internal() -> ProblemDetails {
    ApiError::Internal.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_has_its_own_slug_and_a_page_to_say() {
        let mut slugs = std::collections::HashSet::new();
        for problem in Problem::ALL {
            assert!(slugs.insert(problem.slug()), "{problem:?}");
            assert!(!problem.doc().is_empty(), "{problem:?}");
        }
        assert_eq!(Problem::InvalidAmount.slug(), "invalid-amount");
        assert_eq!(
            Problem::NotFound.type_uri(),
            "https://docs.monokulo.com/problems/not-found"
        );
    }
}
