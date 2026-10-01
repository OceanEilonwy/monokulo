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

use super::{layout_with_head, PageChrome};
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
    ("scanner", "The engine only"),
];

const PAGE_STYLE: &str = r#"
.wrap.wrap-wide { max-width: 1200px; }
.logs-search { display: grid; gap: .5em; margin-bottom: 1em; }
.logs-search .q-row { display: flex; gap: .5em; align-items: stretch; }
.logs-search .q-row input { flex: 1; min-width: 0; font-family: var(--font-mono); }
/* The box, Syntax and Search: one height, one top edge. */
.logs-search .q-row > .btn, .logs-search .q-row > button { flex: none; margin: 0; }
.q-help { display: inline-flex; align-items: center; gap: .4em; }
.q-help svg { width: 1.15em; height: 1.15em; }
.logs-search .quick input[type=datetime-local] { width: auto; }
.logs-search .quick { display: flex; flex-wrap: wrap; gap: .5em; align-items: center; }
.logs-search .quick label { display: inline-flex; gap: .3em; align-items: center; }
.query-error mark { background: var(--tint-error); color: inherit; border-bottom: 2px solid currentColor; }
/* The title bar holds Refresh and Live, which act on the whole page. */
.logs-head { display: flex; align-items: center; flex-wrap: wrap; gap: .5em; margin: var(--space-lg) 0 var(--space-sm); padding-bottom: .3em; border-bottom: 2px solid var(--line); }
.context-nav + .logs-head { margin-top: var(--space-xs); }
.logs-head h1 { flex: 1; margin: 0; padding: 0; border: 0; }
.logs-head .btn, .logs-head button { display: inline-flex; align-items: center; gap: .4em; margin: 0; padding: .35em .8em; }
.logs-head svg { width: 1.1em; height: 1.1em; flex: none; }
.log-live > span { display: inline-flex; }
.log-live .i-pause, .log-live[aria-pressed="true"] .i-play { display: none; }
.log-live[aria-pressed="true"] .i-pause { display: inline-flex; }
.log-live:disabled { opacity: .5; cursor: not-allowed; }
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
.log-row > summary { display: grid; grid-template-columns: 13em 4.5em 6em 1fr 3.4em; gap: .5em; align-items: center; padding: .25em .3em; cursor: pointer; list-style: none; font-size: .9em; }
.log-row > summary::-webkit-details-marker { display: none; }
.log-row > summary .msg { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.log-row[open] > summary .msg { white-space: normal; overflow-wrap: anywhere; }
/* Trace and session, on every line: a slot each, so they line up. */
.row-acts { display: flex; gap: .2em; justify-content: flex-end; }
.row-acts > * { display: inline-flex; align-items: center; justify-content: center; width: 1.6em; height: 1.6em; border-radius: var(--radius-sm); color: var(--muted); }
.row-acts a:hover, .row-acts a:focus-visible { background: var(--surface-sunken); color: var(--ink); }
.row-acts svg { width: 1.1em; height: 1.1em; }
.log-row .props { margin: .2em 0 .6em 1em; font-size: .85em; }
.log-row .props td { overflow-wrap: anywhere; }
/* Find and Exclude: a column just wide enough for both, never wrapping;
   a long value wraps in its own column instead. */
.log-row .props td.act { width: 1%; white-space: nowrap; overflow-wrap: normal; }
.log-row .props .act a + a { margin-left: .5em; }
.lvl { font-weight: 700; }
.lvl-error { color: var(--error); }
.lvl-warn { color: var(--warning); }
.lvl-debug, .lvl-trace { color: var(--muted); }
.log-paging { display: flex; gap: .5em; margin: .8em 0; flex-wrap: wrap; align-items: center; }
.log-paging .btn { margin: 0; }
/* Pressed, not a second primary: the page's one orange button is Search. */
.log-live[aria-pressed="true"] { background: var(--surface-sunken); border-color: var(--accent); }
html:not(.js) .js-only { display: none; }
.trace-waterfall { font-size: .85em; }
.trace-span { display: grid; grid-template-columns: minmax(12em, 30%) 1fr 6em; gap: .5em; align-items: center; padding: .15em 0; border-bottom: 1px solid var(--line); }
.trace-span .bar-track { position: relative; height: .9em; }
.trace-span .bar { position: absolute; top: 0; bottom: 0; background: var(--accent); min-width: 2px; }
.trace-span.status-error .bar { background: var(--error); }
.trace-span .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
/* The syntax help: a dialog on the Logs page (a bottom sheet on a phone),
   and the same content as its own page (`syntax_page`). */
.qh { width: min(640px, calc(100% - 32px)); max-height: min(80dvh, 720px); padding: 0; border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--paper-raised); color: var(--ink); }
.qh::backdrop { background: color-mix(in srgb, var(--paper) 40%, transparent); backdrop-filter: blur(2px); }
.qh-head { position: sticky; top: 0; z-index: 1; display: flex; align-items: center; justify-content: space-between; gap: 1em; padding: var(--space-md) var(--space-lg); border-bottom: 1px solid var(--line); background: var(--paper-raised); }
.qh-head h2 { margin: 0; padding: 0; border: 0; font-size: 1.1rem; }
.qh-head form { margin: 0; }
.qh-close { margin: 0; padding: .3em .6em; line-height: 1; }
.qh-body { padding: var(--space-sm) var(--space-lg) var(--space-lg); }
.qh-body h3 { margin: 1em 0 .4em; font-size: .8rem; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); }
.qh-ex { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
.qh-ex li { display: grid; grid-template-columns: minmax(0, 1fr) auto; grid-template-areas: "code use" "what use"; gap: 0 .6em; align-items: center; padding: 6px 8px; border: 1px solid var(--line); border-radius: var(--radius-sm); background: var(--paper-raised); }
.qh-ex code { grid-area: code; padding: 0; border: 0; background: none; overflow-wrap: anywhere; }
.qh-ex .qh-what { grid-area: what; font-size: .85em; color: var(--muted); }
.qh-ex .qh-use { grid-area: use; margin: 0; padding: .25em .7em; font-size: .85em; }
.qh-chips { display: flex; flex-wrap: wrap; gap: 4px; margin: .3em 0; }
.qh-chip { margin: 0; padding: .15em .55em; border: 1px solid var(--line); border-radius: 999px; background: var(--surface-sunken); color: var(--ink); font: .85em var(--font-mono); font-weight: 400; }
@media (max-width: 40em) {
  .qh { width: 100%; max-width: none; margin: auto 0 0; border-radius: var(--radius-lg) var(--radius-lg) 0 0; max-height: 85dvh; }
  .logs-search .quick { display: grid; grid-template-columns: 1fr 1fr; }
  .logs-search .quick select { width: 100%; min-width: 0; }
  .logs-search .quick label:has(select[name=range]), .logs-search .quick .own-requests { grid-column: 1 / -1; }
  .logs-search .quick label { display: grid; gap: .2em; font-size: .85em; }
  .logs-search .quick input[type=datetime-local] { width: 100%; min-width: 0; }
  .logs-search .quick .own-requests { display: flex; gap: .4em; }
  .q-help { padding-inline: .7em; }
  .q-help-label { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
}
@media (max-width: 40em) {
  .log-row > summary { grid-template-columns: 1fr auto auto; }
  .log-row > summary .svc { display: none; }
  .log-row > summary .row-acts { grid-row: 1; grid-column: 3; }
  .log-row > summary .msg { grid-column: 1 / -1; }
  .log-row .props td.act { width: auto; }
}
"#;

/// Caps the rows kept on the page while live, and toggles Live/Pause.
/// Starting a new search stops the stream, since its lines would belong
/// to the old one.
const PAGE_SCRIPT: &str = r##"(function () {
  var MAX_ROWS = 1000, live = null;
  function button() { return document.getElementById("log-live"); }
  function tailUrl() { var rows = document.getElementById("log-rows"); return rows && rows.dataset.tailUrl; }
  // Pause shows the moment Live is pressed, not when the stream answers.
  function show(on) {
    var b = button();
    if (!b) return;
    b.setAttribute("aria-pressed", on ? "true" : "false");
    b.querySelector(".log-live-label").textContent = on ? "Pause" : "Live";
    b.disabled = !on && !tailUrl();
  }
  function stop() {
    var cfg = live;
    live = null;
    if (cfg) { if (cfg.sse) cfg.sse.close(); else cfg.abort(); }
    show(false);
  }
  document.addEventListener("fx:config", function (evt) {
    // A line's properties load the first time it opens, never on close.
    if (evt.target.matches && evt.target.matches("details.log-row")) {
      if (!evt.target.open || evt.target.dataset.loaded) evt.preventDefault();
      else evt.target.dataset.loaded = "1";
      return;
    }
    if (evt.target.id === "log-live") {
      // Live follows the search shown: its stream starts after the newest
      // line the results hold.
      if (live || !tailUrl()) { evt.preventDefault(); stop(); return; }
      evt.detail.cfg.action = tailUrl();
      live = evt.detail.cfg;
      show(true);
    } else if (evt.target.id === "log-search") stop();
  });
  // The stream ended for good (or never opened).
  document.addEventListener("fx:finally", function (evt) {
    if (evt.target.id === "log-live" && live === evt.detail.cfg) { live = null; show(false); }
  });
  // New results: Live starts after their newest line, or not at all on an
  // older page.
  document.addEventListener("fx:swapped", function () { if (!live) show(false); });
  document.addEventListener("fx:sse:swapped", function () {
    var rows = document.querySelectorAll("#log-rows > .log-row");
    for (var i = MAX_ROWS; i < rows.length; i++) rows[i].remove();
  });
  // Syntax opens its help as a dialog instead of following the link to the
  // same help as a page. Use puts an example in the box; a name chip adds
  // that name to it.
  var help = document.getElementById("query-help"), helpLink = document.getElementById("query-help-link");
  var box = document.getElementById("log-q");
  if (help && helpLink && box && typeof help.showModal === "function") {
    helpLink.addEventListener("click", function (evt) { evt.preventDefault(); help.showModal(); });
    help.addEventListener("click", function (evt) {
      if (evt.target === help) { help.close(); return; }
      var use = evt.target.closest(".qh-use"), chip = evt.target.closest(".qh-chip");
      if (!use && !chip) return;
      evt.preventDefault();
      box.value = use ? use.dataset.q : (box.value.trim() ? box.value.trim() + " and " : "") + chip.textContent + " ";
      help.close();
      box.focus();
    });
  }
})();"##;

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
            table class="kv-table" {
                tbody {
                    tr { th { "target" } td { code { (row.target) } } td class="act" {} }
                    @for property in &row.properties {
                        tr {
                            th { (property.name) }
                            td {
                                code { (property.value) }
                                @if let Some(note) = &property.note { " " span class="muted" { (note) } }
                            }
                            td class="act" {
                                @if let Some(find) = &property.find_url {
                                    a href=(find) title=(format!("Only lines where {} is this", property.name)) { "Find" }
                                }
                                @if let Some(exclude) = &property.exclude_url {
                                    a href=(exclude) title=(format!("Hide lines where {} is this", property.name)) { "Exclude" }
                                }
                            }
                        }
                    }
                }
            }
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

