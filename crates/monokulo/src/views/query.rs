//! The admin Query page (`http::query_page`): read-only SQL against
//! monokulo's database or the engine's, with the tables beside the editor
//! and the rows under them, the whole width of a wide page.
//!
//! Without JavaScript everything is a GET form or a link: the URL holds
//! the database and the statement, so a query can be bookmarked or shared
//! like a Logs search, and a long value opens on a page of its own. With
//! it (fixi and `static/query.js`), Run and Ctrl/⌘+Enter swap only the
//! results, picking the other database swaps the tables and the editor,
//! a long value opens in a dialog, and the table list can be filtered.

use maud::{html, Markup, PreEscaped};
use shared::sqlite_query::{Cell, QueryError, QueryResult, TableInfo, ROW_COUNT_CAP};
use ui::assets::icons;
use ui::{
    Alert, Button, ButtonType, ButtonVariant, Choice, ControlSize, CopyBlock, DataCell, DataGrid,
    DataKind, Dialog, DialogActions, DialogSize, Disclosure, DisclosureLook, Fact, Facts,
    FormActions, Hint, IconButton, IconLink, Input, LinkButton, PageHeader, Row, Select, Slot,
    TableBrowser, Tag, TagTone, TextArea, Tone,
};

use super::{script, DialogContent, Load, PageChrome};

/// The page's address.
pub const PAGE: &str = "/admin/query";

/// Which database a query reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryDb {
    #[default]
    Monokulo,
    Engine,
}

impl QueryDb {
    /// The one `db` names; monokulo's for anything else.
    pub fn from_id(id: &str) -> QueryDb {
        if id == "engine" {
            QueryDb::Engine
        } else {
            QueryDb::Monokulo
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            QueryDb::Monokulo => "monokulo",
            QueryDb::Engine => "engine",
        }
    }

    fn label(self) -> &'static str {
        match self {
            QueryDb::Monokulo => "Monokulo",
            QueryDb::Engine => "Engine",
        }
    }

    /// Statements worth starting from, as (label, SQL).
    fn examples(self) -> [(&'static str, &'static str); 3] {
        match self {
            QueryDb::Monokulo => [
                (
                    "Latest orders",
                    "SELECT order_id, currency, amount, source, created_at_utc\nFROM order_currency_metadata\nORDER BY created_at_utc DESC\nLIMIT 50",
                ),
                (
                    "Stores and their wallets",
                    "SELECT s.name, s.site, w.name AS wallet, w.network\nFROM store_connections s\nLEFT JOIN wallets w ON w.id = s.wallet_id\nORDER BY s.name",
                ),
                (
                    "Webhooks that gave up",
                    "SELECT id, event_type, order_id, last_status_code, last_error, gave_up_at_utc\nFROM webhook_deliveries\nWHERE gave_up_at_utc IS NOT NULL\nORDER BY gave_up_at_utc DESC",
                ),
            ],
            QueryDb::Engine => [
                (
                    "Latest orders",
                    "SELECT id, tenant_id, status, xmr_amount_piconero, confirmations, created_at_utc\nFROM orders\nORDER BY created_at_utc DESC\nLIMIT 50",
                ),
                (
                    "Unpaid orders per store",
                    "SELECT tenant_id, count(*) AS unpaid\nFROM orders\nWHERE status = 'pending'\nGROUP BY tenant_id\nORDER BY unpaid DESC",
                ),
                (
                    "Scanner progress",
                    "SELECT network, max(height) AS scanned_to\nFROM scanned_blocks\nGROUP BY network",
                ),
            ],
        }
    }
}

/// The page's address for `db` and `sql` (which runs it).
pub fn href(db: QueryDb, sql: &str) -> String {
    format!(
        "{PAGE}?{}",
        serde_urlencoded::to_string([("db", db.id()), ("sql", sql)]).unwrap_or_default()
    )
}

/// What the last statement did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing run yet.
    Nothing,
    Ran(QueryResult),
    Refused(QueryError),
    /// Another query of this admin's, or two in all, is still running.
    Busy,
    /// The engine didn't answer.
    Unreachable(String),
}

