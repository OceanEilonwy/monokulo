//! A connected app's own page (`/stores/{id}/settings/apps/{app_id}`,
//! `http::apps::page`), where the Apps card's line about its pushes leads:
//! its details, the push targets it registered (Remove each), Send a test
//! push, and its recent pushes.
//!
//! A target is shown by its host alone: its endpoint is a capability
//! (whoever has it can push to the device) and its keys encrypt to the
//! device, so neither is read for this page, let alone shown.
//!
//! Every action is a plain form POST that comes back here (`303`) with
//! what happened in the query (`?removed=1`, `?tested=<push id>`), read
//! back from the database for the toast: nothing needs JavaScript. With
//! it (fixi), Recent pushes asks again every few seconds while its newest
//! push is still to be sent, for a minute at most.

use maud::{html, Markup, PreEscaped};

use super::connected_apps::{AppRow, BELL};
use super::settings::{toast_region, Toast, ToastKind};
use super::time::Clock;
use super::PageChrome;
use crate::apps::SaleKind;
use crate::db::{PushDeliveryRow, PushState, PushTargetSummary, TargetPushStats};
use crate::push::PushTargetKind;

/// How many of an app's pushes its page shows, newest first.
pub const RECENT_PUSHES: u32 = 50;
/// "The last 7 days" of the counts.
pub const RECENT_DAYS: i64 = 7;
/// How long, in seconds, Recent pushes asks again for a push still to be
/// sent: a minute after it was queued.
const POLL_FOR: i64 = 60;

/// The push relay this Monokulo's apps are told of (the apps guide):
/// its pushes' host, named as such.
pub const PUBLIC_RELAY_HOST: &str = "relay.monokulo.com";

/// Everything the app's page shows.
pub struct AppPageView {
    pub store_id: String,
    pub store_name: String,
    pub app: AppRow,
    pub clock: Clock,
    /// Its orders: how many, how many in the last [`RECENT_DAYS`], the
    /// newest.
    pub orders: crate::db::AppOrderStats,
    pub connected_at: i64,
    /// Its push targets, oldest first, with what their pushes came to.
    pub targets: Vec<TargetView>,
    /// Its newest pushes, at most [`RECENT_PUSHES`].
    pub recent: Vec<PushDeliveryRow>,
    /// How many it has kept, all told.
    pub kept: u64,
    /// The push settings it is shown with.
    pub limits: Limits,
    /// The test form as sent, and why it was refused, after a refusal.
    pub test_form: Option<TestForm>,
    pub toast: Option<Toast>,
}

/// One push target and its pushes.
pub struct TargetView {
    pub target: PushTargetSummary,
    pub stats: TargetPushStats,
    /// Its host is a push relay's: this Monokulo's own, or the public one.
    pub relay: bool,
}

/// The push settings the page states.
#[derive(Clone, Copy)]
pub struct Limits {
    /// `push.target_failure_limit`.
    pub failure_limit: u32,
    /// `push.max_attempts`.
    pub max_attempts: u32,
    /// `push.keep_days`.
    pub keep_days: i64,
}

/// "Send a test push" as sent, when it was refused.
pub struct TestForm {
    pub target: String,
    pub event: String,
    pub refused: String,
}

impl AppPageView {
    fn path(&self) -> String {
        super::connected_apps::app_path(&self.store_id, &self.app.id)
    }

    fn remove_path(&self, target: &PushTargetSummary) -> String {
        crate::base::url(format!("{}/push-targets/{}/remove", self.path(), target.id))
    }

    fn test_path(&self) -> String {
        crate::base::url(format!("{}/test-push", self.path()))
    }

    fn order_path(&self, order: &str) -> String {
        crate::base::url(format!("/stores/{}/orders/{order}", self.store_id))
    }
}

/// What a push's attempt was answered, briefly: its status, or what went
/// wrong in a few words (`timed out`, `private address`); never an
/// address.
pub fn short_answer(status: Option<u16>, error: Option<&str>) -> String {
    if let Some(status) = status {
        return status.to_string();
    }
    let Some(error) = error else {
        return "no answer".to_owned();
    };
    let lower = error.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("no answer within") {
        "timed out".into()
    } else if lower.contains("private or loopback") {
        "refused: a private address".into()
    } else if lower.contains("onion") {
        "refused: an onion address".into()
    } else if lower.contains("refused") {
        "connection refused".into()
    } else if lower.contains("resolve") || lower.contains("dns") {
        "host not found".into()
    } else if lower.contains("could not connect") || lower.contains("connect") {
        "could not connect".into()
    } else {
        error.split(':').next().unwrap_or(error).trim().to_owned()
    }
}

