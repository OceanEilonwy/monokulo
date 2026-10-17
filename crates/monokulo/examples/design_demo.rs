//! The new designs, built from the real ui components inside monokulo's
//! real page chrome, with made-up data. The setup summary and the verified
//! domains are the real views (`views::setup`, `views::domains`); the
//! wallet card is on the real store settings page. Nothing here saves.
//!
//! ```sh
//! cargo run -p monokulo --example design_demo
//! ```
//!
//! then open the address it prints. `?theme=light` or `?theme=dark` on any
//! screen picks the theme (the index links both).

use axum::{extract::Query, response::Html, routing::get, Router};
use maud::{html, Markup, DOCTYPE};
use monokulo::db::Theme;
use monokulo::http::{build_router, AppState};
use monokulo::views::domains::{DomainStatus, DomainView, StoreRef};
use monokulo::views::{self, Page, PageChrome};
use serde::Deserialize;
use ui::PageHeader;

const ADDRESS: &str = "127.0.0.1:4400";
const STORE: &str = "Rachel's Handicrafts";
const DOMAIN: &str = "rachelshandicrafts.com";
const TOKEN: &str = "monokulo-verify=6c168c735d314966ac858a7a71de5eaf";

#[derive(Deserialize, Default)]
struct Look {
    theme: Option<String>,
    /// A screen's variant: a wizard step's state, a field's error.
    state: Option<String>,
}

impl Look {
    fn theme(&self) -> Theme {
        match self.theme.as_deref() {
            Some("light") => Theme::Light,
            Some("dark") => Theme::Dark,
            _ => Theme::System,
        }
    }

    /// `path` with this look's theme kept.
    fn href(&self, path: &str) -> String {
        match &self.theme {
            Some(theme) => {
                let join = if path.contains('?') { '&' } else { '?' };
                format!("{path}{join}theme={theme}")
            }
            None => path.to_string(),
        }
    }

    /// The site's chrome, signed in as a merchant.
    fn chrome(&self, path: &str) -> PageChrome {
        let mut chrome = PageChrome::from_user(None, self.href(path));
        chrome.logged_in = true;
        chrome.email = "rachel@example.com".into();
        chrome.wallet_count = 2;
        chrome.theme = self.theme();
        chrome.browser_reports = false;
        chrome
    }
}

// ---------------------------------------------------------------- index

async fn index(Query(look): Query<Look>) -> Html<String> {
    let screens: &[(&str, &[(&str, &str)])] = &[
        (
            "1. Setup done: the store summary",
            &[("/setup-done", "The summary as facts")],
        ),
        (
            "3a. Verified domains",
            &[
                ("/domains/on", "Restriction on, a domain in each state"),
                ("/domains/off", "Restriction off, a domain verified"),
                ("/domains/empty", "Nothing verified, no domains yet"),
            ],
        ),
        (
            "3b. Add a domain",
            &[
                ("/domains/add", "Step 1: the domain"),
                ("/domains/add?state=error", "Step 1: a domain refused"),
                ("/domains/record", "Step 2: the record"),
                ("/domains/check?state=checking", "Step 3: checking"),
                ("/domains/check?state=missing", "Step 3: not found yet"),
                ("/domains/check?state=verified", "Step 3: verified"),
            ],
        ),
        (
            "4. Admin Query",
            &[
                ("/query", "Results"),
                ("/query/wide", "Many columns"),
                ("/query/big", "Big values and blobs"),
                ("/query/expand", "A value expanded (the dialog's own page)"),
                ("/query/refused", "A refused query"),
                ("/query/empty", "Nothing run yet"),
            ],
        ),
    ];
    let body = html! {
        p { "The new designs, built from the real ui components in monokulo's page chrome. Nothing here saves." }
        p {
            "Theme: "
            a href="/" { "System" } " · " a href="/?theme=light" { "Light" } " · " a href="/?theme=dark" { "Dark" }
        }
        @for (section, links) in screens {
            h2 { (section) }
            ul {
                @for (path, label) in *links {
                    li { a href=(look.href(path)) { (label) } }
                }
            }
        }
    };
    page(
        &look,
        "/",
        "Design demo",
        PageHeader::new("Design demo"),
        body,
    )
}

