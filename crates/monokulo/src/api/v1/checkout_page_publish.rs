use super::{checkout_page, checkout_page_draft};
use crate::api::{self, prelude::*};
use crate::http::checkout_pages::{self as pages, PublishRefused};

api::resource!(
    "/api/v1/checkout-page/draft/publish",
    rel = "checkout-page-publish",
    doc = "Makes a draft the store's custom page, once its domain has approved it. The secret key alone can't put new words in front of customers: that takes the proof on the domain."
);

/// Publish the draft.
///
/// Monokulo looks for the proof itself, and publishes only if it's there. With it
/// missing, `409` `checkout-page-not-approved`; with the lookup unanswered, `503`
/// `checkout-page-not-checked`, worth trying again. A store with no draft or no
/// verified domain is `404` `not-found`; a page this Monokulo's admin turned off is
/// `403` `checkout-page-turned-off`; a draft saved by an earlier release that no
/// longer passes the checks is `422` `checkout-page-refused`. At most 12 a minute
/// for a store, checks included: past that, `429` `rate-limited`.
#[post(
    sk,
    form = "publish-checkout-page",
    problems = [
        NotFound,
        CheckoutPageNotApproved,
        CheckoutPageNotChecked,
        CheckoutPageTurnedOff,
        CheckoutPageRefused,
        RateLimited,
        Internal
    ]
)]
async fn publish(store: Store, instance: Instance) -> api::Result<checkout_page::Body> {
    let state = instance.0;
    if !pages::CHECKS.check(store.id.clone(), crate::now_unix()) {
        return Err(checkout_page_draft::too_often(
            "That's a lot of lookups. Wait a minute, then try again.",
        ));
    }
    pages::publish_draft(&state, &store.id)
        .await
        .map_err(|refused| match refused {
            PublishRefused::NothingToPublish => {
                Problem::NotFound.detail("This store has no draft, or no verified domain.")
            }
            PublishRefused::NotFound(reason) => Problem::CheckoutPageNotApproved.detail(reason),
            PublishRefused::CantCheck(reason) => Problem::CheckoutPageNotChecked.detail(reason),
            PublishRefused::TurnedOff => Problem::CheckoutPageTurnedOff
                .detail("This Monokulo's admin turned this store's custom page off."),
            PublishRefused::NoLongerPasses(problems) => Problem::CheckoutPageRefused
                .detail("This version no longer passes the checks. Save it again, fixed.")
                .with("errors", checkout_page_draft::errors_of(&problems)),
            PublishRefused::Failed => internal(),
        })?;
    let id = store.id.clone();
    let published = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.published_checkout_page(&id)?,
                db.list_store_domains(&id)?,
            ))
        })
        .await;
    match published {
        Ok((Some(page), domains)) => Ok(checkout_page::body_of(page, &domains, crate::now_unix())),
        _ => Err(internal()),
    }
}
