//! A merchant's wallets (docs/wallets.md): choosing how to set one up,
//! bringing one's own, making a new one in the browser (backing up its
//! recovery phrase and checking the backup), the list, and a wallet's page.
//! Handlers: `http::wallets`.

use maud::{html, Markup};

use super::controls::Choice;
use super::{layout, layout_with_head, PageChrome};
use crate::wallets::{AppMethod, WalletApp, WALLET_APPS};

/// Where a merchant setting up their first wallet is: the steps shown above
/// the page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    Account,
    Wallet,
    Ready,
}

pub fn setup_steps(current: SetupStep) -> Markup {
    let steps = [
        (SetupStep::Account, "Account"),
        (SetupStep::Wallet, "Wallet"),
        (SetupStep::Ready, "Ready"),
    ];
    let at = steps.iter().position(|(s, _)| *s == current).unwrap_or(0);
    html! {
        ol class="setup-steps" aria-label="Setup progress" {
            @for (i, (_, label)) in steps.iter().enumerate() {
                @if i == at {
                    li aria-current="step" { span class="n" { (i + 1) } (label) }
                } @else if i < at {
                    li class="done" { span class="n" { (i + 1) } (label) }
                } @else {
                    li { span class="n" { (i + 1) } (label) }
                }
            }
        }
    }
}

