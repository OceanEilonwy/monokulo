//! `GET /account` - the Account page (`http::account`): a tab each for the
//! profile (email, theme, time zone, language), wallets and security.
//!
//! Its settings are the settings components (`views::settings`): cards in
//! one form, one bottom save bar and a toast. Saving posts the tab's form
//! and reloads the page (the theme, the email and the zone are in the nav
//! too), so it works the same with or without JavaScript. An email change
//! asks first: a dialog with JavaScript (`static/account.js`), a page of its
//! own without ([`email_confirm_page`]).

use maud::{html, Markup};

use super::controls::Choice;
use super::settings::{toast_region, Field, Save, Toast, ToastKind};
use super::wallets::{RetiredListItem, WalletListItem};
use super::{layout, script, Load, PageChrome};
use crate::db::Theme;

/// The page's tabs, as `?tab=` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountTab {
    #[default]
    Profile,
    Wallets,
    Security,
}

impl AccountTab {
    pub const ALL: [AccountTab; 3] = [
        AccountTab::Profile,
        AccountTab::Wallets,
        AccountTab::Security,
    ];

    /// The tab `id` names, Profile for none or one that doesn't exist.
    pub fn from_id(id: Option<&str>) -> Self {
        AccountTab::ALL
            .into_iter()
            .find(|tab| Some(tab.id()) == id)
            .unwrap_or_default()
    }

    pub fn id(self) -> &'static str {
        match self {
            AccountTab::Profile => "profile",
            AccountTab::Wallets => "wallets",
            AccountTab::Security => "security",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AccountTab::Profile => "Profile",
            AccountTab::Wallets => "Wallets",
            AccountTab::Security => "Security",
        }
    }

    pub fn href(self) -> String {
        match self {
            AccountTab::Profile => "/account".to_string(),
            tab => format!("/account?tab={}", tab.id()),
        }
    }
}

/// A card of settings on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Email,
    Appearance,
    Time,
    Password,
}

impl Card {
    pub const ALL: [Card; 4] = [Card::Email, Card::Appearance, Card::Time, Card::Password];

    /// Its name in `data-card` and in the `saved=` a save redirects with.
    pub fn id(self) -> &'static str {
        match self {
            Card::Email => "email",
            Card::Appearance => "appearance",
            Card::Time => "time",
            Card::Password => "password",
        }
    }

    pub fn from_id(id: &str) -> Option<Card> {
        Card::ALL.into_iter().find(|card| card.id() == id)
    }

    /// Its element id: the account menu links to `card-time`.
    pub fn card_id(self) -> String {
        super::settings::card_id(self.id())
    }

    fn title(self) -> &'static str {
        match self {
            Card::Email => "Email",
            Card::Appearance => "Appearance",
            Card::Time => "Language and time",
            Card::Password => "Password",
        }
    }
}

/// What the save the page answers did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing on the tab had changed.
    Unchanged,
    /// These cards' changes were saved.
    Saved(Vec<Card>),
    /// Nothing was saved, because of `card`.
    Refused { card: Card, message: String },
}

/// The profile's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub email: String,
    pub theme: Theme,
    /// A zone name, or `None` for automatic.
    pub timezone: Option<String>,
}

pub struct AccountViewModel {
    pub tab: AccountTab,
    /// The profile as it is saved.
    pub saved: Profile,
    /// The profile as the form shows it: as saved, or what a refused save
    /// sent.
    pub shown: Profile,
    /// The browser's own zone (the `tz` cookie), for Automatic's note.
    pub browser_zone: Option<String>,
    pub outcome: Option<Outcome>,
    /// The Wallets tab's lists.
    pub wallets: Vec<WalletListItem>,
    pub retired: Vec<RetiredListItem>,
}

impl AccountViewModel {
    fn failure(&self, card: Card) -> Option<&str> {
        match &self.outcome {
            Some(Outcome::Refused {
                card: refused,
                message,
            }) if *refused == card => Some(message),
            _ => None,
        }
    }

    fn refused(&self) -> bool {
        matches!(self.outcome, Some(Outcome::Refused { .. }))
    }

    fn was_saved(&self, card: Card) -> bool {
        matches!(&self.outcome, Some(Outcome::Saved(cards)) if cards.contains(&card))
    }

