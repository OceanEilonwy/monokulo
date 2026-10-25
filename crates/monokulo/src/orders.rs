//! Making an order: the one path the API (`api::v1::orders::create`), the
//! dashboard's form (`http::orders::create_order`) and the till
//! (`http::pos::create_order`) share. Each surface reads its own input and
//! answers in its own shape (a problem, the form again, the till's JSON);
//! everything between is here, once: the currency, the price, the
//! confirmations, the engine's order, its local records, and what an
//! idempotency key means.
//!
//! **Idempotency**: a key names one purchase, as the caller asked for it
//! (the amount as sent, the currency, the reference, the description,
//! whether the reference shows: its fingerprint). The first attempt
//! reserves the key with that fingerprint and the price it was made at
//! (`order_requests`, migration 0047) before it asks the engine, so a retry
//! after a lost answer asks the engine for exactly the same order however
//! the rate moved since, and the engine hands back the first one. The key
//! for anything else is refused before any pricing. Keys are kept apart by
//! where they came from ([`IdempotencyScope`]).

use shared::xmr_amount::Piconero;

use crate::api::runtime::problem::{internal, Problem, ProblemDetails};
use crate::api::runtime::scalars::IdempotencyKey;
use crate::confirmation_thresholds::{PolicyGuard, ResolveError};
use crate::currencies::KnownCurrency;
use crate::db::{
    AppOrder, ConnectionId, Db, NewOrderRecord, OrderId, OrderPrice, Reserved, StoreConnectionRow,
};
use crate::engine_client::{
    EngineClientError, ENGINE_MAX_IDEMPOTENCY_KEY_CHARS, MAX_CALLER_IDEMPOTENCY_KEY_CHARS,
};
use crate::exchange_rate_config::{ExchangeRateLookupError, RatesUnavailable};
use crate::http::store_key::Credential;
use crate::http::AppState;

/// Where an order's idempotency key came from. The engine keeps one set of
/// keys per store, so each source's keys carry their own prefix there and
/// never name another source's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdempotencyScope {
    /// A browser page, with the store's public key. Anyone can send that
    /// key, so its keys never name an order the store's server or apps
    /// made.
    Website,
    /// The store's server or one of its apps: a key that proves the caller
    /// is the store.
    Server,
    /// The dashboard's form.
    Dashboard,
    /// The till.
    Pos,
}

impl IdempotencyScope {
    pub const ALL: [IdempotencyScope; 4] = [
        IdempotencyScope::Website,
        IdempotencyScope::Server,
        IdempotencyScope::Dashboard,
        IdempotencyScope::Pos,
    ];

    /// The prefix its keys have on the engine.
    pub const fn prefix(self) -> &'static str {
        match self {
            IdempotencyScope::Website => "pay-pk:",
            IdempotencyScope::Server => "pay:",
            IdempotencyScope::Dashboard => "dash:",
            IdempotencyScope::Pos => "pos:",
        }
    }
}

/// The longest [`IdempotencyScope::prefix`].
const LONGEST_PREFIX: usize = {
    let (mut longest, mut i) = (0, 0);
    while i < IdempotencyScope::ALL.len() {
        let length = IdempotencyScope::ALL[i].prefix().len();
        if length > longest {
            longest = length;
        }
        i += 1;
    }
    longest
};

// A caller's longest key, with the longest prefix, is still one the engine
// takes.
const _: () =
    assert!(MAX_CALLER_IDEMPOTENCY_KEY_CHARS + LONGEST_PREFIX <= ENGINE_MAX_IDEMPOTENCY_KEY_CHARS);

/// Who is making an order: what its record says made it, and its key's
/// scope.
pub enum Origin {
    /// `POST /api/v1/orders`.
    Api(Box<ApiCaller>),
    /// The dashboard's form: the merchant's own signed-in session.
    Dashboard,
    /// The till: the merchant's own signed-in session.
    Pos,
}

/// Who called `POST /api/v1/orders`: with `credential`, from the client
/// that named itself in `Monokulo-Client` (its name and version), if one
/// did.
pub struct ApiCaller {
    pub credential: Credential,
    pub client: Option<(String, String)>,
}

impl Origin {
    fn scope(&self) -> IdempotencyScope {
        match self {
            Origin::Api(api) if api.credential == Credential::Public => IdempotencyScope::Website,
            Origin::Api(_) => IdempotencyScope::Server,
            Origin::Dashboard => IdempotencyScope::Dashboard,
            Origin::Pos => IdempotencyScope::Pos,
        }
    }

