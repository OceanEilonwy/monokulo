//! A merchant's wallets (docs/wallets.md): choosing how to add one, bringing
//! one's own, making a new one in the browser (backing up its recovery
//! phrase and checking the backup), the list, and a wallet's page.
//!
//! The screens for adding a wallet are the same in setup's Wallet step
//! (`/setup/wallet/...`) and on the Account page (`/account/wallets/...`):
//! a [`Flow`] says which. Handlers: `http::setup`, `http::wallets`.

use maud::{html, Markup};

use super::controls::Choice;
use super::settings::{self, Card, Field, Save, Toast, ToastKind};
use super::setup::{SetupContext, WalletAt, WalletPath};
use super::{layout, layout_with_head, PageChrome};
use crate::wallets::{AppMethod, WalletApp, WALLET_APPS};

/// Where a wallet is being added: in setup's Wallet step, on the way to
/// making a store, or from the Account page.
#[derive(Clone, Copy)]
pub enum Flow<'a> {
    Setup(&'a SetupContext),
    Account,
}

impl Flow<'_> {
    /// The choice screen, with the name and network kept.
    pub fn choice_href(&self, name: &str, network: &str) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if let Flow::Setup(setup) = self {
            for (key, value) in &setup.fields {
                query.append_pair(key, value);
            }
        }
        query
            .append_pair("name", name)
            .append_pair("network", network);
        let base = match self {
            Flow::Setup(_) => "/setup/wallet",
            Flow::Account => "/account/wallets/add",
        };
        format!("{base}?{}", query.finish())
    }

    fn keys_action(&self) -> &'static str {
        match self {
            Flow::Setup(_) => "/setup/wallet/keys",
            Flow::Account => "/account/wallets/import",
        }
    }

    fn new_action(&self) -> &'static str {
        match self {
            Flow::Setup(_) => "/setup/wallet/new",
            Flow::Account => "/account/wallets/new",
        }
    }

    fn hidden(&self) -> Markup {
        match self {
            Flow::Setup(setup) => setup.hidden(),
            Flow::Account => html! {},
        }
    }

    fn steps(&self, path: WalletPath, at: usize) -> Markup {
        let wallet = WalletAt { path, at };
        match self {
            Flow::Setup(_) => super::setup::steps(super::setup::Step::Wallet, Some(wallet)),
            Flow::Account => super::setup::wallet_steps(wallet),
        }
    }

    /// The button that adds the wallet (and, in setup, makes the store).
    fn add_label(&self) -> &'static str {
        match self {
            Flow::Setup(_) => "Add the wallet and make the store",
            Flow::Account => "Add wallet",
        }
    }
}

/// A wallet app's icon (`/static/wallet-logos/{key}.png`).
pub fn app_logo(key: &str, size: u32, label: Option<&str>) -> Markup {
    html! {
        img class="app-logo" src=(crate::assets::url(&format!("wallet-logos/{key}.png"))) alt=(label.unwrap_or("")) title=[label] width=(size) height=(size);
    }
}

/// Trezor's mark (trezor-suite's `trezor_logo_symbol.svg`), in the text colour.
fn trezor_logo(height: u32) -> Markup {
    html! {
        svg class="hw-logo" viewBox="0 0 177 256" width=(height * 69 / 100) height=(height) role="img" aria-label="Trezor" {
            path fill="currentColor" d="M150.66 59.407C150.66 26.947 122.488 0 88.191 0S25.722 26.947 25.722 59.407v18.985H0v136.575L88.191 256l88.192-41.033V79.005H150.66zm-93.09 0c0-15.311 13.473-27.56 30.621-27.56 17.149 0 30.622 12.249 30.622 27.56v18.985H57.57zm83.291 133.512-52.67 24.497-52.67-24.497v-82.067h105.34z" {}
        }
    }
}

/// Ledger's mark (ledger-live's `Ledger-Logo.svg`), in the text colour.
fn ledger_logo(size: u32) -> Markup {
    html! {
        svg class="hw-logo" viewBox="0 0 24 24" width=(size) height=(size) role="img" aria-label="Ledger" {
            path d="M3 7V4.5C3 3.67157 3.67157 3 4.5 3H9M14 15.0001H11.5C10.6716 15.0001 10 14.3285 10 13.5001V9.00012M21 7V4.5C21 3.67157 20.3284 3 19.5 3H15M3 17V19.5C3 20.3284 3.67157 21 4.5 21H9M21 17V19.5C21 20.3284 20.3284 21 19.5 21H15" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" {}
        }
    }
}

fn lock_icon() -> Markup {
    html! {
        svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
            rect x="4" y="11" width="16" height="10" rx="2" {}
            path d="M8 11V7a4 4 0 0 1 8 0v4" {}
        }
    }
}

/// The refresh arrow of "Make a different phrase".
fn regenerate_icon() -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" {
            path d="M20 11a8 8 0 1 0-2.3 5.7" {}
            path d="M20 4v7h-7" {}
        }
    }
}

/// The setup flow's network dropdown: each network as its badge
/// (`views::network_badge`), `[Stagenet]` without JavaScript.
fn network_select(selected: &str) -> Markup {
    html! {
        mk-select {
            select name="network" {
                @for network in ["mainnet", "stagenet", "testnet"] {
                    (Choice::new(network, "")
                        .network(network)
                        .selected(network == selected))
                }
            }
        }
    }
}