    /// The toast the save leaves.
    fn toast(&self) -> Option<Toast> {
        let toast = |kind, title: &str, lines: Vec<String>| Toast {
            lines,
            ..Toast::new(kind, title)
        };
        Some(match self.outcome.as_ref()? {
            Outcome::Unchanged => toast(
                ToastKind::Neutral,
                "Nothing to save",
                vec!["Nothing had changed.".to_string()],
            ),
            Outcome::Saved(cards) if cards.contains(&Card::Password) => toast(
                ToastKind::Success,
                "Password changed",
                vec!["Every other session was logged out.".to_string()],
            ),
            Outcome::Saved(cards) => toast(
                ToastKind::Success,
                "Profile saved",
                if cards.contains(&Card::Email) {
                    vec![format!("You log in with {} from now on.", self.saved.email)]
                } else {
                    Vec::new()
                },
            ),
            Outcome::Refused { message, .. } => {
                toast(ToastKind::Error, "Not saved", vec![message.clone()])
            }
        })
    }
}

/// The theme as three radio cards with a small picture of each: the
/// Appearance card's, and the sign-up form's. `saved` is the theme as
/// saved, for a form that marks unsaved changes.
pub fn theme_choice(selected: Theme, saved: Option<Theme>, help: &str) -> Markup {
    Field::group("Theme", "theme-label")
        .help(None, html! { (help) })
        .render(html! {
            div class="themes" {
                @for (theme, label) in [(Theme::System, "System"), (Theme::Light, "Light"), (Theme::Dark, "Dark")] {
                    label class="radio-card" {
                        input type="radio" name="theme" value=(theme.as_str()) checked[theme == selected]
                            data-saved=[saved.map(|saved| if saved == theme { "on" } else { "off" })];
                        span class=(format!("mini-page {}", theme.as_str())) aria-hidden="true" { span class="bar" {} }
                        (label)
                    }
                }
            }
        })
}

/// A card as the last save left it: "Not saved" and why after a refused
/// save, "Saved" after a save.
fn card(data: &AccountViewModel, card: Card, body: Markup) -> Markup {
    let failure = data.failure(card);
    super::settings::Card::new(card.id(), card.title())
        .failed(failure.is_some(), failure)
        .saved(data.was_saved(card).then_some(""), false)
        .render(body)
}

/// The save bar: what saving does, or why the last save didn't.
fn save_bar(data: &AccountViewModel, about: &str) -> Markup {
    let message = html! {
        @match &data.outcome {
            Some(Outcome::Refused { card, message }) => {
                strong { "Nothing saved." } " " (message) " "
                a href=(format!("#{}", card.card_id())) data-show-card=(card.id()) { "Show" }
            }
            _ => (about),
        }
    };
    super::settings::save_bar(data.refused(), false, message, &data.tab.href())
}

fn profile_panel(data: &AccountViewModel) -> Markup {
    let refused = data.refused();
    let (saved, shown) = (&data.saved, &data.shown);
    let automatic = match &data.browser_zone {
        Some(zone) => Choice::new("", "Automatic").note(format!("this browser: {zone}")),
        None => Choice::new("", "Automatic").note("this browser's zone, UTC until it's known"),
    };
    super::settings::form(
        "/account/profile",
        Save::Reload,
        "Profile",
        html! {
            input type="hidden" name="tab" value="profile";
            (card(data, Card::Email, Field::new("Email", "email")
                .help(None, html! { "What you log in with." })
                .render(html! {
                    input type="email" id="email" name="email" value=(shown.email) required autocomplete="email"
                        data-email-input data-saved=[refused.then_some(&saved.email)];
                })))
            (card(data, Card::Appearance, theme_choice(shown.theme, refused.then_some(saved.theme), "The account menu changes it too.")))
            (card(data, Card::Time, html! {
                (Field::new("Time zone", "timezone")
                    .help(None, html! { "Every date and time on the site is shown in this zone. Automatic follows this browser." })
                    .render(html! {
                        mk-select {
                            select id="timezone" name="timezone" data-saved=[refused.then(|| saved.timezone.clone().unwrap_or_default())] {
                                (automatic.selected(shown.timezone.is_none()))
                                @for name in super::time::zone_names() {
                                    @let selected = shown.timezone.as_deref() == Some(name.as_str());
                                    (Choice::new(&name, &name).selected(selected))
                                }
                            }
                        }
                    }))
                (Field::new("Language", "language")
                    .help(None, html! { "Only English for now: more languages come with translation." })
                    .render(html! {
                        mk-select {
                            select id="language" disabled { (Choice::new("en", "English").selected(true)) }
                        }
                    }))
            }))
            (email_confirm_dialog(saved))
            (save_bar(data, "Saving changes your profile straight away."))
        },
    )
}

