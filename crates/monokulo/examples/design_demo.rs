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
use maud::{html, Markup, PreEscaped, DOCTYPE};
use monokulo::db::Theme;
use monokulo::http::{build_router, AppState};
use monokulo::views::domains::{DomainStatus, DomainView, StoreRef};
use monokulo::views::{self, Page, PageChrome};
use serde::Deserialize;
use ui::assets::icons;
use ui::{
    Alert, Button, ButtonType, ButtonVariant, Choice, ControlSize, CopyBlock, DataCell, DataGrid,
    DataKind, Dialog, DialogActions, DialogPage, DialogTitle, Disclosure, DisclosureLook, Fact,
    Facts, FormActions, Hint, IconButton, IconLink, Input, LinkButton, PageHeader, Row, Select,
    Slot, TableBrowser, Tag, TagTone, TextArea, Tone,
};

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

/// An icon from ui's set, before (or after) a button's words.
fn icon(svg: &'static str) -> Markup {
    html! { (PreEscaped(svg)) }
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
            // The Query page takes the full width, as the Logs page does.
            .class(if path.starts_with("/query") {
                "settings-page wrap-wide"
            } else {
                "settings-page"
            })
            .head(html! { style { (PreEscaped(DEMO_STYLE)) } })
            .render(&look.chrome(path), body)
            .into_string(),
    )
}

/// Page layout the designs add (no colours: those are the ui's roles).
const DEMO_STYLE: &str = r#"
.wrap.wrap-wide { max-width: 1200px; }
.query-band { display: grid; grid-template-columns: minmax(12rem, 16rem) minmax(0, 1fr); gap: var(--space-md); height: 17rem; }
.query-band > * { min-height: 0; }
.query-editor { display: flex; flex-direction: column; gap: var(--space-sm); margin: 0; min-width: 0; }
.query-editor textarea { flex: 1; min-height: 6rem; font-family: var(--font-mono); resize: vertical; }
.query-editor .form-actions { margin: 0; align-items: center; }
.query-label { margin: 0; font-size: var(--text-xs); font-weight: 800; letter-spacing: .06em; text-transform: uppercase; color: var(--muted); }
.query-empty { text-align: center; }
.query-empty .form-actions { justify-content: center; }
@media (max-width: 640px) {
  .query-band { grid-template-columns: minmax(0, 1fr); height: auto; }
  .query-band > .mk-table-browser { order: 2; }
}
"#;

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

#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Results,
    Wide,
    Big,
    Refused,
    Empty,
}

fn query_header(look: &Look) -> PageHeader {
    PageHeader::new("Query")
        .crumbs(vec![ui::Crumb::new(look.href("/"), "Home")])
        .actions(html! {
            form class="engine-network" method="get" action="/query" {
                label for="query-db" class="visually-hidden" { "Database" }
                (Select::new("db", vec![Choice::new("monokulo", "Monokulo").selected(true), Choice::new("engine", "Engine")])
                    .id("query-db")
                    .compact(true))
                (Button::new("Go").kind(ButtonType::Submit).attr("data-only", "no-script"))
            }
            (LinkButton::new("Export CSV", "#").size(ControlSize::Compact))
            (IconLink::new("Run again", look.href("/query"), icon(icons::REFRESH)))
        })
}

fn table_browser() -> Markup {
    let column = |name: &str, value: Markup| Fact {
        term: name.into(),
        value: value.into(),
    };
    let text = |t: &str| html! { (t) };
    let tables: Vec<(&str, &str, Vec<Fact>)> = vec![
        (
            "orders",
            "1,204",
            vec![
                column("id", text("TEXT")),
                column("status", text("TEXT")),
                column("amount_xmr", text("INTEGER")),
            ],
        ),
        (
            "sessions",
            "4",
            vec![
                column("user_id", text("TEXT")),
                column(
                    "token",
                    html! { (Tag::new("hidden").tone(TagTone::Muted).icon(icon(icons::LOCK))) },
                ),
            ],
        ),
        (
            "store_domains",
            "4",
            vec![
                column("domain", text("TEXT")),
                column(
                    "token",
                    html! { (Tag::new("hidden").tone(TagTone::Muted).icon(icon(icons::LOCK))) },
                ),
            ],
        ),
        (
            "stores",
            "3",
            vec![column("id", text("TEXT")), column("name", text("TEXT"))],
        ),
        (
            "users",
            "12",
            vec![
                column("id", text("TEXT")),
                column("email", text("TEXT")),
                column(
                    "password_hash",
                    html! { (Tag::new("hidden").tone(TagTone::Muted).icon(icon(icons::LOCK))) },
                ),
            ],
        ),
        (
            "wallets",
            "5",
            vec![column("id", text("TEXT")), column("name", text("TEXT"))],
        ),
        (
            "webhook_deliveries",
            "38",
            vec![column("id", text("TEXT")), column("payload", text("TEXT"))],
        ),
    ];
    html! {
        (TableBrowser::new("Tables", Slot::many(tables.into_iter().map(|(name, rows, columns)| {
            Slot::from(Disclosure::new(
                html! { (name) (Tag::new(rows).tone(TagTone::Muted)) },
                Facts::new(columns),
            ).look(DisclosureLook::Compact))
        })))
        .filter(Input::new("table-filter").label("Filter tables").placeholder("Filter tables…").size(ControlSize::Compact)))
    }
}

