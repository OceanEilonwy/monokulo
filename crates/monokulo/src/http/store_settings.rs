//! `POST /dashboard/stores/{id}/settings`: the store settings page's
//! settings form (`views::store_settings`), saved with its one save bar.
//!
//! The form carries every card's fields; only the cards whose values differ
//! from what's saved are saved, in the page's order: the store's name and
//! site, the base currency, the
//! confirmation thresholds (the default in the engine, the custom ones
//! here), the exchange rate providers, diagnostics. Every card is checked
//! before anything is written, so a value one refuses saves nothing; only a
//! write that fails after another succeeded leaves the save partly done,
//! and the page says which. A save redirects back to the page (303) with
//! what it saved in `saved=`, for the cards' marks and the toast; a refused
//! one answers with the page (422), showing why on the card and keeping
//! what was sent. The same with or without JavaScript.
//!
//! A field the form doesn't carry is left as it is, so a script posting
//! only some of them (the browser tests' fixtures) changes only those.

use std::collections::HashMap;

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::db::UserRow;
use crate::engine_client::EngineClientError;
use crate::views::store_settings::{StoreOutcome, StoreSection};

use super::dashboard::redirect_303;
use super::orders::{
    decrypt_sk, load_owned_connection, parse_fx_providers_form, render_store_settings_page_with,
    PageState,
};
use super::{AppState, AuthedUser, OwnedStore};

/// The most confirmations a store can ask for.
const MAX_CONFIRMATIONS: u64 = 720;
/// The default a store goes back to when it stops accepting unconfirmed
/// payments with a default of 0.
const ORDINARY_DEFAULT: u64 = 10;

/// A custom threshold to add: its confirmations, and its amount.
type NewThreshold = (u64, String);

/// What a save changes, once every card's values were checked.
/// A saved website changed in the form, to be asked about first.
enum WebsiteRequest {
    To(String),
    Remove,
}

#[derive(Default)]
struct Plan {
    /// The website changed or emptied: the save goes on to ask about it.
    website: Option<WebsiteRequest>,
    /// The store's name, and its site (empty: none).
    name: Option<String>,
    site: Option<String>,
    base_currency: Option<String>,
    default_confirmations: Option<u64>,
    /// Thresholds to delete, and one to add (confirmations, amount).
    thresholds: Option<(Vec<String>, Option<NewThreshold>)>,
    fx: Option<(Vec<String>, crate::fx_provider_settings::FxProviderSettings)>,
    client_logging: Option<bool>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.site.is_none()
            && self.base_currency.is_none()
            && self.default_confirmations.is_none()
            && self.thresholds.is_none()
            && self.fx.is_none()
            && self.client_logging.is_none()
    }
}

/// A card's values refused: the card, and why.
type Refusal = (StoreSection, String);

pub async fn save(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    Form(sent): Form<Vec<(String, String)>>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // The policy (thresholds, base currency, providers) changes under the
    // store's lock, so no order is priced against one half saved.
    let policy = crate::confirmation_thresholds::lock_policy(&row.tenant_public_key).await;

    let mut plan = match plan(&state, &row, &sk, &sent).await {
        Ok(plan) => plan,
        Err((section, message)) => {
            drop(policy);
            return refused(&state, row, &user, sent, section, message, Vec::new()).await;
        }
    };
    let website = plan.website.take().map(|request| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        match request {
            WebsiteRequest::To(site) => query.append_pair("site", &site),
            WebsiteRequest::Remove => query.append_pair("remove", "1"),
        };
        format!("/dashboard/stores/{id}/settings/website?{}", query.finish())
    });
    if plan.is_empty() {
        drop(policy);
        return redirect_303(
            &website.unwrap_or_else(|| format!("/dashboard/stores/{id}/settings?saved=nothing")),
        );
    }

    let mut saved = Vec::new();
    let failed = apply(&state, &row, &sk, &policy, plan, &mut saved).await;
    drop(policy);
    if let Err((section, message)) = failed {
        // Read again: what was saved before the failure is saved now.
        let row = match load_owned_connection(&state.db, &user, &id).await {
            Ok(Some(fresh)) => fresh,
            _ => row,
        };
        return refused(&state, row, &user, sent, section, message, saved).await;
    }
    let ids: Vec<&str> = saved.iter().map(|section| section.id()).collect();
    tracing::info!(store.id = %id, saved = %ids.join(","), "store settings saved");
    if let Some(website) = website {
        return redirect_303(&website);
    }
    redirect_303(&format!(
        "/dashboard/stores/{id}/settings?saved={}",
        ids.join(",")
    ))
}

