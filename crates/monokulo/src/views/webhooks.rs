//! A store's webhooks on its settings page (`docs/design/user-testing/
//! webhooks.html`, variation 2): one health line per webhook, its recent
//! deliveries folded under it (open by itself when one is retrying or gave
//! up), and a delivery's detail as a dialog, or a page without JavaScript.
//!
//! Every action is a plain POST form: "Send again" on a delivery, "Retry
//! failed (N)" on a webhook whose deliveries gave up, adding and deleting a
//! webhook. "Details" and "Delete…" are links to pages holding the same
//! content as their dialogs (`confirm-dialogs.js` opens the dialog
//! instead).

use maud::{html, Markup};

use super::time::Clock;
use super::{layout, short_id, PageChrome};
use crate::db::{DeliveryRow, DeliveryState, WebhookHealth, WebhookRow};

/// How many deliveries each webhook shows on the settings page.
pub const RECENT: usize = 20;
/// How many the page of all a webhook's deliveries shows.
pub const ALL: usize = 200;
/// "Delivered recently" is in the last this many days.
pub const RECENT_DAYS: i64 = 30;

/// The Webhooks card's data.
pub struct WebhooksCard {
    pub store_id: String,
    pub store_name: String,
    pub clock: Clock,
    /// `webhooks.max_attempts`.
    pub max_attempts: u32,
    pub webhooks: Vec<WebhookEntry>,
    /// They couldn't be read: the card says so.
    pub unavailable: bool,
    /// A webhook was just made: its signing secret, shown this once.
    pub created_secret: Option<String>,
}

/// One webhook, how it's doing and its latest deliveries (newest first).
pub struct WebhookEntry {
    pub webhook: WebhookRow,
    pub health: WebhookHealth,
    pub recent: Vec<DeliveryRow>,
    /// The plugin that registered it, while it's connected
    /// ("WooCommerce").
    pub plugin: Option<String>,
}

/// Where a store's webhooks' pages and actions are.
pub struct Paths<'a> {
    store_id: &'a str,
}

impl<'a> Paths<'a> {
    pub fn new(store_id: &'a str) -> Self {
        Paths { store_id }
    }
    pub fn settings(&self) -> String {
        format!("/dashboard/stores/{}/settings#card-webhooks", self.store_id)
    }
    pub fn add(&self) -> String {
        format!("/dashboard/stores/{}/settings/webhooks", self.store_id)
    }
    fn webhook(&self, webhook: &str) -> String {
        format!("{}/{webhook}", self.add())
    }
    pub fn delete(&self, webhook: &str) -> String {
        format!("{}/delete", self.webhook(webhook))
    }
    pub fn retry_failed(&self, webhook: &str) -> String {
        format!("{}/retry-failed", self.webhook(webhook))
    }
    pub fn all(&self, webhook: &str) -> String {
        format!("{}/deliveries", self.webhook(webhook))
    }
    pub fn detail(&self, webhook: &str, delivery: i64) -> String {
        format!("{}/deliveries/{delivery}", self.webhook(webhook))
    }
    pub fn send_again(&self, webhook: &str, delivery: i64) -> String {
        format!("{}/send-again", self.detail(webhook, delivery))
    }
    fn order(&self, order: &str) -> String {
        format!("/dashboard/stores/{}/orders/{order}", self.store_id)
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", super::scaling::thousands(n))
    }
}

/// `184 ms`, `5.0 s`.
pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
    }
}

/// A delivery's status as a tag: its word and its tint.
fn state_tag(state: DeliveryState) -> Markup {
    let (class, word) = match state {
        DeliveryState::Delivered => ("tag-ok", "delivered"),
        DeliveryState::Retrying => ("tag-slow", "retrying"),
        DeliveryState::Queued => ("tag-unknown", "queued"),
        DeliveryState::GaveUp => ("tag-error", "gave up"),
    };
    html! { span class=(format!("tag {class}")) { (word) } }
}

/// What went wrong, briefly: a status code, or the error's first words.
fn failure(status: Option<u16>, error: Option<&str>) -> String {
    match (status, error) {
        (Some(status), _) => status.to_string(),
        (None, Some(error)) => short_error(error),
        (None, None) => "no answer".to_string(),
    }
}