/// The question an email change asks before it's saved, with JavaScript
/// (`static/account.js` opens it on Save). Its box is sent with the form:
/// empty unless the dialog filled it in.
fn email_confirm_dialog(saved: &Profile) -> Markup {
    html! {
        dialog id="email-confirm" class="settings-dialog email-confirm" aria-labelledby="email-confirm-title" {
            h2 id="email-confirm-title" { "Log in with a new email?" }
            p { "From now on you log in with " strong data-email-new {} " instead of " (saved.email) "." }
            p class="hint" { "Monokulo doesn't send a confirmation email, so check it's typed right: a mistake here could lock you out." }
            div class="setting-field" {
                div class="setting-label-row" { label class="setting-label" for="email-again" { "Type the new email again" } }
                input type="email" id="email-again" name="confirm_email" autocomplete="off" data-email-again;
                p class="field-help setting-problem" data-email-mismatch hidden { "That isn't the same email." }
            }
            div class="form-actions dialog-actions" {
                span class="spacer" {}
                button type="button" data-email-cancel { "Cancel" }
                button type="button" class="btn-primary" data-email-go { "Change email" }
            }
        }
    }
}

fn security_panel(data: &AccountViewModel) -> Markup {
    let min = crate::http::MIN_PASSWORD_LEN;
    super::settings::form(
        "/account/password",
        Save::Reload,
        "Security",
        html! {
            input type="hidden" name="tab" value="security";
            (card(data, Card::Password, html! {
                (Field::new("Current password", "current-password").render(html! {
                    input type="password" id="current-password" name="current_password" autocomplete="current-password";
                }))
                (Field::new("New password", "new-password")
                    .help(None, html! { "At least " (min) " characters. Changing it logs you out everywhere else." })
                    .render(html! {
                        input type="password" id="new-password" name="new_password" minlength=(min) autocomplete="new-password";
                    }))
            }))
            (save_bar(data, "Saving changes your password."))
        },
    )
}

pub fn account_page(chrome: &PageChrome, data: &AccountViewModel) -> Markup {
    let tab = data.tab;
    let toast = data.toast();
    let body = html! {
        div class="wrap settings-page" {
            h1 { "Account" }
            nav id="settings-tabs" class="tab-bar" aria-label="Account" {
                @for each in AccountTab::ALL {
                    a href=(each.href()) aria-current=[(each == tab).then_some("page")] { (each.label()) }
                }
            }
            section id="settings-panel" aria-labelledby="settings-panel-title" data-tab=(tab.id()) data-tab-label=(tab.label()) {
                h2 id="settings-panel-title" class="visually-hidden" { (tab.label()) }
                @match tab {
                    AccountTab::Profile => (profile_panel(data)),
                    AccountTab::Wallets => (super::wallets::list_section(&data.wallets, &data.retired)),
                    AccountTab::Security => (security_panel(data)),
                }
            }
            (toast_region(toast.as_ref(), false))
            // The email question: it holds back a save before the settings
            // components (loaded by every page) count it as sent.
            (script("account.js", Load::Now))
        }
    };
    layout(
        chrome,
        &format!("{} - Account - Monokulo", tab.label()),
        body,
    )
}