fn editor(sql: &str) -> Markup {
    html! {
        form class="box query-editor" method="get" action="/query" {
            label class="query-label" for="query-sql" { "SQL" }
            (TextArea::new("sql").id("query-sql").value(sql).rows(5usize).described_by("query-sql-help"))
            (FormActions::new(Slot::many([
                Slot::from(Hint::new("One statement. Rows past 1,000 are cut.").inline(true).attr("id", "query-sql-help")),
                Slot::from(Disclosure::new("Examples", html! {
                    ul { li { a href="#" { "Latest orders" } } li { a href="#" { "Stores and their wallets" } } li { a href="#" { "Unpaid orders per store" } } }
                })),
                Slot::from(html! { span class="spacer" {} }),
                Slot::from(Button::new("Run").variant(ButtonVariant::Primary).kind(ButtonType::Submit)),
            ])))
        }
    }
}

fn head_row(names: &[&str]) -> Slot {
    Slot::from(Row::new(Slot::many(
        names
            .iter()
            .map(|n| Slot::from(DataCell::new(n.to_string()).header(true))),
    )))
}

fn cell(value: &str) -> Slot {
    match value {
        "NULL" => Slot::from(DataCell::new("NULL").kind(DataKind::Null)),
        "hidden" => Slot::from(DataCell::new("hidden").kind(DataKind::Hidden)),
        v if v.parse::<f64>().is_ok() => {
            Slot::from(DataCell::new(v.to_string()).kind(DataKind::Number))
        }
        v => Slot::from(DataCell::new(v.to_string())),
    }
}

fn rows(rows: &[&[&str]]) -> Slot {
    Slot::many(
        rows.iter()
            .map(|row| Slot::from(Row::new(Slot::many(row.iter().map(|v| cell(v)))))),
    )
}

