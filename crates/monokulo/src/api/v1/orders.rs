use super::order;
use crate::api::{self, prelude::*};
use crate::http::store_key::Credential;
use crate::now_unix;

api::resource!("/api/v1/orders", rel = "orders");

/// The store's orders, newest first, a page at a time.
///
/// `next` is the next page, while there may be one. Make an order with the
/// `create-order` form.
#[api::body]
struct Body {
    /// This page's orders.
    orders: Vec<order::Body>,
    next: Option<Link<Path>>,
}

/// The most orders one page holds.
pub const MAX_PAGE: u32 = 200;
const DEFAULT_PAGE: u32 = 50;

/// List the store's orders, newest first.
#[get(server)]
async fn list(orders: Orders, page: Page) -> api::Result<Body> {
    let limit = page.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let offset = page.offset.unwrap_or(0);
    let state = &orders.state;
    let onion = state.onion_uri();
    let store = &orders.store;
    let views = match state
        .engine
        .client
        .list_orders_page(&orders.sk, false, None, limit, offset)
        .await
    {
        Ok(views) => views,
        Err(e) => {
            tracing::warn!(store.id = %store.id, error = %e, "the store's orders could not be read for the API");
            return Err(crate::http::ApiError::EngineUnavailable.into());
        }
    };
    let (id, ids) = (
        store.id.clone(),
        views.iter().map(|o| o.order_id.clone()).collect::<Vec<_>>(),
    );
    let metadata = state
        .db
        .read(move |db| db.order_currency_metadata_for(&id, &ids))
        .await
        .unwrap_or_default();
    // An order without its own snapshot uses the store's present
    // threshold, read once for the page and only when one needs it.
    let fallback = if views.iter().any(|view| {
        metadata
            .get(&view.order_id)
            .and_then(|m| m.confirmations_required_applied)
            .is_none()
    }) {
        state
            .engine
            .client
            .get_tenant(&orders.sk)
            .await
            .map(|t| t.confirmations_required)
            .unwrap_or(crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS)
    } else {
        crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS
    };
    let full = u32::try_from(views.len()).unwrap_or(u32::MAX) >= limit;
    Ok(Body {
        orders: views
            .iter()
            .map(|view| {
                let metadata = metadata.get(&view.order_id);
                let required = metadata
                    .and_then(|m| m.confirmations_required_applied)
                    .unwrap_or(fallback);
                order::of(&store.id, view, metadata, required, onion.as_deref())
            })
            .collect(),
        next: full.then(|| {
            Link::with_query(
                Path {},
                &crate::api::prelude::Page {
                    limit: Some(limit),
                    offset: Some(offset.saturating_add(limit)),
                },
            )
        }),
    })
}

/// An order to make.
#[api::input]
struct Input {
    /// The price, in `currency`.
    amount: Amount,
    /// The currency of `amount`.
    currency: CurrencyCode,
    /// The shop's own reference for the order (its order or cart number).
    merchant_order_id: Option<MerchantRef>,
    /// What the order is for, shown to the payer under the store's name.
    /// Without one the checkout shows the store's payment description.
    description: Option<Description>,
    /// Whether the checkout shows the payer `merchant_order_id`; true when
    /// left out. A shop whose reference means nothing to its customers
    /// sends false.
    show_merchant_order_id: Option<bool>,
    /// The caller's key for this one purchase: a retry with the same key
    /// gets the order the first attempt made, never a second one.
    idempotency_key: Option<IdempotencyKey>,
}

