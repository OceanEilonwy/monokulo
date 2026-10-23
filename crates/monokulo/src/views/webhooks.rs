//! A store's webhooks on its settings page (`docs/design/user-testing/
//! webhooks.html`, variation 2): one health line per webhook, its recent
//! deliveries folded under it (open by itself when one is retrying or gave
//! up), and a delivery's detail as a dialog, or a page without JavaScript.
//!
//! Every action is a plain POST form: "Send again" on a delivery, "Retry
//! failed (N)" on a webhook whose deliveries gave up, adding and deleting a
//! webhook. "Details" and "Delete" are links to pages holding the same
//! content as their dialogs (`ui::Dialog`'s island opens the dialog
//! instead).

use maud::{html, Markup};

use super::time::Clock;
use super::{short_id, DialogContent, PageChrome};
use crate::db::{DeliveryRow, DeliveryState, WebhookHealth, WebhookRow};

/// How many of a webhook's deliveries a page of them shows: in its fold on
/// the settings page, and on the page of all its deliveries.
pub const PAGE: usize = 20;
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

/// One webhook, how it's doing and one page of its deliveries (newest
/// first).
pub struct WebhookEntry {
    pub webhook: WebhookRow,
    pub health: WebhookHealth,
    /// Page `page` (from 0, the newest) of its deliveries.
    pub recent: Vec<DeliveryRow>,
    pub page: usize,
    /// There are older deliveries than these.
    pub has_more: bool,
    /// The plugin that registered it, while it's connected
    /// ("PrestaShop").
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
        format!(
            "/stores/{}/settings?tab=connections#card-webhooks",
            self.store_id
        )
    }
    pub fn add(&self) -> String {
        format!("/stores/{}/settings/webhooks", self.store_id)
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
    /// Page `page` of the webhook's deliveries (0, the newest, is
    /// [`Self::all`]).
    pub fn deliveries_page(&self, webhook: &str, page: usize) -> String {
        match page {
            0 => self.all(webhook),
            page => format!("{}?page={page}", self.all(webhook)),
        }
    }
    pub fn detail(&self, webhook: &str, delivery: i64) -> String {
        format!("{}/deliveries/{delivery}", self.webhook(webhook))
    }
    pub fn send_again(&self, webhook: &str, delivery: i64) -> String {
        format!("{}/send-again", self.detail(webhook, delivery))
    }
    fn order(&self, order: &str) -> String {
        format!("/stores/{}/orders/{order}", self.store_id)
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
fn state_tag(state: DeliveryState) -> ui::Tag {
    let (tone, word) = match state {
        DeliveryState::Delivered => (ui::TagTone::Ok, "delivered"),
        DeliveryState::Retrying => (ui::TagTone::Slow, "retrying"),
        DeliveryState::Queued => (ui::TagTone::Unknown, "queued"),
        DeliveryState::GaveUp => (ui::TagTone::Error, "gave up"),
    };
    ui::Tag::new(word).tone(tone)
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
                (ui::Tag::new("gave up").tone(ui::TagTone::Error))
                (ui::Hint::new(html! {
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
                }).inline(true))
                form method="post" action=(paths.retry_failed(entry.webhook.id.as_str())) class="inline-form" {
                    (ui::Button::new(format!("Retry failed ({})", health.gave_up))
                        .size(ui::ControlSize::Compact)
                        .kind(ui::ButtonType::Submit))
                }
            } @else if health.retrying > 0 {
                (ui::Tag::new("retrying").tone(ui::TagTone::Slow))
                (ui::Hint::new(html! {
                    @if let Some(next) = health.next_attempt_at {
                        "next try at " (clock.text(next)) ", " (clock.relative(next))
                    }
                    @if let Some(worst) = &health.worst_retrying {
                        " · attempt " (worst.attempt_count) " of " (card.max_attempts) " failed ("
                        (failure(worst.last_status_code, worst.last_error.as_deref())) ")"
                    }
                    " · " (plural(health.waiting, "delivery waiting", "deliveries waiting"))
                }).inline(true))
            } @else if health.waiting > 0 {
                (ui::Tag::new("sending").tone(ui::TagTone::Unknown))
                (ui::Hint::new(format!("{} about to be sent", plural(health.waiting, "delivery", "deliveries"))).inline(true))
            } @else if let Some(last) = &health.latest_delivered {
                (ui::Tag::new("delivering").tone(ui::TagTone::Ok))
                (ui::Hint::new(html! {
                    @if let Some(at) = last.delivered_at { "last " (clock.relative(at)) }
                    @if let Some(status) = last.last_status_code {
                        " · " (status)
                        @if let Some(ms) = last.last_duration_ms { " in " (duration(ms)) }
                    }
                    " · " (super::scaling::thousands(health.delivered_recently)) " delivered in the last " (RECENT_DAYS) " days"
                }).inline(true))
            } @else {
                (ui::Tag::new("no deliveries yet").tone(ui::TagTone::Unknown))
                (ui::Hint::new("It gets the store's next order event.").inline(true))
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
    let head = html! { tr {
        th { "Status" } th { "Event" } th { "Order" } th { "Time" }
        th { "Response" } th { "Took" } th { "Attempt" } th { span class="visually-hidden" { "Actions" } }
    } };
    let rows = html! {
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
                        // One word where it can be: the row stays narrow.
                        (short_error(delivery.last_error.as_deref().unwrap_or_default())
                            .trim_start_matches("connection ")
                            .to_string())
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
                // Two fixed slots, the same in every row: Details,
                // then Send again or nothing, so each lines up.
                td class="row-acts" {
                    div class="act-slots" {
                        a href=(paths.detail(webhook.id.as_str(), delivery.id))
                            data-opens-dialog=(dialog_id(delivery.id)) { "Details" }
                        @if state == DeliveryState::GaveUp {
                            form method="post" action=(paths.send_again(webhook.id.as_str(), delivery.id)) class="inline-form" {
                                // An icon, so the row stays narrow enough to show it.
                                (ui::IconButton::new("Send again", super::wallets::regenerate_icon()).kind(ui::ButtonType::Submit))
                            }
                        } @else {
                            span class="act-slot" aria-hidden="true" {}
                        }
                    }
                }
            }
        }
    };
    ui::Table::new(rows)
        .head(head)
        .look(ui::TableLook::Stack)
        .attr("data-table", "deliveries")
        .render()
}

