use super::checkout_page_draft;
use crate::api::{self, prelude::*};
use crate::checkout_pages::TEMPLATE;
use crate::http::checkout_pages as pages;

api::resource!(
    "/api/v1/checkout-page/draft/check",
    rel = "checkout-page-check",
    doc = "Looks for a draft's proof now, without publishing it: for a client waiting on DNS."
);

/// What the look found.
#[api::body]
struct Body {
    /// Whether the proof is there.
    found: bool,
    /// Whether the lookup got an answer: `false` says nothing about the proof.
    answered: bool,
    /// What's wrong, for a person; left out when it's found.
    error: Option<String>,
    draft: Link<checkout_page_draft::Path>,
}

/// Look for the draft's proof.
///
/// At most 12 a minute for a store, publishing included: past that, `429`
/// `rate-limited`. A store with no draft, or no verified domain to approve one,
/// is `404` `not-found`.
#[post(sk, form = "check-checkout-page-approval", problems = [NotFound, RateLimited, Internal])]
async fn check(store: Store, instance: Instance) -> api::Result<Body> {
    let state = instance.0;
    if !pages::CHECKS.check(store.id.clone(), crate::now_unix()) {
        return Err(checkout_page_draft::too_often(
            "That's a lot of lookups. Wait a minute, then try again.",
        ));
    }
    let approval = match pages::approval_for(&state, &store.id).await {
        Ok(Some(approval)) => approval,
        Ok(None) => {
            return Err(Problem::NotFound.detail("This store has no draft, or no verified domain."))
        }
        Err(_) => return Err(internal()),
    };
    let outcome =
        domain_proof::check(&state.proofs, &TEMPLATE, &approval.domain, &approval.token).await;
    Ok(Body {
        found: matches!(outcome, domain_proof::CheckOutcome::Found),
        answered: !matches!(outcome, domain_proof::CheckOutcome::LookupFailed(_)),
        error: outcome.error_message(&TEMPLATE, &approval.domain, &approval.token),
        draft: Link::new(checkout_page_draft::Path {}),
    })
}