fn page(look: &Look, path: &str, title: &str, header: PageHeader, body: Markup) -> Html<String> {
    Html(
        Page::new(format!("{title} - Design demo"), header)
            .class("settings-page")
            .render(&look.chrome(path), body)
            .into_string(),
    )
}

// ------------------------------------------------------ 1. setup done

/// The real setup done page (`views::setup::done_page`).
async fn setup_done(Query(look): Query<Look>) -> Html<String> {
    let data = views::setup::DoneViewModel {
        store_id: "rachels-handicrafts".into(),
        name: STORE.into(),
        site: DOMAIN.into(),
        has_orders: false,
        public_key: "pk_6c168c735d314966ac858a7a71de5eaf".into(),
        base_currency: "AUD".into(),
        wallet_name: STORE.into(),
        wallet_network: "mainnet".into(),
        skipped_backup: false,
        plugin: None,
    };
    Html(views::setup::done_page(&look.chrome("/setup-done"), &data).into_string())
}

// ------------------------------------------------ 3. verified domains
//
// The real views (`views::domains`) with made-up domains.

fn store_ref() -> StoreRef<'static> {
    StoreRef {
        id: "rachels-handicrafts",
        name: STORE,
    }
}

fn domain(
    id: &str,
    name: &str,
    status: DomainStatus,
    meta: &str,
    reason: Option<&str>,
) -> DomainView {
    DomainView {
        id: id.into(),
        domain: name.into(),
        status,
        meta: meta.into(),
        record_host: "_monokulo",
        record_value: TOKEN.into(),
        reason: reason.map(str::to_string),
        wait: None,
    }
}

fn all_states() -> Vec<DomainView> {
    vec![
        domain("d1", DOMAIN, DomainStatus::Verified, "Checked 3 h ago · checked again every day", None),
        domain(
            "d2",
            "shop.rachelshandicrafts.com",
            DomainStatus::Waiting,
            "Added 10 min ago · checked 2 min ago",
            Some("No TXT record at _monokulo.shop.rachelshandicrafts.com with the value monokulo-verify=6c16…5eaf was found."),
        ),
        domain(
            "d3",
            "old-shop.example",
            DomainStatus::Failing,
            "Record missing for 2h · still counts as verified for 2d 22h",
            Some("The record was there, and went 2h ago. Publish it again within 2d 22h to keep the domain."),
        ),
        domain(
            "d4",
            "handicrafts.example",
            DomainStatus::Lapsed,
            "Record missing for 3d · it no longer counts",
            Some("The record has been missing for 3d, so the domain no longer counts. Publish it again and check."),
        ),
    ]
}

fn domains_page(
    look: &Look,
    path: &str,
    domains: &[DomainView],
    restricted: bool,
    can_restrict: bool,
) -> Html<String> {
    let body = views::settings::plain_card(
        "verified-domains",
        "Verified domains",
        views::domains::domains_body(&store_ref(), domains, restricted, can_restrict),
    );
    page(look, path, "Verified domains", settings_header(look), body)
}

async fn domains_on(Query(look): Query<Look>) -> Html<String> {
    domains_page(&look, "/domains/on", &all_states(), true, true)
}

async fn domains_off(Query(look): Query<Look>) -> Html<String> {
    let mut domains = all_states();
    domains.truncate(2);
    domains_page(&look, "/domains/off", &domains, false, true)
}

async fn domains_empty(Query(look): Query<Look>) -> Html<String> {
    domains_page(&look, "/domains/empty", &[], false, false)
}

fn settings_header(look: &Look) -> PageHeader {
    PageHeader::new("Settings").crumbs(vec![ui::Crumb::new(look.href("/"), STORE)])
}