/// An error as a few words: `refused`, `timed out`, `private address`.
fn short_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("refused") {
        "connection refused".into()
    } else if lower.contains("timed out") || lower.contains("no answer within") {
        "timed out".into()
    } else if lower.contains("private or loopback") {
        "private address".into()
    } else if lower.contains("resolve") || lower.contains("dns") {
        "host not found".into()
    } else if lower.contains("could not connect") {
        "could not connect".into()
    } else {
        error.split(':').next().unwrap_or(error).trim().to_string()
    }
}

/// The webhook's one line: how it's doing.
fn health_line(card: &WebhooksCard, entry: &WebhookEntry) -> Markup {
    let health = &entry.health;
    let paths = Paths::new(&card.store_id);
    let clock = &card.clock;
    html! {
        p class="wh-health" {
            @if health.gave_up > 0 {
                span class="tag tag-error" { "gave up" }
                span class="hint" {
                    (plural(health.gave_up, "delivery", "deliveries"))
                    @if let Some(last) = &health.latest_gave_up {
                        @if let Some(at) = last.gave_up_at {
                            " · last at " (clock.text(at))
                        }
                        " after " (plural(u64::from(last.attempt_count), "attempt", "attempts"))
                        " (" (failure(last.last_status_code, last.last_error.as_deref())) ")"
                    }
                    @if health.waiting > 0 {
                        " · " (plural(health.waiting, "more waiting", "more waiting"))
                    }
                }
                form method="post" action=(paths.retry_failed(entry.webhook.id.as_str())) class="inline-form" {
                    button type="submit" class="btn-sm" { "Retry failed (" (health.gave_up) ")" }
                }
            } @else if health.retrying > 0 {
                span class="tag tag-slow" { "retrying" }
                span class="hint" {
                    @if let Some(next) = health.next_attempt_at {
                        "next try at " (clock.text(next)) ", " (clock.relative(next))
                    }
                    @if let Some(worst) = &health.worst_retrying {
                        " · attempt " (worst.attempt_count) " of " (card.max_attempts) " failed ("
                        (failure(worst.last_status_code, worst.last_error.as_deref())) ")"
                    }
                    " · " (plural(health.waiting, "delivery waiting", "deliveries waiting"))
                }
            } @else if health.waiting > 0 {
                span class="tag tag-unknown" { "sending" }
                span class="hint" { (plural(health.waiting, "delivery", "deliveries")) " about to be sent" }
            } @else if let Some(last) = &health.latest_delivered {
                span class="tag tag-ok" { "delivering" }
                span class="hint" {
                    @if let Some(at) = last.delivered_at { "last " (clock.relative(at)) }
                    @if let Some(status) = last.last_status_code {
                        " · " (status)
                        @if let Some(ms) = last.last_duration_ms { " in " (duration(ms)) }
                    }
                    " · " (super::scaling::thousands(health.delivered_recently)) " delivered in the last " (RECENT_DAYS) " days"
                }
            } @else {
                span class="tag tag-unknown" { "no deliveries yet" }
                span class="hint" { "It gets the store's next order event." }
            }
        }
    }
}

