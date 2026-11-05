use serde_json::json;

use super::{checkout_page, checkout_page_check, checkout_page_publish};
use crate::api::{self, prelude::*};
use crate::checkout_pages::TEMPLATE;
use crate::http::checkout_pages as pages;

api::resource!(
    "/api/v1/checkout-page/draft",
    rel = "checkout-page-draft",
    doc = "A custom checkout page being written, kept for a week from its last save. A store has one, and customers never see it."
);

/// A store's draft, and what approving it takes.
#[api::body]
pub(crate) struct Body {
    /// The template, as it was written.
    template: String,
    /// The CSS, as it was written.
    css: String,
    /// Which version this is. `approval.value` has what approves it.
    version: String,
    /// A page showing this draft in each order state. Anyone with the link can
    /// see it until the draft is replaced, published, discarded or expires.
    preview_url: String,
    /// When it was last saved, in Unix seconds.
    created_at: i64,
    /// When it's discarded unless saved again, in Unix seconds.
    expires_at: i64,
    /// What approves it; left out for a store with no verified domain, which
    /// can't publish one until it has.
    approval: Option<Approval>,
    page: Link<checkout_page::Path>,
    check_approval: Form<checkout_page_check::check>,
    publish: Form<checkout_page_publish::publish>,
    discard: Form<delete>,
}

/// The proof a draft needs. Each version has its own.
#[api::object]
struct Approval {
    /// The domain or onion address that approves it, and the only site its page
    /// will show on.
    domain: String,
    /// `dns`: a TXT record at `name`. `file`: a file at `name`, on an onion
    /// address, as it has no DNS.
    kind: String,
    /// Where the proof goes: the record's full name, or the file's address.
    name: String,
    /// The proof itself: the TXT record's value, or the file's contents.
    value: String,
}

/// The store's draft as the API shows it, if it has one.
pub(crate) async fn current(
    state: &crate::http::AppState,
    id: &crate::db::ConnectionId,
) -> Result<Option<Body>, crate::db::DbError> {
    let now = crate::now_unix();
    let store_id = id.clone();
    let (draft, domains) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.checkout_page_draft(&store_id, now)?,
                db.list_store_domains(&store_id)?,
            ))
        })
        .await?;
    let Some(draft) = draft else {
        return Ok(None);
    };
    let version = checkout_template::version_token(id.as_str(), &draft.template, &draft.css);
    let approval = crate::checkout_pages::approver(&domains, now).map(|row| {
        let onion = domain_proof::is_onion(&row.domain);
        Approval {
            domain: row.domain.clone(),
            kind: if onion { "file" } else { "dns" }.to_string(),
            name: if onion {
                TEMPLATE.well_known_url(&row.domain)
            } else {
                TEMPLATE.record_name(&row.domain)
            },
            value: TEMPLATE.value(&version),
        }
    });
    Ok(Some(Body {
        template: draft.template,
        css: draft.css,
        version,
        preview_url: pages::preview_link(
            crate::base::network().and_then(|network| state.network_of(network).public_uri()),
            &draft.preview_token,
        ),
        created_at: draft.created_at,
        expires_at: draft.created_at + crate::db::DRAFT_SECS,
        approval,
        page: Link::new(checkout_page::Path {}),
        check_approval: Form::new(),
        publish: Form::new(),
        discard: Form::new(),
    }))
}

/// Read the store's draft.
///
/// A store without one is `404` `not-found`.
#[get(sk, problems = [NotFound, Internal])]
async fn get(store: Store, instance: Instance) -> api::Result<Body> {
    match current(&instance.0, &store.id).await {
        Ok(Some(body)) => Ok(body),
        Ok(None) => Err(Problem::NotFound.detail("This store has no draft.")),
        Err(_) => Err(internal()),
    }
}

/// A custom page to save as the store's draft.
#[api::input]
struct Input {
    /// The template: HTML with `{{…}}` features, up to 32 KiB. See the custom pages
    /// guide.
    template: String,
    /// CSS for it, up to 32 KiB; none when left out.
    css: Option<String>,
}

/// Save a draft, replacing any the store has.
///
/// What's in use keeps showing. A store needs a verified domain first: without
/// one, `409` `checkout-page-needs-domain`, as nobody could approve it and its
/// preview would be anyone's words at this Monokulo's address. A template or CSS
/// with something wrong is `422` `checkout-page-refused`, saving nothing, with up
/// to 20 problems in `errors`; its CSS is read only once its template does. At most
/// 30 a minute for a store: past that, `429` `rate-limited`.
#[post(
    sk,
    form = "save-checkout-page-draft",
    problems = [InvalidRequest, CheckoutPageNeedsDomain, CheckoutPageRefused, RateLimited, Internal]
)]
async fn save(store: Store, instance: Instance, input: Input) -> api::Result<Body> {
    let state = instance.0;
    let now = crate::now_unix();
    if !pages::SAVES.check(store.id.clone(), now) {
        return Err(too_often(
            "That's a lot of saves. Wait a minute, then try again.",
        ));
    }
    let id = store.id.clone();
    let domains = state
        .db
        .read(move |db| db.list_store_domains(&id))
        .await
        .map_err(|_| internal())?;
    if crate::checkout_pages::approver(&domains, now).is_none() {
        return Err(Problem::CheckoutPageNeedsDomain
            .detail("Verify a web domain or onion address for this store first."));
    }
    let checked = pages::check_draft(&input.template, input.css.as_deref().unwrap_or_default())
        .map_err(|problems| {
            Problem::CheckoutPageRefused
                .detail("Something in this template or CSS can't be used.")
                .with("errors", errors_of(&problems))
        })?;
    pages::save_checked(&state, &store.id, checked, now)
        .await
        .map_err(|_| internal())?;
    match current(&state, &store.id).await {
        Ok(Some(body)) => Ok(body),
        _ => Err(internal()),
    }
}

/// Past a store's limit: these count per minute, so a minute is always enough.
pub(crate) fn too_often(detail: &str) -> crate::api::runtime::problem::ProblemDetails {
    Problem::RateLimited
        .detail(detail)
        .with("retry_after", json!(60))
        .header(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("60"),
        )
}

/// `checkout-page-refused`'s `errors`.
pub(crate) fn errors_of(problems: &[checkout_template::Problem]) -> serde_json::Value {
    problems.iter().map(error_of).collect()
}

/// One of `checkout-page-refused`'s `errors`.
fn error_of(problem: &checkout_template::Problem) -> serde_json::Value {
    use checkout_template::Place;
    let (place, line) = match problem.place {
        Place::Template(line) => ("template", Some(line)),
        Place::Css(line) => ("css", Some(line)),
        Place::EveryState => ("template", None),
    };
    let mut error = json!({"place": place, "message": problem.message});
    if let Some(line) = line {
        error["line"] = json!(line);
    }
    if let Some(hint) = &problem.hint {
        error["hint"] = json!(hint);
    }
    error
}

/// Discard the store's draft. What's in use keeps showing.
///
/// A store without one answers the same.
#[delete(sk, form = "discard-checkout-page-draft", problems = [Internal])]
async fn delete(store: Store, instance: Instance) -> api::Result<NoContent> {
    let id = store.id.clone();
    instance
        .0
        .db
        .write(move |db| db.discard_checkout_page_draft(&id))
        .await
        .map_err(|_| internal())?;
    Ok(NoContent)
}
