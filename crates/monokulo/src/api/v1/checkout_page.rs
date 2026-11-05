use super::{checkout_page_draft, store};
use crate::api::{self, prelude::*};
use crate::checkout_pages::{approver, standing, PageStanding};

api::resource!(
    "/api/v1/checkout-page",
    rel = "checkout-page",
    doc = "The store's custom checkout page: the one version its domain approved, which its customers see in a frame of the site that approved it.\n\nEverything else (payment links, other sites, a page asking for a layout) gets the store's layout. To change the page, save a draft (`save-checkout-page-draft`), approve it on the domain, then publish it: what's in use keeps showing until then."
);

/// The custom page in use.
#[api::body]
pub(crate) struct Body {
    /// The template, as it was written.
    template: String,
    /// The CSS, as it was written.
    css: String,
    /// Which version this is.
    version: String,
    /// `working`, `failing` (its proof is gone, and it shows until its grace
    /// period ends), `not-showing` (its grace period ended, its domain no longer
    /// counts, or it no longer passes the checks) or `turned-off` (by this
    /// Monokulo's admin).
    status: String,
    /// The domain whose proof it needs; left out when there's none.
    domain: Option<String>,
    /// When its proof was last looked at, in Unix seconds.
    last_checked_at: i64,
    /// What the last look found wrong, for a person; left out when nothing was.
    last_error: Option<String>,
    /// When it stops showing if its proof stays missing, in Unix seconds; left
    /// out unless `failing`.
    stops_showing_at: Option<i64>,
    /// Why this Monokulo's admin turned it off, when they said.
    turned_off_reason: Option<String>,
    draft: Link<checkout_page_draft::Path>,
    store: Link<store::Path>,
    save_draft: Form<checkout_page_draft::save>,
    stop_using: Form<delete>,
}

/// What `id`'s page looks like to the API, at `now`.
pub(crate) fn body_of(
    page: crate::db::PublishedCheckoutPage,
    domains: &[crate::db::StoreDomainRow],
    now: i64,
) -> Body {
    let standing = standing(&page, domains, now);
    let status = match &standing {
        PageStanding::Working => "working",
        PageStanding::Failing { .. } => "failing",
        PageStanding::NotShowing => "not-showing",
        PageStanding::TurnedOff => "turned-off",
    };
    Body {
        status: status.to_string(),
        domain: approver(domains, now).map(|row| row.domain.clone()),
        last_checked_at: page.last_checked_at.unwrap_or(page.published_at),
        stops_showing_at: match standing {
            PageStanding::Failing { since } => Some(since + domain_proof::GRACE_SECS),
            _ => None,
        },
        template: page.template,
        css: page.css,
        version: page.token,
        last_error: page.last_error,
        turned_off_reason: page.turned_off_reason.filter(|reason| !reason.is_empty()),
        draft: Link::new(checkout_page_draft::Path {}),
        store: Link::new(store::Path {}),
        save_draft: Form::new(),
        stop_using: Form::new(),
    }
}

/// Read the custom page in use, and how it stands.
///
/// A store without one is `404` `not-found`.
#[get(sk, problems = [NotFound, Internal])]
async fn get(store: Store, instance: Instance) -> api::Result<Body> {
    let (id, state) = (store.id.clone(), instance.0);
    let found = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.published_checkout_page(&id)?,
                db.list_store_domains(&id)?,
            ))
        })
        .await;
    match found {
        Ok((Some(page), domains)) => Ok(body_of(page, &domains, crate::now_unix())),
        Ok((None, _)) => Err(Problem::NotFound.detail("This store has no custom page in use.")),
        Err(_) => Err(internal()),
    }
}

/// Stop using the custom page: the store's layout shows instead.
///
/// A page this Monokulo's admin turned off stays, as a record of that:
/// `403` `checkout-page-turned-off`. A store without one answers the same as
/// having stopped.
#[delete(sk, form = "stop-using-checkout-page", problems = [CheckoutPageTurnedOff, Internal])]
async fn delete(store: Store, instance: Instance) -> api::Result<NoContent> {
    let (id, state) = (store.id.clone(), instance.0);
    let stopped = state
        .db
        .write(move |db| {
            let had = db.published_checkout_page(&id)?.is_some();
            Ok::<_, crate::db::DbError>(!had || db.stop_using_checkout_page(&id)?)
        })
        .await;
    match stopped {
        Ok(true) => {
            tracing::info!(store.id = %store.id, "custom checkout page stopped, by the API");
            Ok(NoContent)
        }
        Ok(false) => Err(Problem::CheckoutPageTurnedOff
            .detail("This Monokulo's admin turned this store's custom page off.")),
        Err(_) => Err(internal()),
    }
}