fn backup_steps(at: usize) -> Markup {
    html! {
        ol class="setup-steps" aria-label="New wallet progress" {
            @for (i, label) in ["Back up", "Check"].iter().enumerate() {
                @if i == at {
                    li aria-current="step" { span class="n" { (i + 1) } (label) }
                } @else if i < at {
                    li class="done" { span class="n" { (i + 1) } (label) }
                } @else {
                    li { span class="n" { (i + 1) } (label) }
                }
            }
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

fn back_arrow() -> Markup {
    html! {
        svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
            path d="M19 12H5M11 18l-6-6 6-6" {}
        }
    }
}

fn network_select(selected: &str) -> Markup {
    html! {
        mk-select {
            select name="network" {
                @for network in ["mainnet", "stagenet", "testnet"] {
                    (Choice::new(network, network)
                        .note(if network == "mainnet" { "real payments" } else { "test network" })
                        .selected(network == selected))
                }
            }
        }
    }
}

// -- Choosing how to set up a wallet ---------------------------------------

pub struct ChoiceViewModel {
    /// The first wallet of a new account: the setup steps are shown.
    pub onboarding: bool,
    /// The plugin's site, when setting up a wallet on the way to
    /// connecting a shop.
    pub connecting_site: Option<String>,
    /// The name it will get if none is typed.
    pub suggested_name: String,
    pub name: String,
    pub network: String,
    /// Where to go once the wallet is added (`?next=`).
    pub next: Option<String>,
}

/// `GET /dashboard/wallets/setup`: create a new wallet, bring your own, or
/// (coming soon) a hardware wallet. Creating one needs JavaScript: the card
/// is drawn unavailable, and `wallet-setup.js` turns it on.
pub fn choice_page(chrome: &PageChrome, data: &ChoiceViewModel) -> Markup {
    let body = html! {
        div class="wrap wallet-choice" {
            @if data.onboarding {
                (setup_steps(SetupStep::Wallet))
            } @else {
                nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard/wallets" { "Wallets" } }
            }
            h1 { @if data.onboarding { "Set up your wallet" } @else { "Add a wallet" } }
            @if let Some(site) = &data.connecting_site {
                p class="notice" { "This is the wallet " strong { (site) } "'s Monero payments will go to." }
            }
            p {
                "Payments go straight to your own Monero wallet. Monokulo only gets watch-only keys: it sees "
                "payments arrive and can never spend them."
            }
            form method="get" action="/dashboard/wallets/import" data-wallet-choice {
                @if let Some(next) = &data.next {
                    input type="hidden" name="next" value=(next);
                }
                label {
                    "Wallet name " span class="hint" { "(optional)" }
                    input type="text" name="name" value=(data.name) placeholder=(data.suggested_name) maxlength=(crate::wallets::MAX_NAME_LEN) autocomplete="off";
                    span class="field-help" {
                        "Leave it blank and we'll call it " strong { (data.suggested_name) } ". You can rename it any time."
                    }
                }
                input type="hidden" name="suggested" value=(data.suggested_name);
                details {
                    summary { "More options" }
                    label {
                        "Network"
                        (network_select(&data.network))
                        span class="field-help" { "Leave on mainnet unless this is a test wallet." }
                    }
                }
                div class="pick-grid three" {
                    section class="pick-card recommended unavailable" data-needs-js {
                        div class="faded" {
                            div class="pick-card-head" {
                                span class="icon-tile" aria-hidden="true" {
                                    svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true" { path d="M12 5v14M5 12h14" {} }
                                }
                                span class="tag" data-js-tag { "Unavailable" }
                            }
                            h3 { "Create a new wallet" }
                            p class="grow" {
                                "Your browser makes a 16-word recovery phrase. Scan it straight into Cake Wallet or "
                                "Stack Wallet, or write it down, then we check it."
                            }
                            p class="hint" { (lock_icon()) " The phrase is made and kept on this page. It is never sent to Monokulo." }
                            button type="submit" class="btn-primary" formaction="/dashboard/wallets/new" disabled data-create-wallet { "Create a new wallet" }
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
                        button type="submit" formaction="/dashboard/wallets/import" { "Bring your own wallet" }
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
                div class="warning" id="create-needs-js" data-needs-js-reason {
                    "Creating a new wallet needs JavaScript: its recovery phrase is made inside your browser so it "
                    "never reaches our server. Turn JavaScript on for this site and reload, or bring your own wallet."
                }
            }
            @if !data.onboarding {
                p class="hint" { "Need separate wallets for different shops? Add as many as you like, and pick one per store." }
            }
        }
        // The script fetches its module from where the page says.
        script src=(crate::assets::url("wallet-setup.js")) data-module=(crate::assets::url("wallet-setup.wasm")) defer {}
    };
    layout(chrome, "Set up your wallet - Monokulo", body)
}

// -- Bringing your own wallet ----------------------------------------------

pub struct ImportViewModel {
    pub onboarding: bool,
    pub error: Option<String>,
    pub name: String,
    pub suggested_name: String,
    pub spend_pubkey_hex: String,
    pub network: String,
    pub next: Option<String>,
    pub custody_choices: Vec<super::connect::CustodyChoice>,
    pub snp_entry: Option<super::key_entry::SnpKeyEntry>,
}

/// `GET`/`POST /dashboard/wallets/import`: "Bring your own wallet".
pub fn import_page(chrome: &PageChrome, data: &ImportViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if data.onboarding { (setup_steps(SetupStep::Wallet)) }
            nav class="context-nav" aria-label="Breadcrumb" {
                a href=(setup_link(data.next.as_deref())) { "Set up your wallet" }
            }
            h1 { "Bring your own wallet" }
            p {
                "Connect a Monero wallet you already have. Monokulo asks for two watch-only keys and never your "
                "recovery phrase or private spend key."
            }
            @if let Some(error) = &data.error {
                p class="error" role="alert" { (error) }
            }
            form method="post" action="/dashboard/wallets/import" {
                @if let Some(next) = &data.next {
                    input type="hidden" name="next" value=(next);
                }
                label {
                    "Wallet name " span class="hint" { "(optional)" }
                    input type="text" name="name" value=(data.name) placeholder=(data.suggested_name) maxlength=(crate::wallets::MAX_NAME_LEN) autocomplete="off";
                    span class="field-help" { "Shown when you pick a wallet for a store. Blank: we'll call it " strong { (data.suggested_name) } "." }
                }
                input type="hidden" name="suggested" value=(data.suggested_name);
                (super::key_entry::key_fields(
                    "",
                    &data.spend_pubkey_hex,
                    data.snp_entry.as_ref(),
                    html! { "Lets Monokulo see payments arriving. It cannot spend." },
                    html! { "The " em { "public" } " half of your spend key. Never enter the private spend key anywhere." },
                ))
                details {
                    summary { "Where do I find these keys?" }
                    dl class="facts" {
                        dt { span class="logo-row" { (app_logo("cake", 20, None)) "Cake Wallet" } }
                        dd { "Settings (gear icon), Recovery & Keys, Show my Recovery Phrase & Keys, Keys tab" }
                        dt { span class="logo-row" { (app_logo("feather", 20, None)) "Feather" } }
                        dd { "Wallet menu, Keys" }
                        dt { span class="logo-row" { (app_logo("gui", 20, None)) "Monero GUI" } }
                        dd { "Settings, Seed & keys" }
                        dt { span class="logo-row" { (app_logo("gui", 20, None)) "Monero CLI" } }
                        dd { code { "viewkey" } " and " code { "spendkey" } " (copy the public line only)" }
                    }
                }
                label {
                    "Network"
                    (network_select(&data.network))
                    span class="field-help" { "Leave on mainnet unless this is a test wallet." }
                }
                (super::connect::custody_select(&data.custody_choices))
                @if let Some(entry) = &data.snp_entry {
                    (super::key_entry::snp_section(entry, (!data.custody_choices.is_empty()).then_some("key_custody_backend")))
                }
                div class="form-actions" {
                    button type="submit" class="btn-primary" { "Add wallet" }
                    a class="btn" href=(setup_link(data.next.as_deref())) { "Back" }
                }
            }
        }
    };
    layout(chrome, "Bring your own wallet - Monokulo", body)
}

fn setup_link(next: Option<&str>) -> String {
    match next {
        Some(next) => format!("/dashboard/wallets/setup?next={}", url_encode(next)),
        None => "/dashboard/wallets/setup".to_owned(),
    }
}

// -- Making a new wallet ---------------------------------------------------

pub struct CreateViewModel {
    pub onboarding: bool,
    /// The name the wallet gets: typed, or picked now so the restore link
    /// and the pages can use it.
    pub name: String,
    pub network: String,
    /// The chain's height now, for restoring from the 25-word phrase.
    pub restore_height: Option<u64>,
    pub next: Option<String>,
    /// Set when the keys go to SEV-SNP key storage, encrypted in the page.
    pub snp_entry: Option<super::key_entry::SnpKeyEntry>,
    pub error: Option<String>,
}

/// `GET /dashboard/wallets/new`: the recovery phrase is made, backed up and
/// checked on this page (`static/wallet-setup.js` with the `wallet-setup`
/// WebAssembly module); only the watch-only keys are posted. Every screen is
/// drawn here and shown by the script.
pub fn create_page(chrome: &PageChrome, data: &CreateViewModel) -> Markup {
    let height = data
        .restore_height
        .map(|h| h.to_string())
        .unwrap_or_default();
    let body = html! {
        div class="wrap" {
            noscript {
                h1 { "Create a new wallet" }
                div class="error" role="alert" {
                    "Creating a wallet needs JavaScript: its recovery phrase is made inside your browser so it never "
                    "reaches our server. Turn JavaScript on for this site and reload, or "
                    a href=(format!("/dashboard/wallets/import{}", next_query(data.next.as_deref()))) { "bring your own wallet" } "."
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
                    (backup_steps(0))
                    h1 { "Back up " (data.name) }
                    p {
                        "Its 16-word recovery phrase is the wallet. Save it one of these ways: either is enough, both "
                        "is safest. Monokulo never sees it and can't show it again later."
                    }
                    fieldset class="backup-methods" {
                        legend class="visually-hidden" { "Backup method" }
                        label class="radio-card" {
                            input type="radio" name="backup-method" value="app" checked data-method-choice;
                            span {
                                strong { "Save it in a wallet app" } br;
                                span class="hint" { "Scan a QR code with your phone. The app keeps the phrase for you." }
                            }
                        }
                        label class="radio-card" {
                            input type="radio" name="backup-method" value="paper" data-method-choice;
                            span {
                                strong { "Write it down" } br;
                                span class="hint" { "16 words on paper, kept somewhere safe." }
                            }
                        }
                    }
                    div class="warning" {
                        "Anyone with this phrase or QR code can spend your money. Show it only where no one can see your "
                        "screen. Don't photograph it or store it online."
                    }
                    div data-method-panel="app" {
                        div class="app-tabs" role="tablist" aria-label="Wallet app" {
                            @for (i, app) in WALLET_APPS.iter().enumerate() {
                                button type="button" role="tab" id=(format!("tab-{}", app.key)) aria-controls=(format!("app-{}", app.key)) aria-selected=(if i == 0 { "true" } else { "false" }) data-app-tab=(app.key) {
                                    (app_logo(app.key, 22, None)) (app.name)
                                }
                            }
                        }
                        @for (i, app) in WALLET_APPS.iter().enumerate() {
                            (app_panel(app, i == 0, &height))
                        }
                    }
                    div data-method-panel="paper" hidden {
                        div class="box" {
                            ol class="seed-words" data-words aria-label="Recovery phrase" {}
                            dl class="facts" {
                                dt { "Phrase type" } dd { "Polyseed, 16 words" }
                                dt { "Wallet birthday" } dd { "Today " span class="hint" { "(kept in the phrase, so apps restore quickly)" } }
                            }
                            div class="form-actions" {
                                button type="button" data-toggle-words { "Hide words" }
                                button type="button" data-print { "Print a backup sheet" }
                                span class="hint" { "No copy button on purpose: other apps can read your clipboard." }
                            }
                        }
                    }
                    div class="form-actions" {
                        label class="check-line" {
                            input type="checkbox" data-backed-up;
                            span data-backed-up-label="app" { "It's open in my wallet app." }
                            span data-backed-up-label="paper" hidden { "I've written all 16 words down, in order, and put them somewhere safe." }
                        }
                    }
                    div class="form-actions" {
                        button type="button" class="btn-primary" data-go="check" disabled { "Next: check my backup" }
                        span class="spacer" {}
                        button type="button" data-go="skip" { "Skip backup" }
                    }
                }
                section data-screen="check" hidden {
                    (backup_steps(1))
                    h1 { "Check your backup" }
                    @for app in WALLET_APPS {
                        div class="box find-words" data-find=(app.key) hidden {
                            div class="logo-row" { (app_logo(app.key, 36, None)) strong { "Find the words in " (app.name) ":" } }
                            ol class="steps" { @for step in app.find_words_steps { li { (step) } } }
                        }
                    }
                    div class="box find-words" data-find="paper" hidden {
                        strong { "Get your paper copy." }
                        p { "Read each word asked for below from it." }
                    }
                    form class="box" data-check-form novalidate {
                        p { "Type these three words. This page no longer shows them, so it checks what you saved." }
                        @for i in 0..3 {
                            label class="word-check" data-word-check=(i) {
                                span data-word-label { "Word" }
                                input type="text" autocomplete="off" autocapitalize="off" spellcheck="false" class="mono";
                                span data-word-message aria-live="polite" {}
                            }
                        }
                        p class="hint" { (lock_icon()) " When all three match, your browser works out the wallet's watch-only keys and sends only those to Monokulo." }
                        div class="form-actions" {
                            button type="button" data-go="backup" { (back_arrow()) " Back to my backup" }
                            span class="spacer" {}
                            button type="submit" class="btn-primary" { "Check and add wallet" }
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
                        button type="button" class="btn-primary" data-go="backup" { (back_arrow()) " Go back and back up" }
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
                    p { a href=(format!("/dashboard/wallets/import{}", next_query(data.next.as_deref()))) { "Bring your own wallet" } " instead." }
                }
                form method="post" action="/dashboard/wallets/new" data-register hidden {
                    input type="hidden" name="name" value=(data.name);
                    input type="hidden" name="network" value=(data.network);
                    input type="hidden" name="backup" data-field="backup";
                    input type="hidden" name="primary_address" data-field="address";
                    input type="hidden" name="view_key_hex" data-field="view" data-key-custody="view";
                    input type="hidden" name="spend_pubkey_hex" data-field="spend" data-key-custody="spend";
                    @if let Some(next) = &data.next {
                        input type="hidden" name="next" value=(next);
                    }
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

fn next_query(next: Option<&str>) -> String {
    next.map(|n| format!("?next={}", url_encode(n)))
        .unwrap_or_default()
}

fn app_panel(app: &WalletApp, shown: bool, height: &str) -> Markup {
    html! {
        div class="app-panel" id=(format!("app-{}", app.key)) role="tabpanel" aria-labelledby=(format!("tab-{}", app.key)) data-app-panel=(app.key) hidden[!shown] {
            div class="app-steps" {
                p { span class=(if app.method.is_scanned() { "tag tag-ok" } else if app.method == AppMethod::TypeWords { "tag tag-unknown" } else { "tag tag-slow" }) { (app.method.label()) } }
                p class="hint" { (app.platforms) }
                ol class="steps" { @for step in app.restore_steps { li { (step) } } }
            }
            aside class="box qr-panel" {
                @match app.method {
                    AppMethod::ScanRestoreLink | AppMethod::ScanWordList => {
                        div class="qr-hidden" data-qr-cover { "This code holds your whole recovery phrase." }
                        div class="qr-frame" data-qr=(app.method.qr_kind()) hidden {}
                        p class="hint" data-qr-caption hidden {
                            @if app.method == AppMethod::ScanRestoreLink {
                                "Restore link: the phrase, its birthday and the wallet's name."
                            } @else {
                                "The 16 words, in the format Stack Wallet reads."
                            }
                        }
                        button type="button" class="btn-primary" data-show-qr { "Show QR code" }
                        button type="button" data-hide-qr hidden { "Hide QR code" }
                    }
                    AppMethod::TypeWords => {
                        p { strong { "Feather has no QR restore for a full wallet. Type the words in." } }
                        button type="button" data-show-paper { "Show the 16 words" }
                    }
                    AppMethod::TypeLegacyWords => {
                        p { strong { "These apps only read 25-word phrases. The same wallet can be shown in that older format." } }
                        div data-legacy hidden {
                            ol class="seed-words legacy" data-legacy-words aria-label="25-word phrase" {}
                            dl class="facts" {
                                dt { "Restore height" }
                                dd { @if height.is_empty() { "the block height on the wallet's birthday" } @else { (height) } }
                            }
                        }
                        button type="button" data-show-legacy { "Show the 25-word version" }
                    }
                }
            }
        }
    }
}

// -- Added -----------------------------------------------------------------

pub struct ReadyViewModel {
    pub onboarding: bool,
    pub wallet: crate::db::WalletRow,
    /// Where the merchant was going (`?next=`), and what it is, for the button.
    pub next: Option<(String, String)>,
    pub skipped_backup: bool,
}

pub fn ready_page(chrome: &PageChrome, data: &ReadyViewModel) -> Markup {
    let w = &data.wallet;
    let body = html! {
        div class="wrap" {
            @if data.onboarding { (setup_steps(SetupStep::Ready)) }
            h1 { @if data.onboarding { "You're ready to take payments" } @else { (w.name) " is added" } }
            @if data.skipped_backup {
                p class="warning" { "This wallet's recovery phrase was not backed up. Payments to it can't be spent unless you have it." }
            }
            div class="box" {
                h2 { (w.name) }
                dl class="facts" {
                    dt { "Address" } dd { code { (short_address(&w.primary_address)) } }
                    dt { "Network" } dd { (w.network) }
                    dt { "Kind" } dd { (origin_label(w)) }
                }
            }
            @match &data.next {
                Some((path, label)) => {
                    p { a class="btn btn-primary" href=(path) { (label) } }
                }
                None => {
                    p { "Connect a store to it next." }
                    p { a class="btn btn-primary" href="/dashboard/stores/new" { "Add a store" } }
                }
            }
        }
    };
    layout(chrome, "Wallet added - Monokulo", body)
}

pub fn short_address(address: &str) -> String {
    if address.len() > 12 {
        format!("{}…{}", &address[..5], &address[address.len() - 4..])
    } else {
        address.to_owned()
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

/// `GET /dashboard/wallets`: read-only; a wallet's page changes it.
pub fn list_page(
    chrome: &PageChrome,
    wallets: &[WalletListItem],
    retired: &[RetiredListItem],
) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Wallets" }
            p { "Where your stores' payments go. Monokulo holds watch-only keys for each." }
            @if wallets.is_empty() {
                p class="notice" { "No wallets yet." }
            } @else {
                div class="table-scroll" {
                    table {
                        thead { tr { th { "Name" } th { "Kind" } th { "Address" } th { "Network" } th { "Stores" } } }
                        tbody {
                            @for w in wallets {
                                tr {
                                    td { a href=(format!("/dashboard/wallets/{}", w.id)) { (w.name) } }
                                    td { (w.kind) }
                                    td { code { (w.address) } }
                                    td { @if w.network == "mainnet" { (w.network) } @else { span class="tag tag-slow" { (w.network) } } }
                                    td { (w.stores) }
                                }
                            }
                        }
                    }
                }
                p class="hint" { "Open a wallet to rename it, see its history or retire it." }
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
                                        td { a href=(format!("/dashboard/wallets/{}", w.id)) { (w.name) } }
                                        td { (w.retired) }
                                        td { span class="keys-deleted" { (key_gone_icon(16)) " Deleted" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            p { a class="btn btn-primary" href="/dashboard/wallets/setup" { "+ add a wallet" } }
        }
    };
    layout(chrome, "Wallets - Monokulo", body)
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
    pub name_field: String,
}

pub fn detail_page(chrome: &PageChrome, data: &DetailViewModel) -> Markup {
    let w = &data.wallet;
    let in_use = !data.stores.is_empty();
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard/wallets" { "Wallets" } }
            h1 { (w.name) }
            p class="hint" { (origin_label(w)) " · " (w.network) }
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
                    section class="box" {
                        h2 { "Details" }
                        form class="rename-form" method="post" action=(format!("/dashboard/wallets/{}/rename", w.id)) {
                            label { "Name" input type="text" name="name" value=(data.name_field) maxlength=(crate::wallets::MAX_NAME_LEN) required; }
                            button type="submit" { "Rename" }
                        }
                        dl class="facts" {
                            dt { "Address" } dd { code { (w.primary_address) } }
                            dt { "Network" } dd { (w.network) }
                            dt { "Kind" } dd { (origin_label(w)) }
                            @if let Some(at) = w.retired_at {
                                dt { "Keys" } dd { "Deleted " (chrome.clock.time(at)) }
                            }
                        }
                    }
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
                form method="post" action=(format!("/dashboard/wallets/{}/retire", w.id)) {
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
            form method="post" action=(format!("/dashboard/wallets/{}/restore", w.id)) {
                (super::key_entry::key_fields(
                    "",
                    "",
                    restore.snp_entry.as_ref(),
                    html! { "Lets Monokulo see payments arriving. It cannot spend." },
                    html! { "The " em { "public" } " half of your spend key." },
                ))
                (super::connect::custody_select(&restore.custody_choices))
                @if let Some(entry) = &restore.snp_entry {
                    (super::key_entry::snp_section(entry, (!restore.custody_choices.is_empty()).then_some("key_custody_backend")))
                }
                button type="submit" { "Bring back " (w.name) }
            }
        }
    }
}

pub fn wallet_select(wallets: &[crate::db::WalletSummary], selected: Option<&str>) -> Markup {
    let only = (wallets.len() == 1).then(|| wallets[0].wallet.id.as_str());
    let selected = selected.or(only);
    html! {
        label {
            "Wallet"
            mk-select {
                select name="wallet_id" required {
                    @if selected.is_none() {
                        (Choice::prompt("Choose a wallet…", true))
                    }
                    @for w in wallets {
                        (Choice::new(&w.wallet.id, &w.wallet.name)
                            .detail(short_address(&w.wallet.primary_address))
                            .note(wallet_note(w))
                            .selected(selected == Some(w.wallet.id.as_str())))
                    }
                }
            }
            span class="field-help" {
                @if wallets.len() == 1 {
                    "Your only wallet, picked for you. The store uses its network."
                } @else {
                    "You have " (wallets.len()) " wallets, so pick one. The store uses its network."
                }
            }
        }
    }
}

/// Under a wallet in a picker: how many stores use it, and its network
/// when that isn't mainnet.
fn wallet_note(w: &crate::db::WalletSummary) -> String {
    if w.wallet.network == "mainnet" {
        stores_label(w.store_count)
    } else {
        format!("{} · {}", w.wallet.network, stores_label(w.store_count))
    }
}

fn stores_label(count: u64) -> String {
    match count {
        0 => "no stores".to_owned(),
        1 => "1 store".to_owned(),
        n => format!("{n} stores"),
    }
}

/// The links under a wallet picker, to add one on the way.
pub fn add_wallet_links(next: &str) -> Markup {
    let next = url_encode(next);
    html! {
        p class="hint" {
            "Or " a href=(format!("/dashboard/wallets/new?next={next}")) { "create a new wallet" }
            " or " a href=(format!("/dashboard/wallets/import?next={next}")) { "bring your own" }
            ". You'll come back here with it ready to pick."
        }
    }
}

/// `text` as a query string value.
fn url_encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
