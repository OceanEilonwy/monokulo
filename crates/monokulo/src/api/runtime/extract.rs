//! What a handler takes. Every parameter is an [`ApiParam`], which says
//! what it adds to the operation in the OpenAPI document: the path's
//! parameters, the query's, the request body. The short extractors here
//! read the store, its orders, the app and so on from the request's key
//! (`http::store_key::ApiCaller`, set by `http::abuse`).

use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::StatusCode;
use schemars::{JsonSchema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::problem::{internal, Problem, ProblemDetails};
use crate::db::{AppConnection, StoreConnectionRow};
use crate::http::store_key::{ApiCaller, Credential};
use crate::http::AppState;

/// One operation of the OpenAPI document while it's described.
pub struct OperationDoc<'a> {
    pub generator: &'a mut SchemaGenerator,
    pub path_params: Vec<&'static str>,
    pub query: Option<Value>,
    pub body: Option<Value>,
    /// Its answer: status, media type and schema (none for no body).
    pub answers: Vec<(StatusCode, &'static str, Option<Value>)>,
}

impl<'a> OperationDoc<'a> {
    pub fn new(generator: &'a mut SchemaGenerator) -> Self {
        OperationDoc {
            generator,
            path_params: Vec::new(),
            query: None,
            body: None,
            answers: Vec::new(),
        }
    }

    pub fn path_params(&mut self, names: &[&'static str]) {
        self.path_params.extend_from_slice(names);
    }

    pub fn body<T: JsonSchema>(&mut self) {
        self.body = Some(self.generator.subschema_for::<T>().to_value());
    }

    pub fn query<T: JsonSchema>(&mut self) {
        self.query = Some(self.generator.subschema_for::<T>().to_value());
    }

    pub fn answer<T: JsonSchema>(&mut self, status: StatusCode) {
        let schema = self.generator.subschema_for::<T>().to_value();
        self.answers
            .push((status, super::hal::HAL_FORMS, Some(schema)));
    }

    pub fn answer_media(&mut self, status: StatusCode, media_type: &'static str) {
        self.answers.push((status, media_type, None));
    }

    pub fn answer_empty(&mut self, status: StatusCode) {
        self.answers.push((status, "", None));
    }
}

/// A handler's parameter.
pub trait ApiParam {
    /// What it adds to the operation in the OpenAPI document.
    fn describe(operation: &mut OperationDoc<'_>) {
        let _ = operation;
    }
}

/// A form's input (`#[api::input]`).
pub trait Input {}

/// A JSON body of type `T`: a [`Problem`] when it isn't JSON or doesn't fit
/// `T` (whose scalars check their own values).
pub async fn json_body<T: serde::de::DeserializeOwned, S: Send + Sync>(
    request: Request,
    state: &S,
) -> Result<T, ProblemDetails> {
    use axum::extract::rejection::JsonRejection;
    match axum::Json::<T>::from_request(request, state).await {
        Ok(axum::Json(value)) => Ok(value),
        Err(JsonRejection::MissingJsonContentType(rejection)) => {
            Err(Problem::UnsupportedMediaType.detail(rejection.body_text()))
        }
        Err(JsonRejection::BytesRejection(rejection))
            if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE =>
        {
            Err(Problem::TooLarge.detail(rejection.body_text()))
        }
        Err(rejection) => Err(Problem::InvalidRequest.detail(rejection.body_text())),
    }
}

/// A JSON body for a route a machine calls outside the API (a plugin's or
/// app's handshake, the point of sale): `T`, or the problem
/// [`json_body`] answers with, never axum's plain-text rejection.
pub struct JsonBody<T>(pub T);

impl<T: serde::de::DeserializeOwned, S: Send + Sync> FromRequest<S> for JsonBody<T> {
    type Rejection = ProblemDetails;

    async fn from_request(request: Request, state: &S) -> Result<Self, ProblemDetails> {
        json_body(request, state).await.map(JsonBody)
    }
}

/// A query string for such a route: `T`, or an `invalid-request` problem.
pub struct QueryParams<T>(pub T);

impl<T: serde::de::DeserializeOwned, S: Send + Sync> FromRequestParts<S> for QueryParams<T> {
    type Rejection = ProblemDetails;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ProblemDetails> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(query)| QueryParams(query))
            .map_err(|rejection| Problem::InvalidRequest.detail(rejection.body_text()))
    }
}

fn caller(parts: &Parts) -> Result<&ApiCaller, ProblemDetails> {
    parts
        .extensions
        .get::<ApiCaller>()
        .ok_or_else(|| Problem::Unauthorized.detail("Send a key: Authorization: Bearer <key>."))
}