fn results(shown: Shown) -> Markup {
    let hidden = || {
        Slot::from(
            Tag::new("1 column hidden")
                .tone(TagTone::Muted)
                .icon(icon(icons::LOCK)),
        )
    };
    match shown {
        Shown::Results => html! {
            (DataGrid::new(
                head_row(&["id", "email", "created_at", "password_hash"]),
                rows(&[
                    &["us_1", "rachel@example.com", "1760700000", "hidden"],
                    &["us_2", "ann@example.org", "NULL", "hidden"],
                    &["us_3", "sam@example.net", "1760610000", "hidden"],
                    &["us_4", "kim@example.com", "1760500000", "hidden"],
                ]),
                "Query results",
            )
            .stats(Slot::many([Slot::from("4 rows · 3 ms · monokulo"), hidden()])))
        },
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
            let row = |id: &'static str,
                       store: &'static str,
                       status: &'static str,
                       xmr: &'static str,
                       fiat: &'static str|
             -> Vec<&'static str> {
                vec![
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
            };
            let data = [
                row("order_2282ae", "st_8f3a", "confirmed", "0.0842", "24.50"),
                row("order_77c0d1", "st_8f3a", "paid", "0.0310", "9.00"),
                row("order_09ab44", "st_2c91", "pending", "0.1200", "35.00"),
                row("order_5d6e2f", "st_77d0", "expired", "0.0100", "2.90"),
            ];
            let data: Vec<&[&str]> = data.iter().map(|r| r.as_slice()).collect();
            html! {
                (DataGrid::new(head_row(&names), rows(&data), "Query results")
                    .stats("4 rows · 23 columns · 9 ms · engine")
                    .scroll_hint(html! { (icon(icons::ARROW_LEFT)) " Scroll sideways for more columns " (icon(icons::ARROW_RIGHT)) }))
            }
        }
        Shown::Big => {
            let long = |id: &str, json: &str, size: &str, cut: Option<&str>| {
                let cell = DataCell::new(json.to_string())
                    .size(size.to_string())
                    .actions(Slot::many([
                        Slot::from(
                            IconLink::new(
                                format!("Expand payload of {id}"),
                                "/query/expand",
                                icon(icons::EXPAND),
                            )
                            .attr("data-opens-dialog", "expand-value"),
                        ),
                        Slot::from(IconButton::new(
                            format!("Copy payload of {id}"),
                            icon(icons::COPY),
                        )),
                    ]));
                Slot::from(match cut {
                    Some(cut) => cell.cut(cut.to_string()),
                    None => cell,
                })
            };
            let blob = |hex: &str| {
                Slot::from(
                    DataCell::new(format!("‹blob 64 B› {hex}…"))
                        .kind(DataKind::Blob)
                        .actions(IconButton::new("Copy the bytes as hex", icon(icons::COPY))),
                )
            };
            let row = |id: &str, event: &str, payload: Slot, signature: Slot, status: &str| {
                Slot::from(Row::new(Slot::many([
                    cell(id),
                    cell(event),
                    payload,
                    signature,
                    cell(status),
                ])))
            };
            html! {
                (DataGrid::new(
                    head_row(&["id", "event", "payload", "signature", "status"]),
                    Slot::many([
                        row("wd_1a2b", "order.paid", long("wd_1a2b", r#"{"event":"order.paid","order":{"id":"order_2282ae00c55f42fb8e295d4b45c8bfc3","store":"Rachel's Handicrafts"}}"#, "1.8 KB", None), blob("9f3a1c07e2d4b8"), "delivered"),
                        row("wd_3c4d", "order.confirmed", long("wd_3c4d", r#"{"event":"order.confirmed","order":{"id":"order_77c0d1","store":"Rachel's Handicrafts"}}"#, "1.9 KB", None), blob("41be0d77a01c5e"), "delivered"),
                        row("wd_5e6f", "store.export", long("wd_5e6f", r#"{"event":"store.export","orders":[{"id":"order_09ab44"},{"id":"order_5d6e2f"}]}"#, "2.4 MB", Some("cut at 64 KiB of 2.4 MB")), cell("NULL"), "failed"),
                    ]),
                    "Query results",
                )
                .stats(Slot::many([Slot::from("3 rows · 6 ms · monokulo"), hidden()])))
                (Dialog::new("expand-value", "expand-value-title", expanded()).size(ui::DialogSize::Wide))
            }
        }
        Shown::Refused => html! {
            (Alert::new(html! { code { "UPDATE" } " changes data, so it is refused. Use a " code { "SELECT" } "." }, Tone::Error)
                .title("Not run: this page only reads.")
                .icon(icon(icons::WARNING)))
        },
        Shown::Empty => html! {
            div class="box query-empty" {
                p { strong { "Pick a table on the left, or write a SELECT." } }
                (Hint::new("Nothing here can change data. Results stop at 1,000 rows and 5 seconds. Every query is written to the Logs page."))
                (FormActions::new(Slot::many(["Latest orders", "Stores and their wallets", "Unpaid orders per store"].map(|label| {
                    Slot::from(LinkButton::new(label, "/query").size(ControlSize::Compact))
                }))))
            }
        },
    }
}

const EXPANDED_JSON: &str = r##"{
  "event": "order.paid",
  "order": {
    "id": "order_2282ae00c55f42fb8e295d4b45c8bfc3",
    "store": "Rachel's Handicrafts",
    "status": "paid",
    "amount": { "xmr": "0.084200000000", "fiat": "24.50", "currency": "AUD", "rate": "290.97" },
    "address": "86LpA1yW…Hq3wZ",
    "confirmations": 0,
    "required_confirmations": 10,
    "external_ref": "#1042",
    "created_at": "2026-10-17T05:58:12Z",
    "paid_at": "2026-10-17T06:01:40Z"
  },
  "delivered_at": "2026-10-17T06:01:41Z"
}"##;

fn expanded() -> Markup {
    html! {
        (DialogTitle::new("expand-value-title", "payload").aside(Hint::new("wd_1a2b · 1.8 KB · JSON").inline(true)))
        (CopyBlock::new(EXPANDED_JSON).copy_label("Copy the payload"))
        (DialogActions::new(Button::new("Close").attr("data-closes-dialog", "")))
    }
}

async fn query_page(look: Look, path: &str, shown: Shown) -> Html<String> {
    let sql = match shown {
        Shown::Results => {
            "SELECT id, email, created_at, password_hash\nFROM users\nORDER BY created_at DESC"
        }
        Shown::Wide => "SELECT * FROM orders",
        Shown::Big => "SELECT * FROM webhook_deliveries",
        Shown::Refused => "UPDATE stores SET name = 'Rachel''s' WHERE id = 'st_8f3a'",
        Shown::Empty => "",
    };
    let body = html! {
        div class="query-band" {
            (table_browser())
            (editor(sql))
        }
        (results(shown))
    };
    page(&look, path, "Query", query_header(&look), body)
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

async fn query_expand(Query(look): Query<Look>) -> Html<String> {
    let header = PageHeader::new("payload")
        .crumbs(vec![ui::Crumb::new(look.href("/query/big"), "Query")])
        .aside(Hint::new("wd_1a2b · 1.8 KB · JSON").inline(true));
    let body = html! { (DialogPage::new(CopyBlock::new(EXPANDED_JSON).copy_label("Copy the payload")).size(ui::DialogSize::Wide)) };
    page(&look, "/query/expand", "payload", header, body)
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
