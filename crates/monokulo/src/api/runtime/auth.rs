//! Who may call a handler, in one place: its `who` (`#[get(server)]`),
//! which [`Authorized`] checks before the handler runs and which decides
//! whether a response offers a link or form to it (`super::hal`), so what
//! the API offers a caller is what it lets them do.
//!
//! The key a request carries is found once, by `http::abuse` from its
//! `Authorization: Bearer` header (`http::store_key`); while the handler
//! runs and its response is written it is [`current`], and where its links
//! start is [`base`].

use std::marker::PhantomData;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use super::problem::{Problem, ProblemDetails};
use crate::http::store_key::{ApiCaller, Credential};
use crate::http::AppState;

/// The kind of credential a request carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// The store's public key, `pk_…`: a browser page embedding the
    /// checkout. It proves nothing beyond knowing the store.
    Pk,
    /// The store's secret key, `sk_…`: the shop's own server.
    Sk,
    /// A connected app's key, `ak_…`.
    Ak,
    /// A merchant signed in to the dashboard.
    Session,
}

impl Key {
    pub const ALL: [Key; 4] = [Key::Pk, Key::Sk, Key::Ak, Key::Session];

    pub fn name(self) -> &'static str {
        match self {
            Key::Pk => "pk",
            Key::Sk => "sk",
            Key::Ak => "ak",
            Key::Session => "session",
        }
    }
}

impl Credential {
    pub fn key(&self) -> Key {
        match self {
            Credential::Secret => Key::Sk,
            Credential::App(_) => Key::Ak,
            Credential::Public => Key::Pk,
        }
    }
}

/// Who may call a handler: a set of keys, and maybe no key at all. Written
/// in `#[get(...)]` as `pk`, `sk`, `ak`, `session`, or the sets `any_key`
/// (pk, sk, ak), `server` (sk, ak: a caller that keeps a secret, the shop's
/// server or a connected app), `app` (ak) and `anyone` (with or without a
/// key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Who(pub u8);

impl Who {
    const NONE: u8 = 1;

    fn bit(key: Key) -> u8 {
        match key {
            Key::Pk => 2,
            Key::Sk => 4,
            Key::Ak => 8,
            Key::Session => 16,
        }
    }

    /// Whether a caller with `key` (`None`: no key) may.
    pub fn admits(self, key: Option<Key>) -> bool {
        let bit = key.map_or(Self::NONE, Self::bit);
        self.0 & bit != 0
    }

    /// The keys it admits, by name.
    pub fn keys(self) -> Vec<&'static str> {
        Key::ALL
            .into_iter()
            .filter(|key| self.admits(Some(*key)))
            .map(Key::name)
            .collect()
    }
}

/// A handler's `who`, as a type, for [`Authorized`].
pub trait WhoMarker {
    const WHO: Who;
}

/// What a response is written for: who asked, and where its links start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// The request's key; `None` for none.
    pub key: Option<Key>,
    /// The path every link starts with (`crate::base::for_request`): `/monokulo`
    /// for a Monokulo served at `https://host/monokulo`, empty at the root
    /// of its host.
    pub base: String,
    /// The request's `If-None-Match`, for an answer that can be cached
    /// (`super::hal::Cached`).
    pub if_none_match: Option<String>,
}

/// The request's key, once its handler's `who` admitted it. The first
/// thing a handler extracts, so a caller who may not is refused before
/// anything else is read: `401` with no key, `403` with one that may not.
pub struct Authorized<W> {
    pub context: Context,
    who: PhantomData<W>,
}

impl<W: WhoMarker> FromRequestParts<AppState> for Authorized<W> {
    type Rejection = ProblemDetails;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, ProblemDetails> {
        let key = parts
            .extensions
            .get::<ApiCaller>()
            .map(|caller| caller.credential.key());
        if !W::WHO.admits(key) {
            return Err(refusal(W::WHO, key));
        }
        Ok(Authorized {
            context: Context {
                key,
                base: crate::base::for_request(state, &parts.extensions),
                if_none_match: parts
                    .headers
                    .get(axum::http::header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
            },
            who: PhantomData,
        })
    }
}

/// Why `key` may not call a handler `who` admits.
pub fn refusal(who: Who, key: Option<Key>) -> ProblemDetails {
    match key {
        None => Problem::Unauthorized.detail(format!(
            "Send a key: Authorization: Bearer <key>. This takes {}.",
            who.keys().join(", ")
        )),
        Some(key) => Problem::Forbidden.detail(format!(
            "A {} key may not do this. It takes {}.",
            key.name(),
            who.keys().join(", ")
        )),
    }
}

tokio::task_local! {
    /// The request being answered (see [`current`]).
    static CALLER: Context;
}

/// The key of the request being answered: links and forms are offered
/// only when it may follow them. `None` outside a request, or for a
/// request with no key.
pub fn current() -> Option<Key> {
    CALLER.try_with(|context| context.key).ok().flatten()
}

/// Where the links of the request being answered start (see
/// [`Context::base`]); empty outside a request.
pub fn base() -> String {
    CALLER
        .try_with(|context| context.base.clone())
        .unwrap_or_default()
}

/// The request being answered, for writing part of its answer later (an
/// event on its stream) with [`as_caller`]; the default outside one.
pub fn context() -> Context {
    CALLER.try_with(Context::clone).unwrap_or_default()
}

/// `f`, run as if answering the request `context` describes: for writing
/// a body outside its handler, such as an event on a stream.
pub fn as_caller<T>(context: Context, f: impl FnOnce() -> T) -> T {
    CALLER.sync_scope(context, f)
}

/// Runs a handler and writes its answer, for its request.
pub async fn respond<W, T: super::hal::Respond>(
    authorized: Authorized<W>,
    handler: impl std::future::Future<Output = Result<T, ProblemDetails>>,
) -> Response {
    CALLER
        .scope(authorized.context, async move {
            match handler.await {
                Ok(answer) => answer.respond(),
                Err(problem) => problem.into_response(),
            }
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_named_sets_admit_their_keys() {
        // As `api-macros` writes them.
        let any_key = Who(2 | 4 | 8);
        let server = Who(4 | 8);
        let anyone = Who(1 | 2 | 4 | 8 | 16);
        assert!(any_key.admits(Some(Key::Pk)) && !any_key.admits(None));
        assert!(!server.admits(Some(Key::Pk)) && server.admits(Some(Key::Ak)));
        assert!(anyone.admits(None));
        assert_eq!(server.keys(), ["sk", "ak"]);
    }
}
