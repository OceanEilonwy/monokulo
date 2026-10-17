//! The Logs page (structured_logging.md part 5): one search box with the
//! filter language, quick filters, a histogram strip, expandable lines
//! with find/exclude links, paging, saved searches, and a trace waterfall.
//!
//! Every part works without JavaScript as ordinary links and GET forms.
//! With it, fixi swaps `#log-results` in place of a reload (and puts the
//! URL in the address bar), "Older" appends the next page, and "Live"
//! streams new lines in through ssexi. The handlers are in
//! `http::logs_page`; this module only renders.

use maud::{html, Markup, PreEscaped};
use telemetry::query::Severity;

use super::{script, Load, PageChrome};
use crate::db::SavedLogSearch;

/// One line, ready to show.
pub struct RowView {
    /// Unique on the page (`service-id`), for the `<details>` element.
    pub dom_id: String,
    pub time_display: String,
    pub time_iso: String,
    pub severity: Severity,
    pub service: String,
    pub target: String,
    pub message: String,
    pub trace_url: Option<String>,
    /// Every line of the signed-in session this line was written in.
    pub session_url: Option<String>,
    /// The POS session this line belongs to, as a timeline.
    pub pos_session_url: Option<String>,
    pub properties: Vec<PropertyView>,
    /// Where the properties load from on first open. `None` renders them
    /// in the page (the trace page, a single line's page).
    pub properties_url: Option<String>,
    /// Shown opened (a single line's page).
    pub open: bool,
}

/// One property of an expanded line, with links that narrow or widen the
/// search by it.
pub struct PropertyView {
    pub name: String,
    pub value: String,
    pub find_url: Option<String>,
    pub exclude_url: Option<String>,
    /// What the value names, when it's an id ("someone@example.com").
    pub note: Option<String>,
}

/// One bar of the histogram strip: a slice of the time range, linking to
/// that slice.
pub struct BarView {
    pub count: u64,
    pub height_pct: u32,
    pub href: String,
    pub label: String,
}

pub struct HistogramView {
    pub bars: Vec<BarView>,
    pub from_label: String,
    pub to_label: String,
}

/// The search form's values, echoed back.
#[derive(Default)]
pub struct FormView {
    pub q: String,
    pub level: String,
    pub service: String,
    pub range: String,
    pub from: String,
    pub to: String,
    /// The Logs page's own requests are shown.
    pub logs_requests: bool,
}

pub struct QueryErrorView {
    pub message: String,
    /// The query with the offending part marked: before, the part, after.
    pub before: String,
    pub marked: String,
    pub after: String,
}

pub struct LogsViewModel {
    pub form: FormView,
    pub query_error: Option<QueryErrorView>,
    pub problems: Vec<String>,
    pub rows: Vec<RowView>,
    pub histogram: Option<HistogramView>,
    /// The next older page, when there may be one.
    pub older_url: Option<String>,
    /// The same with `part=more`, for fixi's "Older" that appends.
    pub more_url: Option<String>,
    /// Back towards the newest, when not on the first page.
    pub newer_url: Option<String>,
    /// The first page of this search, now.
    pub refresh_url: String,
    /// The live stream of new lines for this search (JavaScript only);
    /// `None` on an older page, which Live wouldn't add to.
    pub tail_url: Option<String>,
    pub export_ndjson_url: String,
    pub export_csv_url: String,
    /// The current search's query string, for "Save this search".
    pub query_string: String,
    pub saved: Vec<SavedLogSearch>,
    pub saved_error: Option<String>,
    pub attribute_names: Vec<String>,
}

const RANGES: &[(&str, &str)] = &[
    ("15m", "Last 15 minutes"),
    ("1h", "Last hour"),
    ("6h", "Last 6 hours"),
    ("24h", "Last 24 hours"),
    ("7d", "Last 7 days"),
    ("14d", "Last 14 days"),
    ("all", "Everything kept"),
    ("custom", "From / to below"),
];

const LEVELS: &[(&str, &str)] = &[
    ("", "Any level"),
    ("debug", "Debug and up"),
    ("info", "Info and up"),
    ("warn", "Warnings and errors"),
    ("error", "Errors"),
];

const SERVICES: &[(&str, &str)] = &[
    ("", "Monokulo and the engine"),
    ("monokulo", "Monokulo only"),
    ("engine", "The engine only"),
];

