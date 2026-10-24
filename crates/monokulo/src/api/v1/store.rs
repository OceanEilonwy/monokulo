use super::{app, events, orders};
use crate::api::{self, prelude::*};

api::resource!("/api/v1/store", rel = "store");

/// The store the key belongs to, and where its payments go.
///
/// Read it each time it's shown: the merchant can rename the store or move
/// it to another wallet at any time.
#[api::body]
struct Body {
    /// What the merchant calls the store.
    name: String,
    /// The currency the store's confirmation rules are counted in (`EUR`).
    base_currency: String,
    /// The wallet the store's payments go to; left out for a store without
    /// one.
    wallet: Option<Wallet>,
    orders: Link<orders::Path>,
    events: Link<events::Path>,
    app: Link<app::Path>,
    create_order: Form<orders::create>,
}

/// A wallet payments go to.
#[api::object]
struct Wallet {
    /// What the merchant calls the wallet.
    name: String,
    /// `mainnet`, `stagenet` or `testnet`.
    network: String,
    /// Its primary address, in full. Shorten it for display yourself.
    address: String,
}

/// Read the store and its wallet, as they are now.
#[get(server, problems = [Unauthorized, Internal])]
async fn get(store: Store, instance: Instance) -> api::Result<Body> {
    let (id, state) = (store.id.clone(), instance.0);
    let found = state
        .db
        .read(move |db| {
            let Some(store) = db.get_store_connection_by_id(&id)? else {
                return Ok(None);
            };
            let wallet = match &store.wallet_id {
                Some(wallet_id) => db.get_wallet(&store.user_id, wallet_id)?,
                None => None,
            };
            Ok::<_, crate::db::DbError>(Some((store, wallet)))
        })
        .await;
    let (store, wallet) = match found {
        Ok(Some(found)) => found,
        Ok(None) => return Err(Problem::Unauthorized.detail("This store no longer exists.")),
        Err(_) => return Err(internal()),
    };
    Ok(Body {
        name: store.name,
        base_currency: store.base_currency,
        wallet: wallet.map(|wallet| Wallet {
            name: wallet.name,
            network: wallet.network,
            address: wallet.primary_address,
        }),
        orders: Link::new(orders::Path {}),
        events: Link::new(events::Path {}),
        app: Link::new(app::Path {}),
        create_order: Form::new(),
    })
}