/// "Where do I find these keys?": where each app shows a wallet's private
/// view key and public spend key, on the forms that ask for them.
pub fn keys_help() -> Markup {
    html! {
        details class="keys-help" {
            summary { "Where do I find these keys?" }
            div class="keys-help-body" {
                h4 { span class="logo-row" { (app_logo("cake", 20, None)) "Cake Wallet" } }
                p { "Settings (the gear icon), Recovery & Keys, Show my Recovery Phrase & Keys, then the Keys tab." }
                h4 { span class="logo-row" { (app_logo("feather", 20, None)) "Feather" } }
                p { "The Wallet menu, then Keys." }
                h4 { span class="logo-row" { (app_logo("gui", 20, None)) "Monero GUI" } }
                p { "Settings, then Seed & keys." }
                h4 { span class="logo-row" { (app_logo("gui", 20, None)) "Monero CLI (monero-wallet-cli)" } }
                ol {
                    li {
                        code { "viewkey" } " shows two lines. Copy the " strong { "secret" } " one: that's the private view key."
                        pre { span class="copy-this" { "secret: 6a1b…e04d" } "\npublic: 9c2e…71aa" }
                    }
                    li {
                        code { "spendkey" } " shows two lines too. Copy the " strong { "public" } " one only. Never enter the secret spend key anywhere."
                        pre { "secret: (never share this)\n" span class="copy-this" { "public: 3f9a…c21e" } }
                    }
                    li { code { "address" } " shows the wallet's main address, to check against the one Monokulo shows once it's added." }
                    li { code { "restore_height" } " shows where the wallet's history starts. Monokulo doesn't need it: it watches for payments from now on." }
                }
            }
        }
    }
}

// -- Choosing how to add a wallet ----------------------------------------------

pub struct ChoiceViewModel {
    /// The account's wallets, to use one already added (in setup only).
    pub wallets: Vec<crate::db::WalletSummary>,
    pub name: String,
    /// Why the name can't be used, when it can't.
    pub name_problem: Option<String>,
    pub network: String,
    pub error: Option<String>,
}

/// Whether a wallet name is free, checked before anything is made: drawn
/// with the field, and again by `GET /account/wallets/name-check` as the
/// name is changed.
pub fn name_check(name: &str, problem: Option<&str>) -> Markup {
    html! {
        @if let Some(problem) = problem {
            span class="field-check bad" id="wallet-name-check" role="status" { "✕ " (problem) }
        } @else if name.trim().is_empty() {
            span class="field-check" id="wallet-name-check" role="status" { "Leave it blank and a name is picked for you." }
        } @else {
            span class="field-check ok" id="wallet-name-check" role="status" { "✓ Free to use. Checked now, before anything is made." }
        }
    }
}

/// `GET /setup/wallet` and `GET /account/wallets/add`: use a wallet already
/// added (in setup), or name a new one and say where it comes from: made in
/// the browser, brought in with its keys, or (coming soon) a hardware
/// wallet. Making one needs JavaScript: that card is drawn unavailable, and
/// `wallet-setup.js` turns it on.
pub fn choice_page(chrome: &PageChrome, flow: Flow<'_>, data: &ChoiceViewModel) -> Markup {
    let body = html! {
        div class="wrap wallet-choice" {
            @match flow {
                Flow::Setup(setup) => {
                    nav class="context-nav" aria-label="Breadcrumb" { a href=(format!("/setup?{}", setup.query())) { "Store" } }
                }
                Flow::Account => {
                    nav class="context-nav" aria-label="Breadcrumb" { a href="/account?tab=wallets" { "Wallets" } }
                }
            }
            (flow.steps(WalletPath::New, 0))
            h1 {
                @match flow {
                    Flow::Setup(setup) => { "Where should " (setup.store_name) "'s money go?" }
                    Flow::Account => "Add a wallet",
                }
            }
            p {
                "Payments go straight to your own Monero wallet. Monokulo only gets watch-only keys: it sees "
                "payments arrive and can never spend them."
            }
            @if let Some(error) = &data.error {
                p class="error" role="alert" { (error) }
            }
            @if let (Flow::Setup(_), false) = (flow, data.wallets.is_empty()) {
                form method="post" action="/setup/wallet/existing" class="already" {
                    (flow.hidden())
                    label for="existing-wallet" { strong { "Use a wallet you already added" } }
                    mk-select {
                        select id="existing-wallet" name="wallet_id" required {
                            @for w in &data.wallets {
                                (Choice::new(&w.wallet.id, &w.wallet.name)
                                    .detail(super::short_address_text(&w.wallet.primary_address))
                                    .network(&w.wallet.network)
                                    .note(stores_label(w.store_count)))
                            }
                        }
                    }
                    button type="submit" { "Use this wallet" }
                }
                p class="or-divider" role="separator" { "or" }
            }
            form method="get" action=(flow.keys_action()) data-wallet-choice {
                (flow.hidden())
                div class="setting-field wallet-name-field" {
                    div class="setting-label-row" { label class="setting-label" for="wallet-name" { "Wallet name" } }
                    p class="field-help hint" { "Shown when you pick a wallet for a store. You can rename it any time." }
                    input id="wallet-name" type="text" name="name" value=(data.name) maxlength=(crate::wallets::MAX_NAME_LEN)
                        autocomplete="off" aria-describedby="wallet-name-check"
                        aria-invalid=[data.name_problem.as_ref().map(|_| "true")]
                        fx-action="/account/wallets/name-check" fx-target="#wallet-name-check" fx-swap="outerHTML";
                    (name_check(&data.name, data.name_problem.as_deref()))
                }
                details class="more-options" {
                    summary { "More options" }
                    div class="setting-field" {
                        div class="setting-label-row" { label class="setting-label" for="wallet-network" { "Network" } }
                        p class="field-help hint" { "Leave on mainnet unless this is a test wallet." }
                        (network_select(&data.network))
                    }
                }
                div class="pick-grid three" {
                    section class="pick-card recommended unavailable" data-needs-js {
                        div class="faded" {
                            div class="pick-card-head" {
                                span class="icon-tile" aria-hidden="true" {
                                    svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true" { path d="M12 5v14M5 12h14" {} }
                                }
                                span class="tag" data-js-tag { "Needs JavaScript" }
                            }
                            h3 { "Create a new wallet" }
                            p class="grow" {
                                "Your browser makes a 16-word recovery phrase. Scan it straight into Cake Wallet or "
                                "Stack Wallet, or write it down, then we check it."
                            }
                            p class="hint" { (lock_icon()) " The phrase is made and kept on this page. It is never sent to Monokulo." }
                            button type="submit" class="btn-primary" formaction=(flow.new_action()) disabled data-create-wallet { "Create a new wallet" }
                        }
                    }
                    section class="pick-card" {
                        div class="pick-card-head" {
                            span class="logo-row" role="img" aria-label="Works with Cake Wallet, Feather, Stack Wallet, the Monero GUI and others" {
                                (app_logo("cake", 28, None))
                                (app_logo("feather", 28, None))
                                (app_logo("stack", 28, None))
                                (app_logo("gui", 28, None))
                            }
                        }
                        h3 { "Bring your own wallet" }
                        p class="grow" {
                            "Use a wallet you already run in Cake Wallet, Feather, the Monero GUI or anything else. "
                            "Paste its private view key and public spend key."
                        }
                        p class="hint" { "Works without JavaScript." }
                        button type="submit" formaction=(flow.keys_action()) { "Bring your own wallet" }
                    }
                    section class="pick-card unavailable" {
                        div class="faded" {
                            div class="pick-card-head" {
                                span class="logo-row" { (trezor_logo(28)) (ledger_logo(28)) }
                                span class="tag" { "Coming soon" }
                            }
                            h3 { "Use a hardware wallet" }
                            p class="grow" {
                                "Plug in or pair a Trezor or Ledger and confirm on its screen. Your recovery phrase "
                                "stays on the device."
                            }
                            button type="button" disabled { "Connect a device" }
                        }
                    }
                }
                p class="warning" id="create-needs-js" data-needs-js-reason {
                    "Creating a new wallet needs JavaScript: its recovery phrase is made inside your browser so it "
                    "never reaches our server. Turn JavaScript on for this site and reload, or bring your own wallet."
                }
            }
            div class="step-foot" {
                @match flow {
                    Flow::Setup(setup) => { a class="btn" href=(format!("/setup?{}", setup.query())) { "Back" } }
                    Flow::Account => { a class="btn" href="/account?tab=wallets" { "Back" } }
                }
            }
        }
        // The script fetches its module from where the page says.
        script src=(crate::assets::url("wallet-setup.js")) data-module=(crate::assets::url("wallet-setup.wasm")) defer {}
    };
    let title = match flow {
        Flow::Setup(_) => "Choose a wallet - Monokulo",
        Flow::Account => "Add a wallet - Monokulo",
    };
    layout(chrome, title, body)
}