pub struct QueryViewModel {
    pub db: QueryDb,
    pub sql: String,
    /// The database's tables (none when it can't be read).
    pub tables: Vec<TableInfo>,
    /// The engine was asked and didn't answer.
    pub engine_unreachable: bool,
    pub outcome: Outcome,
}

fn icon(svg: &'static str) -> Markup {
    html! { (PreEscaped(svg)) }
}

/// `n` with thousands separated: 1,204.
fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A size in bytes, short: 512 B, 1.8 KB, 2.4 MB.
fn size(bytes: usize) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// A value longer than this shows on one line with its size, Expand and
/// Copy.
const LONG: usize = 80;

pub fn page(chrome: &PageChrome, vm: &QueryViewModel) -> Markup {
    let header = PageHeader::new("Query")
        .crumbs(super::crumbs(&[("/", "Home")]))
        .actions(html! {
            // With JavaScript, picking a database swaps the tables and the
            // editor; without, Go opens it.
            form class="engine-network" method="get" action=(crate::base::url(PAGE))
                fx-action=(crate::base::url(format!("{PAGE}/schema"))) fx-target="#query-workspace" fx-swap="outerHTML"
                fx-submit-on-change {
                label for="query-db" class="visually-hidden" { "Database" }
                (Select::new("db", [QueryDb::Monokulo, QueryDb::Engine].map(|db| {
                    let choice = Choice::new(db.id(), db.label()).selected(db == vm.db);
                    if db == QueryDb::Engine && vm.engine_unreachable {
                        choice.note("unreachable")
                    } else {
                        choice
                    }
                }).to_vec())
                .id("query-db")
                .compact(true))
                (Button::new("Go").kind(ButtonType::Submit).attr("data-only", "no-script"))
            }
            @if !vm.sql.trim().is_empty() {
                (LinkButton::new("Export CSV", crate::base::url(format!("{PAGE}/export?{}", serde_urlencoded::to_string([("db", vm.db.id()), ("sql", vm.sql.as_str()), ("format", "csv")]).unwrap_or_default())))
                    .size(ControlSize::Compact))
                (IconLink::new("Run again", crate::base::url(href(vm.db, &vm.sql)), icon(icons::REFRESH)))
            }
        });
    let body = html! {
        (workspace(vm))
        (Dialog::new("expand-value", "expand-value-title", html! {
            div id="expand-value-body" {}
        }).size(DialogSize::Wide))
        (script("query.js", Load::Defer))
    };
    super::Page::new("Query - Monokulo", header)
        .class("query-page")
        .wide()
        .head(html! { style { (PreEscaped(QUERY_STYLE)) } })
        .render(chrome, body)
}

/// The page's layout (no colours: those are the ui's roles): the tables
/// and the editor in one band, the results the whole width under them; on
/// a phone the editor first, then the tables folded, then the results.
const QUERY_STYLE: &str = r#"
.query-band { display: grid; grid-template-columns: minmax(12rem, 16rem) minmax(0, 1fr); gap: var(--space-md); height: 17rem; }
.query-band > * { min-height: 0; }
.query-tables { min-height: 0; margin: 0; }
.query-tables > summary { display: none; }
/* The band's own height: a percentage doesn't reach through <details>. */
.query-tables > .mk-table-browser { height: 17rem; }
.query-table-rows { display: inline-block; margin-top: var(--space-xs); font-size: var(--text-sm); }
.query-editor { display: flex; flex-direction: column; gap: var(--space-sm); margin: 0; min-width: 0; }
.query-editor textarea { flex: 1; min-height: 6rem; font-family: var(--font-mono); resize: vertical; }
.query-editor .form-actions { margin: 0; align-items: center; }
.query-label { margin: 0; font-size: var(--text-xs); font-weight: 800; letter-spacing: .06em; text-transform: uppercase; color: var(--muted); }
.query-examples { margin: var(--space-xs) 0 0; padding-left: 1.2em; }
.query-results { margin-top: var(--space-md); }
.query-empty { text-align: center; }
.query-empty .form-actions { justify-content: center; }
@media (max-width: 640px) {
  .query-band { grid-template-columns: minmax(0, 1fr); height: auto; gap: var(--space-md); }
  .query-tables { order: 2; }
  .query-tables > summary { display: list-item; font-weight: 700; cursor: pointer; }
  .query-tables > .mk-table-browser { height: auto; max-height: 20rem; margin-top: var(--space-sm); }
}
"#;

