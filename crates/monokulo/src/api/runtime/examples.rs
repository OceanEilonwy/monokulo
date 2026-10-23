//! Examples from real exchanges: under test, every `/api/v1` request and
//! answer is checked against the OpenAPI document and recorded
//! ([`record_layer`]). The reference test (`super::reference`) makes one of
//! each kind against a real engine and keeps them, with their ids and
//! times made stable ([`stable`]): the published OpenAPI document's and the
//! docs' examples, recorded afresh each time the docs are built. So an
//! example is always something the API really said.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One request and its answer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Exchange {
    pub method: String,
    /// The route's template.
    pub route: String,
    /// The path asked for.
    pub path: String,
    /// The key it carried: `pk`, `sk`, `ak`, or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}

#[cfg(test)]
pub use recording::*;

#[cfg(test)]
mod recording {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::body::{Body, Bytes};
    use axum::extract::{MatchedPath, Request};
    use axum::http::header;
    use axum::middleware::Next;
    use axum::response::Response;
    use http_body_util::BodyExt;
    use serde_json::Value;

    use super::Exchange;

    /// Every exchange recorded in this process.
    static RECORDED: Mutex<Vec<Exchange>> = Mutex::new(Vec::new());

    /// The exchanges recorded so far, taken.
    pub fn take() -> Vec<Exchange> {
        std::mem::take(&mut *RECORDED.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn json_of(bytes: &Bytes) -> Option<Value> {
        serde_json::from_slice(bytes).ok()
    }

    /// Checks each `/api/v1` answer against the OpenAPI document and
    /// records the exchange. In the router only under test.
    pub async fn record_layer(request: Request, next: Next) -> Response {
        let Some(route) = request
            .extensions()
            .get::<MatchedPath>()
            .map(|m| m.as_str().to_owned())
        else {
            return next.run(request).await;
        };
        let method = request.method().as_str().to_owned();
        let path = request.uri().to_string();
        let key = request
            .extensions()
            .get::<crate::http::store_key::ApiCaller>()
            .map(|caller| caller.credential.key().name().to_owned());
        let (parts, body) = request.into_parts();
        let request_bytes = body
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        let request_json = json_of(&request_bytes);
        let request = Request::from_parts(parts, Body::from(request_bytes));
        let response = next.run(request).await;
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        // A stream stays a stream; only JSON answers are read here.
        if !content_type.contains("json") {
            RECORDED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Exchange {
                    method,
                    route,
                    path,
                    key,
                    request: request_json,
                    status: response.status().as_u16(),
                    response: None,
                });
            return response;
        }
        let (parts, body) = response.into_parts();
        let bytes = body
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        let json = json_of(&bytes).unwrap_or(Value::Null);
        if !content_type.starts_with(super::super::openapi::OPENAPI_JSON) {
            super::super::openapi::check_response(
                &method,
                &route,
                parts.status,
                &content_type,
                &json,
            );
        }
        RECORDED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Exchange {
                method,
                route,
                path,
                key,
                request: request_json,
                status: parts.status.as_u16(),
                response: (!content_type.starts_with(super::super::openapi::OPENAPI_JSON))
                    .then_some(json),
            });
        Response::from_parts(parts, Body::from(bytes))
    }

    /// `exchanges` with what changes from run to run (ids, addresses,
    /// keys, times) replaced by stable stand-ins, the same one each time
    /// the same value appears.
    pub fn stable(exchanges: Vec<Exchange>) -> Vec<Exchange> {
        let mut names: HashMap<String, String> = HashMap::new();
        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        let mut rename = |value: &str| -> Option<String> {
            let kind = if value.starts_with("order_") && value.len() > 30 {
                "order"
            } else if value.starts_with("app_") && value.len() > 20 {
                "app"
            } else if (value.starts_with('4')
                || value.starts_with('8')
                || value.starts_with('5')
                || value.starts_with('7'))
                && value.len() == 95
                && value.chars().all(|c| c.is_ascii_alphanumeric())
            {
                "address"
            } else {
                return None;
            };
            if let Some(name) = names.get(value) {
                return Some(name.clone());
            }
            let n = counts.entry(kind).or_insert(0);
            *n += 1;
            let name = match kind {
                "order" => format!("order_00000000-0000-4000-8000-{:012}", n),
                "app" => format!("app_{:032}", n),
                _ => format!("8{}{:04}", "A".repeat(90), n),
            };
            names.insert(value.to_owned(), name.clone());
            Some(name)
        };
        fn walk(value: &mut Value, rename: &mut dyn FnMut(&str) -> Option<String>) {
            match value {
                Value::String(s) => {
                    // Whole values, and ids inside paths.
                    if let Some(name) = rename(s) {
                        *s = name;
                    } else {
                        let parts: Vec<String> = s
                            .split('/')
                            .map(|part| rename(part).unwrap_or_else(|| part.to_owned()))
                            .collect();
                        *s = parts.join("/");
                    }
                }
                Value::Array(items) => items.iter_mut().for_each(|v| walk(v, rename)),
                Value::Object(map) => {
                    let created = map.get("created_at").and_then(Value::as_i64);
                    for (key, v) in map.iter_mut() {
                        if key.ends_with("_at") {
                            if let Some(at) = v.as_i64() {
                                // Times: a fixed moment, keeping how long an
                                // order runs (to the minute).
                                let base = 1_767_225_600;
                                *v = match (key.as_str(), created) {
                                    ("expires_at", Some(created)) => {
                                        Value::from(base + ((at - created) + 30) / 60 * 60)
                                    }
                                    _ => Value::from(base),
                                };
                                continue;
                            }
                        }
                        walk(v, rename);
                    }
                }
                _ => {}
            }
        }
        exchanges
            .into_iter()
            .map(|mut exchange| {
                let mut path = Value::String(exchange.path.clone());
                walk(&mut path, &mut rename);
                exchange.path = path.as_str().unwrap_or_default().to_owned();
                if let Some(request) = &mut exchange.request {
                    walk(request, &mut rename);
                }
                if let Some(response) = &mut exchange.response {
                    walk(response, &mut rename);
                }
                exchange
            })
            .collect()
    }
}