/// When something happened, as the Apps card and this page say it: how
/// long ago within the last day ("2 min ago"), the time otherwise
/// ("25 Oct, 14:02").
pub fn when(clock: &Clock, at: i64) -> String {
    if clock.now().saturating_sub(at) < 86_400 {
        clock.relative(at)
    } else {
        clock.text(at)
    }
}

/// A kind's name, as the page says it.
pub fn kind_name(kind: PushTargetKind) -> &'static str {
    match kind {
        PushTargetKind::Encrypted => "Encrypted push",
        PushTargetKind::LiveActivity => "Live Activity",
    }
}

/// A phone: a Live Activity, on the lock screen and in the island.
const PHONE: &str = r#"<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="6" y="2" width="12" height="20" rx="2"/><path d="M10 5h4"/></svg>"#;
/// A paper plane: sending.
const SEND: &str = r#"<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M22 2 11 13"/><path d="M22 2 15 22l-4-9-9-4z"/></svg>"#;

fn kind_icon(kind: PushTargetKind) -> &'static str {
    match kind {
        PushTargetKind::Encrypted => BELL,
        PushTargetKind::LiveActivity => PHONE,
    }
}

/// `184 ms`, `5.0 s`, as the webhooks' pages say it.
fn took(ms: u64) -> String {
    super::webhooks::duration(ms)
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", super::scaling::thousands(n))
    }
}

/// When a push happened, as the table says it: the time alone today, the
/// day and time before (`24 Oct, 09:12`).
fn short_time(clock: &Clock, at: i64) -> Markup {
    let full = clock.text(at);
    let today = clock.text(clock.now());
    let day = |text: &str| text.split_once(", ").map(|(day, _)| day.to_owned());
    let shown = match (day(&full), day(&today)) {
        (Some(a), Some(b)) if a == b => full
            .split_once(", ")
            .map_or(full.clone(), |(_, time)| time.to_owned()),
        _ => full.clone(),
    };
    html! { span class="when" title=(full) { (shown) } }
}

fn fact(term: &str, value: Markup) -> ui::Fact {
    ui::Fact {
        term: term.into(),
        value: value.into(),
    }
}

/// The page.
pub fn page(chrome: &PageChrome, view: &AppPageView) -> Markup {
    let settings = format!("/stores/{}/settings", view.store_id);
    let apps = format!("{settings}?tab=apps#card-apps");
    let mut crumbs = super::store_crumbs(&view.store_id, &view.store_name, false);
    crumbs.extend(super::crumbs(&[(&settings, "Settings"), (&apps, "Apps")]));
    let disconnect = crate::base::url(format!("{}/disconnect", view.path()));
    let header = ui::PageHeader::new(view.app.title())
        .crumbs(crumbs)
        .actions(html! {
            form method="post" action=(disconnect) class="inline-form" {
                (ui::Button::new("Disconnect").kind(ui::ButtonType::Submit)
                    .attr("aria-label", format!("Disconnect {}", view.app.title())))
            }
        });
    let body = html! {
        p class="app-page-meta" { (view.app.meta()) }
        (details_card(view))
        (targets_card(view))
        (recent_card(view))
        (toast_region(view.toast.as_ref(), false))
    };
    super::Page::new(
        format!("{} - {} - Monokulo", view.app.title(), view.store_name),
        header,
    )
    .class("settings-page app-page")
    .render(chrome, body)
}