const PAGE_STYLE: &str = r#"
.wrap.wrap-wide { max-width: 1200px; }
.logs-search { display: grid; gap: .5em; margin-bottom: 1em; }
.logs-search .q-row { display: flex; gap: .5em; align-items: stretch; }
.logs-search .q-row input { flex: 1; min-width: 0; font-family: var(--font-mono); }
/* The box, Syntax and Search: one height, one top edge. */
.logs-search .q-row > :is(a, button) { flex: none; }
#query-help-link svg { width: 1.15em; height: 1.15em; }
.logs-search .quick input[type=datetime-local] { width: auto; }
.logs-search .quick { display: flex; flex-wrap: wrap; gap: .5em; align-items: center; }
.logs-search .quick label { display: inline-flex; gap: .3em; align-items: center; }
[data-query-error] mark { background: var(--tint-error); color: inherit; border-bottom: 2px solid currentColor; }
/* The title bar holds Refresh and Live, which act on the whole page. */
.mk-page-actions svg { width: 1.1em; height: 1.1em; flex: none; }
#log-live > span { display: inline-flex; }
#log-live .i-pause, #log-live[aria-pressed="true"] .i-play { display: none; }
#log-live[aria-pressed="true"] .i-pause { display: inline-flex; }
/* The histogram sits above the search it narrows, but comes with the
   results (a search swaps both), so the results box lets its parts lay
   out in the page's column and the histogram moves up. */
