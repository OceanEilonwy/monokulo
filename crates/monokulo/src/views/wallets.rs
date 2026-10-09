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
            select name="network" id="wallet-network" {
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
    /// Where it sells, under its name: its host when that isn't already
    /// its name, "in person" for a store with no website.
    pub place: Option<String>,
    /// For a store that changed to another wallet: when, in the viewer's
    /// time zone.
    pub until: Option<String>,
}

pub struct WalletEvent {
    /// Formatted in the viewer's time zone.
    pub when: String,
    pub what: Markup,
}

/// Whether a wallet can be retired (`http::wallets::retire_state`): the
/// retire dialog's checklist.
pub enum RetireState {
    /// What the engine says uses it.
    Checked {
        /// Stores (of any account) taking payments into it.
        stores: u64,
        /// Orders on it that can still be paid.
        orders: u64,
        /// Until about when, in the viewer's time zone.
        until: Option<String>,
    },
    /// The engine couldn't say.
    Unknown,
    Retired,
}

impl RetireState {
    /// Every condition is ticked.
    pub fn ready(&self) -> bool {
        matches!(
            self,
            RetireState::Checked {
                stores: 0,
                orders: 0,
                ..
            }
        )
    }
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
fn details_card(data: &DetailViewModel) -> Markup {
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
                        dt { "Address" } dd { code { (super::short_address(&w.primary_address)) } }
                        dt { "Kind" } dd { (kind_label(w)) }
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

fn wallet_path(w: &crate::db::WalletRow) -> String {
    format!("/account/wallets/{}", w.id)
}

/// `GET /account/wallets/{id}`: one centred column. The name and its
/// network; where the wallet lives; its stores; its details (the rename);
/// its history, folded; and at the bottom, Retire (or, once retired,
/// Restore), each opening a dialog, or without JavaScript a page holding
/// the same content (`retire_page`, `restore_page`).
pub fn detail_page(chrome: &PageChrome, data: &DetailViewModel) -> Markup {
    let w = &data.wallet;
    let body = html! {
        div class="wrap wallet-page" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/account?tab=wallets" { "Wallets" } }
            div class="wallet-title" { h1 { (w.name) } (super::network_badge(&w.network)) }
            @if let Some(at) = w.retired_at {
                p class="wallet-meta" {
                    span class="tag tag-unknown" { "retired" }
                    span { "Keys deleted " (chrome.clock.time(at)) ". History kept." }
                }
            }
            @if let Some(notice) = &data.notice { p class="success" role="status" { (notice) } }
            @if let Some(error) = &data.error { p class="error" role="alert" { (error) } }
            (where_banner(w))
            (stores_card(&data.stores, &data.past_stores))
            (details_card(data))
            details class="history-fold" {
                summary { "History " span class="hint" { (events_count(data.history.len())) } }
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
            @if let Some(restore) = &data.restore {
                div class="wallet-foot" {
                    div {
                        strong { "Restore this wallet" }
                        p class="hint" { "Enter its keys again and Monokulo watches it for payments." }
                    }
                    a class="btn" href=(format!("{}/restore", wallet_path(w))) data-opens-dialog="restore-dialog" { "Restore wallet…" }
                }
                dialog id="restore-dialog" class="settings-dialog wallet-dialog" aria-labelledby="restore-title" {
                    (restore_content(w, restore, None, true))
                }
            } @else {
                div class="wallet-foot" {
                    div {
                        strong { "Retire this wallet" }
                        p class="hint" { "Takes it out of every list and deletes its keys from Monokulo. The money stays in your wallet app." }
                    }
                    a class="btn btn-danger" href=(format!("{}/retire", wallet_path(w))) data-opens-dialog="retire-dialog" { "Retire wallet…" }
                }
                dialog id="retire-dialog" class="settings-dialog wallet-dialog" aria-labelledby="retire-title" {
                    (retire_content(w, &data.retire, &data.stores, None, true))
                }
            }
        }
        (super::script("wallet-page.js", super::Load::Defer))
    };
    layout(chrome, &format!("{} - Wallets - Monokulo", w.name), body)
}

fn events_count(n: usize) -> String {
    match n {
        0 => "nothing yet".to_owned(),
        1 => "1 event".to_owned(),
        n => format!("{n} events"),
    }
}

/// The Details card's "Kind": where the wallet came from. Where it lives
/// is the banner's to say.
fn kind_label(w: &crate::db::WalletRow) -> &'static str {
    match w.origin {
        crate::db::WalletOrigin::Created => "Made in Monokulo",
        crate::db::WalletOrigin::Imported => "Brought in",
    }
}

/// "Where it lives", first on a wallet's page: the app holding its keys
/// and recovery phrase, from how a made wallet was backed up
/// (`wallets.backup`) or which app a brought-in one is in (`wallets.app`).
/// A skipped backup is tinted as a warning, gently.
pub fn where_banner(w: &crate::db::WalletRow) -> Markup {
    use crate::db::WalletOrigin;
    const WATCHES: &str = "Monokulo only watches this wallet; it can never spend from it.";
    let app = |key: &str| crate::wallets::wallet_app(key);
    let (icon, title, text, warn) = match (w.origin, w.backup.as_deref(), w.app.as_deref()) {
        (WalletOrigin::Created, Some("paper"), _) => (
            paper_glyph(),
            "Backed up on paper".to_owned(),
            format!("Its recovery phrase is the words you wrote down: keep them safe, they're the only copy. {WATCHES}"),
            false,
        ),
        (WalletOrigin::Created, Some("skipped"), _) => (
            warning_glyph(),
            "Backup skipped".to_owned(),
            "Its recovery phrase wasn't saved, so money paid into it can't be spent. Consider taking payments into a wallet you've backed up.".to_owned(),
            true,
        ),
        (WalletOrigin::Created, Some(key), _) if app(key).is_some() => {
            let app = app(key).expect("checked");
            (
                app_logo(app.key, 40, None),
                format!("Backed up to {}", app.name),
                format!("Its keys and recovery phrase live in {}. {WATCHES}", app.name),
                false,
            )
        }
        (WalletOrigin::Created, _, _) => (
            muted_glyph(),
            "Made in Monokulo".to_owned(),
            format!("Its keys and recovery phrase live wherever you saved them. {WATCHES}"),
            false,
        ),
        (WalletOrigin::Imported, _, Some(key)) if app(key).is_some() => {
            let app = app(key).expect("checked");
            (
                app_logo(app.key, 40, None),
                format!("Brought in from {}", app.name),
                format!("Its keys and recovery phrase live in {}. {WATCHES}", app.name),
                false,
            )
        }
        (WalletOrigin::Imported, _, Some("other")) => (
            muted_glyph(),
            "Brought in from another app".to_owned(),
            format!("Its keys and recovery phrase live in your wallet app. {WATCHES}"),
            false,
        ),
        (WalletOrigin::Imported, _, _) => (
            muted_glyph(),
            "Brought in · app not recorded".to_owned(),
            format!("Its keys and recovery phrase live in your wallet app. {WATCHES}"),
            false,
        ),
    };
    html! {
        section class=(if warn { "where-banner is-warn" } else { "where-banner" }) aria-label="Where it lives" {
            (icon)
            div {
                strong { (title) }
                p class="hint" { (text) }
            }
        }
    }
}

/// A sheet of paper with lines: a phrase written down.
fn paper_glyph() -> Markup {
    html! {
        span class="where-glyph" aria-hidden="true" {
            svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" {
                path d="M6 3h9l4 4v14H6z" {}
                path d="M15 3v4h4M9 11h7M9 14h7M9 17h5" {}
            }
        }
    }
}