    /// Whether whoever made the order proved it is the store: its key, an
    /// app's, or the merchant's session.
    fn created_with_key(&self) -> bool {
        match self {
            Origin::Api(api) => api.credential.is_store_key(),
            Origin::Dashboard | Origin::Pos => true,
        }
    }

    /// The app that made it, if one did.
    fn app(&self) -> Option<&crate::db::AppConnection> {
        match self {
            Origin::Api(api) => match &api.credential {
                Credential::App(app) => Some(app),
                _ => None,
            },
            Origin::Dashboard | Origin::Pos => None,
        }
    }
}

/// An order to make, as the caller asked for it.
pub struct OrderRequest {
    /// The price, in `currency`, as sent.
    pub amount: String,
    /// The currency as sent: a code in any case, or one of its tickers.
    pub currency: String,
    pub merchant_order_id: Option<String>,
    /// What the order is for, already checked
    /// (`crate::stores::clean_description`).
    pub description: Option<String>,
    pub show_merchant_order_id: bool,
    pub idempotency_key: Option<IdempotencyKey>,
}

/// What making an order came to.
pub enum Made {
    /// Made, and recorded, now.
    Created(Box<NewOrder>),
    /// Made before, by an earlier attempt with the same idempotency key.
    Found(OrderId),
}

/// An order just made, as the engine made it and monokulo recorded it.
pub struct NewOrder {
    pub order_id: OrderId,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub expires_at: i64,
    /// When it was priced, and so made.
    pub created_at: i64,
    pub amount: String,
    pub currency: KnownCurrency,
    pub merchant_order_id: Option<String>,
    pub description: Option<String>,
    pub show_merchant_order_id: bool,
    pub created_with_key: bool,
    pub price: OrderPrice,
}

impl NewOrder {
    /// Its local record, as `Db::order_currency_metadata_for` reads it.
    pub fn metadata(&self, store: &ConnectionId) -> crate::db::OrderCurrencyMetadataRow {
        crate::db::OrderCurrencyMetadataRow {
            connection_id: store.clone(),
            order_id: self.order_id.clone(),
            currency: self.currency.to_string(),
            amount: self.amount.clone(),
            piconero_per_unit: self.price.piconero_per_unit,
            provider: self.price.provider.clone(),
            created_at: self.created_at,
            store_base_currency: Some(self.price.base_currency.clone()),
            base_currency_piconero_per_unit: self.price.base_currency_piconero_per_unit,
            confirmations_required_applied: Some(self.price.confirmations_required),
            created_with_key: self.created_with_key,
            show_merchant_order_id: self.show_merchant_order_id,
        }
    }
}

/// Why an order wasn't made. Each surface answers it in its own way.
#[derive(Debug)]
pub enum MakeError {
    /// The store is gone.
    NoStore,
    /// Not a currency at all (as sent).
    UnknownCurrency(String),
    /// A real currency none of this store's rate providers prices here.
    UnsupportedCurrency(KnownCurrency),
    /// The rate providers are down or rate limited: worth retrying.
    RatesUnavailable(RatesUnavailable),
    /// The amount isn't one the currency can have.
    InvalidAmount(String),
    /// The confirmations it needs couldn't be decided.
    Threshold(ResolveError),
    /// The idempotency key was used for another purchase.
    IdempotencyConflict,
    /// The engine refused the order (an unconfigured network), as it said.
    EngineRejected(String),
    EngineUnavailable,
    Internal,
}

impl MakeError {
    /// What a person is told.
    pub fn message(&self) -> String {
        match self {
            MakeError::NoStore => "There is no such store.".to_owned(),
            MakeError::UnknownCurrency(currency) => format!("unknown currency: {currency}"),
            MakeError::UnsupportedCurrency(currency) => {
                format!("unsupported currency: {currency}")
            }
            MakeError::RatesUnavailable(unavailable) => unavailable.message(),
            MakeError::InvalidAmount(message) | MakeError::EngineRejected(message) => {
                message.clone()
            }
            MakeError::Threshold(e) => e.message(),
            MakeError::IdempotencyConflict => {
                "This idempotency key was already used for a different order.".to_owned()
            }
            MakeError::EngineUnavailable => {
                "The payment engine is busy or unreachable. Try again in a moment.".to_owned()
            }
            MakeError::Internal => "Something went wrong. Please try again.".to_owned(),
        }
    }