// -- Bringing your own wallet ----------------------------------------------

pub struct ImportViewModel {
    pub error: Option<String>,
    /// Decided on the choice screen, shown here and sent on.
    pub name: String,
    pub network: String,
    pub spend_pubkey_hex: String,
    /// "Which app is it in?": a `WALLET_APPS` key, `other`, or blank.
    pub app: String,
    pub custody_choices: Vec<super::connect::CustodyChoice>,
    pub snp_entry: Option<super::key_entry::SnpKeyEntry>,
}

/// `GET`/`POST /setup/wallet/keys` and `/account/wallets/import`: "Bring
/// your own wallet". Works without JavaScript.
pub fn import_page(chrome: &PageChrome, flow: Flow<'_>, data: &ImportViewModel) -> Markup {
    let back = flow.choice_href(&data.name, &data.network);
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href=(back) { "Wallet kind" } }
            (flow.steps(WalletPath::Keys, 1))
            h1 { "Bring your own wallet" }
            p {
                "Connect a Monero wallet you already have as " strong { (if data.name.is_empty() { "a new wallet" } else { &data.name }) }
                " on " (data.network) ". Monokulo asks for two watch-only keys and never your recovery phrase or private spend key."
            }
            @if let Some(error) = &data.error {
                p class="error" role="alert" { (error) }
            }
            form method="post" action=(flow.keys_action()) {
                (flow.hidden())
                input type="hidden" name="name" value=(data.name);
                input type="hidden" name="network" value=(data.network);
                (super::key_entry::key_fields(
                    "",
                    &data.spend_pubkey_hex,
                    data.snp_entry.as_ref(),
                    html! { "Lets Monokulo see payments arriving. It cannot spend." },
                    html! { "The " em { "public" } " half of your spend key. Never enter the private spend key anywhere." },
                ))
                (keys_help())
                (app_field(&data.app))
                (super::connect::custody_select(&data.custody_choices))
                @if let Some(entry) = &data.snp_entry {
                    (super::key_entry::snp_section(entry, (!data.custody_choices.is_empty()).then_some("key_custody_backend")))
                }
                div class="step-foot" {
                    a class="btn" href=(back) { "Back" }
                    span class="spacer" {}
                    button type="submit" class="btn-primary" { (flow.add_label()) }
                }
            }
        }
    };
    layout(chrome, "Bring your own wallet - Monokulo", body)
}

/// "Which app is it in?" on "Bring your own wallet": optional, saved as
/// `wallets.app`, and the wallet's page says "Brought in from Feather".
/// Each app with its logo, then Other.
pub fn app_field(selected: &str) -> Markup {
    html! {
        label {
            "Which app is it in? " span class="hint" { "(optional)" }
            mk-select {
                select name="app" {
                    (Choice::new("", "Not saying").selected(selected.is_empty()))
                    @for app in WALLET_APPS {
                        (Choice::new(app.key, app.name)
                            .logo(crate::assets::url(&format!("wallet-logos/{}.png", app.key)))
                            .selected(selected == app.key))
                    }
                    (Choice::new("other", "Other").selected(selected == "other"))
                }
            }
            span class="field-help" { "The wallet's page then says where its keys and recovery phrase live." }
        }
    }
}

// -- Making a new wallet ---------------------------------------------------

pub struct CreateViewModel {
    /// The name the wallet gets: typed on the choice screen, or picked for
    /// it, so the restore link and the pages can use it.
    pub name: String,
    pub network: String,
    /// The chain's height now, for restoring from the 25-word phrase.
    pub restore_height: Option<u64>,
    /// Set when the keys go to SEV-SNP key storage, encrypted in the page.
    pub snp_entry: Option<super::key_entry::SnpKeyEntry>,
    pub error: Option<String>,
}