async fn refused(
    state: &AppState,
    row: OwnedStore,
    user: &UserRow,
    sent: Vec<(String, String)>,
    section: StoreSection,
    message: String,
    saved: Vec<StoreSection>,
) -> Response {
    render_store_settings_page_with(
        state,
        row,
        user,
        PageState {
            outcome: Some(StoreOutcome::Refused {
                section,
                message,
                saved,
            }),
            sent: Some(sent),
            ..Default::default()
        },
    )
    .await
}

/// Every card's values checked against what's saved: what to change, or
/// the first card (in the page's order) whose values are refused.
async fn plan(
    state: &AppState,
    row: &OwnedStore,
    sk: &shared::auth::RawToken,
    sent: &[(String, String)],
) -> Result<Plan, Refusal> {
    let form: HashMap<String, String> = sent.iter().cloned().collect();
    let switches: Vec<&str> = sent
        .iter()
        .filter(|(key, _)| key == "switches")
        .map(|(_, value)| value.as_str())
        .collect();
    // A switch: on when sent, off when the form names it (`switches`) but
    // it wasn't sent, untouched otherwise.
    let switch = |name: &str| {
        if form.contains_key(name) {
            Some(true)
        } else {
            switches.contains(&name).then_some(false)
        }
    };
    let mut plan = Plan::default();

    // The store's name and site. A site is optional, kept as its host, and
    // no other store on this instance may have it.
    const STORE: StoreSection = StoreSection::Store;
    if let Some(raw) = form.get("store_name") {
        let name = crate::stores::clean_name(raw).map_err(|m| (STORE, m.to_owned()))?;
        if name != row.name {
            plan.name = Some(name);
        }
    }
    // The settings form only ever adds a first site. Changing or removing
    // one asks first (`http::store_site`): the save goes on to that page,
    // the rest of the form saved. A connected plugin's site isn't in the
    // form at all; one sent anyway is ignored.
    let locked = {
        let store_id = row.id.clone();
        state
            .db
            .read(move |db| db.active_integration(&store_id))
            .await
            .map_err(|_| (STORE, something_went_wrong()))?
            .is_some()
    };
    if let Some(raw) = form.get("store_site").filter(|_| !locked) {
        let site = if raw.trim().is_empty() {
            String::new()
        } else if row.site.is_empty() {
            crate::stores::normalize_site(raw).map_err(|m| (STORE, m.to_owned()))?
        } else {
            raw.trim().to_owned()
        };
        if !row.site.is_empty() && site != row.site {
            plan.website = Some(if site.is_empty() {
                WebsiteRequest::Remove
            } else {
                WebsiteRequest::To(site)
            });
        } else if site != row.site {
            if !site.is_empty() {
                let (user_id, wanted) = (row.user_id.clone(), site.clone());
                let taken = state
                    .db
                    .read(move |db| super::connections::SiteTaken::of(db, &user_id, &wanted))
                    .await
                    .map_err(|_| (STORE, something_went_wrong()))?;
                if let Some(taken) = taken {
                    return Err((STORE, taken.message(&site)));
                }
            }
            plan.site = Some(site);
        }
    }

    // Base currency.
    if let Some(requested) = form.get("base_currency") {
        let requested = requested.clone();
        let resolved = state
            .db
            .read(move |db| crate::currencies::resolve_currency(db, &requested))
            .await;
        match resolved {
            Ok(Some(code)) if code != row.base_currency => plan.base_currency = Some(code),
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err((
                    StoreSection::BaseCurrency,
                    format!("{:?} is not a known currency.", form["base_currency"]),
                ))
            }
            Err(_) => return Err((StoreSection::BaseCurrency, something_went_wrong())),
        }
    }

    // The default confirmations, and accepting unconfirmed payments.
    const CONFIRMATIONS: StoreSection = StoreSection::Confirmations;
    if let Some(text) = form.get("confirmations_required") {
        let zero_conf = switch("zero_conf_enabled");
        let wanted: u64 = if zero_conf == Some(true) {
            0
        } else {
            match text.trim().parse() {
                // Turning unconfirmed payments off brings back the
                // ordinary default.
                Ok(0) if zero_conf == Some(false) => ORDINARY_DEFAULT,
                Ok(n) if n <= MAX_CONFIRMATIONS => n,
                Ok(_) => {
                    return Err((
                        CONFIRMATIONS,
                        "Enter a whole number of confirmations from 0 to 720.".to_string(),
                    ))
                }
                Err(_) => {
                    return Err((
                        CONFIRMATIONS,
                        "Enter a whole number of confirmations.".to_string(),
                    ))
                }
            }
        };
        let current = state
            .engine
            .client
            .get_tenant(sk)
            .await
            .ok()
            .map(|t| t.confirmations_required);
        if current != Some(wanted) {
            plan.default_confirmations = Some(wanted);
        }
    }

    // The custom thresholds: those ticked to delete, and one to add.
    let new_amount = form.get("new_unit_amount").map_or("", |s| s.trim());
    let new_confirmations = form
        .get("new_confirmations_required")
        .map_or("", |s| s.trim());
    let new_threshold = if new_amount.is_empty() && new_confirmations.is_empty() {
        None
    } else {
        let confirmations =
            match new_confirmations.parse() {
                Ok(n) if n <= MAX_CONFIRMATIONS => n,
                _ => return Err((
                    CONFIRMATIONS,
                    "Enter a whole number of confirmations from 0 to 720 for the new threshold."
                        .to_string(),
                )),
            };
        match crate::confirmation_thresholds::ThresholdAmount::parse(new_amount) {
            Ok(amount) => Some((confirmations, amount.canonical())),
            Err(_) => {
                return Err((
                    CONFIRMATIONS,
                    "Enter a non-negative amount for the new threshold.".to_string(),
                ))
            }
        }
    };
    let store_id = row.id.clone();
    let existing = state
        .db
        .read(move |db| db.list_confirmation_thresholds(&store_id))
        .await
        .map_err(|_| (CONFIRMATIONS, something_went_wrong()))?;
    let deleted: Vec<String> = existing
        .iter()
        .filter(|t| form.contains_key(&format!("delete_{}", t.id)))
        .map(|t| t.id.clone())
        .collect();
    if !deleted.is_empty() || new_threshold.is_some() {
        if plan.base_currency.is_some() {
            return Err((
                CONFIRMATIONS,
                "A new base currency deletes every custom threshold: save it on its own first, then change the thresholds in it."
                    .to_string(),
            ));
        }
        let kept = |t: &&crate::db::ConfirmationThresholdRow| !deleted.contains(&t.id);
        let parse = crate::confirmation_thresholds::ThresholdAmount::parse;
        if existing
            .iter()
            .filter(kept)
            .any(|t| parse(&t.unit_amount).is_err())
        {
            return Err((
                CONFIRMATIONS,
                "An existing threshold amount is invalid. Delete it before saving.".to_string(),
            ));
        }
        if let Some((_, amount)) = &new_threshold {
            if existing.len() - deleted.len() >= 5 {
                return Err((
                    CONFIRMATIONS,
                    "You can define at most 5 custom thresholds. Delete one to add another."
                        .to_string(),
                ));
            }
            if existing
                .iter()
                .filter(kept)
                .any(|t| parse(&t.unit_amount).is_ok_and(|a| a.canonical() == *amount))
            {
                return Err((
                    CONFIRMATIONS,
                    format!("A threshold for {amount} already exists."),
                ));
            }
        }
        plan.thresholds = Some((deleted, new_threshold));
    }

    // The exchange rate providers, with Haveno's limits.
    const FX: StoreSection = StoreSection::FxProvider;
    let fx_fields = form.keys().any(|key| {
        key.starts_with("use_")
            || key.starts_with("position_")
            || crate::fx_provider_settings::HAVENO_FIELDS.contains(&key.as_str())
    });
    if fx_fields {
        let available = state.exchange_rate.available_providers();
        let providers = parse_fx_providers_form(&available, &form).map_err(|m| (FX, m))?;
        let mut settings = row.fx_provider_settings.clone();
        if state
            .exchange_rate
            .is_available(crate::exchange_rate_config::HAVENO)
        {
            let form = form.clone();
            let parsed = state
                .db
                .read(move |db| {
                    let resolve = |input: &str| {
                        crate::currencies::resolve_currency(db, input).map_err(|_| ())
                    };
                    Ok::<_, crate::db::DbError>(crate::fx_provider_settings::parse_haveno_form(
                        &form, &resolve,
                    ))
                })
                .await
                .unwrap_or_else(|_| Err(something_went_wrong()));
            if let Some(haveno) = parsed.map_err(|m| (FX, m))? {
                settings.haveno = haveno;
            }
        }
        // A provider this instance no longer offers isn't a change: the
        // form never showed it.
        let current: Vec<String> = row
            .fx_providers
            .iter()
            .filter(|name| available.contains(&name.as_str()))
            .cloned()
            .collect();
        if providers != current || settings != row.fx_provider_settings {
            plan.fx = Some((providers, settings));
        }
    }

    // Diagnostics.
    if let Some(on) = switch("client_logging") {
        let store_id = row.id.clone();
        let current = state
            .db
            .read(move |db| db.client_logging(&store_id))
            .await
            .unwrap_or(false);
        if on != current {
            plan.client_logging = Some(on);
        }
    }
    Ok(plan)
}