/// Without JavaScript, an email change answers with this page first: the
/// new address and the old, why to check it, and the new one typed again.
/// It posts the whole profile again, with `confirm_email`.
pub fn email_confirm_page(
    chrome: &PageChrome,
    old_email: &str,
    submitted: &Profile,
    error: Option<&str>,
) -> Markup {
    let body = html! {
        div class="wrap settings-page email-confirm-page" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/account" { "Account" } }
            h1 { "Log in with a new email?" }
            @if let Some(error) = error {
                p class="error" role="alert" { (error) }
            }
            p { "From now on you log in with " strong { (submitted.email) } " instead of " (old_email) "." }
            p class="hint" { "Monokulo doesn't send a confirmation email, so check it's typed right: a mistake here could lock you out." }
            form method="post" action="/account/profile" {
                input type="hidden" name="tab" value="profile";
                input type="hidden" name="email" value=(submitted.email);
                input type="hidden" name="theme" value=(submitted.theme.as_str());
                input type="hidden" name="timezone" value=(submitted.timezone.as_deref().unwrap_or(""));
                div class="setting-field" {
                    div class="setting-label-row" { label class="setting-label" for="email-again" { "Type the new email again" } }
                    input type="email" id="email-again" name="confirm_email" required autocomplete="off" autofocus;
                }
                div class="form-actions" {
                    button type="submit" class="btn-primary" { "Change email" }
                    a class="btn" href="/account" { "Cancel" }
                }
            }
        }
    };
    layout(
        chrome,
        "Log in with a new email? - Account - Monokulo",
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/account")
    }

    fn profile() -> Profile {
        Profile {
            email: "ann@example.org".to_string(),
            theme: Theme::Dark,
            timezone: Some("Australia/Perth".to_string()),
        }
    }

    fn view(tab: AccountTab, outcome: Option<Outcome>) -> AccountViewModel {
        AccountViewModel {
            tab,
            saved: profile(),
            shown: profile(),
            browser_zone: Some("Europe/London".to_string()),
            outcome,
            wallets: Vec::new(),
            retired: Vec::new(),
        }
    }

    #[test]
    fn tabs_are_named_by_their_query_and_profile_is_the_default() {
        assert_eq!(AccountTab::from_id(None), AccountTab::Profile);
        assert_eq!(AccountTab::from_id(Some("nope")), AccountTab::Profile);
        assert_eq!(AccountTab::from_id(Some("security")), AccountTab::Security);
        assert_eq!(AccountTab::Profile.href(), "/account");
        assert_eq!(AccountTab::Wallets.href(), "/account?tab=wallets");
        let html = account_page(&chrome(), &view(AccountTab::Wallets, None)).into_string();
        assert!(
            html.contains("<title>Wallets - Account - Monokulo</title>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<nav id="settings-tabs" class="tab-bar" aria-label="Account"><a href="/account">Profile</a><a href="/account?tab=wallets" aria-current="page">Wallets</a><a href="/account?tab=security">Security</a></nav>"#),
            "{html}"
        );
        assert!(html.contains("No wallets yet."), "{html}");
        assert!(
            !html.contains("save-bar"),
            "the wallets tab saves nothing: {html}"
        );
    }

    /// The profile's cards, as the admin page's: each a settings card in
    /// one settings form with one save bar.
    #[test]
    fn the_profile_tab_has_email_appearance_and_language_and_time_cards_and_one_save_bar() {
        let html = account_page(&chrome(), &view(AccountTab::Profile, None)).into_string();
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {html}"))
        };
        let form = at(
            r#"<mk-settings-form label="Profile"><form method="post" action="/account/profile" id="settings-form">"#,
        );
        let email = at(r#"name="email" role="region""#);
        let appearance = at(r#"name="appearance" role="region""#);
        let time = at(r#"<mk-settings-card id="card-time""#);
        let bar = at(r#"<mk-save-bar id="save-bar" class="save-bar""#);
        assert!(
            form < email && email < appearance && appearance < time && time < bar,
            "{html}"
        );
        assert_eq!(
            html.matches("btn-primary").count(),
            2,
            "Save, and the dialog's Change email: {html}"
        );
        assert!(html.contains(r#"<input type="email" id="email" name="email" value="ann@example.org" required autocomplete="email" data-email-input>"#), "{html}");
        assert!(
            html.contains(r#"<input type="radio" name="theme" value="dark" checked>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="Australia/Perth" selected>"#),
            "{html}"
        );
        assert!(
            html.contains("Automatic - this browser: Europe/London"),
            "{html}"
        );
        assert!(html.contains(r#"<select id="language" disabled><option value="en" selected>English</option></select>"#), "{html}");
        assert!(
            html.contains("more languages come with translation"),
            "{html}"
        );
        assert!(html.contains(r#"<dialog id="email-confirm""#), "{html}");
        // The email question runs as it's reached; the settings
        // components, deferred, after it.
        assert!(
            html.contains(&format!(
                r#"<script src="{}"></script>"#,
                crate::assets::url("account.js")
            )),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<script src="{}" defer></script>"#,
                crate::assets::url("settings-form.js")
            )),
            "{html}"
        );
    }

    #[test]
    fn a_refused_save_marks_its_card_and_keeps_what_was_sent_against_what_is_saved() {
        let mut data = view(
            AccountTab::Profile,
            Some(Outcome::Refused {
                card: Card::Email,
                message: "That email is already used by another account.".to_string(),
            }),
        );
        data.shown.email = "taken@example.org".to_string();
        let html = account_page(&chrome(), &data).into_string();
        assert!(
            html.contains(
                r#"<mk-settings-card id="card-email" class="settings-card is-failed" name="email""#
            ),
            "{html}"
        );
        assert!(html.contains(r#"value="taken@example.org" required autocomplete="email" data-email-input data-saved="ann@example.org">"#), "{html}");
        assert!(
            html.contains(
                r#"<input type="radio" name="theme" value="dark" checked data-saved="on">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<select id="timezone" name="timezone" data-saved="Australia/Perth">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<mk-save-bar id="save-bar" class="save-bar is-failed""#),
            "{html}"
        );
        assert!(html.contains(r##"<strong>Nothing saved.</strong> That email is already used by another account. <a href="#card-email" data-show-card="email">Show</a>"##), "{html}");
        assert!(
            html.contains(r#"<div class="toast toast-error" role="alert" data-toast>"#),
            "{html}"
        );
    }

    #[test]
    fn a_save_says_what_it_saved() {
        let html = account_page(
            &chrome(),
            &view(
                AccountTab::Profile,
                Some(Outcome::Saved(vec![Card::Email, Card::Time])),
            ),
        )
        .into_string();
        assert!(html.contains("<strong>Profile saved</strong><span class=\"toast-line\">You log in with ann@example.org from now on.</span>"), "{html}");
        assert_eq!(html.matches("data-card-saved>Saved<").count(), 2, "{html}");

        let html = account_page(
            &chrome(),
            &view(
                AccountTab::Security,
                Some(Outcome::Saved(vec![Card::Password])),
            ),
        )
        .into_string();
        assert!(html.contains("<strong>Password changed</strong><span class=\"toast-line\">Every other session was logged out.</span>"), "{html}");

        let html = account_page(
            &chrome(),
            &view(AccountTab::Security, Some(Outcome::Unchanged)),
        )
        .into_string();
        assert!(html.contains("<strong>Nothing to save</strong>"), "{html}");
    }

    #[test]
    fn the_security_tab_asks_for_the_current_password_and_a_new_one() {
        let html = account_page(&chrome(), &view(AccountTab::Security, None)).into_string();
        assert!(
            html.contains(r#"<form method="post" action="/account/password" id="settings-form">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="current_password" autocomplete="current-password">"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"name="new_password" minlength="{}" autocomplete="new-password">"#,
                crate::http::MIN_PASSWORD_LEN
            )),
            "{html}"
        );
        assert!(
            html.contains("Changing it logs you out everywhere else."),
            "{html}"
        );
        assert!(!html.contains("session list"), "{html}");
    }

    #[test]
    fn without_javascript_an_email_change_asks_on_a_page_of_its_own() {
        let submitted = Profile {
            email: "new@example.org".to_string(),
            ..profile()
        };
        let html = email_confirm_page(
            &chrome(),
            "ann@example.org",
            &submitted,
            Some("That isn't the same email."),
        )
        .into_string();
        assert!(html.contains("<h1>Log in with a new email?</h1>"), "{html}");
        assert!(html.contains("From now on you log in with <strong>new@example.org</strong> instead of ann@example.org."), "{html}");
        assert!(html.contains("doesn't send a confirmation email"), "{html}");
        for hidden in [
            r#"<input type="hidden" name="email" value="new@example.org">"#,
            r#"<input type="hidden" name="theme" value="dark">"#,
            r#"<input type="hidden" name="timezone" value="Australia/Perth">"#,
        ] {
            assert!(html.contains(hidden), "{hidden} in {html}");
        }
        assert!(html.contains(r#"name="confirm_email" required"#), "{html}");
        assert!(
            html.contains(r#"<p class="error" role="alert">That isn't the same email.</p>"#),
            "{html}"
        );
    }

    #[test]
    fn the_theme_choice_previews_each_theme_and_marks_the_chosen_one() {
        let html = theme_choice(
            Theme::System,
            None,
            "You can change it any time from the account menu.",
        )
        .into_string();
        assert!(html.contains(r#"<input type="radio" name="theme" value="system" checked><span class="mini-page system" aria-hidden="true"><span class="bar"></span></span>System</label>"#), "{html}");
        assert!(
            html.contains(r#"value="light"><span class="mini-page light""#),
            "{html}"
        );
        assert!(
            html.contains(r#"value="dark"><span class="mini-page dark""#),
            "{html}"
        );
    }
}