/// `GET /setup/wallet/new` and `/account/wallets/new`: the recovery phrase
/// is made, backed up and checked on this page (`static/wallet-setup.js`
/// with the `wallet-setup` WebAssembly module); only the watch-only keys
/// are posted, and only once the check passes or is skipped, so nothing is
/// made before then and Back is always safe. Every screen is drawn here and
/// shown by the script.
pub fn create_page(chrome: &PageChrome, flow: Flow<'_>, data: &CreateViewModel) -> Markup {
    let height = data
        .restore_height
        .map(|h| h.to_string())
        .unwrap_or_default();
    let back = flow.choice_href(&data.name, &data.network);
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href=(back) { "Wallet kind" } }
            noscript {
                h1 { "Create a new wallet" }
                div class="error" role="alert" {
                    "Creating a wallet needs JavaScript: its recovery phrase is made inside your browser so it never "
                    "reaches our server. Turn JavaScript on for this site and reload, or go "
                    a href=(back) { "back and bring your own wallet" } "."
                }
            }
            @if let Some(error) = &data.error {
                p class="error" role="alert" { (error) }
            }
            div data-wallet-setup data-network=(data.network) data-name=(data.name) data-restore-height=(height) {
                section data-screen="loading" {
                    h1 { "Making " (data.name) "…" }
                    p role="status" data-loading-status { "Your browser is making the recovery phrase." }
                }
                section data-screen="backup" hidden {
                    (flow.steps(WalletPath::New, 1))
                    h1 { "Back up " (data.name) }
                    p {
                        "These words are the wallet. Anyone with them can spend from it; without them, nobody can get it "
                        "back, not even us. Save them one of these ways."
                    }
                    div class="app-tabs" role="tablist" aria-label="Where to save it" {
                        @for (i, app) in WALLET_APPS.iter().enumerate() {
                            button type="button" role="tab" id=(format!("tab-{}", app.key)) aria-controls=(format!("app-{}", app.key)) aria-selected=(if i == 0 { "true" } else { "false" }) data-app-tab=(app.key) {
                                @for logo in app.logos { (app_logo(logo, 22, None)) }
                                (app.tab)
                            }
                        }
                        button type="button" role="tab" id="tab-paper" aria-controls="app-paper" aria-selected="false" data-app-tab="paper" { "On paper" }
                    }
                    @for (i, app) in WALLET_APPS.iter().enumerate() {
                        (app_panel(app, i == 0, &height))
                    }
                    (paper_panel())
                    div class="step-foot" {
                        a class="btn" href=(back) { "Back" }
                        button type="button" data-go="skip" { "Skip backup…" }
                        span class="spacer" {}
                        button type="button" class="btn-primary" data-go="check" disabled { "Next: check two words" }
                    }
                }
                section data-screen="check" hidden {
                    (flow.steps(WalletPath::New, 2))
                    h1 { "Check two words" }
                    p data-check-intro { "Pick each word from your backup. This checks that what you saved is right." }
                    div class="word-quiz" {
                        @for i in 0..2 {
                            div class="word-q" data-question=(i) {
                                h3 data-question-label { "Word" }
                                div class="picks" role="group" {
                                    @for _ in 0..4 { button type="button" data-pick {} }
                                }
                                p class="q-note" data-question-note aria-live="polite" {}
                            }
                        }
                    }
                    p class="hint" { (lock_icon()) " Then your browser works out the wallet's watch-only keys and sends only those to Monokulo." }
                    div class="step-foot" {
                        button type="button" data-go="backup" { "Back" }
                        span class="spacer" {}
                        button type="button" class="btn-primary" data-check-next disabled {
                            "Continue anyway in " span class="countdown" data-countdown { "20 s" }
                        }
                    }
                }
                section data-screen="skip" hidden {
                    div class="skip-warning" role="alertdialog" aria-labelledby="skip-title" aria-describedby="skip-body" {
                        h2 id="skip-title" { "You won't see this phrase again" }
                        div id="skip-body" {
                            p {
                                "Monokulo never had a copy of " (data.name) "'s recovery phrase. Once you leave, it's gone from "
                                "this browser too. " strong { "Nobody can show it to you again, including us." }
                            }
                            p {
                                "Payments to this wallet will still arrive and show as paid in Monokulo. Without the phrase, "
                                strong { "nobody can ever spend that money." } " It's lost for good."
                            }
                        }
                        button type="button" class="btn-primary" data-go="backup" { "Go back and back up" }
                        hr class="rule";
                        label class="check-line" {
                            input type="checkbox" data-skip-understood;
                            "I understand that without the phrase, money paid to " (data.name) " can never be spent."
                        }
                        label {
                            "Type " code { "skip" } " to confirm"
                            input type="text" autocomplete="off" spellcheck="false" data-skip-typed;
                        }
                        button type="button" disabled data-skip-confirm { "Skip backup and add the wallet" }
                        span class="field-help" { "Turns on when the box is ticked and you've typed skip." }
                    }
                }
                section data-screen="failed" hidden {
                    h1 { "This browser can't make a wallet" }
                    p class="error" role="alert" data-failed-reason {}
                    p { a href=(back) { "Bring your own wallet" } " instead." }
                }
                form method="post" action=(flow.new_action()) data-register hidden {
                    (flow.hidden())
                    input type="hidden" name="name" value=(data.name);
                    input type="hidden" name="network" value=(data.network);
                    input type="hidden" name="backup" data-field="backup";
                    input type="hidden" name="primary_address" data-field="address";
                    input type="hidden" name="view_key_hex" data-field="view" data-key-custody="view";
                    input type="hidden" name="spend_pubkey_hex" data-field="spend" data-key-custody="spend";
                    @if let Some(entry) = &data.snp_entry {
                        (super::key_entry::snp_bundle(entry))
                    }
                }
            }
        }
        script src=(crate::assets::url("wallet-setup.js")) data-module=(crate::assets::url("wallet-setup.wasm")) defer {}
    };
    layout_with_head(chrome, "Create a new wallet - Monokulo", html! {}, body)
}

