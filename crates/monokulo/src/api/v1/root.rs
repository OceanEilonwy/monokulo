use super::{app, events, logs, openapi, orders, store};
use crate::api::{self, prelude::*};

api::resource!("/api/v1", rel = "root");

/// The API's entry: where to go from here.
///
/// Links to the store, its orders and its events (and, for a connected
/// app, the app itself), the forms the key may use (a shop's server also
/// gets the one for sending its warnings and errors), and the API's
/// description. A link or form the key may not use is left out, so a
/// client can find out what its key does by reading this. While this
/// Monokulo has an onion address, `mk:onion-api` says where the same entry
/// is on it.
#[api::body]
struct Body {
    /// This entry on this Monokulo's onion address.
    ///
    /// An absolute URL, `http://….onion/api/v1`, present while the Monokulo
    /// has an onion address, whichever address the entry was read at;
    /// absent otherwise.
    ///
    /// A client that wants to reach Monokulo over Tor starts here, and
    /// every link it follows from there is relative, so it stays on Tor.
    /// The embed library (`monokulo-client.js`) on a page served from a
    /// `.onion` address reads it to make orders and show the checkout over
    /// Tor. The plugin and app handshakes give the same URL as `onion_api`.
    onion_api: Option<OnionLink<Path>>,
    store: Link<store::Path>,
    orders: Link<orders::Path>,
    events: Link<events::Path>,
    app: Link<app::Path>,
    service_desc: Link<openapi::Path>,
    service_doc: Href,
    create_order: Form<orders::create>,
    send_logs: Form<logs::send>,
    disconnect: Form<app::delete>,
}

/// Read the entry. Open without a key, so a client can check it found a
/// Monokulo API; with one, it shows what that key may do.
#[get(anyone)]
async fn get(instance: Instance) -> api::Result<Body> {
    Ok(Body {
        onion_api: OnionLink::new(instance.0.onion_uri().as_deref(), Path {}),
        store: Link::new(store::Path {}),
        orders: Link::new(orders::Path {}),
        events: Link::new(events::Path {}),
        app: Link::new(app::Path {}),
        service_desc: Link::new(openapi::Path {}),
        service_doc: Href {
            href: format!("{}/api/", api::runtime::DOCS),
            media_type: "text/html",
        },
        create_order: Form::new(),
        send_logs: Form::new(),
        disconnect: Form::new(),
    })
}