/// A warning triangle: a backup skipped.
fn warning_glyph() -> Markup {
    html! {
        span class="where-glyph" aria-hidden="true" {
            svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" {
                path d="M12 3 2 20h20z" {}
                path d="M12 10v4M12 17h.01" {}
            }
        }
    }
}

/// A wallet, drawn plain: no app recorded.
fn muted_glyph() -> Markup {
    html! {
        span class="where-glyph" aria-hidden="true" {
            svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" {
                rect x="3" y="6" width="18" height="13" rx="2" {}
                path d="M16 12.5h2M3 9h15" {}
            }
        }
    }
}

/// The stores taking payments into a wallet, each with a link to change
/// its wallet; those that did before, muted under "Before".
fn stores_card(stores: &[WalletStore], past: &[WalletStore]) -> Markup {
    let meta = match stores.len() {
        0 => "none now".to_owned(),
        1 => "1 takes payments into this wallet".to_owned(),
        n => format!("{n} take payments into this wallet"),
    };
    html! {
        section class="settings-card" aria-labelledby="stores-title" {
            header class="card-head" { h3 id="stores-title" { "Stores" } span class="card-meta" { (meta) } }
            div class="card-body" {
                @if stores.is_empty() && past.is_empty() {
                    p class="hint" { "No stores use this wallet." }
                }
                @if !stores.is_empty() {
                    ul class="store-rows" {
                        @for store in stores {
                            li {
                                span class="sr-name" {
                                    a href=(format!("/dashboard/stores/{}", store.id)) { (store.name) }
                                    @if let Some(place) = &store.place { span class="sr-host" { (place) } }
                                }
                                a class="sr-act" href=(change_wallet_link(&store.id)) { "change the wallet" }
                            }
                        }
                    }
                }
                @if !past.is_empty() {
                    h4 { "Before" }
                    ul class="store-rows past" {
                        @for store in past {
                            li {
                                span class="sr-name" {
                                    a href=(format!("/dashboard/stores/{}", store.id)) { (store.name) }
                                    @if let Some(place) = &store.place { span class="sr-host" { (place) } }
                                }
                                @if let Some(until) = &store.until { span class="sr-host" { "until " (until) } }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A store's Wallet section, where its wallet is changed.
fn change_wallet_link(store_id: &str) -> String {
    format!("/dashboard/stores/{store_id}/settings#wallet")
}

/// The retire dialog (D1, the checklist), and the same on the page
/// without JavaScript (`retire_page`): what retiring does, then whether no
/// store takes payments into it and no order on it can still be paid,
/// each with its fix. When both are ticked, the name typed to confirm;
/// until then the red button is off.
fn retire_content(
    w: &crate::db::WalletRow,
    state: &RetireState,
    stores: &[WalletStore],
    error: Option<&str>,
    in_dialog: bool,
) -> Markup {
    let action = format!("{}/retire", wallet_path(w));
    let title = html! { "Retire \u{201c}" (w.name) "\u{201d}?" };
    html! {
        @if in_dialog {
            h2 id="retire-title" class="dialog-title" {
                (title)
                button type="button" class="dialog-x" aria-label="Close" data-closes-dialog { "×" }
            }
        } @else {
            h1 id="retire-title" class="dialog-title" { (title) }
        }
        p { "Retiring deletes its keys from Monokulo, so it stops seeing payments. The money stays in your wallet app." }
        @if let Some(error) = error { p class="error" role="alert" { (error) } }
        ul class="checks" {
            @match state {
                RetireState::Checked { stores: in_use, orders, until } => {
                    (check(*in_use == 0, html! { "No store takes payments into it" }, html! {
                        @if stores.is_empty() {
                            (if *in_use == 1 { "A store does".to_owned() } else { format!("{in_use} stores do") })
                        }
                        @for store in stores {
                            span class="fix-line" {
                                (store.name) " does · "
                                a href=(change_wallet_link(&store.id)) { "change its wallet" }
                            }
                        }
                    }))
                    (check(*orders == 0, html! { "No order on it can still be paid" }, html! {
                        @match until {
                            Some(until) => { "until about " (until) }
                            None => { (if *orders == 1 { "1 order can".to_owned() } else { format!("{orders} orders can") }) }
                        }
                    }))
                }
                RetireState::Unknown => {
                    (check(false, html! { "Monokulo can't check right now" }, html! {
                        a href=(action) { "try again" }
                    }))
                }
                RetireState::Retired => {
                    (check(true, html! { "Already retired" }, html! {}))
                }
            }
        }
        @if state.ready() {
            form method="post" action=(action) {
                div class="setting-field" {
                    div class="setting-label-row" {
                        label class="setting-label" for="retire-confirm" { "Type " strong { "\u{201c}" (w.name) "\u{201d}" } " to confirm" }
                    }
                    input type="text" id="retire-confirm" name="confirm" autocomplete="off" spellcheck="false" required;
                }
                div class="dialog-actions" {
                    a class="btn" href=(wallet_path(w)) data-closes-dialog { "Cancel" }
                    button type="submit" class="btn-danger" { "Retire wallet" }
                }
            }
        } @else {
            p class="hint" id="retire-why" {
                @if matches!(state, RetireState::Unknown) {
                    "Retire becomes available once Monokulo can check."
                } @else {
                    "Retire becomes available when both are ticked."
                }
            }
            div class="dialog-actions" {
                a class="btn" href=(wallet_path(w)) data-closes-dialog { "Close" }
                button type="button" class="btn-danger" disabled aria-describedby="retire-why" { "Retire wallet" }
            }
        }
    }
}

/// One row of the retire checklist: ✓, or ✕ with its fix beside it.
fn check(ok: bool, what: Markup, fix: Markup) -> Markup {
    html! {
        li class=(if ok { "ok" } else { "no" }) {
            span class="mark" aria-hidden="true" { (if ok { "✓" } else { "✕" }) }
            span {
                span class="visually-hidden" { (if ok { "Done: " } else { "Not yet: " }) }
                (what)
                @if !ok { span class="fix" { (fix) } }
            }
        }
    }
}

/// Restoring a retired wallet: its keys entered again, in the dialog and
/// on the page without JavaScript (`restore_page`).
fn restore_content(
    w: &crate::db::WalletRow,
    restore: &RestoreForm,
    error: Option<&str>,
    in_dialog: bool,
) -> Markup {
    let title = html! { "Restore \u{201c}" (w.name) "\u{201d}" };
    html! {
        @if in_dialog {
            h2 id="restore-title" class="dialog-title" {
                (title)
                button type="button" class="dialog-x" aria-label="Close" data-closes-dialog { "×" }
            }
        } @else {
            h1 id="restore-title" class="dialog-title" { (title) }
        }
        p { "Paste its keys again and Monokulo watches it as before. It must be the same wallet: the address has to match." }
        @if let Some(error) = error { p class="error" role="alert" { (error) } }
        form method="post" action=(format!("{}/restore", wallet_path(w))) {
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
            div class="dialog-actions" {
                a class="btn" href=(wallet_path(w)) data-closes-dialog { "Cancel" }
                button type="submit" class="btn-primary" { "Restore wallet" }
            }
        }
    }
}

/// A page holding a wallet dialog's content, for a browser without
/// JavaScript.
fn dialog_page(
    chrome: &PageChrome,
    w: &crate::db::WalletRow,
    title: &str,
    content: Markup,
) -> Markup {
    let body = html! {
        div class="wrap wallet-page" {
            nav class="context-nav" aria-label="Breadcrumb" {
                a href="/account?tab=wallets" { "Wallets" }
                " › "
                a href=(wallet_path(w)) { (w.name) }
            }
            div class="wallet-dialog-page" { (content) }
        }
    };
    layout(
        chrome,
        &format!("{title} {} - Wallets - Monokulo", w.name),
        body,
    )
}

/// `GET /account/wallets/{id}/retire`: the retire dialog as a page.
pub fn retire_page(
    chrome: &PageChrome,
    w: &crate::db::WalletRow,
    state: &RetireState,
    stores: &[WalletStore],
    error: Option<&str>,
) -> Markup {
    dialog_page(
        chrome,
        w,
        "Retire",
        retire_content(w, state, stores, error, false),
    )
}

/// `GET /account/wallets/{id}/restore`: the restore dialog as a page.
pub fn restore_page(
    chrome: &PageChrome,
    w: &crate::db::WalletRow,
    restore: &RestoreForm,
    error: Option<&str>,
) -> Markup {
    dialog_page(
        chrome,
        w,
        "Restore",
        restore_content(w, restore, error, false),
    )
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

    fn row(
        origin: crate::db::WalletOrigin,
        backup: Option<&str>,
        app: Option<&str>,
    ) -> crate::db::WalletRow {
        let mut w = summary("Shop takings", "mainnet", 0).wallet;
        w.origin = origin;
        w.backup = backup.map(str::to_owned);
        w.app = app.map(str::to_owned);
        w
    }

    /// Each state of "Where it lives": title, logo or glyph, and whether
    /// it's tinted as a warning.
    #[test]
    fn where_it_lives_says_which_app_holds_the_keys() {
        use crate::db::WalletOrigin::{Created, Imported};
        let cases: [(_, _, _, &str, &str, bool); 8] = [
            (
                Created,
                Some("cake"),
                None,
                "Backed up to Cake Wallet",
                "wallet-logos/cake.",
                false,
            ),
            (
                Created,
                Some("stack"),
                None,
                "Backed up to Stack Wallet",
                "wallet-logos/stack.",
                false,
            ),
            (
                Created,
                Some("feather"),
                None,
                "Backed up to Feather",
                "wallet-logos/feather.",
                false,
            ),
            (
                Created,
                Some("gui"),
                None,
                "Backed up to Monero GUI / CLI",
                "wallet-logos/gui.",
                false,
            ),
            (
                Created,
                Some("paper"),
                None,
                "Backed up on paper",
                "where-glyph",
                false,
            ),
            (
                Created,
                Some("skipped"),
                None,
                "Backup skipped",
                "where-glyph",
                true,
            ),
            (
                Imported,
                None,
                Some("feather"),
                "Brought in from Feather",
                "wallet-logos/feather.",
                false,
            ),
            (
                Imported,
                None,
                None,
                "Brought in · app not recorded",
                "where-glyph",
                false,
            ),
        ];
        for (origin, backup, app, title, picture, warn) in cases {
            let html = where_banner(&row(origin, backup, app)).into_string();
            assert!(
                html.contains(&format!("<strong>{title}</strong>")),
                "{html}"
            );
            assert!(html.contains(picture), "{title}: {html}");
            assert_eq!(html.contains("where-banner is-warn"), warn, "{html}");
        }
        let other = where_banner(&row(Imported, None, Some("other"))).into_string();
        assert!(
            other.contains("<strong>Brought in from another app</strong>"),
            "{other}"
        );
        let feather = where_banner(&row(Imported, None, Some("feather"))).into_string();
        assert!(feather.contains("Its keys and recovery phrase live in Feather. Monokulo only watches this wallet; it can never spend from it."), "{feather}");
        let skipped = where_banner(&row(Created, Some("skipped"), None)).into_string();
        assert!(
            skipped.contains("money paid into it can't be spent"),
            "{skipped}"
        );
    }

    fn store(id: &str, name: &str, place: Option<&str>, until: Option<&str>) -> WalletStore {
        WalletStore {
            id: id.to_owned(),
            name: name.to_owned(),
            place: place.map(str::to_owned),
            until: until.map(str::to_owned),
        }
    }

    #[test]
    fn the_stores_card_lists_stores_flush_with_a_change_link_and_past_ones_muted() {
        let html = stores_card(
            &[
                store("s1", "bakery.example", None, None),
                store("s2", "Saturday market", Some("in person"), None),
            ],
            &[store("s0", "old-shop.example", None, Some("1 Oct"))],
        )
        .into_string();
        assert!(
            html.contains(r#"<span class="card-meta">2 take payments into this wallet</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<li><span class="sr-name"><a href="/dashboard/stores/s1">bakery.example</a></span><a class="sr-act" href="/dashboard/stores/s1/settings#wallet">change the wallet</a></li>"#), "{html}");
        assert!(
            html.contains(r#"Saturday market</a><span class="sr-host">in person</span>"#),
            "{html}"
        );
        let before = between(&html, "<h4>Before</h4>", "</ul>");
        assert!(
            before.contains(r#"<ul class="store-rows past">"#),
            "{before}"
        );
        assert!(
            before.contains(r#"<span class="sr-host">until 1 Oct</span>"#),
            "{before}"
        );
        assert!(!before.contains("change the wallet"), "{before}");

        let none = stores_card(&[], &[]).into_string();
        assert!(
            none.contains("none now") && none.contains("No stores use this wallet."),
            "{none}"
        );
        let one = stores_card(&[store("s1", "a", None, None)], &[]).into_string();
        assert!(one.contains("1 takes payments into this wallet"), "{one}");
    }

    fn retire(state: RetireState, stores: &[WalletStore]) -> String {
        retire_content(
            &summary("Shop takings", "mainnet", 0).wallet,
            &state,
            stores,
            None,
            true,
        )
        .into_string()
    }

    const DISABLED: &str = r#"<button type="button" class="btn-danger" disabled aria-describedby="retire-why">Retire wallet</button>"#;

    #[test]
    fn retire_is_blocked_by_a_store_with_its_fix_beside_it() {
        let html = retire(
            RetireState::Checked {
                stores: 1,
                orders: 0,
                until: None,
            },
            &[store("s1", "Bakery", None, None)],
        );
        assert!(html.starts_with("<h2 id=\"retire-title\" class=\"dialog-title\">Retire \u{201c}Shop takings\u{201d}?<button type=\"button\" class=\"dialog-x\" aria-label=\"Close\" data-closes-dialog>×</button></h2>"), "{html}");
        assert!(html.contains("<p>Retiring deletes its keys from Monokulo, so it stops seeing payments. The money stays in your wallet app.</p>"), "{html}");
        assert!(html.contains(r#"<li class="no"><span class="mark" aria-hidden="true">✕</span><span><span class="visually-hidden">Not yet: </span>No store takes payments into it<span class="fix"><span class="fix-line">Bakery does · <a href="/dashboard/stores/s1/settings#wallet">change its wallet</a></span></span></span></li>"#), "{html}");
        assert!(html.contains(r#"<li class="ok"><span class="mark" aria-hidden="true">✓</span><span><span class="visually-hidden">Done: </span>No order on it can still be paid</span></li>"#), "{html}");
        assert!(
            html.contains("Retire becomes available when both are ticked."),
            "{html}"
        );
        assert!(html.contains(DISABLED), "{html}");
        assert!(
            html.contains(">Close</a>") && !html.contains("<form"),
            "{html}"
        );
        // A store of another account: counted, not named.
        let html = retire(
            RetireState::Checked {
                stores: 2,
                orders: 0,
                until: None,
            },
            &[],
        );
        assert!(
            html.contains(r#"<span class="fix">2 stores do</span>"#),
            "{html}"
        );
    }

    #[test]
    fn retire_is_blocked_by_orders_until_about_when() {
        let html = retire(
            RetireState::Checked {
                stores: 0,
                orders: 2,
                until: Some("9 Oct, 14:00".to_owned()),
            },
            &[],
        );
        assert!(html.contains(r#"No order on it can still be paid<span class="fix">until about 9 Oct, 14:00</span>"#), "{html}");
        assert!(html.contains(r#"<li class="ok"><span class="mark" aria-hidden="true">✓</span><span><span class="visually-hidden">Done: </span>No store takes payments into it</span></li>"#), "{html}");
        assert!(html.contains(DISABLED), "{html}");
        let html = retire(
            RetireState::Checked {
                stores: 0,
                orders: 1,
                until: None,
            },
            &[],
        );
        assert!(
            html.contains(r#"<span class="fix">1 order can</span>"#),
            "{html}"
        );
    }

    #[test]
    fn retire_waits_when_monokulo_cant_check() {
        let html = retire(RetireState::Unknown, &[]);
        assert!(html.contains(r#"Monokulo can't check right now<span class="fix"><a href="/account/wallets/w_Shop takings/retire">try again</a></span>"#), "{html}");
        assert!(
            html.contains("Retire becomes available once Monokulo can check."),
            "{html}"
        );
        assert!(html.contains(DISABLED), "{html}");
    }

    #[test]
    fn retire_is_ready_once_both_are_ticked_and_asks_for_the_name() {
        let html = retire(
            RetireState::Checked {
                stores: 0,
                orders: 0,
                until: None,
            },
            &[],
        );
        assert_eq!(html.matches(r#"<li class="ok">"#).count(), 2, "{html}");
        assert!(!html.contains(r#"class="no""#), "{html}");
        assert!(html.contains(r#"<form method="post" action="/account/wallets/w_Shop takings/retire"><div class="setting-field"><div class="setting-label-row"><label class="setting-label" for="retire-confirm">Type <strong>“Shop takings”</strong> to confirm</label></div><input type="text" id="retire-confirm" name="confirm" autocomplete="off" spellcheck="false" required></div>"#), "{html}");
        assert!(html.contains(r#"<div class="dialog-actions"><a class="btn" href="/account/wallets/w_Shop takings" data-closes-dialog>Cancel</a><button type="submit" class="btn-danger">Retire wallet</button></div>"#), "{html}");
        assert!(
            !html.contains("disabled") && !html.contains("retire-why"),
            "{html}"
        );
    }

    fn detail(retired: bool) -> String {
        let mut wallet = summary("Shop takings", "mainnet", 0).wallet;
        wallet.retired_at = retired.then_some(1_791_000_000);
        let data = DetailViewModel {
            retire: if retired {
                RetireState::Retired
            } else {
                RetireState::Checked {
                    stores: 1,
                    orders: 0,
                    until: None,
                }
            },
            restore: retired.then(|| RestoreForm {
                custody_choices: Vec::new(),
                snp_entry: None,
            }),
            wallet,
            stores: if retired {
                Vec::new()
            } else {
                vec![store("s1", "Bakery", None, None)]
            },
            past_stores: Vec::new(),
            history: vec![WalletEvent {
                when: "2 Oct, 09:40".to_owned(),
                what: html! { "Brought in" },
            }],
            error: None,
            notice: None,
            rename: None,
        };
        detail_page(&PageChrome::from_user(None, ""), &data).into_string()
    }

    /// One column: the name and its badge, where it lives, Stores, Details,
    /// History (folded), and Retire at the bottom opening its dialog.
    #[test]
    fn a_wallets_page_is_one_column_in_the_agreed_order() {
        let html = detail(false);
        assert!(html.contains(r#"<div class="wallet-title"><h1>Shop takings</h1><span class="tag-network is-main">"#), "{html}");
        let order = [
            r#"<section class="where-banner""#,
            r#"<h3 id="stores-title">Stores</h3>"#,
            r#"<h3 id="card-details-title">Details</h3>"#,
            r#"<details class="history-fold"><summary>History <span class="hint">1 event</span></summary>"#,
            r#"<div class="wallet-foot"><div><strong>Retire this wallet</strong>"#,
            r#"<dialog id="retire-dialog" class="settings-dialog wallet-dialog" aria-labelledby="retire-title">"#,
        ];
        let at: Vec<usize> = order
            .iter()
            .map(|part| {
                html.find(part)
                    .unwrap_or_else(|| panic!("{part} in {html}"))
            })
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
        assert!(
            !html.contains("wallet-layout") && !html.contains("<aside"),
            "{html}"
        );
        // No network fact, and no "real money": the badge says it.
        assert!(!html.contains("<dt>Network</dt>"), "{html}");
        assert!(!html.to_lowercase().contains("real money"), "{html}");
        assert!(
            html.contains(r#"<dt>Address</dt><dd><code><span class="mid-ellipsis""#),
            "{html}"
        );
        assert!(html.contains("<dt>Kind</dt><dd>Brought in</dd>"), "{html}");
        // The rename: the Details card's settings form.
        assert!(
            html.contains(r#"action="/account/wallets/w_Shop takings/rename""#)
                && html.contains(r#"id="wallet-name" name="name" value="Shop takings""#),
            "{html}"
        );
        assert!(html.contains("wallet-page."), "its script: {html}");
        // One orange button, the save bar's Save; the red one is Retire's.
        assert_eq!(html.matches("btn-primary").count(), 1, "{html}");
        assert!(
            html.contains(r#"class="btn-primary" data-save>Save</button>"#),
            "{html}"
        );
    }

    #[test]
    fn a_retired_wallets_page_offers_restore_at_the_bottom() {
        let html = detail(true);
        assert!(html.contains(r#"<p class="wallet-meta"><span class="tag tag-unknown">retired</span><span>Keys deleted "#), "{html}");
        assert!(
            html.contains(r#"<div class="wallet-foot"><div><strong>Restore this wallet</strong>"#),
            "{html}"
        );
        assert!(html.contains(r#"<a class="btn" href="/account/wallets/w_Shop takings/restore" data-opens-dialog="restore-dialog">Restore wallet…</a>"#), "{html}");
        let dialog = between(&html, r#"<dialog id="restore-dialog""#, "</dialog>");
        assert!(dialog.contains("<h2 id=\"restore-title\" class=\"dialog-title\">Restore \u{201c}Shop takings\u{201d}"), "{dialog}");
        assert!(
            dialog.contains(r#"name="view_key_hex""#)
                && dialog.contains(r#"name="spend_pubkey_hex""#),
            "{dialog}"
        );
        assert!(dialog.contains("Monero CLI"), "the key help: {dialog}");
        assert!(
            dialog.contains(r#"<button type="submit" class="btn-primary">Restore wallet</button>"#),
            "{dialog}"
        );
        assert!(
            !html.contains("Retire wallet") && !html.contains("btn-danger"),
            "{html}"
        );
    }

    /// The pages holding the dialogs' content, for a browser without
    /// JavaScript: the same content, with an h1 and no close button.
    #[test]
    fn the_no_javascript_pages_hold_the_dialogs_content() {
        let w = summary("Shop takings", "mainnet", 0).wallet;
        let chrome = PageChrome::from_user(None, "");
        let state = RetireState::Checked {
            stores: 0,
            orders: 0,
            until: None,
        };
        let page = retire_page(&chrome, &w, &state, &[], Some("Type it exactly.")).into_string();
        assert!(page.contains("<h1 id=\"retire-title\" class=\"dialog-title\">Retire \u{201c}Shop takings\u{201d}?</h1>"), "{page}");
        assert!(
            page.contains(r#"<p class="error" role="alert">Type it exactly.</p>"#),
            "{page}"
        );
        assert!(!page.contains("dialog-x"), "{page}");
        let content = retire_content(&w, &state, &[], None, true).into_string();
        let after_title = |html: &str| html[html.find("<p>Retiring").unwrap()..].to_owned();
        let plain = retire_page(&chrome, &w, &state, &[], None).into_string();
        assert!(
            plain.contains(&after_title(&content)),
            "the dialog's content: {plain}"
        );

        let restore = RestoreForm {
            custody_choices: Vec::new(),
            snp_entry: None,
        };
        let page = restore_page(&chrome, &w, &restore, None).into_string();
        assert!(page.contains("<h1 id=\"restore-title\" class=\"dialog-title\">Restore \u{201c}Shop takings\u{201d}</h1>"), "{page}");
        assert!(
            page.contains(
                r#"<form method="post" action="/account/wallets/w_Shop takings/restore">"#
            ),
            "{page}"
        );
    }

    /// The setup flow's network dropdown: each network as its badge.
    #[test]
    fn the_setup_network_dropdown_shows_each_networks_badge() {
        let html = network_select("stagenet").into_string();
        assert!(
            html.contains(
                r#"<option value="mainnet" data-label="" data-network="mainnet">[Mainnet]</option>"#
            ),
            "{html}"
        );
        assert!(html.contains(r#"<option value="stagenet" selected data-label="" data-network="stagenet">[Stagenet]</option>"#), "{html}");
        assert!(
            html.contains(r#"data-network="testnet">[Testnet]</option>"#),
            "{html}"
        );
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