/// Saves what `plan` changes, card by card, noting each saved card in
/// `saved`; stops at the first write that fails.
async fn apply(
    state: &AppState,
    row: &OwnedStore,
    sk: &shared::auth::RawToken,
    policy: &crate::confirmation_thresholds::PolicyGuard,
    plan: Plan,
    saved: &mut Vec<StoreSection>,
) -> Result<(), Refusal> {
    if plan.name.is_some() || plan.site.is_some() {
        let name = plan.name.unwrap_or_else(|| row.name.clone());
        let site = plan.site.unwrap_or_else(|| row.site.clone());
        let new_site = site != row.site && !site.is_empty();
        let (store_id, wanted) = (row.id.clone(), site.clone());
        let written = state
            .db
            .write(move |db| {
                db.set_store_name_and_site(&store_id, &name, &wanted)?;
                // A new site's domain waits to be verified, as at setup.
                if new_site {
                    crate::embed_domains::suggest_site_domain(
                        db,
                        &store_id,
                        &wanted,
                        crate::now_unix(),
                    );
                }
                Ok::<_, crate::db::DbError>(())
            })
            .await;
        match written {
            Ok(()) => saved.push(StoreSection::Store),
            Err(e) if e.is_unique_violation() => {
                return Err((
                    StoreSection::Store,
                    super::connections::SiteTaken::Someone.message(&site),
                ))
            }
            Err(_) => return Err((StoreSection::Store, something_went_wrong())),
        }
    }

    if let Some(code) = plan.base_currency {
        let (store_id, proof) = (row.id.clone(), policy.proof());
        state
            .db
            .write(move |db| db.update_store_connection_base_currency(proof, &store_id, &code))
            .await
            .map_err(|_| (StoreSection::BaseCurrency, something_went_wrong()))?;
        saved.push(StoreSection::BaseCurrency);
    }

    const CONFIRMATIONS: StoreSection = StoreSection::Confirmations;
    if let Some(n) = plan.default_confirmations {
        match state.engine.client.set_confirmations_required(sk, n).await {
            Ok(_) => {}
            Err(EngineClientError::EngineError { status, message })
                if status == reqwest::StatusCode::BAD_REQUEST =>
            {
                return Err((CONFIRMATIONS, message));
            }
            Err(_) => return Err((CONFIRMATIONS, something_went_wrong())),
        }
    }
    let changed_thresholds = plan.thresholds.is_some();
    if let Some((deleted, new_threshold)) = plan.thresholds {
        let threshold_id = uuid::Uuid::new_v4().to_string();
        let (store_id, proof) = (row.id.clone(), policy.proof());
        let new_amount = new_threshold.as_ref().map(|(_, amount)| amount.clone());
        let written = state
            .db
            .write(move |db| {
                let new_threshold = new_threshold.as_ref().map(|(n, amount)| {
                    (
                        threshold_id.as_str(),
                        amount.as_str(),
                        *n,
                        crate::now_unix(),
                    )
                });
                db.replace_confirmation_thresholds(proof, &store_id, &deleted, new_threshold)
            })
            .await;
        match written {
            Ok(true) => {}
            Ok(false) => {
                return Err((
                    CONFIRMATIONS,
                    "You can define at most 5 custom thresholds. Delete one to add another."
                        .to_string(),
                ))
            }
            Err(e) if e.is_unique_violation() => {
                return Err((
                    CONFIRMATIONS,
                    format!(
                        "A threshold for {} already exists.",
                        new_amount.unwrap_or_default()
                    ),
                ))
            }
            Err(_) => return Err((CONFIRMATIONS, something_went_wrong())),
        }
    }
    if plan.default_confirmations.is_some() || changed_thresholds {
        saved.push(CONFIRMATIONS);
    }

    if let Some((providers, settings)) = plan.fx {
        let (store_id, proof) = (row.id.clone(), policy.proof());
        state
            .db
            .write(move |db| db.update_store_connection_fx(proof, &store_id, &providers, &settings))
            .await
            .map_err(|_| (StoreSection::FxProvider, something_went_wrong()))?;
        saved.push(StoreSection::FxProvider);
    }

    if let Some(on) = plan.client_logging {
        let store_id = row.id.clone();
        state
            .db
            .write(move |db| db.set_client_logging(&store_id, on))
            .await
            .map_err(|_| (StoreSection::Diagnostics, something_went_wrong()))?;
        tracing::info!(store.id = %row.id, client_logging = on, "store diagnostics changed");
        saved.push(StoreSection::Diagnostics);
    }
    Ok(())
}

fn something_went_wrong() -> String {
    "Something went wrong. Please try again.".to_string()
}