/// Make an order.
///
/// With the store's secret key or an app's (a shop's server, a till) it is
/// recorded as made with the key; with the public key it's the website's,
/// and a store that takes orders only from its verified websites refuses it
/// from anywhere else. A shop plugin names itself in `Monokulo-Client:
/// <slug>/<version>`. Answers `201 Created` with the order and its
/// `Location`.
#[post(any_key, form = "create-order")]
async fn create(
    caller: Caller,
    orders: Orders,
    client: Client,
    input: Input,
) -> api::Result<Created<order::Body>> {
    let state = orders.state.clone();
    let onion = state.onion_uri();
    let row = orders.store.clone();
    let caller = caller.0;
    let created_with_key = caller.credential.is_store_key();
    let app = match &caller.credential {
        Credential::App(app) => Some(app.clone()),
        _ => None,
    };
    let amount = input.amount.into_string();
    let currency = input.currency.into_string();
    let merchant_order_id = input.merchant_order_id.map(MerchantRef::into_string);
    let description = input.description.map(Description::into_string);
    let show_merchant_order_id = input.show_merchant_order_id.unwrap_or(true);
    let pk = row.tenant_public_key.clone();
    let policy = crate::confirmation_thresholds::lock_policy(&pk).await;
    let asked = currency.clone();
    let currency_known = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>(crate::currencies::is_known_currency(db, &asked))
        })
        .await
        .map_err(|_| internal())?;

    // "Unknown currency" (it doesn't exist at all) is a clearer error than
    // "unsupported currency" (a real currency this instance can't price
    // right now), so they're told apart (`crate::currencies`).
    match currency_known {
        Ok(true) => {}
        Ok(false) => {
            return Err(Problem::InvalidAmount.detail(format!("unknown currency: {currency}")))
        }
        Err(_) => return Err(internal()),
    }
    // `XMR` always uses the identity rate; any other currency is priced by
    // *this store's own* chosen providers.
    let unsupported = || Problem::InvalidAmount.detail(format!("unsupported currency: {currency}"));
    let (piconero_per_unit, provider) = match state
        .exchange_rate
        .piconero_per_unit_for(&row, &currency)
        .await
    {
        Ok(Some(result)) => result,
        Ok(None) => return Err(unsupported()),
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            return Err(unsupported())
        }
        Err(e) => {
            tracing::warn!(store.id = %row.id, currency = ?currency, error = %e, "no usable exchange rate right now");
            return Err(e
                .unavailable(&currency)
                .map_or_else(internal, |unavailable| unavailable.problem()));
        }
    };
    let xmr_amount_piconero =
        shared::exchange_rate::compute_order_amount(&currency, &amount, piconero_per_unit)
            .map_err(|e| Problem::InvalidAmount.detail(e.to_string()))?;
    let resolution = crate::confirmation_thresholds::resolve_for_order(
        &state,
        &row,
        &policy,
        &orders.sk,
        &currency,
        piconero_per_unit,
        xmr_amount_piconero,
    )
    .await
    .map_err(|e| e.problem())?;

    let made = match state
        .engine
        .client
        .create_order(
            &orders.sk,
            shared::xmr_amount::Piconero(xmr_amount_piconero),
            merchant_order_id.clone(),
            description.clone(),
            Some(resolution.confirmations_required),
            input
                .idempotency_key
                .as_ref()
                .map(|key| format!("pay:{key}")),
        )
        .await
    {
        Ok(made) => made,
        // The engine's own validation (an unconfigured network), as it is.
        Err(crate::engine_client::EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::BAD_REQUEST =>
        {
            return Err(Problem::InvalidRequest.detail(message))
        }
        // The key was already used for a different purchase.
        Err(crate::engine_client::EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::CONFLICT =>
        {
            return Err(Problem::IdempotencyConflict.detail(message))
        }
        Err(_) => return Err(internal()),
    };

    // The order's local record: its price, where it came from, and how its
    // checkout finds its store. Only a keyed order that names its client
    // can be a plugin's.
    let (client_name, version) = client.0.filter(|_| created_with_key).unzip();
    let app_order = app.as_ref().map(|app| crate::db::AppOrder {
        order_id: made.order_id.clone(),
        store_id: row.id.clone(),
        app_id: app.id.clone(),
        device_name: app.device_name.clone(),
        amount: amount.clone(),
        currency: currency.clone(),
        address: made.address.clone(),
        xmr_amount_piconero: made.xmr_amount_piconero,
        merchant_order_id: merchant_order_id.clone(),
        expires_at: made.expires_at,
        created_at: now_unix(),
    });
    let (recorded_app, announced) = (app.clone(), app_order.clone());
    let (id, order_id, written_currency, written_amount, provider_name) = (
        row.id.clone(),
        made.order_id.clone(),
        currency.clone(),
        amount.clone(),
        provider.to_string(),
    );
    let base_currency = resolution.base_currency.clone();
    let (base_rate, confirmations) = (
        resolution.base_currency_piconero_per_unit,
        resolution.confirmations_required,
    );
    let recorded = state
        .db
        .write(move |db| {
            // A failed lookup costs the order its plugin, never its record.
            // An app's order is never a plugin's.
            let plugin = match client_name {
                Some(_) if recorded_app.is_some() => None,
                Some(_) => db
                    .active_integration_kind(&id)
                    .inspect_err(
                        |e| tracing::warn!(error = %e, "the store's plugin could not be read"),
                    )
                    .ok()
                    .flatten(),
                None => None,
            };
            let source = match &recorded_app {
                Some(app) => app.slug.clone(),
                None => order_source(created_with_key, client_name.as_deref(), plugin.as_deref()),
            };
            db.create_order_currency_metadata(
                &id,
                &order_id,
                &written_currency,
                &written_amount,
                shared::xmr_amount::Piconero(piconero_per_unit),
                &provider_name,
                now_unix(),
                &base_currency,
                base_rate,
                confirmations,
                created_with_key,
                Some(&source),
                show_merchant_order_id,
            )?;
            if let (Some(app), Some(app_order)) = (&recorded_app, &app_order) {
                db.record_app_order(app_order)?;
                let version = match client_name.as_deref() {
                    Some(name) if name.eq_ignore_ascii_case(&app.slug) => {
                        version.as_deref().unwrap_or_default()
                    }
                    _ => "",
                };
                db.app_seen(&app.id, version, now_unix())?;
            }
            // An order from the plugin: its integration was seen now.
            if plugin.as_deref() == Some(source.as_str()) {
                db.integration_seen(
                    &id,
                    &source,
                    version.as_deref().unwrap_or_default(),
                    now_unix(),
                )?;
            }
            Ok::<_, crate::db::DbError>(())
        })
        .await;
    if let (Ok(()), Some(order)) = (&recorded, &announced) {
        state
            .apps
            .announce(&pk, crate::apps::SaleEvent::waiting(order));
    }
    if let Err(e) = &recorded {
        tracing::error!(
            order.id = %made.order_id,
            store.id = %row.id,
            error = %e,
            "failed to record an order's local record - the order exists on the engine, but monokulo \
             can't show its price or find its checkout"
        );
        // Without its record the order's checkout can't find its store, so
        // the order isn't handed out. A retry with the same idempotency key
        // gets the same engine order and records it then; without one the
        // unrecorded order just expires.
        return Err(internal());
    }
    // A retry with the same idempotency key gets the order the first
    // attempt made, which may have moved on since: read it as it is now. A
    // new order is as the engine just made it.
    let now = match input.idempotency_key {
        Some(_) => order::load(&orders, &made.order_id, None)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let body = now.unwrap_or_else(|| {
        let view = crate::engine_client::OrderView {
            order_id: made.order_id.clone(),
            merchant_order_id: merchant_order_id.clone(),
            description: description.clone(),
            address: made.address.clone(),
            xmr_amount_piconero: made.xmr_amount_piconero,
            amount_received_piconero: 0,
            status: shared::order_status::OrderStatus::Pending,
            confirmations: 0,
            double_spend_detected_at: None,
            cancelled_at: None,
            refund_address: None,
            created_at: now_unix(),
            expires_at: made.expires_at,
            updated_at: now_unix(),
            first_scanned_height: None,
            last_scanned_height: None,
            currently_scanning: false,
        };
        let metadata = crate::db::OrderCurrencyMetadataRow {
            connection_id: row.id.clone(),
            order_id: made.order_id.clone(),
            currency,
            amount,
            piconero_per_unit: shared::xmr_amount::Piconero(piconero_per_unit),
            provider: provider.to_string(),
            created_at: now_unix(),
            store_base_currency: Some(resolution.base_currency.clone()),
            base_currency_piconero_per_unit: resolution.base_currency_piconero_per_unit,
            confirmations_required_applied: Some(resolution.confirmations_required),
            created_with_key,
            show_merchant_order_id,
        };
        order::of(
            &row.id,
            &view,
            Some(&metadata),
            resolution.confirmations_required,
            onion.as_deref(),
        )
    });
    Ok(Created(body))
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
