//! Common settings immediately after connecting a store. Plugin credentials
//! are minted only on completion, so time spent in this form cannot expire them.
use super::{dashboard::redirect_302, orders, AppState, AuthedUser};
use crate::{db::ConnectionId, views};
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use maud::html;
use serde::Deserialize;

#[derive(Default, Deserialize)]
pub struct Setup {
    #[serde(default)]
    pub return_url: String,
    #[serde(default)]
    pub nonce: String,
    #[serde(default)]
    pub base_currency: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub confirmations: String,
    #[serde(default)]
    pub skip: String,
}

async fn render(
    state: &AppState,
    user: &crate::db::UserRow,
    row: super::OwnedStore,
    input: Setup,
    error: Option<String>,
) -> Response {
    let chrome = super::page_chrome(
        state,
        Some(user),
        format!("/dashboard/stores/{}/setup", row.id),
    )
    .await;
    let requested = if input.base_currency.is_empty() {
        row.base_currency.clone()
    } else {
        input.base_currency.clone()
    };
    let currencies = state
        .db
        .read(move |db| crate::currencies::currency_options(db, &requested))
        .await
        .unwrap_or_default();
    let confirmations = if input.confirmations.is_empty() {
        match orders::decrypt_sk(&state.encryption_key, &row) {
            Ok(sk) => state
                .engine
                .client
                .get_tenant(&sk)
                .await
                .map(|t| t.confirmations_required.to_string())
                .unwrap_or_else(|_| "10".into()),
            Err(()) => "10".into(),
        }
    } else {
        input.confirmations.clone()
    };
    let providers = state.exchange_rate.available_providers();
    let selected_provider = if error.is_none() {
        row.fx_providers.first().cloned().unwrap_or_default()
    } else {
        input.provider.clone()
    };
    let body = html! {
        main class="wrap" {
            h1 { "Store connected" }
            p { "Choose the settings you will use most often. You can change these later in store settings." }
            @if let Some(error) = error { p class="error" role="alert" { (error) } }
            form method="post" action=(format!("/dashboard/stores/{}/setup", row.id)) class="box store-setup" {
                input type="hidden" name="return_url" value=(input.return_url);
                input type="hidden" name="nonce" value=(input.nonce);
                label { "Base currency" select name="base_currency" { @for currency in &currencies { option value=(currency.code) selected[currency.selected] { (currency.description) } } } }
                label { "Exchange rate provider" select name="provider" {
                    option value="" selected[selected_provider.is_empty()] { "None (XMR orders only)" }
                    @for provider in providers { option value=(provider) selected[selected_provider == provider] { (provider) } }
                } }
                label { "Payment confirmations" input type="number" name="confirmations" value=(confirmations) min="0" max="720" required; }
                p class="field-help" { "0 accepts unconfirmed payments. 10 is the usual default." }
                button type="submit" class="btn-primary" { "Save and continue" }
                button type="submit" name="skip" value="yes" formnovalidate { "Skip for now" }
            }
        }
    };
    views::layout(&chrome, "Store setup - Monokulo", body).into_response()
}

pub async fn page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Query(input): Query<Setup>,
) -> Response {
    match orders::load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => render(&state, &user, row, input, None).await,
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(()) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn save(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Form(input): Form<Setup>,
) -> Response {
    let row = match orders::load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let target = if input.return_url.is_empty() {
        None
    } else {
        match super::connect::ConnectTarget::parse(&row.site_url, &input.return_url) {
            Ok(target) => Some(target),
            Err(error) => return render(&state, &user, row, input, Some(error.into())).await,
        }
    };
    if input.skip != "yes" {
        let requested = input.base_currency.clone();
        let currency = state
            .db
            .read(move |db| crate::currencies::resolve_currency(db, &requested))
            .await;
        let (Ok(Some(currency)), Ok(confirmations)) =
            (currency, input.confirmations.parse::<u64>())
        else {
            return render(
                &state,
                &user,
                row,
                input,
                Some(
                    "Choose a known currency and a whole number of confirmations from 0 to 720."
                        .into(),
                ),
            )
            .await;
        };
        if confirmations > 720
            || (!input.provider.is_empty()
                && !state
                    .exchange_rate
                    .available_providers()
                    .contains(&input.provider.as_str()))
        {
            return render(
                &state,
                &user,
                row,
                input,
                Some("Choose an available provider and confirmations from 0 to 720.".into()),
            )
            .await;
        }
        let sk = match orders::decrypt_sk(&state.encryption_key, &row) {
            Ok(sk) => sk,
            Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        let policy = crate::confirmation_thresholds::lock_policy(&row.tenant_public_key).await;
        let old_confirmations = match state.engine.client.get_tenant(&sk).await {
            Ok(t) => t.confirmations_required,
            Err(_) => {
                return render(
                    &state,
                    &user,
                    row,
                    input,
                    Some("The engine is unavailable. Try again or skip for now.".into()),
                )
                .await
            }
        };
        if let Err(error) = state
            .engine
            .client
            .set_confirmations_required(&sk, confirmations)
            .await
        {
            return render(
                &state,
                &user,
                row,
                input,
                Some(format!("Could not save confirmations: {error}")),
            )
            .await;
        }
        let providers: Vec<String> = if input.provider.is_empty() {
            vec![]
        } else {
            vec![input.provider.clone()]
        };
        let (store_id, proof) = (id.clone(), policy.proof());
        if state
            .db
            .write(move |db| db.update_store_setup(proof, &store_id, &currency, &providers))
            .await
            .is_err()
        {
            let _ = state
                .engine
                .client
                .set_confirmations_required(&sk, old_confirmations)
                .await;
            return render(
                &state,
                &user,
                row,
                input,
                Some("Could not save store settings. Please try again.".into()),
            )
            .await;
        }
    }
    if let Some(mut target) = target {
        let token = shared::auth::generate_connect_token();
        let hash = token.hash();
        let nonce = input.nonce.clone();
        if state
            .db
            .write(move |db| db.create_connect_token(&hash, &id, &nonce, crate::now_unix()))
            .await
            .is_err()
        {
            return render(
                &state,
                &user,
                row,
                input,
                Some("Could not finish connecting. Please try again.".into()),
            )
            .await;
        }
        target
            .return_to
            .query_pairs_mut()
            .append_pair("token", token.expose())
            .append_pair("nonce", &input.nonce);
        redirect_302(target.return_to.as_str())
    } else {
        redirect_302(&format!("/dashboard/stores/{id}"))
    }
}