.logs-body { display: flex; flex-direction: column; }
.logs-body > * { min-width: 0; }
.logs-body > #log-results { display: contents; }
.log-histogram-box { order: -1; margin-bottom: .6em; }
.log-histogram { display: flex; align-items: flex-end; gap: 1px; height: 3.5em; margin: .5em 0 .2em; }
.log-histogram a { flex: 1; background: var(--accent); min-height: 1px; opacity: .8; }
.log-histogram a:hover, .log-histogram a:focus { opacity: 1; }
.log-histogram-axis { display: flex; justify-content: space-between; font-size: .8em; color: var(--muted); }
.log-rows { border-top: 1px solid var(--line); }
/* Live mode adds rows to an empty page: the empty message goes with them. */
.log-empty:has(+ #log-rows .log-row) { display: none; }
.log-row { border-bottom: 1px solid var(--line); }
.log-row > summary { display: grid; grid-template-columns: 13em 4.5em 6em 1fr 3.4em; gap: .5em; align-items: center; padding: .25em .3em; font-size: .9em; }
.log-row > summary:hover, .trace-span:hover { background: var(--surface-sunken); }
.log-row > summary .msg { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.log-row[open] > summary .msg { white-space: normal; overflow-wrap: anywhere; }
/* The trace page's title: its id in plain monospace after "Trace", no
   chip round it. */
.trace-id { font-family: var(--font-mono); font-size: .75em; font-weight: 400; color: var(--muted); overflow-wrap: anywhere; }
/* Trace and session, on every line: a slot each, so they line up. */
.row-acts { display: flex; gap: .2em; justify-content: flex-end; }
.row-acts > * { display: inline-flex; align-items: center; justify-content: center; width: 1.6em; height: 1.6em; border-radius: var(--radius-sm); color: var(--muted); }
.row-acts a:hover, .row-acts a:focus-visible { background: var(--surface-sunken); color: var(--ink); }
.row-acts svg { width: 1.1em; height: 1.1em; }
.log-row .props { margin: .2em 0 .6em 1em; font-size: .85em; }
.log-row .props td { overflow-wrap: anywhere; }
/* Find and Exclude: a column just wide enough for both, never wrapping;
   a long value wraps in its own column instead. */
.log-row .props td[data-act] { width: 1%; white-space: nowrap; overflow-wrap: normal; }
.log-row .props [data-act] a + a { margin-left: .5em; }
.lvl { font-weight: 700; }
.lvl-error { color: var(--error); }
.lvl-warn { color: var(--warning); }
.lvl-debug, .lvl-trace { color: var(--muted); }
.log-paging { display: flex; gap: .5em; margin: .8em 0; flex-wrap: wrap; align-items: center; }
/* Pressed, not a second primary: the page's one orange button is Search. */
#log-live[aria-pressed="true"] { background: var(--surface-sunken); border-color: var(--accent); }
.trace-waterfall { font-size: .85em; }
.trace-span { display: grid; grid-template-columns: minmax(12em, 30%) 1fr 6em; gap: .5em; align-items: center; padding: .15em 0; border-bottom: 1px solid var(--line); }
.trace-span .bar-track { position: relative; height: .9em; }
.trace-span .bar { position: absolute; top: 0; bottom: 0; background: var(--accent); min-width: 2px; }
.trace-span.status-error .bar { background: var(--error); }
.trace-span .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
/* The syntax help: a dialog on the Logs page (a bottom sheet on a phone),
   and the same content as its own page (`syntax_page`). */
#query-help { width: min(640px, calc(100% - 32px)); max-height: min(80dvh, 720px); padding: 0; }
#query-help::backdrop { background: color-mix(in srgb, var(--paper) 40%, transparent); backdrop-filter: blur(2px); }
.qh-head { position: sticky; top: 0; z-index: 1; padding: var(--space-md) var(--space-lg); border-bottom: 1px solid var(--line); background: var(--paper-raised); }
.qh-head h2 { margin: 0; padding: 0; border: 0; font-size: 1.1rem; }
.qh-body { padding: var(--space-sm) var(--space-lg) var(--space-lg); }
.qh-body h3 { margin: 1em 0 .4em; font-size: .8rem; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); }
.qh-ex { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
.qh-ex li { display: grid; grid-template-columns: minmax(0, 1fr) auto; grid-template-areas: "code use" "what use"; gap: 0 .6em; align-items: center; padding: 6px 8px; border: 1px solid var(--line); border-radius: var(--radius-sm); background: var(--paper-raised); }
.qh-ex code { grid-area: code; padding: 0; border: 0; background: none; overflow-wrap: anywhere; }
.qh-ex .qh-what { grid-area: what; font-size: .85em; color: var(--muted); }
.qh-ex [data-q] { grid-area: use; }
.qh-chips { display: flex; flex-wrap: wrap; gap: 4px; margin: .3em 0; }
.qh-chip, .qh-chips [data-chip] { min-height: 0; margin: 0; padding: var(--pill-pad-y) var(--pill-pad-x); border: 1px solid var(--line); border-radius: 999px; background: var(--surface-sunken); color: var(--ink); font: .85em var(--font-mono); font-weight: 400; }
@media (max-width: 40em) {
  #query-help { width: 100%; max-width: none; margin: auto 0 0; border-radius: var(--radius-lg) var(--radius-lg) 0 0; max-height: 85dvh; }
  .logs-search .quick { display: grid; grid-template-columns: 1fr 1fr; }
  .logs-search .quick select { width: 100%; min-width: 0; }
  .logs-search .quick label:has(select[name=range]), .logs-search .quick .check-line { grid-column: 1 / -1; }
  .logs-search .quick label { display: grid; gap: .2em; font-size: .85em; }
  .logs-search .quick input[type=datetime-local] { width: 100%; min-width: 0; }
  .logs-search .quick .check-line { display: flex; gap: .4em; }
  #query-help-link { padding-inline: .7em; }
  .q-help-label { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
}
@media (max-width: 40em) {
  .log-row > summary { grid-template-columns: 1fr auto auto; }
  .log-row > summary .svc { display: none; }
  .log-row > summary .row-acts { grid-row: 1; grid-column: 3; }
  .log-row > summary .msg { grid-column: 1 / -1; }
  .log-row .props td[data-act] { width: auto; }
}
"#;

fn level_class(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "lvl lvl-error",
        Severity::Warn => "lvl lvl-warn",
        Severity::Info => "lvl lvl-info",
        Severity::Debug => "lvl lvl-debug",
        Severity::Trace => "lvl lvl-trace",
    }
}

/// One line: a summary that expands to its properties. In a list the
/// properties aren't sent with the page: opening the line fetches them
/// (fixi, on `toggle`), and without JavaScript the line holds a link to its
/// own page instead.
pub fn row(row: &RowView) -> Markup {
    let props_id = format!("{}-props", row.dom_id);
    html! {
        @if let Some(url) = &row.properties_url {
            details class="log-row" id=(row.dom_id) open[row.open] fx-action=(url) fx-trigger="toggle" fx-target=(format!("#{props_id}")) fx-swap="outerHTML" {
                (summary(row))
                div class="props" id=(props_id) { a href=(url) { "Show this line's properties" } }
            }
        } @else {
            details class="log-row" id=(row.dom_id) open[row.open] {
                (summary(row))
                (properties(row))
            }
        }
    }
}

fn summary(row: &RowView) -> Markup {
    html! {
        summary {
            time datetime=(row.time_iso) { (row.time_display) }
            span class=(level_class(row.severity)) { (row.severity.upper()) }
            span class="svc" { (row.service) }
            span class="msg" { (row.message) }
            span class="row-acts" {
                @if let Some(trace) = &row.trace_url {
                    a href=(trace) title="Show trace" aria-label="Show trace" { (icon(TRACE_ICON)) }
                } @else { span {} }
                @if let Some(session) = row.session_url.as_ref().or(row.pos_session_url.as_ref()) {
                    a href=(session) title="Show session" aria-label="Show session" { (icon(SESSION_ICON)) }
                } @else { span {} }
            }
        }
    }
}

/// An icon drawn in the text's colour; `paths` is its SVG content.
fn icon(paths: &'static str) -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
            (PreEscaped(paths))
        }
    }
}

/// A waterfall: a trace's spans.
const TRACE_ICON: &str = r#"<path d="M4 6h9M8 12h9M12 18h8"/>"#;
/// A person: whoever was signed in.
const SESSION_ICON: &str = r#"<circle cx="12" cy="8" r="3.5"/><path d="M5 20a7 7 0 0 1 14 0"/>"#;
const REFRESH_ICON: &str = r#"<path d="M20 12a8 8 0 1 1-2.34-5.66"/><path d="M20 4v5h-5"/>"#;
const PLAY_ICON: &str = r#"<path d="M8 5.5v13l10.5-6.5z" fill="currentColor"/>"#;
const PAUSE_ICON: &str = r#"<path d="M9 5.5v13M15 5.5v13" stroke-width="3"/>"#;