/// `/dashboard/admin/logs/row/{cursor}` without JavaScript: one line,
/// opened, with a way back to the search it came from.
pub fn row_page(chrome: &PageChrome, row_view: Option<&RowView>, back_url: &str) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) } };
    let body = html! {
        div class="wrap wrap-wide" {
            nav class="context-nav" aria-label="Breadcrumb" {
                a href="/dashboard" { "Dashboard" } " / " a href=(back_url) { "Logs" }
            }
            h1 { "Log line" }
            @if let Some(r) = row_view {
                div class="log-rows" { (row(r)) }
            } @else {
                p class="muted" { "This line is no longer kept: log retention has deleted it." }
            }
            p { a href=(back_url) { "Back to the search" } }
        }
    };
    layout_with_head(chrome, "Log line - Monokulo", extra_head, body)
}

/// The "Older" control: a plain link to the next page, which fixi turns
/// into "append the next page here".
fn more(more_url: Option<&str>, older_url: Option<&str>) -> Markup {
    html! {
        div id="log-more" {
            @if let (Some(older), Some(more)) = (older_url, more_url) {
                p class="log-paging" {
                    a class="btn btn-secondary" href=(older) fx-action=(more) fx-target="#log-more" fx-swap="outerHTML" { "Older" }
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
    html! { @if let Some(problem) = problem { p class="error" role="status" { (problem) } } }
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
                div class="error query-error" role="alert" data-fx-focus {
                    p { "That search doesn't read as a query: " (error.message) "." }
                    p { code { (error.before) mark { (error.marked) } (error.after) } }
                }
            }
            @for problem in &vm.problems {
                p class="error" role="status" { (problem) }
            }
            // Live says here when the engine's lines stop arriving.
            div id="log-tail-problem" {}
            // Shown above the search form (`.log-histogram-box`).
            @if let Some(h) = &vm.histogram { (histogram(h)) }
            @if let Some(newer) = &vm.newer_url {
                p class="log-paging" { a class="btn btn-secondary" href=(newer) { "Newer" } }
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
    html! {
        section id="saved-searches" class="box" {
            h2 { "Saved searches" }
            @if let Some(error) = error {
                p class="error" role="alert" data-fx-focus { (error) }
            }
            @if saved.is_empty() {
                p class="muted" { "None yet." }
            } @else {
                ul {
                    @for s in saved {
                        li {
                            a href=(format!("/dashboard/admin/logs?{}", s.query_string)) { (s.name) }
                            " "
                            form method="post" action=(format!("/dashboard/admin/logs/saved/{}/delete", s.id)) style="display:inline"
                                fx-action=(format!("/dashboard/admin/logs/saved/{}/delete", s.id)) fx-method="POST" fx-target="#saved-searches" {
                                input type="hidden" name="query_string" value=(query_string);
                                button type="submit" class="btn-secondary" aria-label=(format!("Remove {}", s.name)) { "Remove" }
                            }
                        }
                    }
                }
            }
            form method="post" action="/dashboard/admin/logs/saved" fx-action="/dashboard/admin/logs/saved" fx-method="POST" fx-target="#saved-searches" {
                input type="hidden" name="query_string" value=(query_string);
                label { "Save this search as " input type="text" name="name" required maxlength="80"; }
                " "
                button type="submit" { "Save" }
            }
        }
    }
}

fn select(name: &str, label: &str, options: &[(&str, &str)], current: &str) -> Markup {
    html! {
        label {
            span class="sr-only" { (label) }
            select name=(name) aria-label=(label) {
                @for (value, text) in options {
                    option value=(value) selected[*value == current] { (text) }
                }
            }
        }
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
                "service = 'scanner' or has error",
                "either; has means the line has that property",
            ),
            ("not (service = 'scanner')", "brackets group; not negates"),
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
                        a class="btn qh-use" data-q=(query) href=(format!("/dashboard/admin/logs?q={}", url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>())) { "Use" }
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
        p class="hint" { "Built in, and seen on recent lines. Every other name is a property of the line." }
        p class="qh-chips" {
            @for name in names {
                @if in_dialog { button type="button" class="qh-chip" { (name) } } @else { code class="qh-chip" { (name) } }
            }
        }
    }
}

/// The syntax help as a dialog, opened by the Syntax link's script.
fn syntax_dialog(names: &[String]) -> Markup {
    html! {
        dialog id="query-help" class="qh" aria-labelledby="query-help-title" closedby="any" {
            div class="qh-head" {
                h2 id="query-help-title" { "Search syntax" }
                form method="dialog" { button class="qh-close" aria-label="Close" { "✕" } }
            }
            div class="qh-body" { (syntax(names, true)) }
        }
    }
}

/// `GET /dashboard/admin/logs/syntax`: the same help as the dialog, for a
/// browser without script.
pub fn syntax_page(chrome: &PageChrome, names: &[String]) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) } };
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard/admin/logs" { "Logs" } }
            h1 { "Search syntax" }
            div class="qh-page" { (syntax(names, false)) }
        }
    };
    layout_with_head(chrome, "Search syntax - Monokulo", extra_head, body)
}