/// The request's key and the store it names.
#[derive(Clone)]
pub struct Caller(pub ApiCaller);

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ProblemDetails;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ProblemDetails> {
        caller(parts).cloned().map(Caller)
    }
}

impl ApiParam for Caller {}

/// The store the request's key names.
#[derive(Clone)]
pub struct Store(pub Arc<StoreConnectionRow>);

impl std::ops::Deref for Store {
    type Target = StoreConnectionRow;
    fn deref(&self) -> &StoreConnectionRow {
        &self.0
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Store {
    type Rejection = ProblemDetails;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ProblemDetails> {
        caller(parts).map(|caller| Store(caller.store.clone()))
    }
}

impl ApiParam for Store {}

/// The whole of monokulo's state, for a handler that needs much of it.
#[derive(Clone)]
pub struct Instance(pub AppState);

impl FromRequestParts<AppState> for Instance {
    type Rejection = ProblemDetails;

    async fn from_request_parts(_: &mut Parts, state: &AppState) -> Result<Self, ProblemDetails> {
        Ok(Instance(state.clone()))
    }
}

impl ApiParam for Instance {}

/// The store's orders, on the engine: the store's own secret key, read
/// from where it's kept.
#[derive(Clone)]
pub struct Orders {
    pub state: AppState,
    pub store: Arc<StoreConnectionRow>,
    pub sk: shared::auth::RawToken,
}

impl FromRequestParts<AppState> for Orders {
    type Rejection = ProblemDetails;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, ProblemDetails> {
        let store = caller(parts)?.store.clone();
        let sk = crate::http::decrypt_sk(&state.encryption_key, &store).map_err(|()| internal())?;
        Ok(Orders {
            state: state.clone(),
            store,
            sk,
        })
    }
}

impl ApiParam for Orders {}

/// The connected app the request's key belongs to.
#[derive(Clone)]
pub struct ConnectedApp(pub AppConnection);

impl<S: Send + Sync> FromRequestParts<S> for ConnectedApp {
    type Rejection = ProblemDetails;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ProblemDetails> {
        match &caller(parts)?.credential {
            Credential::App(app) => Ok(ConnectedApp(app.clone())),
            _ => Err(Problem::Forbidden.detail("Only a connected app's own key names an app.")),
        }
    }
}

impl ApiParam for ConnectedApp {}

/// The store's connected apps.
#[derive(Clone)]
pub struct Apps(pub AppState);

impl FromRequestParts<AppState> for Apps {
    type Rejection = ProblemDetails;

    async fn from_request_parts(_: &mut Parts, state: &AppState) -> Result<Self, ProblemDetails> {
        Ok(Apps(state.clone()))
    }
}

impl ApiParam for Apps {}

/// A page of a list: `?limit=&offset=`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "PageQuery")]
pub struct Page {
    /// How many, 1 to 200; 50 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// How many of the newest to skip; none when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

impl<S: Send + Sync> FromRequestParts<S> for Page {
    type Rejection = ProblemDetails;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ProblemDetails> {
        axum::extract::Query::<Page>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(page)| page)
            .map_err(|rejection| Problem::InvalidRequest.detail(rejection.body_text()))
    }
}

impl ApiParam for Page {
    fn describe(operation: &mut OperationDoc<'_>) {
        operation.query::<Page>();
    }
}

/// The client the request names in `Monokulo-Client: name/version`, and
/// its version (`crate::http::pay::client`).
#[derive(Clone, Debug)]
pub struct Client(pub Option<(String, String)>);

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = ProblemDetails;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ProblemDetails> {
        Ok(Client(crate::http::pay::client(&parts.headers)))
    }
}

impl ApiParam for Client {}

/// A slot for one more open stream for this client on the store: one
/// client may hold only so many at once (`crate::abuse::streams`).
pub struct StreamSlot(pub Option<crate::abuse::streams::StreamPermit>);

impl FromRequestParts<AppState> for StreamSlot {
    type Rejection = ProblemDetails;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, ProblemDetails> {
        let store = caller(parts)?.store.clone();
        // The client `http::abuse` identified; absent only when a test
        // drives the router without a connection, which fails open.
        match parts.extensions.get::<crate::abuse::ClientIdentity>() {
            Some(client) => match state
                .abuse
                .streams
                .try_acquire(client, &store.tenant_public_key)
            {
                Some(permit) => Ok(StreamSlot(Some(permit))),
                None => Err(Problem::TooManyStreams
                    .detail("Too many open update streams. Close one and try again.")),
            },
            None => Ok(StreamSlot(None)),
        }
    }
}

impl ApiParam for StreamSlot {}