/// A line's properties, with links that narrow or widen the search by
/// each; also what fixi swaps in when a line opens.
pub fn properties(row: &RowView) -> Markup {
    html! {
        div class="props" id=(format!("{}-props", row.dom_id)) {
            (ui::Table::new(html! {
                (property_row("target", html! { code { (row.target) } }, html! {}))
                @for property in &row.properties {
                    (property_row(&property.name, html! {
                        code { (property.value) }
                        @if let Some(note) = &property.note { " " span class="muted" { (note) } }
                    }, html! {
                        @if let Some(find) = &property.find_url {
                            a href=(find) title=(format!("Only lines where {} is this", property.name)) { "Find" }
                        }
                        @if let Some(exclude) = &property.exclude_url {
                            a href=(exclude) title=(format!("Hide lines where {} is this", property.name)) { "Exclude" }
                        }
                    }))
                }
            })
            .look(ui::TableLook::Facts)
            .scroll(false))
            @if let Some(trace) = &row.trace_url {
                p { a href=(trace) { "Show the whole trace" } }
            }
            @if let Some(session) = &row.session_url {
                p { a href=(session) { "Show the whole session" } }
            }
            @if let Some(session) = &row.pos_session_url {
                p { a href=(session) { "Show the POS session timeline" } }
            }
        }
    }
}

/// One property's row: its name, its value, and the links that search by
/// it (`data-act`, a column just wide enough for them).
fn property_row(name: &str, value: Markup, act: Markup) -> ui::Row {
    ui::Row::new(ui::Slot::many([
        ui::Cell::new(name).header(true).into(),
        ui::Cell::new(value).into(),
        ui::Cell::new(act).attr("data-act", "").into(),
    ]))
}

/// `/dashboard/admin/logs/row/{cursor}` without JavaScript: one line,
/// opened, with a way back to the search it came from.
pub fn row_page(chrome: &PageChrome, row_view: Option<&RowView>, back_url: &str) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) } };
    let header =
        ui::PageHeader::new("Log line").crumbs(super::crumbs(&[("/", "Home"), (back_url, "Logs")]));
    let body = html! {
        @if let Some(r) = row_view {
            div class="log-rows" { (row(r)) }
        } @else {
            p class="muted" { "This line is no longer kept: log retention has deleted it." }
        }
        p { a href=(back_url) { "Back to the search" } }
    };
    super::Page::new("Log line - Monokulo", header)
        .class("wrap-wide")
        .head(extra_head)
        .render(chrome, body)
}

/// The "Older" control: a plain link to the next page, which fixi turns
/// into "append the next page here".
fn more(more_url: Option<&str>, older_url: Option<&str>) -> Markup {
    html! {
        div id="log-more" {
            @if let (Some(older), Some(more)) = (older_url, more_url) {
                p class="log-paging" {
                    (ui::LinkButton::new("Older", older)
                        .attr("fx-action", more)
                        .attr("fx-target", "#log-more")
                        .attr("fx-swap", "outerHTML"))
                }
            }
        }
    }
}

/// The answer to fixi's "Older": the next page's lines followed by a new
/// "Older" control, replacing the old control where it stood.
pub fn more_rows(rows: &[RowView], more_url: Option<&str>, older_url: Option<&str>) -> Markup {
    html! {
        @for r in rows { (row(r)) }
        (more(more_url, older_url))
    }
}

/// New lines for the live stream, newest first.
pub fn live_rows(rows: &[RowView]) -> Markup {
    html! { @for r in rows { (row(r)) } }
}

/// Why Live isn't showing the engine's lines, or nothing once it is again.
pub fn tail_problem(problem: Option<&str>) -> Markup {
    html! { @if let Some(problem) = problem { (status_problem(problem)) } }
}

/// A problem with what the page shows (a source that didn't answer), said
/// as a status: the page still works.
fn status_problem(problem: &str) -> ui::Alert {
    ui::Alert::new(problem, ui::Tone::Error)
        .quiet(true)
        .attr("role", "status")
}

fn histogram(histogram: &HistogramView) -> Markup {
    html! {
        div class="log-histogram-box" {
            div class="log-histogram" role="img" aria-label="Lines over time" {
                @for bar in &histogram.bars {
                    a href=(bar.href) style=(format!("height:{}%", bar.height_pct)) title=(bar.label) aria-label=(bar.label) {}
                }
            }
            div class="log-histogram-axis" { span { (histogram.from_label) } span { (histogram.to_label) } }
        }
    }
}

