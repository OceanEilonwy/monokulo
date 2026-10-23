use super::store;
use crate::api::{self, prelude::*};

api::resource!("/api/v1/app", rel = "app");

/// The connected app the key belongs to.
///
/// Only an app's own key (`ak_`) has one. The app shows it in its
/// settings, and disconnects itself with the `disconnect` form.
#[api::body]
struct Body {
    /// The app's connection id.
    app_id: String,
    /// What the merchant (or, when they gave none, the app) calls the
    /// device.
    device_name: String,
    /// What the app said it runs on (`android`, `ios`); empty when it
    /// didn't say.
    platform: String,
    /// When it connected, in Unix seconds.
    connected_at: i64,
    store: Link<store::Path>,
}

/// Read the app's own connection.
#[get(app)]
async fn get(app: ConnectedApp) -> api::Result<Body> {
    let app = app.0;
    Ok(Body {
        app_id: app.id,
        device_name: app.device_name,
        platform: app.platform,
        connected_at: app.connected_at,
        store: Link::new(store::Path {}),
    })
}

/// Disconnect this app.
///
/// Its key stops working at once, and its event stream ends. Its orders
/// stay, and can still be paid.
#[delete(app, form = "disconnect")]
async fn delete(app: ConnectedApp, apps: Apps) -> api::Result<NoContent> {
    let (app, state) = (app.0, apps.0);
    let (store_id, app_id) = (app.store_id.clone(), app.id.clone());
    match state
        .db
        .write(move |db| db.delete_app(&store_id, &app_id))
        .await
    {
        Ok(_) => {
            state.apps.disconnected(&app.id);
            tracing::info!(store.id = %app.store_id, app.id = %app.id, "an app disconnected itself");
            Ok(NoContent)
        }
        Err(e) => {
            tracing::error!(app.id = %app.id, error = %e, "an app could not disconnect itself");
            Err(internal())
        }
    }
}