fn details_card(view: &AppPageView) -> Markup {
    let app = &view.app;
    let platform = [
        app.platform.clone(),
        if app.version.is_empty() {
            String::new()
        } else {
            format!("version {}", app.version)
        },
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let dash = || html! { span class="muted" { "—" } };
    let last_order = match &view.orders.last {
        Some((order_id, reference, at, event)) => html! {
            a href=(view.order_path(order_id.as_str())) {
                (reference.clone().unwrap_or_else(|| super::short_id_text(order_id.as_str())))
            }
            span class="muted" {
                " · " (view.clock.relative(*at))
                @if let Some(event) = event { " · " (event.strip_prefix("sale.").unwrap_or(event).replace('_', " ")) }
            }
        },
        None => html! { span class="muted" { "no orders yet" } },
    };
    html! {
        section id="card-app-details" class="settings-card" aria-labelledby="card-app-details-title" {
            header class="card-head" { h3 id="card-app-details-title" { "Details" } }
            div class="card-body" {
                (ui::Facts::new(vec![
                    fact("App", html! { (app.app) }),
                    fact("Device", if app.device_name.is_empty() { dash() } else { html! { (app.device_name) } }),
                    fact("Platform", if platform.is_empty() { dash() } else { html! { (platform) } }),
                    fact("Key", html! {
                        "An app key of its own "
                        span class="muted" { "(" code { "ak_" } ", never shown again): it makes orders and follows them, never changes the store" }
                    }),
                    fact("Connected", html! { (view.clock.time(view.connected_at)) }),
                    fact("Last order", last_order),
                    fact("Orders", html! {
                        (super::scaling::thousands(view.orders.total))
                        span class="muted" { " · " (super::scaling::thousands(view.orders.recent)) " in the last " (RECENT_DAYS) " days" }
                    }),
                ]).attr("data-facts", "app"))
            }
        }
    }
}

/// A target's health, as its tag says it.
fn health_tag(target: &TargetView) -> ui::Tag {
    if target.target.failures > 0 {
        ui::Tag::new(format!("Failing · {} in a row", target.target.failures))
            .tone(ui::TagTone::Error)
    } else if target.target.last_ok_at.is_none() && target.stats.last.is_none() {
        ui::Tag::new("Not pushed yet").tone(ui::TagTone::Unknown)
    } else {
        ui::Tag::new("Healthy").tone(ui::TagTone::Ok)
    }
}

fn targets_meta(view: &AppPageView) -> String {
    let max = crate::db::MAX_TARGETS_PER_APP;
    let n = view.targets.len();
    if n == 0 {
        return format!("none of {max}");
    }
    let failing = view
        .targets
        .iter()
        .filter(|t| t.target.failures > 0)
        .count();
    let health = match (n, failing) {
        (_, 0)
            if view
                .targets
                .iter()
                .all(|t| t.target.last_ok_at.is_none() && t.stats.last.is_none()) =>
        {
            "not pushed yet".to_owned()
        }
        (1, 0) => "healthy".to_owned(),
        (2, 0) => "both healthy".to_owned(),
        (_, 0) => "all healthy".to_owned(),
        (_, failing) => format!("{failing} failing"),
    };
    format!("{n} of {max} · {health}")
}

/// "Every sale event (…)", or the events it wants.
fn events_words(events: &[SaleKind], long: bool) -> Markup {
    if SaleKind::ALL.iter().all(|kind| events.contains(kind)) {
        let names: Vec<&str> = SaleKind::ALL
            .iter()
            .map(|kind| kind.as_str().strip_prefix("sale.").unwrap_or(kind.as_str()))
            .collect();
        return html! {
            "Every sale event"
            @if long { span class="muted" { " (sale." (names.join(", ")) ")" } }
        };
    }
    let names: Vec<&str> = events.iter().map(|kind| kind.as_str()).collect();
    html! { (names.join(", ")) }
}

fn last_push_words(view: &AppPageView, stats: &TargetPushStats) -> Markup {
    match &stats.last {
        None => html! { span class="muted" { "none yet" } },
        Some(last) => html! {
            (when(&view.clock, last.at))
            span class="muted" {
                " · " (short_answer(last.status, last.error.as_deref()))
                @if last.delivered { @if let Some(ms) = last.duration_ms { " in " (took(ms)) } }
            }
        },
    }
}

fn target_card(view: &AppPageView, target: &TargetView, first: bool) -> Markup {
    let t = &target.target;
    let kind = kind_name(t.kind);
    let remove_label = match t.kind {
        PushTargetKind::Encrypted => format!("Remove the encrypted push target to {}", t.host),
        PushTargetKind::LiveActivity => format!("Remove the Live Activity target to {}", t.host),
    };
    let stats = &target.stats;
    let mut facts = vec![fact("Events", events_words(&t.events, first))];
    if t.kind == PushTargetKind::LiveActivity {
        facts.push(fact(
            "Payment code",
            if t.include_payment_uri {
                html! { "Shown in the island " span class="muted" { "(the app asked for the payment URI while a sale waits)" } }
            } else {
                html! { "Not shown" }
            },
        ));
    }
    facts.push(fact("Last push", last_push_words(view, stats)));
    facts.push(fact(
        &format!("Last {RECENT_DAYS} days"),
        html! {
            (plural(stats.delivered, "delivered", "delivered"))
            span class="muted" {
                " · " (super::scaling::thousands(stats.gave_up)) " given up · "
                (super::scaling::thousands(stats.superseded)) " superseded by a newer event"
            }
        },
    ));
    facts.push(fact(
        "Failed in a row",
        html! { (t.failures) span class="muted" { " · dropped at " (view.limits.failure_limit) } },
    ));
    ui::ItemCard::new(kind)
        .icon(PreEscaped(kind_icon(t.kind).to_owned()))
        .status(health_tag(target))
        .actions(html! {
            form method="post" action=(view.remove_path(t)) class="inline-form" {
                (ui::Button::new("Remove").size(ui::ControlSize::Compact).kind(ui::ButtonType::Submit)
                    .attr("aria-label", remove_label))
            }
        })
        .meta(html! {
            "To " strong { (t.host) }
            @if target.relay { " (the push relay)" }
            " · since " (view.clock.time(t.created_at))
            " · the app calls it " code { (t.client_id) }
        })
        .body(ui::Facts::new(facts).attr("data-facts", "push-target"))
        .attr("data-push-target", t.kind.as_str())
        .render()
}

fn targets_card(view: &AppPageView) -> Markup {
    html! {
        section id="card-push-targets" class="settings-card" aria-labelledby="card-push-targets-title" {
            header class="card-head" {
                h3 id="card-push-targets-title" { "Push targets" }
                span class="card-meta" { (targets_meta(view)) }
            }
            div class="card-body" {
                p class="app-page-intro" {
                    "Where the store's sale events are pushed while the app isn't running. The app registers these itself; "
                    "Monokulo shows where a target goes, never its endpoint or keys. One that fails "
                    (view.limits.failure_limit) " pushes in a row is dropped, and the app registers it again next time it opens."
                }
                @if view.targets.is_empty() {
                    (ui::Hint::new("No push targets yet: the app registers one once it's set up for pushes."))
                } @else {
                    div class="app-targets" {
                        @for (i, target) in view.targets.iter().enumerate() { (target_card(view, target, i == 0)) }
                    }
                    (test_form(view))
                }
            }
        }
    }
}

fn test_form(view: &AppPageView) -> Markup {
    let chosen_target = view.test_form.as_ref().map(|f| f.target.as_str());
    let chosen_event = view
        .test_form
        .as_ref()
        .map_or(SaleKind::Paid.as_str(), |f| f.event.as_str());
    let targets: Vec<ui::Choice> = view
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            ui::Choice::new(t.target.id.as_str(), kind_name(t.target.kind))
                .detail(&t.target.host)
                .selected(chosen_target.map_or(i == 0, |c| c == t.target.id.as_str()))
        })
        .collect();
    let events: Vec<ui::Choice> = SaleKind::ALL
        .iter()
        .map(|kind| {
            ui::Choice::new(kind.as_str(), kind.as_str()).selected(kind.as_str() == chosen_event)
        })
        .collect();
    html! {
        h4 class="mini-h app-test-head" id="test-push-title" { "Send a test push" }
        @if let Some(form) = &view.test_form {
            (ui::Alert::new(form.refused.as_str(), ui::Tone::Error).attr("id", "test-push-refused"))
        }
        form method="post" action=(view.test_path()) class="app-test-form" aria-labelledby="test-push-title" {
            div class="app-test-field" {
                label for="test-target" { "Target" }
                (ui::Select::new("target", targets).id("test-target").search(ui::SearchMode::Auto))
            }
            div class="app-test-field" {
                label for="test-event" { "Event" }
                (ui::Select::new("event", events).id("test-event"))
            }
            (ui::Button::new(html! {
                span class="app-test-icon" aria-hidden="true" { (PreEscaped(SEND)) }
                "Send a test push"
            }).kind(ui::ButtonType::Submit).attr("data-send-test-push", ""))
        }
        (ui::Hint::new("Carries no order: the app shows it as a test. It counts like any push, so a target that is failing stays failing."))
    }
}

