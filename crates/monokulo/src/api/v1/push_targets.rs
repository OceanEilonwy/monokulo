use super::{app, push_target};
use crate::api::{self, prelude::*};
use crate::apps::SaleKind;
use crate::db::{NewPushTarget, Registration, TargetKind, MAX_TARGETS_PER_APP};
use crate::push::PushTargetKind;

api::resource!("/api/v1/push-targets", rel = "push-targets");

/// Where the app wants the store's sales pushed while it isn't running,
/// and the key Monokulo signs its pushes with.
///
/// An app hands `vapid_public_key` to its push service (a UnifiedPush
/// distributor, or the push relay for APNs and FCM), which then takes
/// pushes for its subscription from this Monokulo alone, and registers the
/// subscription it gets with `register-push-target`. Each sale event the
/// target wants is pushed to it, as it is sent on `mk:events`. Only an
/// app's own key (`ak_`) has push targets.
#[api::body]
struct Body {
    /// This Monokulo's VAPID public key (RFC 8292): an uncompressed P-256
    /// point, base64url. Every push it sends is signed with it. Read it
    /// here each time a subscription is made: it is this Monokulo's own,
    /// the same for every app.
    vapid_public_key: String,
    /// The app's push targets, oldest first.
    push_targets: Vec<push_target::Body>,
    app: Link<app::Path>,
}

/// List the app's push targets.
#[get(app, problems = [Internal])]
async fn list(app: ConnectedApp, apps: Apps) -> api::Result<Body> {
    let state = apps.0;
    let app_id = app.0.id;
    let targets = state
        .db
        .read(move |db| db.list_push_targets(&app_id))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "an app's push targets could not be read");
            internal()
        })?;
    Ok(Body {
        vapid_public_key: state.push.vapid_public_key().to_string(),
        push_targets: targets.into_iter().map(push_target::of).collect(),
        app: Link::new(app::Path {}),
    })
}

/// A push subscription to register.
#[api::input]
struct Input {
    /// The app's own name for this subscription. Registering again with a
    /// name the app used before replaces that target's subscription (same
    /// id): register again with it whenever the push service hands out a
    /// new endpoint.
    client_id: ClientId,
    /// How its pushes are sent.
    kind: PushTargetKind,
    /// Where its pushes go: the subscription's endpoint, an `https` URL
    /// (never an onion address: Monokulo doesn't push over Tor) of at most
    /// 2048 characters as Monokulo writes it, with non-ASCII characters
    /// percent-escaped. Plain `http` only to this machine (`127.0.0.1`,
    /// `[::1]`, `localhost`), which Monokulo pushes to only when its admin
    /// allows private addresses.
    endpoint: String,
    /// The subscription's keys, which its pushes are encrypted with: for an
    /// `encrypted` target, and only for one.
    keys: Option<Keys>,
    /// The sale events to push to it; every one when left out.
    events: Option<Vec<SaleKind>>,
    /// For a `live-activity` target, and only for one: whether its pushes
    /// may carry the payment URI (`monero:…`), for the app to show the
    /// payment code in the Dynamic Island. False when left out.
    include_payment_uri: Option<bool>,
}

/// A Web Push subscription's keys, as a browser's `PushSubscription` or a
/// UnifiedPush distributor hands them out.
#[api::input]
struct Keys {
    /// The device's P-256 public key: an uncompressed point (65 bytes),
    /// base64url.
    p256dh: String,
    /// The subscription's authentication secret: 16 bytes, base64url.
    auth: String,
}

/// Register a push target.
///
/// Answers `201 Created` with the target and its `Location`; the same
/// `client_id` again answers the same target with its new subscription.
/// An app may have 8 push targets: past that, `409`
/// `too-many-push-targets`. Monokulo only sends to an endpoint on a public
/// address (unless its admin allowed private ones for webhooks): one on a
/// private address is registered, and its pushes fail.
#[post(
    app,
    form = "register-push-target",
    problems = [InvalidRequest, TooManyPushTargets, Internal]
)]
async fn register(
    app: ConnectedApp,
    apps: Apps,
    input: Input,
) -> api::Result<Created<push_target::Body>> {
    let (app, state) = (app.0, apps.0);
    let refused = |detail: String| Problem::InvalidRequest.detail(detail);
    let endpoint = ::push::Endpoint::parse(&input.endpoint)
        .map_err(|e| refused(format!("`endpoint` isn't one Monokulo can push to: {e}.")))?;
    let kind = match (input.kind, input.keys, input.include_payment_uri) {
        (PushTargetKind::Encrypted, Some(keys), None) => TargetKind::Encrypted {
            p256dh: ::push::P256dh::from_base64url(&keys.p256dh)
                .map_err(|e| refused(format!("`keys.p256dh` isn't a P-256 public key: {e}.")))?,
            auth: ::push::AuthSecret::from_base64url(&keys.auth).map_err(|e| {
                refused(format!("`keys.auth` isn't an authentication secret: {e}."))
            })?,
        },
        (PushTargetKind::Encrypted, None, _) => {
            return Err(refused(
                "An `encrypted` target needs its subscription's `keys`: its pushes are encrypted with them.".into(),
            ))
        }
        (PushTargetKind::Encrypted, Some(_), Some(_)) => {
            return Err(refused(
                "`include_payment_uri` is for a `live-activity` target: an `encrypted` one's pushes carry the whole sale.".into(),
            ))
        }
        (PushTargetKind::LiveActivity, None, include_payment_uri) => TargetKind::LiveActivity {
            include_payment_uri: include_payment_uri.unwrap_or(false),
        },
        (PushTargetKind::LiveActivity, Some(_), _) => {
            return Err(refused(
                "A `live-activity` target takes no `keys`: its pushes go to the relay unencrypted.".into(),
            ))
        }
    };
    let events = input.events.unwrap_or_else(|| SaleKind::ALL.to_vec());
    if events.is_empty() {
        return Err(refused(
            "`events` names no sale event: leave it out for every one, or unregister the target to have none.".into(),
        ));
    }
    let (client_id, app_id) = (input.client_id, app.id.clone());
    let registered = state
        .db
        .write(move |db| {
            db.register_push_target(
                &NewPushTarget {
                    app_id: &app_id,
                    client_id: &client_id,
                    kind: &kind,
                    endpoint: &endpoint,
                    events: &events,
                },
                crate::now_unix(),
            )
        })
        .await;
    match registered {
        Ok(Registration::Registered(target)) => {
            tracing::info!(
                store.id = %app.store_id,
                app.id = %app.id,
                push_target.id = %target.id,
                push_target.kind = target.kind.name().as_str(),
                push_target.host = target.endpoint.host(),
                "an app registered a push target"
            );
            Ok(Created(push_target::of(*target)))
        }
        Ok(Registration::TooMany) => Err(Problem::TooManyPushTargets.detail(format!(
            "This app has {MAX_TARGETS_PER_APP} push targets already. Unregister one, or register again with the `client_id` of one to replace it."
        ))),
        // Disconnected since its key was checked: as if it had been then.
        Ok(Registration::AppGone) => Err(Problem::Unauthorized
            .detail("This app was disconnected: connect it again for a new key.")),
        Err(e) => {
            tracing::error!(app.id = %app.id, error = %e, "a push target could not be registered");
            Err(internal())
        }
    }
}
