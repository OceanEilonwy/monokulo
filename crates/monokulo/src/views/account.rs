//! `GET /account` - the Account page (`http::account`): a tab each for the
//! profile (email, theme, time zone, language, password) and wallets.
//!
//! Its settings are the settings components (`views::settings`): cards in
//! one form, one bottom save bar and a toast. Saving posts the tab's form
//! and reloads the page (the theme, the email and the zone are in the nav
//! too), so it works the same with or without JavaScript. An email change
//! asks first: a dialog with JavaScript (`static/account.js`), a page of its
//! own without ([`email_confirm_page`]).

use maud::{html, Markup};

use super::settings::{toast_region, Field, Save, Toast, ToastKind};
use super::wallets::{RetiredListItem, WalletListItem};
use super::{script, Load, PageChrome};
use crate::db::Theme;
use ui::{Choice, Select};

/// The page's tabs, as `?tab=` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountTab {
    #[default]
    Profile,
    Wallets,
}

impl AccountTab {
    pub const ALL: [AccountTab; 2] = [AccountTab::Profile, AccountTab::Wallets];

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
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AccountTab::Profile => "Profile",
            AccountTab::Wallets => "Wallets",
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
            Outcome::Saved(cards) => {
                let mut lines = Vec::new();
                if cards.contains(&Card::Email) {
                    lines.push(format!("You log in with {} from now on.", self.saved.email));
                }
                if cards.contains(&Card::Password) {
                    lines.push("Every other session was logged out.".to_string());
                }
                let title = if cards == &[Card::Password] {
                    "Password changed"
                } else {
                    "Profile saved"
                };
                toast(ToastKind::Success, title, lines)
            }
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
            (ui::RadioCards::new("theme", [(Theme::System, "System"), (Theme::Light, "Light"), (Theme::Dark, "Dark")]
                .into_iter()
                .map(|(theme, label)| ui::RadioCard {
                    value: theme.as_str().into(),
                    label: label.into(),
                    picture: Some(html! { span class=(format!("mini-page {}", theme.as_str())) aria-hidden="true" { span class="bar" {} } }.into()),
                    checked: theme == selected,
                    saved: saved.map(|saved| ui::Saved::from(saved == theme)),
                })
                .collect::<Vec<_>>())
                .compact(true))
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
                    (ui::Input::new("email")
                        .id("email")
                        .kind(ui::InputType::Email)
                        .value(shown.email.clone())
                        .required(true)
                        .autocomplete("email")
                        .attr("data-email-input", "")
                        .maybe_attr("data-saved", refused.then(|| saved.email.clone())))
                })))
            (card(data, Card::Appearance, theme_choice(shown.theme, refused.then_some(saved.theme), "The account menu changes it too.")))
            (card(data, Card::Time, html! {
                (Field::new("Time zone", "timezone")
                    .help(None, html! { "Every date and time on the site is shown in this zone. Automatic follows this browser." })
                    .render(html! {
                        (Select::new(
                            "timezone",
                            std::iter::once(automatic.selected(shown.timezone.is_none()))
                                .chain(super::time::zone_names().into_iter().map(|name| {
                                    let selected = shown.timezone.as_deref() == Some(name.as_str());
                                    Choice::new(&name, &name).selected(selected)
                                }))
                                .collect::<Vec<_>>(),
                        )
                        .id("timezone")
                        .maybe_attr("data-saved", refused.then(|| saved.timezone.clone().unwrap_or_default())))
                    }))
                (Field::new("Language", "language")
                    .help(None, html! { "Only English for now: more languages come with translation." })
                    .render(html! {
                        (Select::new("language", vec![Choice::new("en", "English").selected(true)])
                            .id("language")
                            .disabled(true))
                    }))
            }))
            (password_card(data))
            (email_confirm_dialog(saved))
            (save_bar(data, "Saving changes your profile straight away."))
        },
    )
}

/// Why to check a new email, on the question and its page.
const CHECK_THE_EMAIL: &str =
    "Monokulo doesn't send a confirmation email, so check it's typed right: a mistake here could lock you out.";