/// The deliveries table: status, event, order, time, response, how long
/// it took, which attempt, and "Details" (and "Send again" on one that
/// gave up).
pub fn deliveries_table(
    paths: &Paths<'_>,
    clock: &Clock,
    max_attempts: u32,
    webhook: &WebhookRow,
    deliveries: &[DeliveryRow],
) -> Markup {
    html! {
        div class="table-scroll" {
            table class="table-stack del-table" {
                thead { tr {
                    th { "Status" } th { "Event" } th { "Order" } th { "Time" }
                    th { "Response" } th { "Took" } th { "Attempt" } th { span class="visually-hidden" { "Actions" } }
                } }
                tbody {
                    @for delivery in deliveries {
                        @let state = delivery.state();
                        tr {
                            td data-label="Status" { (state_tag(state)) }
                            td data-label="Event" { code { (delivery.event_type) } }
                            td data-label="Order" {
                                a href=(paths.order(delivery.order_id.as_str())) { (short_id(delivery.order_id.as_str())) }
                            }
                            td data-label="Time" { (clock.time(delivery.last_attempt_at.unwrap_or(delivery.created_at))) }
                            td data-label="Response" {
                                @match delivery.last_status_code {
                                    Some(status) => code { (status) },
                                    None => code { "—" },
                                }
                            }
                            td data-label="Took" {
                                @if delivery.last_status_code.is_none() && delivery.last_error.is_some() {
                                    (short_error(delivery.last_error.as_deref().unwrap_or_default()))
                                } @else if let Some(ms) = delivery.last_duration_ms {
                                    (duration(ms))
                                } @else {
                                    span class="muted" { "-" }
                                }
                            }
                            td data-label="Attempt" {
                                @match state {
                                    DeliveryState::Queued => span class="muted" { "-" },
                                    DeliveryState::Delivered => (delivery.attempt_count),
                                    _ => { (delivery.attempt_count) " of " (max_attempts) },
                                }
                            }
                            td class="row-acts" {
                                a href=(paths.detail(webhook.id.as_str(), delivery.id))
                                    data-opens-dialog=(dialog_id(delivery.id)) { "Details" }
                                @if state == DeliveryState::GaveUp {
                                    form method="post" action=(paths.send_again(webhook.id.as_str(), delivery.id)) class="inline-form" {
                                        button type="submit" class="btn-sm" { "Send again" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn dialog_id(delivery: i64) -> String {
    format!("delivery-{delivery}-dialog")
}

fn delete_dialog_id(webhook: &str) -> String {
    format!("{webhook}-delete-dialog")
}

/// The Webhooks card. `error`: why the last action on it was refused.
pub fn card(card: &WebhooksCard, error: Markup) -> Markup {
    let paths = Paths::new(&card.store_id);
    let meta = (!card.webhooks.is_empty())
        .then(|| plural(card.webhooks.len() as u64, "webhook", "webhooks"));
    super::settings::plain_card_with_meta(
        "webhooks",
        "Webhooks",
        meta.as_deref(),
        html! {
            (error)
            @if let Some(secret) = &card.created_secret {
                div class="box" data-webhook-secret {
                    h3 { "Webhook created" }
                    p {
                        "Its signing secret (verify the " code { "X-Monokulo-Signature" } " header with this - shown once, right now, and never again):"
                    }
                    pre { (secret) }
                    p class="hint" { "Store it somewhere safe before leaving this page. If you lose it, delete this webhook and add a new one." }
                }
            }
            @if card.unavailable {
                p class="error" role="alert" { "Couldn't read this store's webhooks. Reload the page to try again." }
            } @else if card.webhooks.is_empty() {
                p class="muted" { "No webhooks yet. Monokulo can tell your server when an order is paid, confirming or expired." }
            }
            @for entry in &card.webhooks {
                @let id = entry.webhook.id.as_str();
                div class="wh" id=(format!("webhook-{id}")) {
                    div class="wh-head" {
                        code class="wh-url" { (entry.webhook.url) }
                        @if let Some(plugin) = &entry.plugin {
                            span class="tag tag-unknown" { (plugin) }
                        }
                        span class="card-spacer" {}
                        a class="btn btn-sm" href=(paths.delete(id)) data-opens-dialog=(delete_dialog_id(id)) { "Delete…" }
                    }
                    (health_line(card, entry))
                    @if !entry.recent.is_empty() {
                        details class="wh-deliveries" open[entry.health.retrying > 0 || entry.health.gave_up > 0] {
                            summary { "Recent deliveries " span class="hint" { "(last " (RECENT) ")" } }
                            (deliveries_table(&paths, &card.clock, card.max_attempts, &entry.webhook, &entry.recent))
                            p class="hint" { a href=(paths.all(id)) { "All deliveries for this webhook" } }
                        }
                    }
                }
            }
            h4 { "Add a webhook" }
            form method="post" action=(paths.add()) {
                label {
                    "URL"
                    input type="url" name="url" placeholder="https://your-endpoint.example.com/monokulo-webhook" required;
                    span class="field-help" {
                        "Where Monokulo sends this store's order events: an " code { "http(s)://" } " URL your server answers. "
                        "Private and loopback addresses are refused at delivery."
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
                div class="form-actions" {
                    button type="submit" { "Add webhook" }
                }
            }
        },
    )
}

/// Every dialog the card's links open: each webhook's delete, each recent
/// delivery's detail.
pub fn dialogs(card: &WebhooksCard) -> Markup {
    html! {
        @for entry in &card.webhooks {
            dialog id=(delete_dialog_id(entry.webhook.id.as_str())) class="settings-dialog confirm-dialog"
                aria-labelledby=(format!("{}-delete-title", entry.webhook.id)) {
                (delete_content(&card.store_id, entry, true))
            }
            @for delivery in &entry.recent {
                dialog id=(dialog_id(delivery.id)) class="settings-dialog delivery-dialog"
                    aria-labelledby=(format!("delivery-{}-title", delivery.id)) {
                    (detail_content(&card.store_id, &card.clock, card.max_attempts, &entry.webhook, delivery, true))
                }
            }
        }
    }
}

fn heading(id: &str, title: Markup, in_dialog: bool) -> Markup {
    html! {
        @if in_dialog {
            h2 id=(id) class="dialog-title" {
                (title)
                button type="button" class="dialog-x" aria-label="Close" data-closes-dialog { "×" }
            }
        } @else {
            h1 id=(id) class="dialog-title" { (title) }
        }
    }
}

/// "Delete this webhook?": what stops, and the button.
pub fn delete_content(store_id: &str, entry: &WebhookEntry, in_dialog: bool) -> Markup {
    let paths = Paths::new(store_id);
    let id = entry.webhook.id.as_str();
    html! {
        (heading(&format!("{id}-delete-title"), html! { "Delete this webhook?" }, in_dialog))
        p { code class="wh-url" { (entry.webhook.url) } " stops receiving this store's order events at once, and its deliveries are deleted with it." }
        @if let Some(plugin) = &entry.plugin {
            p class="error" { "The " (plugin) " plugin uses it: orders won't be updated in the shop until the plugin connects again." }
        }
        @if entry.health.waiting > 0 {
            p { (plural(entry.health.waiting, "delivery still waiting is", "deliveries still waiting are")) " never sent." }
        }
        form method="post" action=(paths.delete(id)) {
            div class="dialog-actions" {
                a class="btn" href=(paths.settings()) data-closes-dialog { "Cancel" }
                button type="submit" class="btn-danger" { "Delete webhook" }
            }
        }
    }
}

/// A delivery's detail: its attempts, the request (the signature header
/// shown, the secret never), the start of the last answer, and "Send
/// again".
pub fn detail_content(
    store_id: &str,
    clock: &Clock,
    max_attempts: u32,
    webhook: &WebhookRow,
    delivery: &DeliveryRow,
    in_dialog: bool,
) -> Markup {
    let paths = Paths::new(store_id);
    let state = delivery.state();
    let path = url::Url::parse(&webhook.url).map_or_else(
        |_| webhook.url.clone(),
        |url| {
            let mut path = url.path().to_string();
            if let Some(query) = url.query() {
                path.push('?');
                path.push_str(query);
            }
            path
        },
    );
    let signature = delivery
        .attempts
        .last()
        .map(|a| a.signature.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "t=<when it's sent>,v1=<HMAC-SHA256>".to_string());
    let extra: Vec<String> =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&webhook.extra_headers)
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
    let body = serde_json::from_str::<serde_json::Value>(&delivery.body)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| delivery.body.clone());
    let mut request = format!(
        "POST {path}\nContent-Type: application/json\nX-Monokulo-Signature: {signature}\nX-Monokulo-Event: {}\nX-Monokulo-Event-Id: {}\n",
        delivery.event_type, delivery.event_id
    );
    for name in &extra {
        request.push_str(&format!("{name}: •••\n"));
    }
    request.push('\n');
    request.push_str(&body);
    html! {
        (heading(&format!("delivery-{}-title", delivery.id), html! { "Delivery of " code { (delivery.event_type) } }, in_dialog))
        p {
            "To " code class="wh-url" { (webhook.url) } " for order "
            a href=(paths.order(delivery.order_id.as_str())) { (short_id(delivery.order_id.as_str())) } ". "
            (state_tag(state)) " "
            @match state {
                DeliveryState::Retrying | DeliveryState::Queued => {
                    @if let Some(next) = delivery.next_attempt_at { "next try at " (clock.text(next)) ", " (clock.relative(next)) "." }
                }
                DeliveryState::Delivered => {
                    @if let Some(at) = delivery.delivered_at { "at " (clock.text(at)) "." }
                }
                DeliveryState::GaveUp => {
                    @if let Some(at) = delivery.gave_up_at {
                        "at " (clock.text(at)) " after " (plural(u64::from(delivery.attempt_count), "attempt", "attempts")) "."
                    }
                }
            }
        }
        h4 class="mini-h" { "Attempts" }
        @if delivery.attempts.is_empty() {
            p class="muted" { "Not tried yet." }
        } @else {
            div class="table-scroll" {
                table class="table-stack" {
                    thead { tr { th { "#" } th { "Sent" } th { "Response" } th { "Took" } } }
                    tbody {
                        @for attempt in delivery.attempts.iter().rev() {
                            tr {
                                td data-label="#" { (attempt.n) @if state != DeliveryState::Delivered && attempt.n > 0 { span class="muted" { " of " (max_attempts) } } }
                                td data-label="Sent" { (clock.time(attempt.at)) }
                                td data-label="Response" {
                                    @match attempt.status {
                                        Some(status) => code {
                                            (status)
                                            @if let Some(reason) = axum::http::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()) { " " (reason) }
                                        },
                                        None => { (attempt.error.as_deref().map_or_else(|| "no answer".to_string(), short_error)) },
                                    }
                                }
                                td data-label="Took" { (duration(attempt.ms)) }
                            }
                        }
                    }
                }
            }
        }
        details class="req" {
            summary { "Request" }
            pre { (request) }
            p class="hint" { "The body is shown formatted; it's sent on one line, exactly as signed. The signing secret is never shown again: verify with the copy you saved." }
        }
        @if let Some(response) = &delivery.last_response {
            details class="req" {
                summary { "Last response " span class="hint" { "(first " (crate::webhooks::delivery::RESPONSE_EXCERPT_BYTES) " bytes)" } }
                pre { (response) }
            }
        } @else if let Some(error) = &delivery.last_error {
            details class="req" {
                summary { "Last error" }
                pre { (error) }
            }
        }
        form method="post" action=(paths.send_again(webhook.id.as_str(), delivery.id)) {
            div class="dialog-actions" {
                a class="btn" href=(paths.settings()) data-closes-dialog { "Close" }
                button type="submit" { "Send again now" }
            }
        }
    }
}

fn page(
    chrome: &PageChrome,
    store_id: &str,
    store_name: &str,
    title: &str,
    content: Markup,
) -> Markup {
    let body = html! {
        div class="wrap settings-page" {
            (super::store_breadcrumb(store_id, store_name, false))
            div class="delivery-page" { (content) }
        }
    };
    layout(chrome, &format!("{title} - {store_name} - Monokulo"), body)
}

/// `GET …/settings/webhooks/{webhook}/deliveries/{delivery}`: the detail
/// as a page.
pub fn detail_page(
    chrome: &PageChrome,
    store_name: &str,
    max_attempts: u32,
    webhook: &WebhookRow,
    delivery: &DeliveryRow,
) -> Markup {
    let store_id = webhook.store_id.as_str();
    page(
        chrome,
        store_id,
        store_name,
        "Webhook delivery",
        detail_content(
            store_id,
            &chrome.clock,
            max_attempts,
            webhook,
            delivery,
            false,
        ),
    )
}

/// `GET …/settings/webhooks/{webhook}/delete`: the question as a page.
pub fn delete_page(chrome: &PageChrome, store_name: &str, entry: &WebhookEntry) -> Markup {
    let store_id = entry.webhook.store_id.as_str();
    page(
        chrome,
        store_id,
        store_name,
        "Delete webhook",
        html! { div class="confirm-dialog-page" { (delete_content(store_id, entry, false)) } },
    )
}

/// `GET …/settings/webhooks/{webhook}/deliveries`: its latest [`ALL`]
/// deliveries.
pub fn all_page(
    chrome: &PageChrome,
    store_name: &str,
    max_attempts: u32,
    entry: &WebhookEntry,
) -> Markup {
    let store_id = entry.webhook.store_id.as_str();
    let paths = Paths::new(store_id);
    page(
        chrome,
        store_id,
        store_name,
        "Webhook deliveries",
        html! {
            h1 { "Deliveries" }
            p { code class="wh-url" { (entry.webhook.url) } }
            @if entry.recent.is_empty() {
                p class="muted" { "Nothing sent yet." }
            } @else {
                p class="hint" { "The latest " (entry.recent.len()) ", newest first." }
                (deliveries_table(&paths, &chrome.clock, max_attempts, &entry.webhook, &entry.recent))
            }
            p { a href=(paths.settings()) { "Back to the store's settings" } }
        },
    )
}
