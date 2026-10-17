use shared::sqlite_query::{ColumnInfo, ResultColumn};

use super::*;

fn chrome() -> PageChrome {
    PageChrome::from_user(None, PAGE)
}

fn vm(sql: &str, outcome: Outcome) -> QueryViewModel {
    QueryViewModel {
        db: QueryDb::Monokulo,
        sql: sql.into(),
        tables: vec![TableInfo {
            name: "users".into(),
            rows: 1_204,
            columns: vec![
                ColumnInfo {
                    name: "email".into(),
                    decl_type: "TEXT".into(),
                    hidden: false,
                },
                ColumnInfo {
                    name: "password_hash".into(),
                    decl_type: "TEXT".into(),
                    hidden: true,
                },
            ],
        }],
        engine_unreachable: false,
        outcome,
    }
}

fn column(name: &str) -> ResultColumn {
    ResultColumn {
        name: name.into(),
        decl_type: None,
        hidden: false,
    }
}

#[test]
fn nothing_run_yet_offers_examples_and_says_what_the_page_can_do() {
    let html = page(&chrome(), &vm("", Outcome::Nothing)).into_string();
    assert!(
        html.contains("Pick a table on the left, or write a SELECT."),
        "{html}"
    );
    assert!(
        html.contains("Every query is written to the Logs page."),
        "{html}"
    );
    assert!(html.contains("Latest orders"), "{html}");
    // The table list: its rows counted, its secret column marked.
    assert!(html.contains("Tables (1)"), "{html}");
    assert!(html.contains("1,204"), "{html}");
    assert!(html.contains(">hidden<"), "{html}");
    assert!(
        html.contains(&format!(
            r#"href="{}""#,
            maud::html! { (href(QueryDb::Monokulo, "SELECT * FROM \"users\" LIMIT 100")) }
                .into_string()
        )),
        "{html}"
    );
    // Nothing to export or run again yet.
    assert!(!html.contains("Export CSV"), "{html}");
    // Without script, Go switches the database.
    assert!(html.contains(r#"data-only="no-script""#), "{html}");
    assert!(html.contains(&crate::assets::url("query.js")), "{html}");
}

#[test]
fn rows_show_their_kinds_and_a_wide_result_says_it_scrolls() {
    let mut columns: Vec<ResultColumn> = (0..10).map(|i| column(&format!("c{i}"))).collect();
    columns[1].hidden = true;
    let mut row = vec![Cell::Null; 10];
    row[0] = Cell::Text {
        text: "o_1".into(),
        len: 3,
    };
    row[2] = Cell::Integer { value: 42 };
    row[3] = Cell::Text {
        text: "x".repeat(100),
        len: 200_000,
    };
    row[4] = Cell::Blob {
        hex: "9f3a1c07e2d4b8a1".into(),
        len: 64,
    };
    let result = QueryResult {
        columns,
        rows: vec![row],
        cut: true,
        elapsed_ms: 9,
        hidden: vec!["t.c1".into()],
    };
    let html = results(&vm("SELECT * FROM t", Outcome::Ran(result))).into_string();
    assert!(
        html.contains("1 row · 10 columns · 9 ms · monokulo"),
        "{html}"
    );
    assert!(html.contains("Scroll sideways for more columns"), "{html}");
    assert!(html.contains("1 column hidden"), "{html}");
    assert!(html.contains("Only the first 1 rows are shown"), "{html}");
    assert!(html.contains(r#"data-kind="number">42<"#), "{html}");
    assert!(html.contains(r#"data-kind="null">NULL<"#), "{html}");
    assert!(html.contains(r#"data-kind="hidden">hidden<"#), "{html}");
    assert!(html.contains("cut at 100 B of 195.3 KB"), "{html}");
    assert!(html.contains("‹blob 64 B› 9f3a1c07e2d4b8…"), "{html}");
    assert!(html.contains(r#"data-copy="9f3a1c07e2d4b8a1""#), "{html}");
    assert!(
        html.contains(r#"data-opens-dialog="expand-value""#),
        "{html}"
    );
}

#[test]
fn a_refused_statement_says_why() {
    let html = results(&vm(
        "DROP TABLE users",
        Outcome::Refused(QueryError::Refused {
            action: "DROP".into(),
        }),
    ))
    .into_string();
    assert!(html.contains("Not run: this page only reads."), "{html}");
    assert!(html.contains("<code>DROP</code> isn't a read"), "{html}");
    let html = results(&vm(
        "SELECT * FORM users",
        Outcome::Refused(QueryError::Syntax {
            message: "near \"FORM\": syntax error".into(),
            offset: Some(9),
        }),
    ))
    .into_string();
    assert!(html.contains("(at character 10)"), "{html}");
}

#[test]
fn a_long_json_value_is_pretty_printed_whole() {
    let content = value(
        "payload",
        "wd_1a2b",
        &Cell::Text {
            text: r#"{"event":"order.paid"}"#.into(),
            len: 1_843,
        },
    );
    let html = value_dialog(content).into_string();
    assert!(html.contains("wd_1a2b · 1.8 KB · JSON"), "{html}");
    assert!(
        html.contains("&quot;event&quot;: &quot;order.paid&quot;"),
        "{html}"
    );
}
