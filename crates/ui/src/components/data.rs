//! Rows read straight from a database: a grid of any shape (a few columns
//! or dozens, a short value or a megabyte of JSON), and the tables they
//! can come from.

use crate::{component, Attrs, Slot, UiEnum};

component! {
    /// A query's rows, any number of columns: in a box of its own that
    /// scrolls both ways, its header row and first column staying in sight.
    /// Above it, what came back ("4 rows · 9 ms") and, for a wide one, that
    /// it scrolls sideways.
    pub struct DataGrid {
        /// The header row: a `Row` of header `DataCell`s.
        head: Slot,
        /// The rows: `Row`s of `DataCell`s.
        children: Slot,
        /// What the grid holds, for screen readers: the scroll box's name.
        caption: String,
        /// At the bar's left: what came back, a tag for hidden columns.
        stats: Option<Slot>,
        /// At the bar's right: "Scroll sideways for more columns".
        scroll_hint: Option<Slot>,
        /// The first column stays while the rest scroll sideways.
        #[default(true)]
        sticky_first: bool,
        rest: Attrs,
    }
    html {
        div class="mk-data-grid" data-sticky-first="" if sticky_first ..rest {
            div class="mk-data-grid-bar" {
                @if let Some(stats) = stats { span class="mk-data-grid-stats" { (stats) } }
                @if let Some(scroll_hint) = scroll_hint { span class="mk-data-grid-hint" { (scroll_hint) } }
            }
            div class="mk-data-grid-scroll" tabindex="0" role="region" aria-label=(caption) {
                table {
                    thead { (head) }
                    tbody { (children) }
                }
            }
        }
    }
}

/// What a grid cell holds, beyond plain text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum DataKind {
    /// A number: right-aligned, figures lined up.
    Number,
    /// SQL's NULL: the word, muted and in italics.
    Null,
    /// Bytes: their size and first bytes in hex, monospace.
    Blob,
    /// A value kept back (a secret column): a muted "hidden".
    Hidden,
}

component! {
    /// One cell of a `DataGrid`. A long value (`size`) is one line, cut
    /// with an ellipsis, with its size and its `actions` (Expand, Copy)
    /// after it; `cut` says when only part of it came back at all.
    pub struct DataCell {
        children: Slot,
        kind: Option<DataKind>,
        /// A long value's size ("1.8 KB").
        size: Option<String>,
        /// Only part of the value came back ("cut at 64 KiB of 2.4 MB").
        cut: Option<String>,
        /// After a long value: `IconButton`s or `IconLink`s.
        actions: Option<Slot>,
        #[default(false)]
        header: bool,
        rest: Attrs,
    }
    html {
        @if header {
            th scope="col" class="mk-data-cell" data-kind=[kind] ..rest { (children) }
        } @else {
            td class="mk-data-cell" data-kind=[kind] ..rest {
                @if let Some(actions) = actions {
                    div class="mk-data-long" {
                        span class="mk-data-value" { (children) }
                        @if let Some(size) = size { span class="mk-data-size" { (size) } }
                        @if let Some(cut) = cut { span class="mk-data-cut" { (cut) } }
                        span class="mk-data-actions" { (actions) }
                    }
                } @else {
                    (children)
                }
            }
        }
    }
}

pub(super) fn data_grid_examples() -> Vec<(&'static str, DataGrid)> {
    let head = crate::Row::new(Slot::many(
        ["id", "status", "amount_fiat", "payment_id", "payload"]
            .map(|name| Slot::from(DataCell::new(name).header(true))),
    ));
    let row = |id: &str, status: &str, fiat: &str, payload: &str, size: &str| {
        Slot::from(crate::Row::new(Slot::many([
            DataCell::new(id).into(),
            Slot::from(DataCell::new(status)),
            Slot::from(DataCell::new(fiat).kind(DataKind::Number)),
            Slot::from(DataCell::new("NULL").kind(DataKind::Null)),
            Slot::from(
                DataCell::new(payload)
                    .size(size)
                    .actions(crate::IconButton::new("Expand payload", "⤢")),
            ),
        ])))
    };
    vec![(
        "Rows, a long value in each",
        DataGrid::new(
            head,
            Slot::many([
                row(
                    "order_2282ae",
                    "paid",
                    "24.50",
                    r#"{"event":"order.paid","order":{"id":"order_2282ae"}}"#,
                    "1.8 KB",
                ),
                row(
                    "order_77c0d1",
                    "confirmed",
                    "9.00",
                    r#"{"event":"order.confirmed","order":{"id":"order_77c0d1"}}"#,
                    "1.9 KB",
                ),
            ]),
            "Query results",
        )
        .stats("2 rows · 6 ms · monokulo"),
    )]
}

component! {
    /// The tables a query can read, beside its editor: a label, a filter
    /// box, then one compact `Disclosure` per table (its name and rows)
    /// that opens to its columns. Scrolls inside its own height.
    pub struct TableBrowser {
        /// What it lists ("Tables").
        label: String,
        /// The tables: compact `Disclosure`s.
        children: Slot,
        /// Above the list: an `Input` that narrows it.
        filter: Option<Slot>,
        rest: Attrs,
    }
    html {
        nav class="mk-table-browser" aria-label=(label) ..rest {
            p class="mk-table-browser-label" { (label) }
            @if let Some(filter) = filter { (filter) }
            div class="mk-table-browser-list" { (children) }
        }
    }
}

pub(super) fn table_browser_examples() -> Vec<(&'static str, TableBrowser)> {
    let table = |name: &str, rows: &str, columns: Vec<crate::Fact>| {
        Slot::from(
            crate::Disclosure::new(
                Slot::many([
                    Slot::from(name.to_string()),
                    Slot::from(crate::Tag::new(rows.to_string()).tone(crate::TagTone::Muted)),
                ]),
                crate::Facts::new(columns),
            )
            .look(crate::DisclosureLook::Compact),
        )
    };
    let column = |name: &str, value: Slot| crate::Fact {
        term: name.into(),
        value,
    };
    vec![(
        "Two tables",
        TableBrowser::new(
            "Tables",
            Slot::many([
                table(
                    "stores",
                    "3",
                    vec![column("id", "TEXT".into()), column("name", "TEXT".into())],
                ),
                table(
                    "sessions",
                    "4",
                    vec![
                        column("user_id", "TEXT".into()),
                        column(
                            "token",
                            crate::Tag::new("hidden").tone(crate::TagTone::Muted).into(),
                        ),
                    ],
                ),
            ]),
        )
        .filter(
            crate::Input::new("table-filter")
                .label("Filter tables")
                .placeholder("Filter tables…"),
        ),
    )]
}