/// The question an email change asks before it's saved, with JavaScript
/// (`static/account.js` opens it on Save). Its box is sent with the form:
/// empty unless the dialog filled it in.
fn email_confirm_dialog(saved: &Profile) -> Markup {
    html! {
        (ui::Dialog::new("email-confirm", "email-confirm-title", html! {
            (ui::DialogTitle::new("email-confirm-title", "Log in with a new email?"))
            p { "From now on you log in with " strong data-email-new {} " instead of " (saved.email) "." }
            (ui::Hint::new(CHECK_THE_EMAIL))
            div class="setting-field" {
                div class="setting-label-row" { label class="setting-label" for="email-again" { "Type the new email again" } }
                (ui::Input::new("confirm_email")
                    .id("email-again")
                    .kind(ui::InputType::Email)
                    .autocomplete("off")
                    .attr("data-email-again", ""))
                (ui::FieldNote::new("That isn't the same email.").tone(ui::NoteTone::Problem).flag("data-email-mismatch").flag("hidden"))
            }
            (ui::DialogActions::new(ui::Slot::many([
                ui::Button::new("Cancel").attr("data-email-cancel", "").into(),
                ui::Button::new("Change email")
                    .variant(ui::ButtonVariant::Primary)
                    .attr("data-email-go", "")
                    .into(),
            ])))
        }))
    }
}

/// The Password card: the current password, and the new one typed twice.
/// Left empty, saving the profile leaves the password as it is.
fn password_card(data: &AccountViewModel) -> Markup {
    let min = crate::http::MIN_PASSWORD_LEN;
    let password = |name: &str, id: &str, autocomplete: &str| {
        ui::Input::new(name)
            .id(id)
            .kind(ui::InputType::Password)
            .autocomplete(autocomplete)
    };
    card(
        data,
        Card::Password,
        html! {
            (Field::new("Current password", "current-password").render(html! {
                (password("current_password", "current-password", "current-password"))
            }))
            (Field::new("New password", "new-password")
                .help(None, html! { "At least " (min) " characters. Changing it logs you out everywhere else." })
                .render(html! {
                    (password("new_password", "new-password", "new-password").attr("minlength", min.to_string()))
                }))
            (Field::new("Confirm new password", "confirm-password")
                .help(None, html! { "The new password again." })
                .render(html! {
                    (password("confirm_password", "confirm-password", "new-password").attr("minlength", min.to_string()))
                }))
        },
    )
}

