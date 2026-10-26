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
//! - A store's routes authenticate the store by its `Authorization: Bearer
//!   sk_...` first, and count against that store's own bucket
//!   (`server.rate_limit_per_token_per_min`), keyed on its id, so one busy
//!   store can't use up another's.
//! - Everything else (`/status`, the settings, logs and engine pages, tenant
//!   creation), and a store route whose `sk_` is no store's, counts against
//!   one bucket for the whole instance (`server.instance_rate_limit_per_min`)
//!   when `server.instance_rate_limit` is on, and isn't counted when it is
//!   off. It is off by default for an engine inside monokulo, where nothing
//!   but monokulo can call it, and on for a standalone one, which anything
//!   holding the token can reach.
//!
//! Which bucket a request uses is decided by its route, never by its
//! headers: a made-up `sk_` neither escapes the instance's budget nor makes
//! a bucket of its own.
//!
//! A fixed-window counter (`rate_limit::RateLimiter`), not a proper
//! token bucket: simpler, and enough for "stop one caller hammering the
//! engine".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use serde_json::json;

use rate_limit::RateLimiter;

use super::{AppState, AuthedTenant};
use crate::engine_settings::ApiLimits;
use crate::store::TenantId;

/// The engine API's budgets: one per store, keyed on its id, and one for
/// the instance's other calls, which can be switched off. Every limit
/// changes live (`LimitsReloadable`).
pub struct ApiRateLimits {
    store: RateLimiter<TenantId>,
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

    /// Whether a call on the instance's budget is allowed at `now`, having
    /// counted it. Always, while the instance limit is off.
    fn allows_instance(&self, now: i64) -> bool {
        !self.instance_on.load(Ordering::Relaxed) || self.instance.check((), now)
    }

    /// Whether a request of the store `id` is allowed at `now`, having
    /// counted it against that store's budget.
    fn allows_store(&self, id: &TenantId, now: i64) -> bool {
        self.store.check(id.clone(), now)
    }
}

fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(json!({ "error": "rate limit exceeded" })),
    )
        .into_response()
}

/// For the routes that aren't a store's: answers 429 to a request over the
/// instance's budget. Its headers aren't looked at.
pub async fn instance_rate_limit_middleware(
    State(limits): State<Arc<ApiRateLimits>>,
    req: Request,
    next: Next,
) -> Response {
    if !limits.allows_instance(super::now_unix()) {
        return too_many_requests();
    }
    next.run(req).await
}

/// For a store's routes: counts the request against its store's budget.
///
/// Authenticates the store by its `sk_` first, handing the store on to the
/// route's [`AuthedTenant`]. A request no store's `sk_` authenticates is
/// counted against the instance's budget and refused as unauthenticated.
pub async fn store_rate_limit_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let now = super::now_unix();
    let (mut parts, body) = req.into_parts();
    match AuthedTenant::authenticate(&parts.headers, &state).await {
        Ok(authed) => {
            if !state.rate_limits.allows_store(&authed.0.id, now) {
                return too_many_requests();
            }
            parts.extensions.insert(authed);
            next.run(Request::from_parts(parts, body)).await
        }
        Err(_) if !state.rate_limits.allows_instance(now) => too_many_requests(),
        Err(rejection) => rejection.into_response(),
    }
}