/// Live's stream when the page opens on an older page; the script takes
/// the URL from the results anyway.
const LIVE_NONE: &str = "/dashboard/admin/logs/tail";

pub fn page(chrome: &PageChrome, vm: &LogsViewModel) -> Markup {
    let extra_head = html! { style { (PreEscaped(PAGE_STYLE)) } };
    let body = html! {
        div class="wrap wrap-wide" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            div class="logs-head" {
                h1 { "Logs" }
                // Submits the search as it stands: its newest lines.
                button type="submit" form="log-search" class="btn-secondary" { (icon(REFRESH_ICON)) span { "Refresh" } }
                // Streams from `#log-rows`' tail URL (the page's script).
                button type="button" id="log-live" class="btn-secondary log-live js-only" aria-pressed="false" disabled[vm.tail_url.is_none()]
                    fx-action=(vm.tail_url.as_deref().unwrap_or(LIVE_NONE)) fx-target="#log-rows" fx-swap="afterbegin" fx-sse-reconnect {
                    span class="i-play" { (icon(PLAY_ICON)) }
                    span class="i-pause" { (icon(PAUSE_ICON)) }
                    span class="log-live-label" { "Live" }
                }
            }
            div class="logs-body" {
                form id="log-search" class="logs-search" method="get" action="/dashboard/admin/logs"
                    fx-action="/dashboard/admin/logs" fx-target="#log-results" fx-swap="outerHTML" fx-push-url fx-replace fx-submit-on-change {
                    div class="q-row" {
                        label for="log-q" class="sr-only" { "Search" }
                        input type="search" id="log-q" name="q" value=(vm.form.q) autocomplete="off" spellcheck="false"
                            placeholder="level >= warn and order.id = 'o_1'";
                        // A page of its own without script; a dialog with it.
                        a class="btn q-help" id="query-help-link" href=(SYNTAX_PAGE) aria-haspopup="dialog" {
                            svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true" focusable="false" {
                                circle cx="12" cy="12" r="9.5" {}
                                path d="M9.5 9.2a2.6 2.6 0 0 1 5 .8c0 1.8-2.5 2.2-2.5 3.8" {}
                                circle cx="12" cy="17.3" r=".6" fill="currentColor" {}
                            }
                            span class="q-help-label" { "Syntax" }
                        }
                        button type="submit" class="btn-primary" { "Search" }
                    }
                    div class="quick" {
                        (select("level", "Level", LEVELS, &vm.form.level))
                        (select("service", "Service", SERVICES, &vm.form.service))
                        (select("range", "Time range", RANGES, &vm.form.range))
                        label { "From " input type="datetime-local" name="from" value=(vm.form.from); }
                        label { "To " input type="datetime-local" name="to" value=(vm.form.to); }
                        label class="own-requests" {
                            input type="checkbox" name="logs_requests" value="show" checked[vm.form.logs_requests];
                            "Show the Logs page's own requests"
                        }
                    }
                }
                (results(vm))
            }
            (syntax_dialog(&vm.attribute_names))
            (saved_searches(&vm.saved, &vm.query_string, vm.saved_error.as_deref()))
        }
        script { (PreEscaped(PAGE_SCRIPT)) }
    };
    layout_with_head(chrome, "Logs - Monokulo", extra_head, body)
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
    let body = html! {
        div class="wrap wrap-wide" {
            nav class="context-nav" aria-label="Breadcrumb" {
                a href="/dashboard" { "Dashboard" } " / " a href="/dashboard/admin/logs" { "Logs" }
            }
            h1 { "POS session" }
            p class="hint" {
                code { (vm.session) }
                @if let Some((name, href)) = &vm.store { " · " a href=(href) { (name) } }
                @if let Some(device) = &vm.device { br; span class="muted" { (device) } }
            }
            @for problem in &vm.problems { p class="error" role="status" { (problem) } }
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
                    p class="hint" { "Only the first events of a very long session are shown. " a href=(vm.logs_url) { "Search them all" } }
                }
                p class="hint" {
                    "Times are the tablet's clock, in " (vm.zone_label) ". "
                    a href=(vm.logs_url) { "Show these lines in Logs" }
                }
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
        }
    };
    layout_with_head(chrome, "POS session - Monokulo", extra_head, body)
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
    let body = html! {
        div class="wrap wrap-wide" {
            nav class="context-nav" aria-label="Breadcrumb" {
                a href="/dashboard" { "Dashboard" } " / " a href="/dashboard/admin/logs" { "Logs" }
            }
            h1 { "Trace " code { (vm.trace_id) } }
            @for problem in &vm.problems { p class="error" role="status" { (problem) } }
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
                            table class="kv-table props" {
                                @for (name, value) in &span.attributes { tr { th { (name) } td { code { (value) } } } }
                            }
                        }
                    }
                }
            }
            h2 { "Lines" }
            p class="hint" { a href=(vm.logs_url) { "Search these lines" } }
            div class="log-rows" { @for r in &vm.rows { (row(r)) } }
        }
    };
    layout_with_head(chrome, "Trace - Monokulo", extra_head, body)
}
