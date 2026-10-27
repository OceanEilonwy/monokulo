//! What APNs or FCM made of a push, in the terms the relay answers its
//! sender in, and the parts of their answers both clients read alike.

use std::time::{Duration, SystemTime};

use axum::http::{header, HeaderMap};

/// What became of a push handed to APNs or FCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Taken: the sender is answered `201`.
    Delivered,
    /// The token is dead (the app was removed, or the token replaced): the
    /// handle is forgotten, and the sender answered `404` so it drops its
    /// target too.
    Gone,
    /// Larger than the service carries: `413`.
    TooLarge,
    /// The service is down, busy or limiting this relay: `503`, try again
    /// after this many seconds.
    Unavailable { retry_after: u64 },
    /// Refused for a reason that isn't the device's or the sender's: this
    /// relay's credentials or settings. Logged with the service's reason;
    /// the sender is answered `502`.
    Refused,
}

/// The wait when a service gives none, or can't be reached at all.
pub const DEFAULT_RETRY_AFTER: u64 = 30;

/// Most of a service's error answer read: it is a short JSON reason.
pub const MAX_ERROR_BODY: usize = 16 * 1024;

/// How long the service asks to be left alone (`Retry-After`, seconds or
/// an HTTP date), or [`DEFAULT_RETRY_AFTER`]; at least a second, at most
/// a day, so a sender's backoff stays sane whatever is sent.
pub fn retry_after(headers: &HeaderMap, now: SystemTime) -> u64 {
    let Some(value) = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    else {
        return DEFAULT_RETRY_AFTER;
    };
    let seconds = if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse::<u64>().unwrap_or(u64::MAX)
    } else {
        match httpdate::parse_http_date(value) {
            Ok(at) => at.duration_since(now).unwrap_or(Duration::ZERO).as_secs(),
            Err(_) => DEFAULT_RETRY_AFTER,
        }
    };
    seconds.clamp(1, 86_400)
}

/// The start of a response's body, at most [`MAX_ERROR_BODY`] bytes:
/// enough for the reason in it, never a body of any size.
pub async fn error_body(mut response: reqwest::Response) -> Vec<u8> {
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        let room = MAX_ERROR_BODY.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= MAX_ERROR_BODY {
            break;
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn with(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn retry_after_reads_seconds_and_dates_within_bounds() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert_eq!(retry_after(&HeaderMap::new(), now), 30);
        assert_eq!(retry_after(&with("120"), now), 120);
        assert_eq!(retry_after(&with("0"), now), 1);
        assert_eq!(retry_after(&with("99999999999999999999999"), now), 86_400);
        assert_eq!(retry_after(&with("soon"), now), 30);
        let date = httpdate::fmt_http_date(now + Duration::from_secs(90));
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, HeaderValue::from_str(&date).unwrap());
        assert_eq!(retry_after(&headers, now), 90);
        // A date already past: no wait to speak of.
        assert_eq!(retry_after(&headers, now + Duration::from_secs(600)), 1);
    }
}