/// The phrase's heading line: its title, the button that makes a different
/// phrase, and anything else the tab adds.
fn phrase_head(title: &str, extra: Markup) -> Markup {
    html! {
        div class="phrase-head" {
            h3 { (title) }
            button type="button" class="icon-only regen" data-regenerate aria-label="Make a different phrase" title="Make a different phrase" {
                (regenerate_icon())
            }
            (extra)
        }
    }
}

fn print_button() -> Markup {
    html! {
        span class="card-spacer" {}
        button type="button" data-print { "Print" }
    }
}

/// One wallet app's tab: the words (and its QR code, for an app that scans
/// one) shown straight away, how to restore the wallet in it, and the box
/// its owner ticks once it has.
fn app_panel(app: &WalletApp, shown: bool, height: &str) -> Markup {
    let legacy = app.method == AppMethod::TypeLegacyWords;
    let phrase = html! {
        div class="phrase-box" {
            @if legacy {
                (phrase_head(app.title, html! { span class="tag tag-unknown" { "older wallets" } (print_button()) }))
                ol class="seed-words legacy" data-legacy-words aria-label="25-word phrase" {}
                dl class="facts" {
                    dt { "Restore height" }
                    dd { @if height.is_empty() { "the block height on the wallet's birthday" } @else { (height) } }
                    dt { "Restore with" }
                    dd { code { "monero-wallet-cli --restore-deterministic-wallet" } ", or the GUI's Restore wallet from keys or mnemonic seed" }
                }
            } @else {
                (phrase_head(app.title, html! {}))
                ol class="seed-words" data-words aria-label="Recovery phrase" {}
            }
            ol class="steps" { @for step in app.restore_steps { li { (step) } } }
        }
    };
    html! {
        div class="app-panel" id=(format!("app-{}", app.key)) role="tabpanel" aria-labelledby=(format!("tab-{}", app.key)) data-app-panel=(app.key) hidden[!shown] {
            @match app.method.qr_kind() {
                Some(kind) => {
                    div class="phrase-split" {
                        (phrase)
                        div class="qr-col" {
                            div class="qr-frame" data-qr=(kind) {}
                            p class="hint" {
                                @if kind == "restore-link" {
                                    "Restore link: the phrase, its birthday and the wallet's name."
                                } @else {
                                    "The 16 words, in the format Stack Wallet reads."
                                }
                            }
                        }
                    }
                }
                None => (phrase),
            }
            label class="check-line" {
                input type="checkbox" data-backed-up;
                " " (app.done_check)
            }
        }
    }
}

/// The "On paper" tab: the 16 words to write down or print.
fn paper_panel() -> Markup {
    html! {
        div class="app-panel" id="app-paper" role="tabpanel" aria-labelledby="tab-paper" data-app-panel="paper" hidden {
            div class="phrase-box print-sheet" {
                (phrase_head("Write these 16 words down", print_button()))
                ol class="seed-words" data-words aria-label="Recovery phrase" {}
                dl class="facts" {
                    dt { "Phrase type" } dd { "Polyseed, 16 words" }
                    dt { "Wallet birthday" } dd { "Today " span class="hint" { "(kept in the phrase, so apps restore quickly)" } }
                }
                p class="hint no-print" { "No copy button on purpose: other apps can read your clipboard." }
            }
            label class="check-line" {
                input type="checkbox" data-backed-up;
                " I've written all 16 words down, in order, and put them somewhere safe."
            }
        }
    }
}

fn origin_label(w: &crate::db::WalletRow) -> String {
    match w.origin {
        crate::db::WalletOrigin::Created => match w.backup.as_deref() {
            Some(backup) => format!(
                "Made in Monokulo, {}",
                crate::wallets::backup_label(backup).to_lowercase()
            ),
            None => "Made in Monokulo".to_owned(),
        },
        crate::db::WalletOrigin::Imported => "Brought in".to_owned(),
    }
}

// -- The list --------------------------------------------------------------

pub struct WalletListItem {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub network: String,
    pub stores: u64,
}

pub struct RetiredListItem {
    pub id: String,
    pub name: String,
    /// When, in the viewer's time zone.
    pub retired: String,
}

/// A key, crossed out: a wallet whose keys Monokulo deleted.
pub fn key_gone_icon(size: u32) -> Markup {
    html! {
        svg class="key-gone" viewBox="0 0 24 24" width=(size) height=(size) aria-hidden="true" focusable="false"
            fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" {
            circle cx="7.5" cy="15.5" r="4.5" {}
            path d="M10.7 12.3 20 3" {}
            path d="M16 7l3 3" {}
            path d="M3 3l18 18" {}
        }
    }
}