/// The state's tag.
fn state_tag(state: PushState) -> ui::Tag {
    let (word, tone) = match state {
        PushState::Delivered => ("Delivered", ui::TagTone::Ok),
        PushState::Retrying => ("Retrying", ui::TagTone::Slow),
        PushState::Queued => ("Queued", ui::TagTone::Unknown),
        PushState::GaveUp => ("Gave up", ui::TagTone::Error),
        PushState::Superseded => ("Superseded", ui::TagTone::Muted),
    };
    ui::Tag::new(word).tone(tone)
}

/// What came of a push, after its tag.
fn result_words(view: &AppPageView, push: &PushDeliveryRow) -> String {
    use crate::push::delivery::{NO_ATTEMPTS_LEFT, OVER_BUDGET};
    let answer = || short_answer(push.last_status_code, push.last_error.as_deref());
    match push.state() {
        PushState::Delivered => match push.last_duration_ms {
            Some(ms) => format!("{} in {}", answer(), took(ms)),
            None => answer(),
        },
        PushState::Retrying => {
            let next = push
                .next_attempt_at
                .map(|at| format!(", next {}", view.clock.relative(at)))
                .unwrap_or_default();
            format!(
                "{}{next} · attempt {} of {}",
                answer(),
                push.attempt_count,
                view.limits.max_attempts
            )
        }
        PushState::Queued => "about to be sent".to_owned(),
        PushState::Superseded => {
            let by = view
                .recent
                .iter()
                .filter(|newer| {
                    newer.id > push.id
                        && newer.push_target_id == push.push_target_id
                        && newer.topic == push.topic
                })
                .min_by_key(|newer| newer.id);
            match by {
                Some(newer) => format!("by {} before it was sent", newer.event_type),
                None => "by a newer event before it was sent".to_owned(),
            }
        }
        PushState::GaveUp => match push.last_error.as_deref() {
            Some(reason)
                if [
                    crate::db::EXPIRED_UNSENT,
                    OVER_BUDGET,
                    NO_ATTEMPTS_LEFT,
                    crate::db::REREGISTERED,
                ]
                .contains(&reason) =>
            {
                reason.to_owned()
            }
            _ => {
                let mut words = format!(
                    "{} · {}",
                    answer(),
                    plural(u64::from(push.attempt_count), "attempt", "attempts")
                );
                if push.attempt_count < view.limits.max_attempts
                    && push.last_status_code != Some(413)
                {
                    words.push_str(&format!(
                        " · past its {} s to live",
                        push.expires_at.saturating_sub(push.created_at)
                    ));
                }
                words
            }
        },
    }
}