async fn add(Query(look): Query<Look>) -> Html<String> {
    let error = look.state.as_deref() == Some("error");
    let (typed, error) = if error {
        (
            "rachelshandicrafts",
            Some("Enter a full domain, like shop.example."),
        )
    } else {
        ("", None)
    };
    Html(
        views::domains::add_page(&look.chrome("/domains/add"), &store_ref(), typed, error)
            .into_string(),
    )
}

async fn record_step(Query(look): Query<Look>) -> Html<String> {
    let domain = domain(
        "d1",
        DOMAIN,
        DomainStatus::Waiting,
        "",
        Some("Not checked yet."),
    );
    Html(
        views::domains::record_page(&look.chrome("/domains/record"), &store_ref(), &domain)
            .into_string(),
    )
}

async fn check_step(Query(look): Query<Look>) -> Html<String> {
    let (status, checked) = match look.state.as_deref() {
        Some("verified") => (DomainStatus::Verified, true),
        Some("missing") => (DomainStatus::Waiting, true),
        _ => (DomainStatus::Waiting, false),
    };
    let reason = (status != DomainStatus::Verified).then_some(
        "rachelshandicrafts.com's name servers have no TXT record at _monokulo.rachelshandicrafts.com holding monokulo-verify=6c16…5eaf.",
    );
    let domain = domain("d1", DOMAIN, status, "", reason);
    Html(
        views::domains::check_page(
            &look.chrome("/domains/check"),
            &store_ref(),
            &domain,
            checked,
        )
        .into_string(),
    )
}

// ------------------------------------------------------- 4. the query

use monokulo::views::query::{self as query_view, Outcome, QueryDb, QueryViewModel};
use shared::sqlite_query::{Cell, ColumnInfo, QueryError, QueryResult, ResultColumn, TableInfo};

/// The tables beside the editor: a few of monokulo's, secrets marked.
fn demo_tables() -> Vec<TableInfo> {
    let table = |name: &str, rows: u64, columns: &[(&str, &str, bool)]| TableInfo {
        name: name.into(),
        rows,
        columns: columns
            .iter()
            .map(|(name, decl_type, hidden)| ColumnInfo {
                name: (*name).into(),
                decl_type: (*decl_type).into(),
                hidden: *hidden,
            })
            .collect(),
    };
    vec![
        table(
            "orders",
            1_204,
            &[
                ("id", "TEXT", false),
                ("status", "TEXT", false),
                ("amount_xmr", "INTEGER", false),
            ],
        ),
        table(
            "sessions",
            4,
            &[("user_id", "TEXT", false), ("token", "TEXT", true)],
        ),
        table(
            "store_domains",
            4,
            &[("domain", "TEXT", false), ("token", "TEXT", true)],
        ),
        table(
            "stores",
            3,
            &[("id", "TEXT", false), ("name", "TEXT", false)],
        ),
        table(
            "users",
            12,
            &[
                ("id", "TEXT", false),
                ("email", "TEXT", false),
                ("password_hash", "TEXT", true),
            ],
        ),
        table(
            "wallets",
            5,
            &[("id", "TEXT", false), ("name", "TEXT", false)],
        ),
        table(
            "webhook_deliveries",
            38,
            &[("id", "TEXT", false), ("payload", "TEXT", false)],
        ),
    ]
}

fn text(value: &str) -> Cell {
    Cell::Text {
        text: value.into(),
        len: value.len(),
    }
}

/// A plain value as its cell: NULL, a number or text.
fn plain(value: &str) -> Cell {
    match value {
        "NULL" => Cell::Null,
        v => match v.parse::<i64>() {
            Ok(value) => Cell::Integer { value },
            Err(_) => match v.parse::<f64>() {
                Ok(value) => Cell::Real { value },
                Err(_) => text(v),
            },
        },
    }
}

