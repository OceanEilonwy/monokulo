//! Stores' webhooks (`docs/DESIGN.md` §11): monokulo holds them, reads the
//! engine's order-event log ([`subscriber`]), makes each event's body
//! ([`body`]) and delivers it, signed, with retries ([`delivery`]). The
//! engine only keeps the log.
//!
//! ```text
//! engine: order changes ──(same transaction)──▶ order_events
//!                                                  │ GET /api/v1/admin/order-events (SSE)
//! monokulo: subscriber ──(same transaction)──▶ webhook_deliveries + position
//!                                                  │
//!           delivery worker ──POST, X-Monokulo-Signature──▶ the store's endpoint
//! ```

pub mod body;
pub mod delivery;
pub mod subscriber;

use std::collections::BTreeMap;

use shared::ids::WebhookId;

use crate::db::{ConnectionId, Database, DbError, NewWebhook, WebhookRow};

/// Most extra headers one webhook may carry, and most bytes of their
/// names and values together.
pub const MAX_EXTRA_HEADERS: usize = 20;
pub const MAX_EXTRA_HEADER_BYTES: usize = 4 * 1024;
/// Longest URL a webhook may have.
pub const MAX_URL_LEN: usize = 2048;

/// What the subscriber and the store pages tell the delivery worker: that
/// there's something to send now.
#[derive(Default)]
pub struct Webhooks {
    wake: tokio::sync::Notify,
}

impl Webhooks {
    /// Wakes the delivery worker (it also looks every few seconds).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Until [`Self::wake`] is called (or was since the last time).
    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}

/// `url` as a webhook may have it: an absolute `http(s)` URL. Whether its
/// host is private is checked at every delivery, not here: DNS can change
/// in between.
pub fn check_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Enter a webhook URL.".to_string());
    }
    if url.len() > MAX_URL_LEN {
        return Err(format!(
            "A webhook URL can be at most {MAX_URL_LEN} characters."
        ));
    }
    let parsed = url::Url::parse(url).map_err(|e| format!("That isn't a valid URL ({e})."))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("A webhook URL must start with http:// or https://.".to_string());
    }
    if parsed.host().is_none() {
        return Err("A webhook URL needs a host.".to_string());
    }
    Ok(url.to_string())
}

/// Extra headers as a webhook keeps them: a JSON object of valid header
/// names (lowercase) and values, none of them monokulo's own, within
/// [`MAX_EXTRA_HEADERS`] and [`MAX_EXTRA_HEADER_BYTES`].
pub fn check_extra_headers(headers: &BTreeMap<String, String>) -> Result<String, String> {
    if headers.len() > MAX_EXTRA_HEADERS {
        return Err(format!(
            "A webhook can have at most {MAX_EXTRA_HEADERS} custom headers."
        ));
    }
    let mut checked = serde_json::Map::new();
    let mut bytes = 0;
    for (name, value) in headers {
        let name = axum::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("{name:?} isn't a valid header name."))?;
        axum::http::HeaderValue::from_str(value)
            .map_err(|_| format!("The value of {name} isn't a valid header value."))?;
        if delivery::is_reserved_header(name.as_str()) {
            return Err(format!("{name} is set by Monokulo itself."));
        }
        bytes += name.as_str().len() + value.len();
        if bytes > MAX_EXTRA_HEADER_BYTES {
            return Err(format!(
                "Custom headers can be at most {MAX_EXTRA_HEADER_BYTES} bytes in all."
            ));
        }
        checked.insert(name.as_str().to_string(), value.clone().into());
    }
    Ok(serde_json::Value::Object(checked).to_string())
}

/// A webhook just made, and its signing secret: shown once, never again.
pub struct Created {
    pub webhook: WebhookRow,
    pub signing_secret: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// The URL or headers aren't acceptable: the message says why.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Makes a webhook for `store_id` sending to `url` with `headers`, its
/// signing secret encrypted at rest under `key`.
pub async fn create(
    db: &Database,
    key: &crate::crypto::AtRestKey,
    store_id: &ConnectionId,
    url: &str,
    headers: &BTreeMap<String, String>,
) -> Result<Created, CreateError> {
    let url = check_url(url).map_err(CreateError::Invalid)?;
    let extra_headers = check_extra_headers(headers).map_err(CreateError::Invalid)?;
    let id = WebhookId::new(format!("wh_{}", uuid::Uuid::new_v4().simple()));
    let signing_secret = shared::auth::generate_webhook_secret();
    let encrypted = crate::crypto::encrypt(
        key,
        crate::crypto::Binding::WebhookSecret(id.as_str()),
        &signing_secret,
    );
    let webhook = WebhookRow {
        id,
        store_id: store_id.clone(),
        url,
        signing_secret_encrypted: encrypted,
        extra_headers,
        enabled: true,
        created_at: crate::now_unix(),
    };
    let row = webhook.clone();
    db.write(move |db| {
        db.create_webhook(&NewWebhook {
            id: &row.id,
            store_id: &row.store_id,
            url: &row.url,
            signing_secret_encrypted: &row.signing_secret_encrypted,
            extra_headers: &row.extra_headers,
            at: row.created_at,
        })
    })
    .await?;
    tracing::info!(store.id = %webhook.store_id, webhook.id = %webhook.id, "webhook added");
    Ok(Created {
        webhook,
        signing_secret,
    })
}

#[cfg(test)]
mod tests;