/// Everything below the search form: swapped as one piece by fixi, since
/// a search changes all of it.
pub fn results(vm: &LogsViewModel) -> Markup {
    html! {
        div id="log-results" {
            @if let Some(error) = &vm.query_error {
                (ui::Alert::new(html! {
                    p { "That search doesn't read as a query: " (error.message) "." }
                    p { code { (error.before) mark { (error.marked) } (error.after) } }
                }, ui::Tone::Error)
                .block(true)
                .attr("data-query-error", "")
                .attr("data-fx-focus", ""))
            }
            @for problem in &vm.problems { (status_problem(problem)) }
            // Live says here when the engine's lines stop arriving.
            div id="log-tail-problem" {}
            // Shown above the search form (`.log-histogram-box`).
            @if let Some(h) = &vm.histogram { (histogram(h)) }
            @if let Some(newer) = &vm.newer_url {
                p class="log-paging" { (ui::LinkButton::new("Newer", newer.as_str())) }
            }
            @if vm.rows.is_empty() && vm.query_error.is_none() {
                p class="muted log-empty" { "No lines match." }
            }
            div id="log-rows" class="log-rows" data-tail-url=[vm.tail_url.as_deref()] {
                @for r in &vm.rows { (row(r)) }
                (more(vm.more_url.as_deref(), vm.older_url.as_deref()))
            }
            p class="log-paging" {
                "Download these lines: "
                a href=(vm.export_ndjson_url) download { "NDJSON" }
                " · "
                a href=(vm.export_csv_url) download { "CSV" }
            }
        }
    }
}

/// The saved searches box: swapped by itself when one is saved or removed.
pub fn saved_searches(saved: &[SavedLogSearch], query_string: &str, error: Option<&str>) -> Markup {
    // fixi swaps the whole card for what a save or a removal answers.
    let fixi = |action: String| {
        move |body: Markup| {
            html! {
                form method="post" action=(action) fx-action=(action) fx-method="POST" fx-target="#card-saved-searches" {
                    input type="hidden" name="query_string" value=(query_string);
                    (body)
                }
            }
        }
    };
    let meta = match saved.len() {
        0 => None,
        1 => Some("1 search".to_string()),
        n => Some(format!("{n} searches")),
    };
    super::settings::plain_card_with_meta(
        "saved-searches",
        "Saved searches",
        meta.as_deref(),
        html! {
            @if let Some(error) = error {
                (ui::Alert::new(error, ui::Tone::Error).attr("data-fx-focus", ""))
            }
            @if saved.is_empty() {
                (ui::Hint::new("None yet. Save a search to come back to it in one click."))
            } @else {
                (ui::Table::new(html! {
                    @for s in saved {
                        tr {
                            td { a href=(format!("/dashboard/admin/logs?{}", s.query_string)) { (s.name) } }
                            td class="row-acts" {
                                (fixi(format!("/dashboard/admin/logs/saved/{}/delete", s.id))(html! {
                                    (ui::Button::new("Remove")
                                        .size(ui::ControlSize::Compact)
                                        .kind(ui::ButtonType::Submit)
                                        .attr("aria-label", format!("Remove {}", s.name)))
                                }))
                            }
                        }
                    }
                })
                .scroll(false)
                .attr("data-table", "saved-searches"))
            }
            (fixi("/dashboard/admin/logs/saved".to_string())(html! {
                (super::settings::Field::new("Save this search as", "saved-search-name")
                    .help(Some("saved-search-name-help"), html! { "Its text, filters and time range, as they are now." })
                    .render(html! {
                        (ui::Input::new("name")
                            .id("saved-search-name")
                            .described_by("saved-search-name-help")
                            .required(true)
                            .attr("maxlength", "80"))
                    }))
                (ui::FormActions::new(ui::Button::new("Save search").kind(ui::ButtonType::Submit)))
            }))
        },
    )
}

fn select(name: &str, label: &str, options: &[(&str, &str)], current: &str) -> Markup {
    html! {
        (ui::Select::new(
            name,
            options
                .iter()
                .map(|(value, text)| ui::Choice::new(*value, *text).selected(*value == current))
                .collect::<Vec<_>>(),
        )
        .label(label)
        .compact(true))
    }
}

/// Where the syntax help lives as a page of its own.
pub const SYNTAX_PAGE: &str = "/dashboard/admin/logs/syntax";

/// Examples of each part of the language (`telemetry::query`), with what
/// each shows.
const EXAMPLES: &[(&str, &[(&str, &str)])] = &[
    (
        "Compare a property",
        &[
            ("level >= warn", "levels go trace, debug, info, warn, error"),
            ("order.id = 'o_9'", "= equals; != or <> differs"),
            ("http.status >= 500", "< <= > >= for numbers"),
        ],
    ),
    (
        "Match text",
        &[
            ("message contains 'timeout'", "anywhere in the text"),
            (
                "http.route like '/pay/%'",
                "% is any run of characters, _ one character",
            ),
            ("payment not seen", "plain words search the message"),
        ],
    ),
    (
        "Combine",
        &[
            ("level >= warn and store.id = 's_1'", "both"),
            (
                "service = 'engine' or has error",
                "either; has means the line has that property",
            ),
            ("not (service = 'engine')", "brackets group; not negates"),
        ],
    ),
];