fn result(names: &[&str], hidden: &[&str], rows: Vec<Vec<Cell>>, ms: u64) -> QueryResult {
    QueryResult {
        columns: names
            .iter()
            .map(|name| ResultColumn {
                name: (*name).into(),
                decl_type: None,
                hidden: hidden.contains(name),
            })
            .collect(),
        rows,
        cut: false,
        elapsed_ms: ms,
        hidden: hidden.iter().map(|c| format!("users.{c}")).collect(),
    }
}

const EXPANDED_JSON: &str = r##"{"event":"order.paid","order":{"id":"order_2282ae00c55f42fb8e295d4b45c8bfc3","store":"Rachel's Handicrafts","status":"paid","amount":{"xmr":"0.084200000000","fiat":"24.50","currency":"AUD","rate":"290.97"},"address":"86LpA1yW…Hq3wZ","confirmations":0,"required_confirmations":10,"external_ref":"#1042","created_at":"2026-10-17T05:58:12Z","paid_at":"2026-10-17T06:01:40Z"},"delivered_at":"2026-10-17T06:01:41Z"}"##;

#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Results,
    Wide,
    Big,
    Refused,
    Empty,
}

fn shown(shown: Shown) -> (QueryDb, &'static str, Outcome) {
    match shown {
        Shown::Results => (
            QueryDb::Monokulo,
            "SELECT id, email, created_at, password_hash\nFROM users\nORDER BY created_at DESC",
            Outcome::Ran(result(
                &["id", "email", "created_at", "password_hash"],
                &["password_hash"],
                [
                    ["us_1", "rachel@example.com", "1760700000"],
                    ["us_2", "ann@example.org", "NULL"],
                    ["us_3", "sam@example.net", "1760610000"],
                    ["us_4", "kim@example.com", "1760500000"],
                ]
                .iter()
                .map(|row| row.iter().map(|v| plain(v)).chain([Cell::Null]).collect())
                .collect(),
                3,
            )),
        ),
        Shown::Wide => {
            let names = [
                "id",
                "store_id",
                "status",
                "amount_xmr",
                "amount_fiat",
                "currency",
                "rate",
                "address",
                "subaddress",
                "payment_id",
                "tx_hash",
                "confirmations",
                "required",
                "network",
                "wallet_id",
                "source",
                "external_ref",
                "created_at",
                "paid_at",
                "expires_at",
                "description",
                "customer_note",
                "refund_address",
            ];
            let row = |id: &str, store: &str, status: &str, xmr: &str, fiat: &str| -> Vec<Cell> {
                [
                    id,
                    store,
                    status,
                    xmr,
                    fiat,
                    "AUD",
                    "290.97",
                    "86Lp…Hq3w",
                    "14",
                    "NULL",
                    "a91f…07c2",
                    "12",
                    "10",
                    "mainnet",
                    "w_rh01",
                    "woocommerce",
                    "#1042",
                    "2026-10-17 05:58",
                    "2026-10-17 06:01",
                    "2026-10-17 06:58",
                    "Hand-thrown mug",
                    "NULL",
                    "NULL",
                ]
                .iter()
                .map(|v| plain(v))
                .collect()
            };
            (
                QueryDb::Engine,
                "SELECT * FROM orders",
                Outcome::Ran(result(
                    &names,
                    &[],
                    vec![
                        row("order_2282ae", "st_8f3a", "confirmed", "0.0842", "24.50"),
                        row("order_77c0d1", "st_8f3a", "paid", "0.0310", "9.00"),
                        row("order_09ab44", "st_2c91", "pending", "0.1200", "35.00"),
                        row("order_5d6e2f", "st_77d0", "expired", "0.0100", "2.90"),
                    ],
                    9,
                )),
            )
        }
        Shown::Big => {
            let blob = |hex: &str| Cell::Blob {
                hex: hex.repeat(8),
                len: 64,
            };
            (
                QueryDb::Monokulo,
                "SELECT * FROM webhook_deliveries",
                Outcome::Ran(result(
                    &["id", "event", "payload", "signature", "status"],
                    &[],
                    vec![
                        vec![
                            text("wd_1a2b"),
                            text("order.paid"),
                            text(EXPANDED_JSON),
                            blob("9f3a1c07e2d4b8a1"),
                            text("delivered"),
                        ],
                        vec![
                            text("wd_3c4d"),
                            text("order.confirmed"),
                            text(&EXPANDED_JSON.replace("paid", "confirmed")),
                            blob("41be0d77a01c5e22"),
                            text("delivered"),
                        ],
                        vec![
                            text("wd_5e6f"),
                            text("store.export"),
                            Cell::Text {
                                text: EXPANDED_JSON.repeat(3),
                                len: 2_516_582,
                            },
                            Cell::Null,
                            text("failed"),
                        ],
                    ],
                    6,
                )),
            )
        }
        Shown::Refused => (
            QueryDb::Monokulo,
            "UPDATE stores SET name = 'Rachel''s' WHERE id = 'st_8f3a'",
            Outcome::Refused(QueryError::Refused {
                action: "UPDATE".into(),
            }),
        ),
        Shown::Empty => (QueryDb::Monokulo, "", Outcome::Nothing),
    }
}