/// The tables, the editor and the results: what picking the other
/// database swaps.
pub fn workspace(vm: &QueryViewModel) -> Markup {
    html! {
        div id="query-workspace" {
            div class="query-band" {
                (table_browser(vm))
                (editor(vm))
            }
            (results(vm))
        }
    }
}

fn table_browser(vm: &QueryViewModel) -> Markup {
    let tables = Slot::many(vm.tables.iter().map(|table| {
        let rows = if table.rows_capped() {
            format!("{}+", count(ROW_COUNT_CAP))
        } else {
            count(table.rows)
        };
        let columns = table
            .columns
            .iter()
            .map(|column| Fact {
                term: column.name.clone(),
                value: if column.hidden {
                    Slot::from(
                        Tag::new("hidden")
                            .tone(TagTone::Muted)
                            .icon(icon(icons::LOCK)),
                    )
                } else {
                    Slot::from(column.decl_type.clone())
                },
            })
            .collect::<Vec<_>>();
        let sql = format!(
            "SELECT * FROM \"{}\" LIMIT 100",
            table.name.replace('"', "\"\"")
        );
        Slot::from(
            Disclosure::new(
                html! { (table.name) (Tag::new(rows).tone(TagTone::Muted)) },
                html! {
                    (Facts::new(columns))
                    a class="query-table-rows" href=(crate::base::url(href(vm.db, &sql))) { "Show its rows" }
                },
            )
            .look(DisclosureLook::Compact)
            .attr("data-table", table.name.as_str()),
        )
    }));
    html! {
        // On a phone the list folds behind "Tables (N)" (static/query.js);
        // on a wider screen it is always open beside the editor.
        details class="query-tables" open {
            summary { "Tables (" (vm.tables.len()) ")" }
            (TableBrowser::new("Tables", tables).filter(
                Input::new("table-filter")
                    .label("Filter tables")
                    .placeholder("Filter tables…")
                    .size(ControlSize::Compact)
                    .attr("data-only", "script")
                    .attr("data-table-filter", ""),
            ))
        }
    }
}

fn editor(vm: &QueryViewModel) -> Markup {
    html! {
        form class="box query-editor" method="get" action=(crate::base::url(PAGE))
            fx-action=(crate::base::url(PAGE)) fx-target="#query-results" fx-swap="outerHTML" fx-push-url fx-replace {
            input type="hidden" name="db" value=(vm.db.id());
            label class="query-label" for="query-sql" { "SQL" }
            (TextArea::new("sql")
                .id("query-sql")
                .value(vm.sql.clone())
                .rows(5usize)
                .described_by("query-sql-help")
                .attr("spellcheck", "false"))
            (FormActions::new(Slot::many([
                Slot::from(Hint::new("One statement. Rows past 1,000 are cut.").inline(true).attr("id", "query-sql-help")),
                Slot::from(Disclosure::new("Examples", html! {
                    ul class="query-examples" {
                        @for (label, sql) in vm.db.examples() {
                            li { a href=(crate::base::url(href(vm.db, sql))) { (label) } }
                        }
                    }
                })),
                Slot::from(html! { span class="spacer" {} }),
                Slot::from(Button::new("Run").variant(ButtonVariant::Primary).kind(ButtonType::Submit)),
            ])))
        }
    }
}

/// What the last statement did: rows, why it didn't run, or where to
/// start. `#query-results`, which Run swaps.
pub fn results(vm: &QueryViewModel) -> Markup {
    html! {
        section id="query-results" class="query-results" aria-label="Results" {
            @match &vm.outcome {
                Outcome::Nothing => (empty(vm)),
                Outcome::Ran(result) => (grid(vm, result)),
                Outcome::Refused(error) => (refused(error)),
                Outcome::Busy => (Alert::new("Wait for it to finish, then run this again.", Tone::Warning)
                    .title("A query is already running.")
                    .icon(icon(icons::WARNING))
                    .attr("data-fx-focus", "")),
                Outcome::Unreachable(why) => (Alert::new(why.as_str(), Tone::Error)
                    .title("The engine can't be reached.")
                    .icon(icon(icons::WARNING))
                    .attr("data-fx-focus", "")),
            }
        }
    }
}