/// The query language, for the Logs page's dialog and its own page. Each
/// example links to a search for it; in the dialog, script puts it in the
/// box instead, and the property names become chips that add themselves.
fn syntax(names: &[String], in_dialog: bool) -> Markup {
    html! {
        @for (title, examples) in EXAMPLES {
            h3 { (title) }
            ul class="qh-ex" {
                @for (query, what) in *examples {
                    li {
                        code { (query) }
                        span class="qh-what" { (what) }
                        (ui::LinkButton::new("Use", format!("/dashboard/admin/logs?q={}", url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()))
                            .size(ui::ControlSize::Compact)
                            .attr("data-q", *query))
                    }
                }
            }
        }
        h3 { "Values" }
        p {
            "Quote text with " code { "'" } " or " code { "\"" } "; a backslash escapes a quote. Numbers, "
            code { "true" } ", " code { "false" } " and " code { "null" } " go bare. A bare word is text ("
            code { "network = Stagenet" } "). Keywords ignore case."
        }
        h3 { "Names" }
        (ui::Hint::new("Built in, and seen on recent lines. Every other name is a property of the line."))
        p class="qh-chips" {
            @for name in names {
                @if in_dialog { (ui::Button::new(name.as_str()).attr("data-chip", "")) } @else { code class="qh-chip" { (name) } }
            }
        }
    }
}

/// The syntax help as a dialog, opened by the Syntax link.
fn syntax_dialog(names: &[String]) -> Markup {
    let content = html! {
        div class="qh-head" {
            (ui::DialogTitle::new("query-help-title", "Search syntax"))
        }
        div class="qh-body" { (syntax(names, true)) }
    };
    ui::Dialog::new("query-help", "query-help-title", content)
        .size(ui::DialogSize::Wide)
        .light_dismiss(true)
        .render()
}

/// `GET /dashboard/admin/logs/syntax`: the same help as the dialog, for a
/// browser without script.
pub fn syntax_page(chrome: &PageChrome, names: &[String]) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) } };
    let header = ui::PageHeader::new("Search syntax")
        .crumbs(super::crumbs(&[("/dashboard/admin/logs", "Logs")]));
    let body = html! {
        div class="qh-page" { (syntax(names, false)) }
    };
    super::Page::new("Search syntax - Monokulo", header)
        .head(extra_head)
        .render(chrome, body)
}

/// Live's stream when the page opens on an older page; the script takes
/// the URL from the results anyway.
const LIVE_NONE: &str = "/dashboard/admin/logs/tail";

pub fn page(chrome: &PageChrome, vm: &LogsViewModel) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) } };
    let header = ui::PageHeader::new("Logs")
        .crumbs(super::crumbs(&[("/", "Home")]))
        .actions(html! {
            // Submits the search as it stands: its newest lines.
            (ui::Button::new(html! { (icon(REFRESH_ICON)) span { "Refresh" } })
                .size(ui::ControlSize::Compact)
                .kind(ui::ButtonType::Submit)
                .attr("form", "log-search"))
            // Streams from `#log-rows`' tail URL (the page's script);
            // without script, hidden.
            (ui::Button::new(html! {
                span class="i-play" { (icon(PLAY_ICON)) }
                span class="i-pause" { (icon(PAUSE_ICON)) }
                span class="log-live-label" { "Live" }
            })
            .size(ui::ControlSize::Compact)
            .disabled(vm.tail_url.is_none())
            .attr("id", "log-live")
            .attr("aria-pressed", "false")
            .attr("fx-action", vm.tail_url.as_deref().unwrap_or(LIVE_NONE))
            .attr("fx-target", "#log-rows")
            .attr("fx-swap", "afterbegin")
            .attr("fx-sse-reconnect", "")
            .attr("data-only", "script"))
        });
    let body = html! {
        div class="logs-body" {
            form id="log-search" class="logs-search" method="get" action="/dashboard/admin/logs"
                fx-action="/dashboard/admin/logs" fx-target="#log-results" fx-swap="outerHTML" fx-push-url fx-replace fx-submit-on-change {
                div class="q-row" {
                    label for="log-q" class="sr-only" { "Search" }
                    (ui::Input::new("q")
                        .id("log-q")
                        .kind(ui::InputType::Search)
                        .value(vm.form.q.clone())
                        .autocomplete("off")
                        .placeholder("level >= warn and order.id = 'o_1'")
                        .attr("spellcheck", "false"))
                    // A page of its own without script; a dialog with it.
                    (ui::LinkButton::new(html! {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true" focusable="false" {
                            circle cx="12" cy="12" r="9.5" {}
                            path d="M9.5 9.2a2.6 2.6 0 0 1 5 .8c0 1.8-2.5 2.2-2.5 3.8" {}
                            circle cx="12" cy="17.3" r=".6" fill="currentColor" {}
                        }
                        span class="q-help-label" { "Syntax" }
                    }, SYNTAX_PAGE)
                    .attr("id", "query-help-link")
                    .attr("aria-haspopup", "dialog")
                    .attr("data-opens-dialog", "query-help"))
                    (ui::Button::new("Search").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit))
                }
                div class="quick" {
                    (select("level", "Level", LEVELS, &vm.form.level))
                    (select("service", "Service", SERVICES, &vm.form.service))
                    (select("range", "Time range", RANGES, &vm.form.range))
                    label { "From " (ui::Input::new("from").kind(ui::InputType::DatetimeLocal).value(vm.form.from.as_str())) }
                    label { "To " (ui::Input::new("to").kind(ui::InputType::DatetimeLocal).value(vm.form.to.as_str())) }
                    (ui::Checkbox::new()
                        .text("Show the Logs page's own requests")
                        .name("logs_requests")
                        .value("show")
                        .checked(vm.form.logs_requests))
                }
            }
            (results(vm))
        }
        (syntax_dialog(&vm.attribute_names))
        (saved_searches(&vm.saved, &vm.query_string, vm.saved_error.as_deref()))
        (script("logs.js", Load::Now))
    };
    super::Page::new("Logs - Monokulo", header)
        .class("wrap-wide")
        .head(extra_head)
        .render(chrome, body)
}