pub fn account_page(chrome: &PageChrome, data: &AccountViewModel) -> Markup {
    let tab = data.tab;
    let toast = data.toast();
    let header = ui::PageHeader::new("Account");
    let body = html! {
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
            }
        }
        (toast_region(toast.as_ref(), false))
        // The email question: it holds back a save before the settings
        // components (loaded by every page) count it as sent.
        (script("account.js", Load::Now))
    };
    super::Page::new(format!("{} - Account - Monokulo", tab.label()), header)
        .class("settings-page")
        .render(chrome, body)
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
    let header = ui::PageHeader::new("Log in with a new email?")
        .crumbs(super::crumbs(&[("/account", "Account")]));
    let body = html! {
        @if let Some(error) = error {
            (ui::Alert::new(error, ui::Tone::Error))
        }
        p { "From now on you log in with " strong { (submitted.email) } " instead of " (old_email) "." }
        (ui::Hint::new(CHECK_THE_EMAIL))
        form method="post" action="/account/profile" {
            input type="hidden" name="tab" value="profile";
            input type="hidden" name="email" value=(submitted.email);
            input type="hidden" name="theme" value=(submitted.theme.as_str());
            input type="hidden" name="timezone" value=(submitted.timezone.as_deref().unwrap_or(""));
            div class="setting-field" {
                div class="setting-label-row" { label class="setting-label" for="email-again" { "Type the new email again" } }
                (ui::Input::new("confirm_email")
                    .id("email-again")
                    .kind(ui::InputType::Email)
                    .required(true)
                    .autocomplete("off")
                    .attr("autofocus", ""))
            }
            (ui::FormActions::new(ui::Slot::many([
                ui::Button::new("Change email")
                    .variant(ui::ButtonVariant::Primary)
                    .kind(ui::ButtonType::Submit)
                    .into(),
                ui::LinkButton::new("Cancel", "/account").into(),
            ])))
        }
    };
    super::Page::new("Log in with a new email? - Account - Monokulo", header)
        .class("settings-page email-confirm-page")
        .render(chrome, body)
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
        assert_eq!(AccountTab::from_id(Some("security")), AccountTab::Profile);
        assert_eq!(AccountTab::Profile.href(), "/account");
        assert_eq!(AccountTab::Wallets.href(), "/account?tab=wallets");
        let html = account_page(&chrome(), &view(AccountTab::Wallets, None)).into_string();
        assert!(
            html.contains("<title>Wallets - Account - Monokulo</title>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<nav id="settings-tabs" class="tab-bar" aria-label="Account"><a href="/account">Profile</a><a href="/account?tab=wallets" aria-current="page">Wallets</a></nav>"#),
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
    fn the_profile_tab_has_email_appearance_language_and_time_and_password_cards_and_one_save_bar()
    {
        let html = account_page(&chrome(), &view(AccountTab::Profile, None)).into_string();
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {html}"))
        };
        let form = at(r#"<form method="post" action="/account/profile" id="settings-form""#);
        let email = at(r#"name="email" role="region""#);
        let appearance = at(r#"name="appearance" role="region""#);
        let time = at(r#"<section id="card-time""#);
        let password = at(r#"<section id="card-password""#);
        let bar = at(r#"<div id="save-bar" class="save-bar""#);
        assert!(
            form < email
                && email < appearance
                && appearance < time
                && time < password
                && password < bar,
            "{html}"
        );
        assert_eq!(
            html.matches("btn-primary").count() + html.matches(r#"data-variant="primary""#).count(),
            2,
            "Save, and the dialog's Change email: {html}"
        );
        assert!(html.contains(r#"<input class="mk-input" type="email" name="email" id="email" value="ann@example.org" autocomplete="email" required data-email-input="">"#), "{html}");
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
        assert!(html.contains(r#"<select class="mk-input" name="language" id="language" disabled data-mk="2"><option value="en" selected>English</option></select>"#), "{html}");
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
                r#"<script type="module" src="{}"></script>"#,
                crate::assets::url("ui-islands.js")
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
                r#"<section id="card-email" class="settings-card is-failed" data-name="email""#
            ),
            "{html}"
        );
        assert!(html.contains(r#"value="taken@example.org" autocomplete="email" required data-email-input="" data-saved="ann@example.org">"#), "{html}");
        assert!(
            html.contains(
                r#"<input type="radio" name="theme" value="dark" checked data-saved="on">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<select class="mk-input" name="timezone" id="timezone" data-mk="2" data-saved="Australia/Perth">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div id="save-bar" class="save-bar is-failed""#),
            "{html}"
        );
        assert!(html.contains(r##"<strong>Nothing saved.</strong> That email is already used by another account. <a href="#card-email" data-show-card="email">Show</a>"##), "{html}");
        assert!(
            html.contains(r#"<div class="toast toast-error" role="alert" data-toast data-mk="1""#),
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
        assert_eq!(
            html.matches(r#"data-card-saved data-mk="4">Saved<"#)
                .count(),
            2,
            "{html}"
        );

        let html = account_page(
            &chrome(),
            &view(
                AccountTab::Profile,
                Some(Outcome::Saved(vec![Card::Password])),
            ),
        )
        .into_string();
        assert!(html.contains("<strong>Password changed</strong><span class=\"toast-line\">Every other session was logged out.</span>"), "{html}");

        let html = account_page(
            &chrome(),
            &view(
                AccountTab::Profile,
                Some(Outcome::Saved(vec![Card::Time, Card::Password])),
            ),
        )
        .into_string();
        assert!(html.contains("<strong>Profile saved</strong><span class=\"toast-line\">Every other session was logged out.</span>"), "{html}");

        let html = account_page(
            &chrome(),
            &view(AccountTab::Profile, Some(Outcome::Unchanged)),
        )
        .into_string();
        assert!(html.contains("<strong>Nothing to save</strong>"), "{html}");
    }

    #[test]
    fn the_profile_tab_asks_for_the_current_password_and_the_new_one_twice() {
        let html = account_page(&chrome(), &view(AccountTab::Profile, None)).into_string();
        assert!(
            html.contains(
                r#"name="current_password" id="current-password" autocomplete="current-password">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"name="new_password" id="new-password" autocomplete="new-password" minlength="{}">"#,
                crate::http::MIN_PASSWORD_LEN
            )),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"name="confirm_password" id="confirm-password" autocomplete="new-password" minlength="{}">"#,
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
        assert!(
            html.contains(r#"name="confirm_email" id="email-again" autocomplete="off" required"#),
            "{html}"
        );
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
