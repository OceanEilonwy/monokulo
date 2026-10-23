use crate::api::runtime::hal::Document;
use crate::api::runtime::openapi;
use crate::api::{self, prelude::*};

api::resource!(
    "/api/v1/openapi.json",
    rel = "openapi",
    kind = document,
    doc = "The API's OpenAPI 3.1 document.\n\nEvery route, who may call it, what it takes and what it answers, with real examples."
);

/// Read the OpenAPI document.
#[get(anyone)]
async fn get() -> api::Result<Document> {
    Ok(Document(openapi::to_text(openapi::document())))
}