    /// As a problem, for the API and the till.
    pub fn problem(&self) -> ProblemDetails {
        match self {
            MakeError::NoStore => crate::http::ApiError::NotFound.into(),
            MakeError::UnknownCurrency(_)
            | MakeError::UnsupportedCurrency(_)
            | MakeError::InvalidAmount(_) => Problem::InvalidAmount.detail(self.message()),
            MakeError::RatesUnavailable(unavailable) => unavailable.problem(),
            MakeError::Threshold(e) => e.problem(),
            MakeError::IdempotencyConflict => Problem::IdempotencyConflict.detail(self.message()),
            MakeError::EngineRejected(message) => Problem::InvalidRequest.detail(message.clone()),
            MakeError::EngineUnavailable => crate::http::ApiError::EngineUnavailable.into(),
            MakeError::Internal => internal(),
        }
    }
}

/// Makes the order `request` asks `store` for, or finds the one an earlier
/// attempt with its idempotency key made. Holds the store's policy lock
/// throughout, so the order is priced against a whole policy
/// (`confirmation_thresholds::lock_policy`).
pub async fn make(
    state: &AppState,
    store: &StoreConnectionRow,
    origin: Origin,
    request: OrderRequest,
) -> Result<Made, MakeError> {
    let policy = crate::confirmation_thresholds::lock_policy(&store.tenant_public_key).await;
    // The store as it is now the lock is held: a policy edit that finished
    // while this waited is priced in.
    let id = store.id.clone();
    let row = state
        .db
        .read(move |db| db.get_store_connection_by_id(&id))
        .await
        .map_err(|_| MakeError::Internal)?
        .ok_or(MakeError::NoStore)?;
    let sk =
        crate::http::decrypt_sk(&state.encryption_key, &row).map_err(|()| MakeError::Internal)?;

    // "Unknown currency" (it doesn't exist at all) is a clearer error than
    // "unsupported currency" (a real currency this store can't price right
    // now), so they're told apart (`crate::currencies`).
    let asked = request.currency.clone();
    let currency = state
        .db
        .read(move |db| KnownCurrency::resolve(db, &asked))
        .await
        .map_err(|_| MakeError::Internal)?
        .ok_or_else(|| MakeError::UnknownCurrency(request.currency.trim().to_owned()))?;
    let amount = request.amount.trim().to_owned();
    let fingerprint = fingerprint(&request, &amount, &currency);
    let key = request
        .idempotency_key
        .as_ref()
        .map(|key| format!("{}{key}", origin.scope().prefix()));

    let reserved = match &key {
        Some(key) => {
            let (id, key, fingerprint) = (row.id.clone(), key.clone(), fingerprint.clone());
            state
                .db
                .read(move |db| db.order_request(&id, &key, &fingerprint))
                .await
                .map_err(|_| MakeError::Internal)?
        }
        None => None,
    };
    let (price, at) = match reserved {
        Some(Reserved::Priced { price, at }) => (price, at),
        Some(Reserved::Made(order_id)) => return found(state, &row, origin, order_id).await,
        Some(Reserved::Conflict) => return Err(MakeError::IdempotencyConflict),
        None => {
            let price = price_of(state, &row, &policy, &sk, &currency, &amount).await?;
            let at = crate::now_unix();
            match &key {
                None => (price, at),
                Some(key) => {
                    let (id, key, fingerprint) = (row.id.clone(), key.clone(), fingerprint.clone());
                    let reserved = state
                        .db
                        .write(move |db| {
                            db.reserve_order_request(&id, &key, &fingerprint, &price, at)
                        })
                        .await
                        .map_err(|e| {
                            tracing::error!(store.id = %row.id, error = %e, "an order's idempotency key could not be reserved");
                            MakeError::Internal
                        })?;
                    match reserved {
                        Reserved::Priced { price, at } => (price, at),
                        Reserved::Made(order_id) => {
                            return found(state, &row, origin, order_id).await
                        }
                        Reserved::Conflict => return Err(MakeError::IdempotencyConflict),
                    }
                }
            }
        }
    };

    let made = match state
        .engine
        .client
        .create_order(
            &sk,
            Piconero(price.xmr_amount_piconero),
            request.merchant_order_id.clone(),
            request.description.clone(),
            Some(price.confirmations_required),
            key.clone(),
        )
        .await
    {
        Ok(made) => made,
        // The engine's own validation (an unconfigured network), as it is;
        // or the key was used on the engine for another order before
        // monokulo kept its keys. Either way nothing was made, so the next
        // attempt prices afresh.
        Err(EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::BAD_REQUEST
                || status == reqwest::StatusCode::CONFLICT =>
        {
            if let Some(key) = &key {
                let (id, key) = (row.id.clone(), key.clone());
                if let Err(e) = state
                    .db
                    .write(move |db| db.release_order_request(&id, &key))
                    .await
                {
                    tracing::warn!(store.id = %row.id, error = %e, "a refused order's idempotency key could not be released");
                }
            }
            return Err(if status == reqwest::StatusCode::CONFLICT {
                MakeError::IdempotencyConflict
            } else {
                MakeError::EngineRejected(message)
            });
        }
        // The engine may still have made it: the key stays reserved at this
        // price, so a retry gets that order.
        Err(e) if e.is_unavailable() => {
            tracing::warn!(store.id = %row.id, error = %e, "the engine couldn't make the order");
            return Err(MakeError::EngineUnavailable);
        }
        Err(e) => {
            tracing::error!(store.id = %row.id, error = %e, "the engine's answer to making an order wasn't understood");
            return Err(MakeError::Internal);
        }
    };

    let order = NewOrder {
        order_id: made.order_id,
        address: made.address,
        xmr_amount_piconero: made.xmr_amount_piconero,
        expires_at: made.expires_at,
        created_at: at,
        amount,
        currency,
        merchant_order_id: request.merchant_order_id,
        description: request.description,
        show_merchant_order_id: request.show_merchant_order_id,
        created_with_key: origin.created_with_key(),
        price,
    };
    // The order's local record (its price, what made it, and how its
    // checkout finds its store), its key's order, and the surface's own
    // record, in one transaction: without the record the checkout can't
    // find its store, so a key never names an unrecorded order.
    let (id, recorded_order) = (row.id.clone(), order_record(&order));
    let recorded = state
        .db
        .write(move |db| {
            db.in_transaction(|db| record(db, &id, &recorded_order, key.as_deref(), &origin))
        })
        .await;
    match recorded {
        Ok(Recorded::Now(app_order)) => {
            if let Some(app_order) = app_order {
                state.apps.announce(
                    &row.tenant_public_key,
                    crate::apps::SaleEvent::waiting(&app_order),
                );
            }
            Ok(Made::Created(Box::new(order)))
        }
        Ok(Recorded::Before) => Ok(Made::Found(order.order_id)),
        Err(e) => {
            tracing::error!(
                order.id = %order.order_id,
                store.id = %row.id,
                error = %e,
                "failed to record an order's local record - the order exists on the engine, but monokulo \
                 can't show its price or find its checkout"
            );
            // Without its record the order's checkout can't find its
            // store, so the order isn't handed out. A retry with the same
            // idempotency key gets the same engine order and records it
            // then; without one the unrecorded order just expires.
            Err(MakeError::Internal)
        }
    }
}

/// What a key, and the purchase it was asked for, are: the amount as sent,
/// the currency, the reference, the description and whether the reference
/// shows.
fn fingerprint(request: &OrderRequest, amount: &str, currency: &KnownCurrency) -> String {
    use sha2::{Digest, Sha256};
    let asked = serde_json::to_vec(&(
        amount,
        currency.as_str(),
        &request.merchant_order_id,
        &request.description,
        request.show_merchant_order_id,
    ))
    .expect("strings serialize");
    hex::encode(Sha256::digest(asked))
}

/// The price of `amount` of `currency` for `row`: the store's own rate
/// providers (`XMR` always uses the identity rate), the XMR it comes to,
/// and the confirmations an order of that size needs.
async fn price_of(
    state: &AppState,
    row: &StoreConnectionRow,
    policy: &PolicyGuard,
    sk: &shared::auth::RawToken,
    currency: &KnownCurrency,
    amount: &str,
) -> Result<OrderPrice, MakeError> {
    let (piconero_per_unit, provider) = match state
        .exchange_rate
        .piconero_per_unit_for(row, currency)
        .await
    {
        Ok(Some(rate)) => rate,
        // Not a failure: none of this store's providers (or no provider at
        // all) can price this currency on this instance.
        Ok(None) | Err(ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            return Err(MakeError::UnsupportedCurrency(currency.clone()))
        }
        Err(e) => {
            tracing::warn!(store.id = %row.id, currency = %currency, error = %e, "no usable exchange rate right now");
            return Err(e
                .unavailable(currency.as_str())
                .map_or(MakeError::Internal, MakeError::RatesUnavailable));
        }
    };
    let xmr_amount_piconero =
        shared::exchange_rate::compute_order_amount(currency.as_str(), amount, piconero_per_unit)
            .map_err(|e| MakeError::InvalidAmount(e.to_string()))?;
    let resolution = crate::confirmation_thresholds::resolve_for_order(
        state,
        row,
        policy,
        sk,
        currency,
        piconero_per_unit,
        xmr_amount_piconero,
    )
    .await
    .map_err(MakeError::Threshold)?;
    Ok(OrderPrice {
        xmr_amount_piconero,
        piconero_per_unit: Piconero(piconero_per_unit),
        provider: provider.to_owned(),
        base_currency: resolution.base_currency,
        base_currency_piconero_per_unit: resolution.base_currency_piconero_per_unit,
        confirmations_required: resolution.confirmations_required,
    })
}

/// The part of a [`NewOrder`] its records are written from.
struct OrderRecord {
    order_id: OrderId,
    address: String,
    xmr_amount_piconero: u64,
    expires_at: i64,
    created_at: i64,
    amount: String,
    currency: String,
    merchant_order_id: Option<String>,
    show_merchant_order_id: bool,
    created_with_key: bool,
    price: OrderPrice,
}

fn order_record(order: &NewOrder) -> OrderRecord {
    OrderRecord {
        order_id: order.order_id.clone(),
        address: order.address.clone(),
        xmr_amount_piconero: order.xmr_amount_piconero,
        expires_at: order.expires_at,
        created_at: order.created_at,
        amount: order.amount.clone(),
        currency: order.currency.to_string(),
        merchant_order_id: order.merchant_order_id.clone(),
        show_merchant_order_id: order.show_merchant_order_id,
        created_with_key: order.created_with_key,
        price: order.price.clone(),
    }
}

/// Whether [`record`] recorded the order.
enum Recorded {
    /// Now: an app's order is announced to the store's streams.
    Now(Option<Box<AppOrder>>),
    /// Already, by an earlier attempt that got the same order.
    Before,
}

/// Writes an order's records, inside one transaction ([`make`]).
fn record(
    db: &Db,
    store: &ConnectionId,
    order: &OrderRecord,
    key: Option<&str>,
    origin: &Origin,
) -> Result<Recorded, crate::db::DbError> {
    let (source, plugin) = source_of(db, store, origin);
    seen(db, store, origin, &source, plugin, crate::now_unix())?;
    if let Some(key) = key {
        if !db.order_request_made(store, key, &order.order_id)? {
            return Ok(Recorded::Before);
        }
    }
    let price = &order.price;
    let recorded = db.create_order_currency_metadata(&NewOrderRecord {
        connection_id: store,
        order_id: &order.order_id,
        currency: &order.currency,
        amount: &order.amount,
        piconero_per_unit: price.piconero_per_unit,
        provider: &price.provider,
        created_at: order.created_at,
        base_currency: &price.base_currency,
        base_currency_piconero_per_unit: price.base_currency_piconero_per_unit,
        confirmations_required_applied: price.confirmations_required,
        created_with_key: order.created_with_key,
        source: Some(&source),
        show_merchant_order_id: order.show_merchant_order_id,
    })?;
    // An order recorded before monokulo kept its keys, got back again.
    if !recorded {
        return Ok(Recorded::Before);
    }
    match (origin, origin.app()) {
        (_, Some(app)) => {
            let app_order = AppOrder {
                order_id: order.order_id.clone(),
                store_id: store.clone(),
                app_id: app.id.clone(),
                device_name: app.device_name.clone(),
                amount: order.amount.clone(),
                currency: order.currency.clone(),
                address: order.address.clone(),
                xmr_amount_piconero: order.xmr_amount_piconero,
                merchant_order_id: order.merchant_order_id.clone(),
                expires_at: order.expires_at,
                created_at: order.created_at,
            };
            db.record_app_order(&app_order)?;
            Ok(Recorded::Now(Some(Box::new(app_order))))
        }
        (Origin::Pos, None) => {
            db.insert_pos_order(
                store,
                &order.order_id,
                order.merchant_order_id.as_deref(),
                order.created_at,
            )?;
            Ok(Recorded::Now(None))
        }
        (Origin::Api(_) | Origin::Dashboard, None) => Ok(Recorded::Now(None)),
    }
}

/// An order an earlier attempt with the same key made and recorded: the
/// caller is seen, as on any order it makes.
async fn found(
    state: &AppState,
    row: &StoreConnectionRow,
    origin: Origin,
    order_id: OrderId,
) -> Result<Made, MakeError> {
    if let Origin::Api(_) = origin {
        let id = row.id.clone();
        let noted = state
            .db
            .write(move |db| {
                let (source, plugin) = source_of(db, &id, &origin);
                seen(db, &id, &origin, &source, plugin, crate::now_unix())
            })
            .await;
        if let Err(e) = noted {
            tracing::warn!(store.id = %row.id, error = %e, "a retried order's caller could not be noted as seen");
        }
    }
    Ok(Made::Found(order_id))
}

/// What made an order, as its record names it, and whether that is the
/// store's connected plugin.
fn source_of(db: &Db, store: &ConnectionId, origin: &Origin) -> (String, bool) {
    match origin {
        Origin::Dashboard => ("dashboard".to_owned(), false),
        Origin::Pos => ("pos".to_owned(), false),
        Origin::Api(api) => {
            // An app's order is never a plugin's.
            if let Credential::App(app) = &api.credential {
                return (app.slug.clone(), false);
            }
            // Only a keyed order that names its client can be a plugin's.
            let created_with_key = api.credential.is_store_key();
            let client = api
                .client
                .as_ref()
                .filter(|_| created_with_key)
                .map(|(name, _)| name.as_str());
            // A failed lookup costs the order its plugin, never its record.
            let plugin = match client {
                Some(_) => db
                    .active_integration_kind(store)
                    .inspect_err(
                        |e| tracing::warn!(error = %e, "the store's plugin could not be read"),
                    )
                    .ok()
                    .flatten(),
                None => None,
            };
            let source = order_source(created_with_key, client, plugin.as_deref());
            let is_plugin = plugin.as_deref() == Some(source.as_str());
            (source, is_plugin)
        }
    }
}

/// Notes the app, or the store's plugin, that made an order as seen now,
/// with the version it named.
fn seen(
    db: &Db,
    store: &ConnectionId,
    origin: &Origin,
    source: &str,
    plugin: bool,
    at: i64,
) -> Result<(), crate::db::DbError> {
    let Origin::Api(api) = origin else {
        return Ok(());
    };
    let version = |name: Option<&str>| match (&api.client, name) {
        (Some((client, version)), Some(name)) if client.eq_ignore_ascii_case(name) => {
            version.as_str()
        }
        (Some((_, version)), None) => version.as_str(),
        _ => "",
    };
    match &api.credential {
        Credential::App(app) => db.app_seen(&app.id, version(Some(&app.slug)), at),
        _ if plugin => db.integration_seen(store, source, version(None), at),
        _ => Ok(()),
    }
}

/// Where an order made through `POST /api/v1/orders` came from, for its
/// page and the orders table: the store's connected plugin (its slug) when
/// it was made with the store's key by the client that plugin connected as
/// (`Monokulo-Client: <slug>/...`, any case), `api` for anything else with
/// the key, or `website` (a browser, with the public key). `plugin` is the
/// slug of the store's active integration, if it has one.
pub(crate) fn order_source(
    created_with_key: bool,
    client: Option<&str>,
    plugin: Option<&str>,
) -> String {
    match (created_with_key, client, plugin) {
        (true, Some(client), Some(plugin)) if client.eq_ignore_ascii_case(plugin) => {
            plugin.to_owned()
        }
        (true, _, _) => "api".to_owned(),
        (false, _, _) => "website".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::order_source;

    /// A keyed order is the store's plugin's only when the client names
    /// that plugin, in any case; no plugin, another client or none is the
    /// API; without the key it's the website, whatever it claims.
    #[test]
    fn an_orders_source_is_the_connected_plugin_only_when_it_names_itself() {
        assert_eq!(
            order_source(true, Some("Acme-Cart"), Some("acme-cart")),
            "acme-cart"
        );
        assert_eq!(
            order_source(true, Some("opencart"), Some("acme-cart")),
            "api"
        );
        assert_eq!(order_source(true, Some("acme-cart"), None), "api");
        assert_eq!(order_source(true, None, Some("acme-cart")), "api");
        assert_eq!(
            order_source(false, Some("acme-cart"), Some("acme-cart")),
            "website"
        );
    }
}