/// The wallets list: the Account page's Wallets tab (`views::account`).
/// `wallets` come mainnet first, then stagenet, then testnet, each by name
/// (`Db::list_wallets`). Mainnet wallets are the list; the test networks'
/// are folded below it, the fold open when there's no mainnet wallet.
pub fn list_section(wallets: &[WalletListItem], retired: &[RetiredListItem]) -> Markup {
    let (main, test): (Vec<&WalletListItem>, Vec<&WalletListItem>) =
        wallets.iter().partition(|w| w.network == "mainnet");
    html! {
            p { "Where your stores' payments go. Monokulo holds watch-only keys for each." }
            @if wallets.is_empty() {
                p class="notice" { "No wallets yet." }
            } @else if main.is_empty() {
                p class="hint" { "No mainnet wallets yet. Add one when you're ready to take real payments." }
            } @else {
                div class="net-section-head" {
                    h2 { (super::network_badge("mainnet")) }
                    p class="hint" { "Real money · " (wallets_count(main.len())) }
                }
                (wallets_table(&main, false))
            }
            @if !test.is_empty() {
                details class="test-wallets" open[main.is_empty()] {
                    summary {
                        (super::test_networks_badge())
                        strong { (wallets_count(test.len())) }
                        span class="hint" { "no real value" }
                    }
                    div { (wallets_table(&test, true)) }
                }
            }
            @if !retired.is_empty() {
                details class="retired-wallets" {
                    summary { strong { "Retired wallets (" (retired.len()) ")" } span class="hint" { " · keys deleted, kept for their history" } }
                    div class="table-scroll" {
                        table {
                            thead { tr { th { "Name" } th { "Retired" } th { "Keys" } } }
                            tbody {
                                @for w in retired {
                                    tr {
                                        td { a href=(format!("/account/wallets/{}", w.id)) { (w.name) } }
                                        td { (w.retired) }
                                        td { span class="keys-deleted" { (key_gone_icon(16)) " Deleted" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div class="list-foot" {
                @if !wallets.is_empty() {
                    p class="hint" { "Open a wallet to rename it, see its history or retire it." }
                }
                a class="btn btn-primary" href="/account/wallets/add" { "+ add a wallet" }
            }
    }
}

/// One network group's table. The test networks' carries each row's
/// network, since stagenet and testnet wallets share it. On a phone each
/// row is a card: name and stores, then address and kind.
fn wallets_table(wallets: &[&WalletListItem], with_network: bool) -> Markup {
    html! {
        div class="table-scroll" {
            table class="wallets-table table-cards" {
                thead { tr {
                    @if with_network { th { "Network" } }
                    th { "Name" } th { "Kind" } th { "Address" } th { "Stores" }
                } }
                tbody {
                    @for w in wallets {
                        tr {
                            @if with_network { td class="card-meta" { (super::network_badge(&w.network)) } }
                            td class="card-title" { a href=(format!("/account/wallets/{}", w.id)) { (w.name) } }
                            td class="card-when" { (w.kind) }
                            td class="card-detail" { code { (super::short_address(&w.address)) } }
                            td class="card-status" {
                                (w.stores)
                                span class="card-unit" { @if w.stores == 1 { " store" } @else { " stores" } }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn wallets_count(n: usize) -> String {
    if n == 1 {
        "1 wallet".to_owned()
    } else {
        format!("{n} wallets")
    }
}

// -- A wallet's page -------------------------------------------------------

pub struct WalletStore {
    pub id: String,
    pub name: String,
    /// For a store that changed to another wallet: when, in the viewer's
    /// time zone.
    pub until: Option<String>,
}

pub struct WalletEvent {
    /// Formatted in the viewer's time zone.
    pub when: String,
    pub what: Markup,
}

/// Whether a wallet can be retired (`http::wallets::retire_state`).
pub enum RetireState {
    Ready,
    /// Stores (of any account) take payments into it.
    Stores(u64),
    /// Orders on it can still be paid, until about then.
    Orders {
        count: u64,
        until: Option<String>,
    },
    /// The engine couldn't say.
    Unknown,
    Retired,
}

/// Bringing a retired wallet back: its keys, as when it was brought in.
pub struct RestoreForm {
    pub custody_choices: Vec<super::connect::CustodyChoice>,
    pub snp_entry: Option<super::key_entry::SnpKeyEntry>,
}

pub struct DetailViewModel {
    pub retire: RetireState,
    pub restore: Option<RestoreForm>,
    pub wallet: crate::db::WalletRow,
    pub stores: Vec<WalletStore>,
    /// Stores that took payments into it before changing to another wallet.
    pub past_stores: Vec<WalletStore>,
    pub history: Vec<WalletEvent>,
    pub error: Option<String>,
    pub notice: Option<String>,
    /// What the rename this page answers did.
    pub rename: Option<RenameOutcome>,
}

/// What a rename did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameOutcome {
    Saved,
    /// `name` (as sent) was refused, for `message`.
    Refused {
        name: String,
        message: String,
    },
}

/// "Details": the wallet's name, a settings form of its own (renaming
/// posts and comes back with a toast), and its facts.
fn details_card(chrome: &PageChrome, data: &DetailViewModel) -> Markup {
    let w = &data.wallet;
    let (shown, refusal) = match &data.rename {
        Some(RenameOutcome::Refused { name, message }) => (name.as_str(), Some(message.as_str())),
        _ => (w.name.as_str(), None),
    };
    let toast = match &data.rename {
        Some(RenameOutcome::Saved) => Some(
            Toast::new(ToastKind::Success, "Renamed")
                .line(format!("This wallet is called {} now.", w.name)),
        ),
        Some(RenameOutcome::Refused { message, .. }) => Some(
            Toast::new(ToastKind::Error, "Not renamed")
                .line(message.clone())
                .show("details"),
        ),
        None => None,
    };
    html! {
        (settings::form(&format!("/account/wallets/{}/rename", w.id), Save::Reload, "Details", html! {
            (Card::new("details", "Details")
                .failed(refusal.is_some(), refusal)
                .saved(matches!(data.rename, Some(RenameOutcome::Saved)).then_some(""), false)
                .render(html! {
                    (Field::new("Name", "wallet-name")
                        .help(None, html! { "Only you see it, in wallet lists and pickers." })
                        .render(html! {
                            input type="text" id="wallet-name" name="name" value=(shown) maxlength=(crate::wallets::MAX_NAME_LEN) required
                                data-saved=[refusal.map(|_| w.name.as_str())];
                        }))
                    dl class="facts" {
                        dt { "Address" } dd { code { (w.primary_address) } }
                        dt { "Network" } dd { (super::network_badge(&w.network)) }
                        dt { "Kind" } dd { (origin_label(w)) }
                        @if let Some(at) = w.retired_at {
                            dt { "Keys" } dd { "Deleted " (chrome.clock.time(at)) }
                        }
                    }
                }))
            (settings::save_bar(refusal.is_some(), false, html! {
                @if let Some(message) = refusal {
                    strong { "Not renamed." } " " (message)
                } @else {
                    "Saving renames this wallet."
                }
            }, &format!("/account/wallets/{}", w.id)))
        }))
        (settings::toast_region(toast.as_ref(), false))
    }
}

pub fn detail_page(chrome: &PageChrome, data: &DetailViewModel) -> Markup {
    let w = &data.wallet;
    let in_use = !data.stores.is_empty();
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/account?tab=wallets" { "Wallets" } }
            div class="wallet-title" { h1 { (w.name) } (super::network_badge(&w.network)) }
            p class="hint" {
                (origin_label(w))
                @if w.network == "mainnet" { " · real money" } @else { " · test network, no real value" }
            }
            @if let Some(notice) = &data.notice { p class="success" role="status" { (notice) } }
            @if let Some(error) = &data.error { p class="error" role="alert" { (error) } }
            @if let Some(at) = w.retired_at {
                div class="keys-gone-banner" role="status" {
                    (key_gone_icon(28))
                    div {
                        p class="keys-gone-title" { "Retired. Its keys are deleted." }
                        p {
                            "On " (chrome.clock.time(at)) " Monokulo deleted " (w.name) "'s private view key and public spend key "
                            "from key storage. It no longer sees payments into this wallet, and no store can use it."
                        }
                        p class="hint" { "The money is still yours, in your wallet app." }
                    }
                }
            }
            div class="wallet-layout" {
                div class="main" {
                    (details_card(chrome, data))
                    section class="box" {
                        h2 { "History" }
                        @if data.history.is_empty() {
                            p class="hint" { "Nothing yet." }
                        } @else {
                            ol class="timeline" {
                                @for event in &data.history {
                                    li { time { (event.when) } span { (event.what) } }
                                }
                            }
                        }
                    }
                }
                aside {
                    section class="box" {
                        h2 { "Stores" }
                        @if in_use {
                            ul {
                                @for store in &data.stores {
                                    li { a href=(format!("/dashboard/stores/{}", store.id)) { (store.name) } }
                                }
                            }
                        } @else {
                            p class="hint" { "No stores use this wallet." }
                        }
                        @if !data.past_stores.is_empty() {
                            h3 { "Before" }
                            ul class="past-stores" {
                                @for store in &data.past_stores {
                                    li {
                                        a href=(format!("/dashboard/stores/{}", store.id)) { (store.name) }
                                        @if let Some(until) = &store.until { span class="muted" { " until " (until) } }
                                    }
                                }
                            }
                        }
                    }
                    @if let Some(restore) = &data.restore {
                        (restore_section(w, restore))
                    } @else {
                        (retire_section(w, &data.retire))
                    }
                }
            }
        }
    };
    layout(chrome, &format!("{} - Wallets - Monokulo", w.name), body)
}

/// "Retire wallet": offered only once no store uses the wallet and no
/// order on it can still be paid; otherwise it says what it waits for.
fn retire_section(w: &crate::db::WalletRow, state: &RetireState) -> Markup {
    let why = match state {
        RetireState::Ready | RetireState::Retired => None,
        RetireState::Stores(n) => Some(if *n == 1 {
            "1 store still uses this wallet. Change its wallet first.".to_owned()
        } else {
            format!("{n} stores still use this wallet. Change their wallet first.")
        }),
        RetireState::Orders { count, until } => Some(format!(
            "{} on it can still be paid{}. Retire it after then.",
            if *count == 1 {
                "1 order".to_owned()
            } else {
                format!("{count} orders")
            },
            until
                .as_ref()
                .map(|u| format!(", until about {u}"))
                .unwrap_or_default()
        )),
        RetireState::Unknown => Some(
            "Monokulo can't check whether it's still in use right now. Try again in a minute."
                .to_owned(),
        ),
    };
    html! {
        section class="danger-zone" {
            h2 { "Retire wallet" }
            p {
                "Retiring takes " (w.name) " out of every wallet list and deletes its keys from Monokulo: its private view key "
                "and public spend key. Monokulo will no longer see payments into it. Its name and history stay, and the money "
                "stays yours, in your wallet app."
            }
            @if let Some(why) = why {
                p class="field-error" id="retire-why" { (why) }
                button type="button" disabled aria-describedby="retire-why" { "Retire wallet" }
            } @else {
                form method="post" action=(format!("/account/wallets/{}/retire", w.id)) {
                    label { "Type " strong { (w.name) } " to confirm" input type="text" name="confirm" autocomplete="off" required; }
                    button type="submit" class="btn-danger" { "Retire wallet" }
                }
            }
        }
    }
}

/// "Bring it back": a retired wallet's keys, entered again.
fn restore_section(w: &crate::db::WalletRow, restore: &RestoreForm) -> Markup {
    html! {
        section class="box" {
            h2 { "Bring it back" }
            p { "Enter the keys again to watch " (w.name) " and offer it to stores. They must be this wallet's: Monokulo checks them against its address." }
            form method="post" action=(format!("/account/wallets/{}/restore", w.id)) {
                (super::key_entry::key_fields(
                    "",
                    "",
                    restore.snp_entry.as_ref(),
                    html! { "Lets Monokulo see payments arriving. It cannot spend." },
                    html! { "The " em { "public" } " half of your spend key." },
                ))
                (keys_help())
                (super::connect::custody_select(&restore.custody_choices))
                @if let Some(entry) = &restore.snp_entry {
                    (super::key_entry::snp_section(entry, (!restore.custody_choices.is_empty()).then_some("key_custody_backend")))
                }
                button type="submit" { "Bring back " (w.name) }
            }
        }
    }
}

/// After a wallet in a picker: how many stores use it.
fn stores_label(count: u64) -> String {
    match count {
        0 => "no stores".to_owned(),
        1 => "1 store".to_owned(),
        n => format!("{n} stores"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet(name: &str, network: &str, stores: u64) -> WalletListItem {
        WalletListItem {
            id: format!("w_{}", name.to_lowercase().replace(' ', "_")),
            name: name.to_owned(),
            kind: "Brought in",
            address: format!("{name}-address-0123456789abcdef"),
            network: network.to_owned(),
            stores,
        }
    }

    /// The part of `html` from `from` on, to `to`.
    fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
        let start = html
            .find(from)
            .unwrap_or_else(|| panic!("{from} in {html}"));
        let rest = &html[start..];
        &rest[..rest.find(to).map_or(rest.len(), |end| end + to.len())]
    }

    #[test]
    fn mainnet_wallets_are_the_list_with_no_fold() {
        let html = list_section(
            &[
                wallet("Cake", "mainnet", 2),
                wallet("Savings", "mainnet", 1),
            ],
            &[],
        )
        .into_string();
        let head = between(&html, r#"<div class="net-section-head">"#, "</div>");
        assert!(
            head.contains(r#"<h2><span class="tag-network is-main">"#),
            "{head}"
        );
        assert!(head.contains("Real money · 2 wallets"), "{head}");
        assert!(
            html.contains(
                "<thead><tr><th>Name</th><th>Kind</th><th>Address</th><th>Stores</th></tr></thead>"
            ),
            "{html}"
        );
        assert!(!html.contains("test-wallets"), "{html}");
        assert!(!html.contains("No mainnet wallets yet"), "{html}");
        // The address shortened by `views::short_address`, all of it in reach.
        assert!(html.contains(r#"<code><span class="short-value" title="Cake-address-0123456789abcdef"><span class="short-value-text" aria-hidden="true">Cake-a…s-0123…abcdef</span><span class="short-value-full">Cake-address-0123456789abcdef</span></span></code>"#), "{html}");
        assert!(!html.contains("..."), "{html}");
        // On a phone, "2 stores" and "1 store".
        assert!(
            html.contains(r#"2<span class="card-unit"> stores</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"1<span class="card-unit"> store</span>"#),
            "{html}"
        );
        // The one orange button.
        assert_eq!(html.matches("btn-primary").count(), 1, "{html}");
    }

    #[test]
    fn with_only_test_wallets_the_fold_starts_open_and_says_why() {
        let html = list_section(
            &[
                wallet("Feather test", "stagenet", 1),
                wallet("Lab", "testnet", 0),
            ],
            &[],
        )
        .into_string();
        assert!(html.contains(r#"<p class="hint">No mainnet wallets yet. Add one when you're ready to take real payments.</p>"#), "{html}");
        assert!(!html.contains("net-section-head"), "{html}");
        let fold = between(&html, r#"<details class="test-wallets""#, "</details>");
        assert!(
            fold.starts_with(
                r#"<details class="test-wallets" open><summary><span class="tag-network is-test">"#
            ),
            "{fold}"
        );
        assert!(fold.contains("Test networks</span><strong>2 wallets</strong><span class=\"hint\">no real value</span></summary>"), "{fold}");
        assert!(fold.contains("<th>Network</th><th>Name</th>"), "{fold}");
        // Each row carries its own network's badge.
        assert!(
            fold.contains(r#"<td class="card-meta"><span class="tag-network is-test">"#),
            "{fold}"
        );
        assert!(fold.contains("</svg>Stagenet</span>"), "{fold}");
        assert!(fold.contains("</svg>Testnet</span>"), "{fold}");
        assert!(!html.contains("tag-slow"), "{html}");
    }

    #[test]
    fn mixed_wallets_put_mainnet_first_and_fold_the_test_ones_shut() {
        let html = list_section(
            &[
                wallet("Cake", "mainnet", 2),
                wallet("Feather test", "stagenet", 1),
            ],
            &[RetiredListItem {
                id: "w_old".to_owned(),
                name: "Old till".to_owned(),
                retired: "1 Sep 2026".to_owned(),
            }],
        )
        .into_string();
        let main_at = html.find("net-section-head").unwrap();
        let fold_at = html.find(r#"<details class="test-wallets">"#).unwrap();
        let retired_at = html.find(r#"<details class="retired-wallets">"#).unwrap();
        assert!(main_at < fold_at && fold_at < retired_at, "{html}");
        assert!(html.contains("Real money · 1 wallet<"), "{html}");
        assert!(html.contains("<strong>1 wallet</strong>"), "{html}");
        let main = &html[main_at..fold_at];
        assert!(
            main.contains("Cake") && !main.contains("Feather test"),
            "{main}"
        );
        assert!(html[fold_at..retired_at].contains("Feather test"), "{html}");
    }

    #[test]
    fn no_wallets_says_so_and_offers_to_add_one() {
        let html = list_section(&[], &[]).into_string();
        assert!(
            html.contains(r#"<p class="notice">No wallets yet.</p>"#),
            "{html}"
        );
        assert!(
            !html.contains("test-wallets") && !html.contains("net-section-head"),
            "{html}"
        );
        assert!(html.contains(r#"href="/account/wallets/add""#), "{html}");
    }

    fn summary(name: &str, network: &str, stores: u64) -> crate::db::WalletSummary {
        crate::db::WalletSummary {
            wallet: crate::db::WalletRow {
                id: crate::db::WalletId::new(format!("w_{name}")),
                user_id: crate::db::UserId::new("u1"),
                name: name.to_owned(),
                network: network.to_owned(),
                primary_address: "5B8s3obCY2ETeQB3GNAGPK2zRGen5UeW1WzegSizVsmf6z5NvM2GLoN6zzk1vHyzGAAfA8pGhuYAeCFZjHAp59jRVQkunGS".to_owned(),
                engine_wallet_id: crate::db::EngineWalletId::new("e"),
                origin: crate::db::WalletOrigin::Imported,
                backup: None,
                app: None,
                created_at: 0,
                retired_at: None,
            },
            store_count: stores,
        }
    }

    /// Setup's picker of wallets already added names each wallet's network
    /// as the badge, and in words without JavaScript.
    #[test]
    fn the_wallets_already_added_show_each_wallets_network() {
        let setup = SetupContext {
            store_name: "Bakery".into(),
            fields: vec![("kind", "web".into())],
        };
        let html = choice_page(
            &PageChrome::from_user(None, "/setup/wallet"),
            Flow::Setup(&setup),
            &ChoiceViewModel {
                wallets: vec![
                    summary("Cake", "mainnet", 2),
                    summary("Feather", "stagenet", 1),
                ],
                name: "Bakery takings".into(),
                name_problem: None,
                network: "mainnet".into(),
                error: None,
            },
        )
        .into_string();
        assert!(html.contains(r#"data-network="mainnet" data-note="2 stores">Cake (5B8s3o…6z5NvM…QkunGS) [Mainnet] - 2 stores</option>"#), "{html}");
        assert!(
            html.contains(r#"data-network="stagenet" data-note="1 store">"#),
            "{html}"
        );
        assert!(html.contains("[Stagenet] - 1 store</option>"), "{html}");
    }
}
