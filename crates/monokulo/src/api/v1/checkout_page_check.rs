use super::checkout_page_draft;
use crate::api::{self, prelude::*};
use crate::checkout_pages::{approved, check_every, Approved};
use crate::http::checkout_pages as pages;

api::resource!(
    "/api/v1/checkout-page/draft/check",
    rel = "checkout-page-check",
    doc = "Looks for a draft's proof on each of the store's domains now, without publishing it: for a client waiting on DNS."
);

/// What the look found, together and on each domain.
#[api::body]
struct Body {
    /// Whether every domain has it, so the draft can be published.
    found: bool,
    /// Whether every domain answered: `false` says nothing about those that didn't.
    answered: bool,
    /// What's wrong, for a person; left out when it's found.
    error: Option<String>,
    /// What it found on each domain.
    domains: Vec<DomainFound>,
    draft: Link<checkout_page_draft::Path>,
}

/// What the look found on one domain.
#[api::object]
struct DomainFound {
    /// The domain or onion address.
    domain: String,
    /// Whether it has the proof.
    found: bool,
    /// What's wrong there, for a person; left out when it's found.
    error: Option<String>,
}

/// Look for the draft's proof.
///
/// At most 12 a minute for a store, publishing included: past that, `429`
/// `rate-limited`. A store with no draft, or no verified domain to approve one,
/// is `404` `not-found`.
#[post(sk, form = "check-checkout-page-approval", problems = [NotFound, RateLimited, Internal])]
async fn check(store: Store, instance: Instance) -> api::Result<Body> {
    let state = instance.0;
    if !pages::CHECKS.check(store.id.clone(), state.clock.now().as_secs()) {
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
    let checks = check_every(&state.proofs, &approval.domains, &approval.token).await;
    let together = approved(&checks);
    Ok(Body {
        found: together == Approved::Yes,
        answered: !checks
            .iter()
            .any(|check| matches!(check.outcome, domain_proof::CheckOutcome::LookupFailed(_))),
        error: match together {
            Approved::Yes => None,
            Approved::Missing(reason) | Approved::NoAnswer(reason) => Some(reason),
        },
        domains: checks
            .into_iter()
            .map(|check| DomainFound {
                found: matches!(check.outcome, domain_proof::CheckOutcome::Found),
                domain: check.domain,
                error: check.reason,
            })
            .collect(),
        draft: Link::new(checkout_page_draft::Path {}),
    })
}
