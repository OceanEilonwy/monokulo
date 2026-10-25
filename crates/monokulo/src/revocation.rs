//! Ending what was opened with a credential once it stops working. A
//! request's key or session is checked when the request arrives; an open
//! stream is not a request, so a key rotated, an app disconnected or a
//! session ended meanwhile would otherwise go on streaming for as long as
//! the connection lasts.
//!
//! Each stream holds the credential it was opened with ([`Hold`], by the
//! credential's hash), and whatever ends a credential revokes it
//! ([`Revocations::revoke`]) once the change is written: every stream
//! holding it ends at once. One token per credential, not a broadcast every
//! stream filters, so there is nothing to fall behind on and miss.
//!
//! A hold is taken before the credential is looked up, so a revocation
//! that lands between the lookup and the stream opening still reaches it.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{Stream, StreamExt as _};
use parking_lot::Mutex;
use shared::auth::TokenHash;
use tokio_util::sync::CancellationToken;

/// The credentials something holds, by hash, each with the token that
/// ends its holders. A credential nobody holds has no entry.
#[derive(Default)]
pub struct Revocations {
    held: Mutex<HashMap<TokenHash, Held>>,
}

struct Held {
    token: CancellationToken,
    holders: usize,
}

impl Revocations {
    /// A hold on the credential whose hash is `credential`: revoked once
    /// [`Revocations::revoke`] is called for it.
    pub fn hold(self: &Arc<Self>, credential: &TokenHash) -> Hold {
        let mut held = self.held.lock();
        let entry = held.entry(credential.clone()).or_insert_with(|| Held {
            token: CancellationToken::new(),
            holders: 0,
        });
        entry.holders += 1;
        Hold {
            inner: Arc::new(HoldInner {
                revocations: Arc::clone(self),
                credential: credential.clone(),
                token: entry.token.clone(),
            }),
            expires: None,
        }
    }

    /// The credential whose hash is `credential` no longer works: everything
    /// holding it ends. Called once the change is written, so a hold taken
    /// after this finds the credential gone when it looks it up.
    pub fn revoke(&self, credential: &TokenHash) {
        if let Some(held) = self.held.lock().remove(credential) {
            held.token.cancel();
        }
    }
}

/// A credential something was opened with. Cheap to clone; the last clone
/// dropped lets the credential go.
#[derive(Clone)]
pub struct Hold {
    inner: Arc<HoldInner>,
    /// When the credential stops working by itself (a session's lifetime).
    expires: Option<tokio::time::Instant>,
}

struct HoldInner {
    revocations: Arc<Revocations>,
    credential: TokenHash,
    token: CancellationToken,
}

impl Drop for HoldInner {
    fn drop(&mut self) {
        let mut held = self.revocations.held.lock();
        // A revoked credential's entry is already gone; one held again
        // since is a new entry, not this one's.
        if self.token.is_cancelled() {
            return;
        }
        if let Some(entry) = held.get_mut(&self.credential) {
            entry.holders -= 1;
            if entry.holders == 0 {
                held.remove(&self.credential);
            }
        }
    }
}

impl Hold {
    /// The same hold, also ending at `expires`.
    pub fn expiring_at(mut self, expires: tokio::time::Instant) -> Self {
        self.expires = Some(expires);
        self
    }

    /// Whether the credential has stopped working.
    pub fn is_revoked(&self) -> bool {
        self.inner.token.is_cancelled()
            || self
                .expires
                .is_some_and(|expires| tokio::time::Instant::now() >= expires)
    }

    /// Done once the credential stops working.
    pub async fn revoked(&self) {
        match self.expires {
            Some(expires) => {
                tokio::select! {
                    () = self.inner.token.cancelled() => {}
                    () = tokio::time::sleep_until(expires) => {}
                }
            }
            None => self.inner.token.cancelled().await,
        }
    }

    /// `stream`, ending once the credential stops working. It holds the
    /// credential for as long as it runs.
    pub fn bound<S>(&self, stream: S) -> impl Stream<Item = S::Item> + Send + 'static
    where
        S: Stream + Send + 'static,
    {
        let hold = self.clone();
        stream.take_until(Box::pin(async move { hold.revoked().await }))
    }
}