/// Whether Recent pushes should ask again: its newest push is still to
/// be sent, and was queued less than a minute ago.
fn polls(view: &AppPageView) -> bool {
    view.recent.first().is_some_and(|newest| {
        matches!(newest.state(), PushState::Queued | PushState::Retrying)
            && view.clock.now().saturating_sub(newest.created_at) < POLL_FOR
    })
}

/// The Recent pushes card: also what its fixi poll puts in its place.
pub fn recent_card(view: &AppPageView) -> Markup {
    let (delivered, gave_up, superseded) = view.targets.iter().fold((0, 0, 0), |(d, g, s), t| {
        (
            d + t.stats.delivered,
            g + t.stats.gave_up,
            s + t.stats.superseded,
        )
    });
    let poll_url = crate::base::url(view.path());
    let poll = polls(view);
    let head = html! { tr {
        th scope="col" { "Time" } th scope="col" { "Event" } th scope="col" { "Target" }
        th scope="col" { "Order" } th scope="col" { "Result" }
    } };
    let rows = html! {
        @for push in &view.recent {
            tr data-push-state=(format!("{:?}", push.state()).to_ascii_lowercase()) {
                td data-label="Time" { (short_time(&view.clock, push.created_at)) }
                td data-label="Event" {
                    code class="event-name" { (push.event_type) }
                    @if push.is_test() {
                        " " (ui::Tag::new("test").tone(ui::TagTone::Unknown).attr("data-test-push", ""))
                    }
                }
                td data-label="Target" { (kind_name(push.target_kind)) }
                td data-label="Order" {
                    @match &push.order_id {
                        Some(order) => a href=(view.order_path(order.as_str())) {
                            (push.merchant_order_id.clone().unwrap_or_else(|| super::short_id_text(order.as_str())))
                        },
                        None => span class="muted" {
                            span class="no-value-table" aria-label="no order" { "—" }
                            em class="no-value-card" { "no order" }
                        },
                    }
                }
                td data-label="Result" class="app-push-result" {
                    (state_tag(push.state())) " " span { (result_words(view, push)) }
                }
            }
        }
    };
    html! {
        section id="card-recent-pushes" class="settings-card" aria-labelledby="card-recent-pushes-title"
            fx-action=[poll.then_some(poll_url.as_str())] fx-trigger=[poll.then_some("fx:poll")]
            fx-swap=[poll.then_some("outerHTML")] fx-poll=[poll.then_some("3000")] fx-own-errors[poll] {
            header class="card-head" {
                h3 id="card-recent-pushes-title" { "Recent pushes" }
                span class="card-meta" {
                    "last " (RECENT_DAYS) " days · " (plural(delivered, "delivered", "delivered"))
                    " · " (super::scaling::thousands(gave_up)) " given up · "
                    (super::scaling::thousands(superseded)) " superseded"
                }
            }
            div class="card-body" {
                @if view.recent.is_empty() {
                    (ui::Hint::new("Nothing pushed yet: the app's targets get the store's next sale events."))
                } @else {
                    (ui::Table::new(rows).head(head).look(ui::TableLook::Stack)
                        .caption("The app's recent pushes, newest first")
                        .attr("data-table", "app-pushes"))
                    p class="hint app-pushes-foot" {
                        "The last " (super::scaling::thousands(view.recent.len() as u64)) " of "
                        (super::scaling::thousands(view.kept)) ". Pushes are kept "
                        (plural(u64::try_from(view.limits.keep_days).unwrap_or(0), "day", "days"))
                        " (" code { "push.keep_days" } "), then pruned."
                    }
                }
            }
        }
    }
}