/// One event of a POS session timeline.
pub struct TimelineEntryView {
    /// A stretch with nothing recorded before this event, as text
    /// ("Nothing recorded for 4 min").
    pub gap_before: Option<String>,
    /// The tablet's clock.
    pub time_display: String,
    pub time_iso: String,
    /// Since the event before ("+1.2 s").
    pub since_previous: String,
    pub severity: Severity,
    pub kind: String,
    pub order: Option<(String, String)>,
    /// A period the event closes, worth pointing out ("offline for 42 s").
    pub period: Option<String>,
    pub detail: Vec<(String, String)>,
    /// Queued on the tablet and sent later ("sent 5 min later").
    pub late: Option<String>,
}

/// Totals shown above a POS session timeline.
pub struct TimelineSummaryView {
    pub events: usize,
    pub span: String,
    pub offline: String,
    pub hidden: String,
    pub stream_drops: usize,
    pub orders_created: usize,
    pub problems: usize,
}

pub struct PosTimelineViewModel {
    pub session: String,
    pub store: Option<(String, String)>,
    pub device: Option<String>,
    pub summary: TimelineSummaryView,
    pub entries: Vec<TimelineEntryView>,
    pub problems: Vec<String>,
    pub truncated: bool,
    pub logs_url: String,
    pub zone_label: String,
}

/// `/dashboard/admin/logs/pos/{session}`: one POS session, event by event
/// in the order the tablet recorded them, with gaps and the periods it was
/// offline or hidden called out.
pub fn pos_timeline_page(chrome: &PageChrome, vm: &PosTimelineViewModel) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) (PreEscaped(TIMELINE_STYLE)) } };
    let header = ui::PageHeader::new("POS session").crumbs(super::crumbs(&[
        ("/", "Home"),
        ("/dashboard/admin/logs", "Logs"),
    ]));
    let body = html! {
        (ui::Hint::new(html! {
            code { (vm.session) }
            @if let Some((name, href)) = &vm.store { " · " a href=(href) { (name) } }
            @if let Some(device) = &vm.device { br; span class="muted" { (device) } }
        }))
        @for problem in &vm.problems { (status_problem(problem)) }
        @if vm.entries.is_empty() {
            p class="muted" { "Nothing was recorded for this session, or its lines have been deleted by log retention." }
        } @else {
            dl class="timeline-summary" {
                div { dt { "Events" } dd { (vm.summary.events) } }
                div { dt { "Length" } dd { (vm.summary.span) } }
                div { dt { "Offline" } dd { (vm.summary.offline) } }
                div { dt { "Hidden" } dd { (vm.summary.hidden) } }
                div { dt { "Live updates dropped" } dd { (vm.summary.stream_drops) } }
                div { dt { "Orders created" } dd { (vm.summary.orders_created) } }
                div { dt { "Warnings and errors" } dd { (vm.summary.problems) } }
            }
            @if vm.truncated {
                (ui::Hint::new(html! { "Only the first events of a very long session are shown. " a href=(vm.logs_url) { "Search them all" } }))
            }
            (ui::Hint::new(html! {
                "Times are the tablet's clock, in " (vm.zone_label) ". "
                a href=(vm.logs_url) { "Show these lines in Logs" }
            }))
            ol class="pos-timeline" {
                @for entry in &vm.entries {
                    @if let Some(gap) = &entry.gap_before { li class="timeline-gap" { (gap) } }
                    li class=(match entry.severity { Severity::Error => "timeline-event is-error", Severity::Warn => "timeline-event is-warn", _ => "timeline-event" }) {
                        time datetime=(entry.time_iso) { (entry.time_display) }
                        span class="since muted" { (entry.since_previous) }
                        span class="kind" { (entry.kind) }
                        span class="about" {
                            @if let Some((label, href)) = &entry.order { a href=(href) { (label) } " " }
                            @if let Some(period) = &entry.period { strong class="period" { (period) } " " }
                            @for (name, value) in &entry.detail { span class="kv" { span class="muted" { (name) "=" } (value) } " " }
                            @if let Some(late) = &entry.late { span class="late muted" { "(" (late) ")" } }
                        }
                    }
                }
            }
        }
    };
    super::Page::new("POS session - Monokulo", header)
        .class("wrap-wide")
        .head(extra_head)
        .render(chrome, body)
}