fn empty(vm: &QueryViewModel) -> Markup {
    html! {
        div class="box query-empty" {
            p { strong { "Pick a table on the left, or write a SELECT." } }
            (Hint::new("Nothing here can change data. Results stop at 1,000 rows and 5 seconds. Every query is written to the Logs page."))
            (FormActions::new(Slot::many(vm.db.examples().map(|(label, sql)| {
                Slot::from(LinkButton::new(label, crate::base::url(href(vm.db, sql))).size(ControlSize::Compact))
            }))))
        }
    }
}

fn refused(error: &QueryError) -> Markup {
    let (title, body) = match error {
        QueryError::Empty => (
            "There is nothing to run.",
            html! { "Write a SELECT first." },
        ),
        QueryError::NotOneStatement => (
            "Not run: one statement at a time.",
            html! { "Run them one by one." },
        ),
        QueryError::Refused { action } => (
            "Not run: this page only reads.",
            html! { code { (action) } " isn't a read, so it is refused. Use a " code { "SELECT" } "." },
        ),
        QueryError::Syntax { message, offset } => (
            "Not run: SQLite couldn't read it.",
            html! {
                (message)
                @if let Some(offset) = offset { " (at character " (offset + 1) ")" }
            },
        ),
        QueryError::Timeout { seconds } => (
            "Stopped: it ran too long.",
            html! { "Stopped after " (seconds) " s. Narrow it with WHERE or LIMIT." },
        ),
        QueryError::Failed { message } => ("It stopped.", html! { (message) }),
    };
    html! {
        (Alert::new(body, Tone::Error)
            .title(title)
            .icon(icon(icons::WARNING))
            .attr("data-fx-focus", ""))
    }
}

fn grid(vm: &QueryViewModel, result: &QueryResult) -> Markup {
    let wide = result.columns.len() > 8;
    let rows = result.rows.len() as u64;
    let mut stats = format!("{} {}", count(rows), if rows == 1 { "row" } else { "rows" });
    if wide {
        stats.push_str(&format!(" · {} columns", result.columns.len()));
    }
    stats.push_str(&format!(" · {} ms · {}", result.elapsed_ms, vm.db.id()));
    let hidden = result.columns.iter().filter(|c| c.hidden).count();
    let head =
        Slot::from(Row::new(Slot::many(result.columns.iter().map(|column| {
            Slot::from(DataCell::new(column.name.clone()).header(true))
        }))));
    let body = Slot::many(result.rows.iter().enumerate().map(|(r, row)| {
        Slot::from(Row::new(Slot::many(row.iter().enumerate().map(
            |(c, cell)| {
                let column = &result.columns[c];
                Slot::from(data_cell(vm, r, c, &column.name, column.hidden, cell))
            },
        ))))
    }));
    let mut grid = DataGrid::new(head, body, "Query results")
        .stats(Slot::many([
            Slot::from(stats),
            Slot::from(html! {
                @if hidden > 0 {
                    (Tag::new(format!("{hidden} {} hidden", if hidden == 1 { "column" } else { "columns" }))
                        .tone(TagTone::Muted)
                        .icon(icon(icons::LOCK)))
                }
            }),
        ]))
        .attr("data-fx-focus", "");
    if wide {
        grid = grid.scroll_hint(html! {
            (icon(icons::ARROW_LEFT)) " Scroll sideways for more columns " (icon(icons::ARROW_RIGHT))
        });
    }
    html! {
        @if result.cut {
            (Hint::new(format!(
                "Only the first {} rows are shown. Add a LIMIT, or export.",
                count(rows)
            )))
        }
        @if result.rows.is_empty() {
            p class="muted" { (stats_line(result, vm)) }
        } @else {
            (grid)
        }
    }
}

fn stats_line(result: &QueryResult, vm: &QueryViewModel) -> String {
    format!("No rows · {} ms · {}", result.elapsed_ms, vm.db.id())
}

