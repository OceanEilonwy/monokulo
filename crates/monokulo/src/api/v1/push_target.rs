use super::{app, push_targets};
use crate::api::runtime::problem::ProblemDetails;
use crate::api::{self, prelude::*};
use crate::apps::SaleKind;
use crate::db::{PushTarget, PushTargetId, TargetKind};
use crate::push::PushTargetKind;

api::resource!(
    "/api/v1/push-targets/{push_target_id}",
    rel = "push-target",
    params(push_target_id: PushTargetId)
);

/// One place the app's pushes go: a subscription its push service gave
/// it, and the sale events it wants there.
///
/// Its keys are never shown again. Register again with the same
/// `client_id` to give it a new subscription; `unregister-push-target`
/// stops its pushes. Monokulo drops it by itself once its push service says
/// the subscription is gone, or after a long run of failed pushes.
#[api::body]
struct Body {
    /// Its id: `pt_` and 32 hex digits.
    push_target_id: PushTargetId,
    /// The app's own name for it.
    client_id: ClientId,
    /// How its pushes are sent.
    kind: PushTargetKind,
    /// Where its pushes go: the subscription's endpoint.
    endpoint: String,
    /// The sale events pushed to it.
    events: Vec<SaleKind>,
    /// For a `live-activity` target: whether its pushes carry the payment
    /// URI (`monero:…`). Left out for an `encrypted` one, whose pushes
    /// carry the whole sale.
    include_payment_uri: Option<bool>,
    /// When it was first registered, in Unix seconds. Registering it again
    /// keeps this.
    created_at: i64,
    /// When a push last reached it, in Unix seconds; left out until one has.
    last_ok_at: Option<i64>,
    collection: Link<push_targets::Path>,
    app: Link<app::Path>,
}

/// `target` as the API shows it.
pub(crate) fn of(target: PushTarget) -> Body {
    Body {
        include_payment_uri: match target.kind {
            TargetKind::Encrypted { .. } => None,
            TargetKind::LiveActivity {
                include_payment_uri,
            } => Some(include_payment_uri),
        },
        kind: target.kind.name(),
        push_target_id: target.id,
        client_id: target.client_id,
        endpoint: target.endpoint.as_str().to_owned(),
        events: target.events,
        created_at: target.created_at,
        last_ok_at: target.last_ok_at,
        collection: Link::new(push_targets::Path {}),
        app: Link::new(app::Path {}),
    }
}

/// Read one of the app's push targets.
///
/// Another app's, even of the same store, is `404` `not-found`.
#[get(app, problems = [NotFound, Internal])]
async fn get(path: Path, app: ConnectedApp, apps: Apps) -> api::Result<Body> {
    let (app_id, id) = (app.0.id, path.push_target_id);
    match apps
        .0
        .db
        .read(move |db| db.get_push_target(&app_id, &id))
        .await
    {
        Ok(Some(target)) => Ok(of(target)),
        Ok(None) => Err(gone()),
        Err(e) => {
            tracing::error!(error = %e, "a push target could not be read");
            Err(internal())
        }
    }
}

/// Unregister this push target: nothing more is pushed to it.
///
/// Another app's, even of the same store, is `404` `not-found`, as is one
/// already unregistered.
#[delete(app, form = "unregister-push-target", problems = [NotFound, Internal])]
async fn delete(path: Path, app: ConnectedApp, apps: Apps) -> api::Result<NoContent> {
    let app = app.0;
    let (app_id, id) = (app.id.clone(), path.push_target_id.clone());
    match apps
        .0
        .db
        .write(move |db| db.delete_push_target(&app_id, &id))
        .await
    {
        Ok(true) => {
            tracing::info!(store.id = %app.store_id, app.id = %app.id, push_target.id = %path.push_target_id, "an app unregistered a push target");
            Ok(NoContent)
        }
        Ok(false) => Err(gone()),
        Err(e) => {
            tracing::error!(app.id = %app.id, error = %e, "a push target could not be unregistered");
            Err(internal())
        }
    }
}

/// No such push target for this app.
fn gone() -> ProblemDetails {
    Problem::NotFound.detail("This app has no such push target.")
}
