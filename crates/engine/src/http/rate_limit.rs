//! Rate limiting for the engine's HTTP API: one budget per store, and one
//! for everything else monokulo asks of the engine.
//!
//! The engine is private: every request carries the
//! engine token, checked before this ([`super::engine_token_middleware`]),
//! so whatever reaches the limiter is monokulo, or an operator holding the
//! token. The caller's address says nothing then: monokulo calls on behalf
//! of every one of its users from one address, and when the engine runs
//! inside monokulo there is no address at all. So the limiter never looks
//! at one. Instead:
//!
//! - A request carrying a store's `Authorization: Bearer sk_...` counts
//!   against that store's own bucket (`server.rate_limit_per_token_per_min`),
//!   so one busy store can't use up another's.
//! - Anything else (`/status`, the settings, logs and engine pages, tenant
//!   creation) counts against one bucket for the whole instance
//!   (`server.instance_rate_limit_per_min`) when `server.instance_rate_limit`
//!   is on, and isn't counted when it is off. It is off by default for an
//!   engine inside monokulo, where nothing but monokulo can call it, and on
//!   for a standalone one, which anything holding the token can reach.
//!
//! A fixed-window counter (`shared::rate_limit::RateLimiter`), not a proper
//! token bucket: simpler, and enough for "stop one caller hammering the
//! engine".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use serde_json::json;

use shared::rate_limit::RateLimiter;

use crate::engine_settings::ApiLimits;

/// The engine API's budgets: one per store, keyed on the hash of the
/// store's `sk_`, and one for the instance's other calls, which can be
/// switched off. Every limit changes live (`LimitsReloadable`).
pub struct ApiRateLimits {
    store: RateLimiter<String>,
    instance: RateLimiter<()>,
    instance_on: AtomicBool,
}

impl ApiRateLimits {
    /// `per_store` requests a minute for each store, and `instance`
    /// requests a minute for everything else, or no limit on it (`None`).
    pub fn new(per_store: u32, instance: Option<u32>) -> Self {
        let limits = Self {
            store: RateLimiter::new(per_store),
            instance: RateLimiter::new(1),
            instance_on: AtomicBool::new(false),
        };
        limits.set_instance(instance);
        limits
    }

    /// Applies saved settings, from the next request on. Counts already
    /// taken in the current window stay.
    pub fn apply(&self, settings: &ApiLimits) {
        self.store.set_limit(settings.rate_limit_per_token_per_min);
        self.set_instance(
            settings
                .instance_rate_limit
                .then_some(settings.instance_rate_limit_per_min),
        );
    }

    fn set_instance(&self, instance: Option<u32>) {
        if let Some(per_minute) = instance {
            self.instance.set_limit(per_minute);
        }
        self.instance_on
            .store(instance.is_some(), Ordering::Relaxed);
    }

    /// Whether a request with these headers is allowed at `now`, having
    /// counted it against its bucket.
    ///
    /// A store's `sk_` is keyed on its hash, not itself: a fixed-size key,
    /// so a caller sending junk tokens as long as a header allows can't
    /// hold gigabytes in the limiter, and no secret sits in memory for a
    /// minute after its request ended. The token isn't checked here: an
    /// invalid one uses its own bucket and is refused downstream
    /// (`AuthedTenant`).
    pub fn allows(&self, headers: &HeaderMap, now: i64) -> bool {
        let store_token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|token| token.starts_with("sk_"));
        match store_token {
            Some(token) => {
                let key = shared::auth::RawToken::presented(token)
                    .hash()
                    .as_str()
                    .to_owned();
                self.store.check(key, now)
            }
            None if self.instance_on.load(Ordering::Relaxed) => self.instance.check((), now),
            None => true,
        }
    }
}

/// Answers 429 to a request over its budget (see the module doc comment
/// for which budget that is).
pub async fn admin_rate_limit_middleware(
    State(limits): State<Arc<ApiRateLimits>>,
    req: Request,
    next: Next,
) -> Response {
    if !limits.allows(req.headers(), super::now_unix()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(json!({ "error": "rate limit exceeded" })),
        )
            .into_response();
    }
    next.run(req).await
}