fn data_cell(
    vm: &QueryViewModel,
    row: usize,
    col: usize,
    column: &str,
    hidden: bool,
    cell: &Cell,
) -> DataCell {
    if hidden {
        return DataCell::new("hidden").kind(DataKind::Hidden);
    }
    let expand = || {
        let url = crate::base::url(format!(
            "{PAGE}/value?{}",
            serde_urlencoded::to_string([
                ("db", vm.db.id()),
                ("sql", vm.sql.as_str()),
                ("row", &row.to_string()),
                ("col", &col.to_string()),
            ])
            .unwrap_or_default()
        ));
        Slot::from(
            IconLink::new(
                format!("Expand {column} of row {}", row + 1),
                url.clone(),
                icon(icons::EXPAND),
            )
            .attr("fx-action", format!("{url}&part=dialog"))
            .attr("fx-target", "#expand-value-body")
            .attr("fx-swap", "innerHTML")
            .attr("data-opens-dialog", "expand-value"),
        )
    };
    let cut = |len: usize, kept: usize| {
        (kept < len).then(|| format!("cut at {} of {}", size(kept), size(len)))
    };
    match cell {
        Cell::Null => DataCell::new("NULL").kind(DataKind::Null),
        Cell::Integer { value } => DataCell::new(value.to_string()).kind(DataKind::Number),
        Cell::Real { value } => DataCell::new(value.to_string()).kind(DataKind::Number),
        Cell::Text { text, len } if *len > LONG || text.contains('\n') => {
            let cell = DataCell::new(text.clone())
                .size(size(*len))
                .actions(Slot::many([
                    expand(),
                    Slot::from(
                        IconButton::new(
                            format!("Copy {column} of row {}", row + 1),
                            icon(icons::COPY),
                        )
                        .attr("data-copy-cell", "")
                        .attr("data-only", "script"),
                    ),
                ]));
            match cut(*len, text.len()) {
                Some(cut) => cell.cut(cut),
                None => cell,
            }
        }
        Cell::Text { text, .. } => DataCell::new(text.clone()),
        Cell::Blob { hex, len } => {
            let shown = &hex[..hex.len().min(14)];
            DataCell::new(format!(
                "‹blob {}› {shown}{}",
                size(*len),
                if hex.len() > shown.len() { "…" } else { "" }
            ))
            .kind(DataKind::Blob)
            .actions(
                IconButton::new("Copy the bytes as hex", icon(icons::COPY))
                    .attr("data-copy", hex.as_str())
                    .attr("data-only", "script"),
            )
        }
    }
}

/// One long value, whole: its column, its row and its size above it, then
/// the value (pretty-printed when it is JSON) to read or copy. In the
/// page's dialog with JavaScript; a page of its own without.
pub fn value(column: &str, row_id: &str, cell: &Cell) -> DialogContent {
    let (text, kind, len) = match cell {
        Cell::Text { text, len } => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(json) if json.is_object() || json.is_array() => (
                serde_json::to_string_pretty(&json).unwrap_or_else(|_| text.clone()),
                "JSON",
                *len,
            ),
            _ => (text.clone(), "text", *len),
        },
        Cell::Blob { hex, len } => (hex.clone(), "bytes, as hex", *len),
        other => (other.to_text().unwrap_or_else(|| "NULL".into()), "value", 0),
    };
    let aside = format!("{row_id} · {} · {kind}", size(len));
    DialogContent {
        title_id: "expand-value-title".into(),
        title: html! { (column) },
        aside: Some(html! { (Hint::new(aside).inline(true)) }),
        body: html! {
            @if cell.is_cut() {
                (Hint::new(format!("Only the first {} came back.", size(text.len().min(len)))))
            }
            (CopyBlock::new(text).copy_label(format!("Copy {column}")))
        },
    }
}

/// [`value`] in the page's dialog.
pub fn value_dialog(content: DialogContent) -> Markup {
    html! {
        (content.in_dialog())
        (DialogActions::new(Button::new("Close").attr("data-closes-dialog", "")))
    }
}

/// [`value`] as a page, for a browser without JavaScript.
pub fn value_page(chrome: &PageChrome, back: &str, content: DialogContent) -> Markup {
    let (header, body) = content.page(
        vec![ui::Crumb::new(crate::base::url(back), "Query")],
        DialogSize::Wide,
    );
    super::Page::new("Value - Query - Monokulo", header)
        .wide()
        .render(chrome, body)
}

#[cfg(test)]
mod tests;
