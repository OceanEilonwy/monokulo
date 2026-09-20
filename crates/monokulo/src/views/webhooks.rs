//! `GET/POST /dashboard/connections/{id}/webhooks(/{webhook_id}/delete)` -
//! `http::orders::webhooks_list`/`webhooks_create`/`webhooks_delete`/
//! `render_webhooks_page`.

use maud::{html, Markup};

use super::{layout, PageChrome};

/// One row of the webhooks list page - mirrors the engine's own
/// `WebhookView` field-for-field.
pub struct WebhookRowViewModel {
    pub webhook_id: String,
    pub url: String,
    pub enabled: bool,
    pub created_at: i64,
}

pub struct WebhooksViewModel {
    pub connection_id: String,
    pub webhooks: Vec<WebhookRowViewModel>,
    pub error: Option<String>,
    /// Set only immediately after a successful `POST .../webhooks` - the
    /// engine hands back a real signing secret exactly once, at creation
    /// time, with no way to ever fetch it again after this moment. `None`
    /// on a plain `GET`, and gone again the moment the page is reloaded.
    pub created_webhook_signing_secret: Option<String>,
}

pub fn page(chrome: &PageChrome, data: &WebhooksViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            p { a href=(format!("/dashboard/connections/{}", data.connection_id)) { "← back to store" } }
            h1 { "Webhooks" }

            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }

            @if let Some(secret) = &data.created_webhook_signing_secret {
                div class="box" {
                    h2 { "Webhook created" }
                    p {
                        "Its signing secret (verify the " code { "X-Monokulo-Signature" } " header with this - shown once, right now, and never again):"
                    }
                    pre { (secret) }
                    p class="hint" { "Store it somewhere safe before leaving this page. If you lose it, delete this webhook and create a new one." }
                }
            }

            table {
                thead { tr { th { "URL" } th { "Enabled" } th { "Created" } th {} } }
                tbody {
                    @for webhook in &data.webhooks {
                        tr {
                            td { (webhook.url) }
                            td {
                                @if webhook.enabled {
                                    span class="tag tag-ok" { "enabled" }
                                } @else {
                                    span class="tag tag-unknown" { "disabled" }
                                }
                            }
                            td { (webhook.created_at) }
                            td {
                                form method="post"
                                    action=(format!("/dashboard/connections/{}/webhooks/{}/delete", data.connection_id, webhook.webhook_id))
                                    onsubmit="return confirm('Delete this webhook? Anything relying on it will stop receiving events immediately.');" {
                                    button type="submit" class="btn-secondary" { "Delete" }
                                }
                            }
                        }
                    }
                }
            }
            @if data.webhooks.is_empty() {
                p class="muted" { "No webhooks yet." }
            }

            div class="box" {
                h2 { "Add a webhook" }
                form method="post" action=(format!("/dashboard/connections/{}/webhooks", data.connection_id)) {
                    label {
                        "URL"
                        input type="url" name="url" placeholder="https://your-endpoint.example.com/monokulo-webhook" required;
                        span class="field-help" {
                            "A plain " code { "http(s)://" } " URL your endpoint controls. Private/loopback addresses are "
                            "checked at delivery time, not registration - registering one won't error here, but nothing will ever actually be "
                            "delivered to it."
                        }
                    }
                    label {
                        "Custom headers (optional)"
                        textarea name="extra_headers" rows="3" placeholder="X-Api-Key: your-value\nAnother-Header: another-value" {}
                        span class="field-help" {
                            "One " code { "Header-Name: value" } " pair per line - sent with every delivery to this "
                            "webhook, alongside the signature headers Monokulo always includes."
                        }
                    }
                    button type="submit" { "Add webhook" }
                }
            }
        }
    };
    layout(chrome, "Webhooks - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/connections/conn_1/webhooks")
    }

    #[test]
    fn shows_no_webhooks_message_when_empty() {
        let data = WebhooksViewModel { connection_id: "conn_1".to_string(), webhooks: vec![], error: None, created_webhook_signing_secret: None };
        let html = page(&chrome(), &data).into_string();
        assert!(html.to_lowercase().contains("no webhooks yet"));
    }

    #[test]
    fn lists_webhooks_with_a_delete_form_each() {
        let data = WebhooksViewModel {
            connection_id: "conn_1".to_string(),
            webhooks: vec![WebhookRowViewModel { webhook_id: "wh_1".to_string(), url: "https://example.com/hook".to_string(), enabled: true, created_at: 1000 }],
            error: None,
            created_webhook_signing_secret: None,
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("https://example.com/hook"));
        assert!(html.contains("tag-ok"));
        assert!(html.contains(r#"action="/dashboard/connections/conn_1/webhooks/wh_1/delete""#));
    }

    #[test]
    fn shows_the_signing_secret_exactly_once_after_creation() {
        let data = WebhooksViewModel {
            connection_id: "conn_1".to_string(),
            webhooks: vec![],
            error: None,
            created_webhook_signing_secret: Some("whsec_abc123".to_string()),
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("whsec_abc123"));
        assert!(html.contains("Webhook created"));
    }

    #[test]
    fn shows_the_error_when_present() {
        let data = WebhooksViewModel { connection_id: "conn_1".to_string(), webhooks: vec![], error: Some("Enter a webhook URL.".to_string()), created_webhook_signing_secret: None };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("Enter a webhook URL."));
    }
}