/// One page of the webhook's deliveries with "← Newer" and "Older →"
/// links: plain links to the page of all its deliveries, which fixi turns
/// into fetching just this part, in place, wherever it is.
pub fn deliveries_results(
    paths: &Paths<'_>,
    clock: &Clock,
    max_attempts: u32,
    entry: &WebhookEntry,
) -> Markup {
    let id = entry.webhook.id.as_str();
    let target = format!("#deliveries-{id}");
    let link = |page: usize, rel: &'static str, text: &'static str| {
        let href = paths.deliveries_page(id, page);
        html! {
            a href=(href) rel=(rel) fx-action=(href) fx-target=(target) fx-swap="outerHTML" { (text) }
        }
    };
    html! {
        div id=(format!("deliveries-{id}")) class="deliveries-results" {
            @if entry.recent.is_empty() {
                p class="muted" { @if entry.page == 0 { "Nothing sent yet." } @else { "No older deliveries." } }
            } @else {
                (deliveries_table(paths, clock, max_attempts, &entry.webhook, &entry.recent))
            }
            @if entry.page > 0 || entry.has_more {
                p class="orders-pages" {
                    @if entry.page > 0 { (link(entry.page - 1, "prev", "← Newer")) }
                    @if entry.page > 0 && entry.has_more { " · " }
                    @if entry.has_more { (link(entry.page + 1, "next", "Older →")) }
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
                (ui::Panel::new(html! {
                    h3 { "Webhook created" }
                    p {
                        "Its signing secret (verify the " code { "X-Monokulo-Signature" } " header with this - shown once, right now, and never again):"
                    }
                    pre { (secret) }
                    (ui::Hint::new("Store it somewhere safe before leaving this page. If you lose it, delete this webhook and add a new one."))
                }).flag("data-webhook-secret"))
            }
            @if card.unavailable {
                (ui::Alert::new("Couldn't read this store's webhooks. Reload the page to try again.", ui::Tone::Error))
            } @else if card.webhooks.is_empty() {
                p class="muted" { "No webhooks yet. Monokulo can tell your server when an order is paid, confirming or expired." }
            }
            @for entry in &card.webhooks {
                @let id = entry.webhook.id.as_str();
                div class="wh" id=(format!("webhook-{id}")) {
                    div class="wh-head" {
                        code class="wh-url" { (entry.webhook.url) }
                        @if let Some(plugin) = &entry.plugin {
                            (ui::Tag::new(plugin.as_str()).tone(ui::TagTone::Unknown))
                        }
                        span class="card-spacer" {}
                        (ui::LinkButton::new("Delete", paths.delete(id))
                            .variant(ui::ButtonVariant::Danger)
                            .size(ui::ControlSize::Compact)
                            .attr("data-opens-dialog", delete_dialog_id(id)))
                    }
                    (health_line(card, entry))
                    @if !entry.recent.is_empty() {
                        (ui::Disclosure::new(
                            html! { "Recent deliveries " (ui::Hint::new(format!("({PAGE} a page)")).inline(true)) },
                            html! {
                                (deliveries_results(&paths, &card.clock, card.max_attempts, entry))
                                (ui::Hint::new(html! { a href=(paths.all(id)) { "All deliveries for this webhook" } }))
                            },
                        )
                        .open(entry.health.retrying > 0 || entry.health.gave_up > 0)
                        .attr("data-disclosure", "deliveries"))
                    }
                }
            }
            h4 { "Add a webhook" }
            form method="post" action=(paths.add()) {
                (super::settings::Field::new("URL", "webhook-url")
                    .help(Some("webhook-url-help"), html! {
                        "Where Monokulo sends this store's order events: an " code { "http(s)://" } " URL your server answers. "
                        "Private and loopback addresses are refused at delivery."
                    })
                    .render(html! {
                        (ui::Input::new("url")
                            .id("webhook-url")
                            .kind(ui::InputType::Url)
                            .described_by("webhook-url-help")
                            .placeholder("https://your-endpoint.example.com/monokulo-webhook")
                            .required(true))
                    }))
                (super::settings::Field::new("Custom headers (optional)", "webhook-headers")
                    .help(Some("webhook-headers-help"), html! {
                        "One " code { "Header-Name: value" } " pair per line - sent with every delivery to this "
                        "webhook, alongside the signature headers Monokulo always includes."
                    })
                    .render(html! {
                        (ui::TextArea::new("extra_headers")
                            .id("webhook-headers")
                            .described_by("webhook-headers-help")
                            .rows(3usize)
                            .placeholder("X-Api-Key: your-value\nAnother-Header: another-value"))
                    }))
                (ui::FormActions::new(ui::Button::new("Add webhook").kind(ui::ButtonType::Submit)))
            }
        },
    )
}

/// Every dialog the card's links open: each webhook's delete, each recent
/// delivery's detail.
pub fn dialogs(card: &WebhooksCard) -> Markup {
    html! {
        @for entry in &card.webhooks {
            (ui::Dialog::new(delete_dialog_id(entry.webhook.id.as_str()), format!("{}-delete-title", entry.webhook.id),
                delete_content(&card.store_id, entry).in_dialog()))
            @for delivery in &entry.recent {
                (ui::Dialog::new(dialog_id(delivery.id), format!("delivery-{}-title", delivery.id),
                    detail_content(&card.store_id, &card.clock, card.max_attempts, &entry.webhook, delivery).in_dialog())
                    .size(ui::DialogSize::Wide))
            }
        }
    }
}

/// "Delete this webhook?": what stops, and the button.
pub fn delete_content(store_id: &str, entry: &WebhookEntry) -> DialogContent {
    let paths = Paths::new(store_id);
    let id = entry.webhook.id.as_str();
    let body = html! {
        p { code class="wh-url" { (entry.webhook.url) } " stops receiving this store's order events at once, and its deliveries are deleted with it." }
        @if let Some(plugin) = &entry.plugin {
            (ui::Alert::new(
                format!("The {plugin} plugin uses it: orders won't be updated in the shop until the plugin connects again."),
                ui::Tone::Error,
            )
            .quiet(true))
        }
        @if entry.health.waiting > 0 {
            p { (plural(entry.health.waiting, "delivery still waiting is", "deliveries still waiting are")) " never sent." }
        }
        form method="post" action=(paths.delete(id)) {
            (ui::DialogActions::new(ui::Slot::many([
                ui::LinkButton::new("Cancel", paths.settings()).flag("data-closes-dialog").into(),
                ui::Button::new("Delete webhook")
                    .variant(ui::ButtonVariant::Danger)
                    .kind(ui::ButtonType::Submit)
                    .into(),
            ])))
        }
    };
    DialogContent {
        title_id: format!("{id}-delete-title"),
        title: html! { "Delete this webhook?" },
        aside: None,
        body,
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
) -> DialogContent {
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
    let body = indented_json(&delivery.body);
    let mut request = format!(
        "POST {path}\nContent-Type: application/json\nX-Monokulo-Signature: {signature}\nX-Monokulo-Event: {}\nX-Monokulo-Event-Id: {}\n",
        delivery.event_type, delivery.event_id
    );
    for name in &extra {
        request.push_str(&format!("{name}: •••\n"));
    }
    request.push('\n');
    request.push_str(&body);
    let fact = |term: &str, value: Markup| ui::Fact {
        term: term.into(),
        value: value.into(),
    };
    let mut facts = vec![
        fact(
            "Endpoint",
            html! { span class="endpoint" { (webhook.url) } },
        ),
        fact(
            "Order",
            html! { a href=(paths.order(delivery.order_id.as_str())) { (short_id(delivery.order_id.as_str())) } },
        ),
    ];
    match state {
        DeliveryState::Retrying | DeliveryState::Queued => {
            if let Some(next) = delivery.next_attempt_at {
                facts.push(fact(
                    "Next try",
                    html! { (clock.text(next)) " (" (clock.relative(next)) ")" },
                ));
            }
        }
        DeliveryState::Delivered => {
            if let Some(at) = delivery.delivered_at {
                facts.push(fact("Delivered", html! { (clock.text(at)) }));
            }
        }
        DeliveryState::GaveUp => {
            if let Some(at) = delivery.gave_up_at {
                facts.push(fact(
                    "Gave up",
                    html! { (clock.text(at)) ", after " (plural(u64::from(delivery.attempt_count), "attempt", "attempts")) },
                ));
            }
        }
    }
    // The title, its status beside it, then what it is, one fact a line.
    let body = html! {
        (ui::Facts::new(facts).attr("data-facts", "delivery"))
        h4 class="mini-h" { "Attempts" }
        @if delivery.attempts.is_empty() {
            p class="muted" { "Not tried yet." }
        } @else {
            (ui::Table::new(html! {
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
            })
            .head(html! { tr { th { "#" } th { "Sent" } th { "Response" } th { "Took" } } })
            .look(ui::TableLook::Stack))
        }
        (ui::Disclosure::new("Request", html! {
            pre { (request) }
            (ui::Hint::new("The body is laid out here for reading; it's sent on one line, exactly as signed. The signing secret is never shown again: verify with the copy you saved."))
        })
        .attr("data-disclosure", "request"))
        @if let Some(response) = &delivery.last_response {
            (ui::Disclosure::new(
                html! {
                    "Last response "
                    (ui::Hint::new(format!("(first {} bytes)", crate::webhooks::delivery::RESPONSE_EXCERPT_BYTES)).inline(true))
                },
                html! { pre { (response) } },
            )
            .attr("data-disclosure", "request"))
        } @else if let Some(error) = &delivery.last_error {
            (ui::Disclosure::new("Last error", html! { pre { (error) } }).attr("data-disclosure", "request"))
        }
        form method="post" action=(paths.send_again(webhook.id.as_str(), delivery.id)) {
            (ui::DialogActions::new(ui::Slot::many([
                ui::LinkButton::new("Close", paths.settings()).flag("data-closes-dialog").into(),
                ui::Button::new("Send again now").kind(ui::ButtonType::Submit).into(),
            ])))
        }
    };
    DialogContent {
        title_id: format!("delivery-{}-title", delivery.id),
        title: html! { span { "Delivery of " span class="event-name" { (delivery.event_type) } } },
        aside: Some(state_tag(state).render()),
        body,
    }
}

/// Compact JSON laid out one field per line, its fields in the order they
/// were sent (a parsed `Value` would sort them). Anything that isn't a JSON
/// object comes back as it is.
fn indented_json(json: &str) -> String {
    if serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json).is_err() {
        return json.to_string();
    }
    fn newline(out: &mut String, depth: usize) {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    }
    let mut out = String::new();
    let mut depth = 0usize;
    let (mut in_string, mut escaped) = (false, false);
    for c in json.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                depth += 1;
                out.push(c);
                newline(&mut out, depth);
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// A page of the store's settings.
fn page(store_name: &str, title: &str, header: ui::PageHeader) -> super::Page {
    super::Page::new(format!("{title} - {store_name} - Monokulo"), header).class("settings-page")
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
    let (header, body) = detail_content(store_id, &chrome.clock, max_attempts, webhook, delivery)
        .page(
            super::store_crumbs(store_id, store_name, false),
            ui::DialogSize::Wide,
        );
    page(store_name, "Webhook delivery", header).render(chrome, body)
}

/// `GET …/settings/webhooks/{webhook}/delete`: the question as a page.
pub fn delete_page(chrome: &PageChrome, store_name: &str, entry: &WebhookEntry) -> Markup {
    let store_id = entry.webhook.store_id.as_str();
    let (header, body) = delete_content(store_id, entry).page(
        super::store_crumbs(store_id, store_name, false),
        ui::DialogSize::Confirm,
    );
    page(store_name, "Delete webhook", header).render(chrome, body)
}

/// `GET …/settings/webhooks/{webhook}/deliveries[?page=N]`: a page of
/// its deliveries, newest first.
pub fn all_page(
    chrome: &PageChrome,
    store_name: &str,
    max_attempts: u32,
    entry: &WebhookEntry,
) -> Markup {
    let store_id = entry.webhook.store_id.as_str();
    let paths = Paths::new(store_id);
    let header =
        ui::PageHeader::new("Deliveries").crumbs(super::store_crumbs(store_id, store_name, false));
    let body = html! {
        div class="deliveries-page" {
            p { code class="wh-url" { (entry.webhook.url) } }
            (ui::Hint::new(format!("Newest first, {PAGE} a page.")))
            (deliveries_results(&paths, &chrome.clock, max_attempts, entry))
            p { a href=(paths.settings()) { "Back to the store's settings" } }
        }
    };
    page(store_name, "Webhook deliveries", header).render(chrome, body)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_body_is_laid_out_in_the_order_it_was_sent() {
        assert_eq!(
            super::indented_json(r#"{"b":1,"a":"x,{y}\"","s":{"id":"1"}}"#),
            "{\n  \"b\": 1,\n  \"a\": \"x,{y}\\\"\",\n  \"s\": {\n    \"id\": \"1\"\n  }\n}"
        );
        assert_eq!(super::indented_json("not json"), "not json");
    }
}