/// One of the Query page's states, drawn by the real view.
async fn query_page(look: Look, path: &str, which: Shown) -> Html<String> {
    let (db, sql, outcome) = shown(which);
    let vm = QueryViewModel {
        db,
        sql: sql.into(),
        tables: demo_tables(),
        engine_unreachable: false,
        outcome,
    };
    let mut chrome = look.chrome(path);
    chrome.is_admin = true;
    chrome.query_enabled = true;
    Html(query_view::page(&chrome, &vm).into_string())
}

async fn query(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query", Shown::Results).await
}
async fn query_wide(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/wide", Shown::Wide).await
}
async fn query_big(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/big", Shown::Big).await
}
async fn query_refused(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/refused", Shown::Refused).await
}
async fn query_empty(Query(look): Query<Look>) -> Html<String> {
    query_page(look, "/query/empty", Shown::Empty).await
}

/// A long value on its own page, as a browser without JavaScript opens it.
async fn query_expand(Query(look): Query<Look>) -> Html<String> {
    let content = query_view::value("payload", "wd_1a2b", &text(EXPANDED_JSON));
    let mut chrome = look.chrome("/query/expand");
    chrome.is_admin = true;
    chrome.query_enabled = true;
    Html(query_view::value_page(&chrome, &look.href("/query/big"), content).into_string())
}

#[tokio::main]
async fn main() {
    let demo = Router::new()
        .route("/", get(index))
        .route("/setup-done", get(setup_done))
        .route("/domains/on", get(domains_on))
        .route("/domains/off", get(domains_off))
        .route("/domains/empty", get(domains_empty))
        .route("/domains/add", get(add))
        .route("/domains/record", get(record_step))
        .route("/domains/check", get(check_step))
        .route("/query", get(query))
        .route("/query/wide", get(query_wide))
        .route("/query/big", get(query_big))
        .route("/query/expand", get(query_expand))
        .route("/query/refused", get(query_refused))
        .route("/query/empty", get(query_empty));
    // The real router serves the pages' scripts, styles and fonts; the
    // demo's routes stand in for its pages.
    let assets = build_router(AppState::for_tests()).layer(axum::middleware::from_fn(only_static));
    let app = demo.fallback_service(assets);
    let address = std::env::var("DESIGN_DEMO_ADDRESS").unwrap_or_else(|_| ADDRESS.to_string());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .expect("bind the demo's address");
    println!("Design demo: http://{}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("serve the demo");
}

/// Only the real router's assets and the status dot's poll: none of its
/// pages answer, so a link the demo doesn't have says so.
async fn only_static(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path();
    if path.starts_with("/static/") || path == "/status/summary" {
        next.run(request).await
    } else {
        (
            axum::http::StatusCode::NOT_FOUND,
            Html(html! { (DOCTYPE) p { "Not part of the demo." } }.into_string()),
        )
            .into_response()
    }
}

use axum::response::IntoResponse;