/// The toast after a test push: how its attempt went, read back from its
/// row (`?tested=<id>`).
pub fn tested_toast(push: &PushDeliveryRow, host: &str, clock: &Clock) -> Toast {
    let answer = short_answer(push.last_status_code, push.last_error.as_deref());
    match push.state() {
        PushState::Delivered => {
            Toast::new(ToastKind::Success, "Test push delivered.").line(format!(
                "{host} answered {answer}{}. The till shows it as a test.",
                push.last_duration_ms
                    .map(|ms| format!(" in {}", took(ms)))
                    .unwrap_or_default()
            ))
        }
        PushState::Retrying => {
            let again = push
                .next_attempt_at
                .map(|at| format!("; it is tried again {}", clock.relative(at)))
                .unwrap_or_default();
            let what = if push.last_status_code.is_some() {
                format!("{host} answered {answer}")
            } else {
                format!("{host}: {answer}")
            };
            Toast::new(ToastKind::Error, "Test push failed.").line(format!("{what}{again}."))
        }
        PushState::GaveUp => Toast::new(ToastKind::Error, "Test push failed.")
            .line(format!("{host}: {answer}; it isn't tried again.")),
        PushState::Queued | PushState::Superseded => {
            Toast::new(ToastKind::Neutral, "Test push queued.")
                .line("Its result shows under Recent pushes.")
        }
    }
}

/// The toast after Remove.
pub fn removed_toast() -> Toast {
    Toast::new(ToastKind::Success, "Push target removed.")
        .line("Nothing more is pushed to it; the app registers again when it next opens.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_its_status_or_a_few_words_never_an_address() {
        assert_eq!(short_answer(Some(502), Some("server error")), "502");
        assert_eq!(
            short_answer(None, Some("timed out: no answer within 5s")),
            "timed out"
        );
        assert_eq!(
            short_answer(
                None,
                Some("the URL is, or resolves to, a private or loopback address")
            ),
            "refused: a private address"
        );
        assert_eq!(short_answer(None, None), "no answer");
    }
}