const TIMELINE_STYLE: &str = r#"
.timeline-summary { display: flex; flex-wrap: wrap; gap: .5em 1.5em; margin: .5em 0 1em; }
.timeline-summary div { display: flex; flex-direction: column; }
.timeline-summary dt { font-size: .8em; color: var(--muted); }
.timeline-summary dd { margin: 0; font-weight: 700; }
.pos-timeline { list-style: none; padding: 0; margin: 0; border-top: 1px solid var(--line); font-size: .9em; }
.timeline-event { display: grid; grid-template-columns: 6.5em 5em 11em 1fr; gap: .5em; padding: .3em .3em; border-bottom: 1px solid var(--line); align-items: baseline; }
.timeline-event .kind { font-family: var(--font-mono); }
.timeline-event.is-warn .kind { font-weight: 700; color: var(--warning); }
.timeline-event.is-error .kind { font-weight: 700; color: var(--error); }
.timeline-event .about { overflow-wrap: anywhere; }
.timeline-event .period { color: var(--warning); }
.timeline-gap { padding: .4em .3em; border-bottom: 1px dashed var(--line); color: var(--muted); font-style: italic; text-align: center; }
@media (max-width: 40em) {
  .timeline-event { grid-template-columns: 1fr auto; }
  .timeline-event .kind, .timeline-event .about { grid-column: 1 / -1; }
}
"#;

/// One span on the trace page.
pub struct SpanView {
    pub name: String,
    pub service: String,
    pub depth: usize,
    pub left_pct: f64,
    pub width_pct: f64,
    pub duration: String,
    pub error: bool,
    pub attributes: Vec<(String, String)>,
}

pub struct TraceViewModel {
    pub trace_id: String,
    pub spans: Vec<SpanView>,
    pub rows: Vec<RowView>,
    pub problems: Vec<String>,
    pub logs_url: String,
}

/// `/dashboard/admin/logs/trace/{id}`: the spans as a waterfall (bars
/// placed by CSS percentages worked out on the server), then the trace's
/// lines in order.
pub fn trace_page(chrome: &PageChrome, vm: &TraceViewModel) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) } };
    let header =
        ui::PageHeader::new(html! { "Trace " span class="trace-id" { (vm.trace_id) } }).crumbs(
            super::crumbs(&[("/", "Home"), ("/dashboard/admin/logs", "Logs")]),
        );
    let body = html! {
        @for problem in &vm.problems { (status_problem(problem)) }
        @if vm.spans.is_empty() {
            p class="muted" { "No spans kept for this trace." }
        } @else {
            div class="trace-waterfall" role="list" {
                @for span in &vm.spans {
                    details class=(if span.error { "trace-span-details status-error" } else { "trace-span-details" }) role="listitem" {
                        summary class=(if span.error { "trace-span status-error" } else { "trace-span" }) {
                            span class="name" style=(format!("padding-left:{}em", span.depth)) {
                                (span.name) " " span class="muted" { (span.service) }
                            }
                            span class="bar-track" {
                                span class="bar" style=(format!("left:{:.2}%;width:{:.2}%", span.left_pct, span.width_pct)) {}
                            }
                            span { (span.duration) }
                        }
                        (ui::Table::new(ui::Slot::many(span.attributes.iter().map(|(name, value)| {
                            ui::Slot::from(ui::Row::new(ui::Slot::many([
                                ui::Cell::new(name.as_str()).header(true).into(),
                                ui::Cell::new(html! { code { (value) } }).into(),
                            ])))
                        })))
                        .look(ui::TableLook::Facts)
                        .scroll(false))
                    }
                }
            }
        }
        h2 { "Lines" }
        (ui::Hint::new(html! { a href=(vm.logs_url) { "Search these lines" } }))
        div class="log-rows" { @for r in &vm.rows { (row(r)) } }
    };
    super::Page::new("Trace - Monokulo", header)
        .class("wrap-wide")
        .head(extra_head)
        .render(chrome, body)
}
