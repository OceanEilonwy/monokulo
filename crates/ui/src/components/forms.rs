use crate::{component, Attrs, ButtonType, ControlSize, Slot, UiEnum, UiRecord};

/// The kind of text an input takes: phones show the matching keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum InputType {
    Text,
    Email,
    Password,
    Number,
    Url,
    Search,
    Tel,
    /// A date and a time of day, in the reader's own format.
    DatetimeLocal,
}

/// What a note under a field says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum NoteTone {
    /// What the field is for, an example: muted.
    Help,
    /// What's wrong with what was entered, or with what's saved.
    Problem,
    /// Saved, but waiting for something (a restart).
    Pending,
    /// A check on what was entered passed ("Free to use").
    Fine,
}

component! {
    /// A note under a field: what it's for, or how what's in it stands.
    /// The control points at it with `aria-describedby` and its `id`.
    pub struct FieldNote {
        children: Slot,
        #[default(NoteTone::Help)]
        tone: NoteTone,
        id: Option<String>,
        /// Said as it changes (a check as you type): `role="status"`.
        #[default(false)]
        live: bool,
        rest: Attrs,
    }
    html {
        span class=join["field-help" if tone == NoteTone::Help, "setting-problem" if tone == NoteTone::Problem,
                "setting-pending" if tone == NoteTone::Pending, "field-check" if tone == NoteTone::Fine]
            id=[id] role="status" if live ..rest {
            (children)
        }
    }
}

pub(super) fn field_note_examples() -> Vec<(&'static str, FieldNote)> {
    vec![
        ("Help", FieldNote::new("The node's host and port, like node.example.com:18089.")),
        ("A problem", FieldNote::new("Add the port, like node.example.com:18081.").tone(NoteTone::Problem)),
        ("Pending", FieldNote::new("Saved - restart needed for it to take effect.").tone(NoteTone::Pending)),
        ("Fine, said as it changes", FieldNote::new("✓ Free to use.").tone(NoteTone::Fine).live(true)),
    ]
}

component! {
    /// A value the page shows but can't change (set on the command line,
    /// by a connected plugin): a disabled box with a padlock, and no
    /// `name`, so it is never sent with a form. A secret shows as dots,
    /// never its value.
    pub struct LockedValue {
        value: String,
        /// For a `label for` to name it.
        id: Option<String>,
        /// What names it, without a `label for`.
        labelled_by: Option<String>,
        described_by: Option<String>,
        #[default(false)]
        secret: bool,
    }
    html {
        span class="locked-input" {
            @if secret {
                input class="mk-input" type="password" value="locked" id=[id] aria-labelledby=[labelled_by]
                    aria-describedby=[described_by] disabled;
            } @else {
                input class="mk-input" type="text" value=(value) id=[id] aria-labelledby=[labelled_by]
                    aria-describedby=[described_by] disabled;
            }
            svg class="lock-icon" viewBox="0 0 16 16" aria-hidden="true" {
                path d="M5 7V5a3 3 0 0 1 6 0v2" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" {}
                rect x="3" y="7" width="10" height="7" rx="1.5" fill="currentColor" {}
            }
        }
    }
}

pub(super) fn locked_value_examples() -> Vec<(&'static str, LockedValue)> {
    vec![
        ("Set elsewhere", LockedValue::new("https://pay.example.com").id("book-locked")),
        ("A secret", LockedValue::new("").id("book-locked-secret").secret(true)),
    ]
}

component! {
    /// A labelled text input, with what it's for and what's wrong with it
    /// tied to it for screen readers. The label is required: an input
    /// never goes without one.
    pub struct Field {
        /// What the input is ("Email").
        label: String,
        /// The input's id: the label, hint and error point at it.
        id: String,
        /// The form field's name.
        name: String,
        #[default(InputType::Text)]
        kind: InputType,
        value: Option<String>,
        placeholder: Option<String>,
        autocomplete: Option<String>,
        /// What it's for, under the input.
        hint: Option<String>,
        /// What's wrong with what was entered. Marks the input invalid.
        error: Option<String>,
        #[default(false)]
        required: bool,
        #[default(false)]
        readonly: bool,
        #[default(false)]
        disabled: bool,
        size: Option<ControlSize>,
        /// Several lines of text (a message, a note): a text area, `rows`
        /// tall, instead of an input; `kind` doesn't apply.
        #[default(false)]
        multiline: bool,
        #[default(4usize)]
        rows: usize,
        /// More attributes on the input: `minlength`, `inputmode`, `fx-*`.
        rest: Attrs,
    }
    html {
        div class="mk-field" {
            label for=(id) { (label) }
            @if multiline {
                textarea class="mk-input" id=(id) name=(name) rows=(rows) placeholder=[placeholder]
                    autocomplete=[autocomplete] required[required] readonly[readonly] disabled[disabled]
                    aria-invalid="true" if error
                    aria-describedby=join[{ (id) "-help" } if hint, { (id) "-error" } if error]
                    ..rest {
                    @if let Some(value) = value { (value) }
                }
            } @else {
                input class="mk-input" id=(id) name=(name) type=(kind) value=[value] placeholder=[placeholder]
                    autocomplete=[autocomplete] data-size=[size] required[required] readonly[readonly] disabled[disabled]
                    aria-invalid="true" if error
                    aria-describedby=join[{ (id) "-help" } if hint, { (id) "-error" } if error]
                    ..rest;
            }
            @if let Some(hint) = hint {
                p class="mk-hint" id={ (id) "-help" } { (hint) }
            }
            @if let Some(error) = error {
                p class="mk-error" id={ (id) "-error" } { (error) }
            }
        }
    }
}

pub(super) fn field_examples() -> Vec<(&'static str, Field)> {
    vec![
        (
            "Several lines",
            Field::new("Message", "book-message", "message")
                .multiline(true)
                .required(true)
                .hint("Who you are, and what you'll use Monokulo for."),
        ),
        (
            "With a hint",
            Field::new("Store name", "book-store-name", "name")
                .placeholder("Your store")
                .hint("Shown to customers on the payment page."),
        ),
        (
            "Refused",
            Field::new("Email", "book-email", "email")
                .kind(InputType::Email)
                .value("incomplete")
                .error("Enter a complete email address."),
        ),
        (
            "Required, with input rules",
            Field::new("Password", "book-password", "password")
                .kind(InputType::Password)
                .required(true)
                .autocomplete("new-password")
                .attr("minlength", "8")
                .hint("At least 8 characters."),
        ),
        (
            "Read only",
            Field::new("Store id", "book-readonly", "store")
                .value("Store 123")
                .readonly(true),
        ),
        (
            "Disabled",
            Field::new("Instance URL", "book-disabled", "url")
                .kind(InputType::Url)
                .value("https://pay.example.com")
                .disabled(true),
        ),
        (
            "Compact",
            Field::new("Refund address", "book-compact", "refund").size(ControlSize::Compact),
        ),
    ]
}

component! {
    /// A text input on its own, for a control whose label is elsewhere: a
    /// `Setting`'s, a table column's (`label`, read by screen readers). A
    /// labelled input with its hint and error is a `Field`.
    pub struct Input {
        name: String,
        id: Option<String>,
        #[default(InputType::Text)]
        kind: InputType,
        value: Option<String>,
        placeholder: Option<String>,
        autocomplete: Option<String>,
        /// Its name for screen readers, when nothing on the page labels it.
        label: Option<String>,
        /// The ids of what describes it: its help, its problem.
        described_by: Option<String>,
        /// What was entered was refused.
        #[default(false)]
        invalid: bool,
        #[default(false)]
        required: bool,
        #[default(false)]
        readonly: bool,
        #[default(false)]
        disabled: bool,
        size: Option<ControlSize>,
        /// Inside its right edge: `IconButton`s (scan a code, choose a
        /// file). Their own names say what they do.
        actions: Option<Slot>,
        /// More attributes: `minlength`, `inputmode`, `data-saved`, `fx-*`.
        rest: Attrs,
    }
    html {
        @if let Some(actions) = actions {
            span class="mk-input-with-actions" {
                input class="mk-input" type=(kind) name=(name) id=[id] value=[value] placeholder=[placeholder]
                autocomplete=[autocomplete] aria-label=[label] aria-describedby=[described_by]
                aria-invalid="true" if invalid data-size=[size] required[required] readonly[readonly]
                disabled[disabled] ..rest;
                span class="mk-input-actions" { (actions) }
            }
        } @else {
            input class="mk-input" type=(kind) name=(name) id=[id] value=[value] placeholder=[placeholder]
                autocomplete=[autocomplete] aria-label=[label] aria-describedby=[described_by]
                aria-invalid="true" if invalid data-size=[size] required[required] readonly[readonly]
                disabled[disabled] ..rest;
        }
    }
}

pub(super) fn input_examples() -> Vec<(&'static str, Input)> {
    vec![
        (
            "A URL",
            Input::new("webhook_url")
                .kind(InputType::Url)
                .placeholder("https://shop.example/webhook")
                .label("Webhook URL"),
        ),
        (
            "Refused",
            Input::new("confirmations")
                .kind(InputType::Number)
                .value("-1")
                .label("Confirmations")
                .invalid(true),
        ),
        ("Compact", Input::new("q").kind(InputType::Search).label("Search").size(ControlSize::Compact)),
        (
            "With actions inside",
            Input::new("refund_address")
                .label("Refund address")
                .placeholder("Your Monero refund address")
                .actions(Slot::many([
                    Slot::from(crate::IconButton::new("Scan refund QR", "⌗")),
                    Slot::from(crate::IconButton::new("Choose QR image", "▣")),
                ])),
        ),
    ]
}

component! {
    /// Text over several lines. Its label is elsewhere, like an `Input`'s.
    pub struct TextArea {
        name: String,
        id: Option<String>,
        value: Option<String>,
        #[default(3usize)]
        rows: usize,
        placeholder: Option<String>,
        label: Option<String>,
        described_by: Option<String>,
        #[default(false)]
        invalid: bool,
        #[default(false)]
        required: bool,
        #[default(false)]
        readonly: bool,
        #[default(false)]
        disabled: bool,
        rest: Attrs,
    }
    html {
        textarea class="mk-input" name=(name) id=[id] rows=(rows) placeholder=[placeholder] aria-label=[label]
            aria-describedby=[described_by] aria-invalid="true" if invalid required[required]
            readonly[readonly] disabled[disabled] ..rest {
            @if let Some(value) = value { (value) }
        }
    }
}

pub(super) fn text_area_examples() -> Vec<(&'static str, TextArea)> {
    vec![(
        "A note",
        TextArea::new("note").label("Note").placeholder("For the customer's receipt"),
    )]
}

component! {
    /// A checkbox and what ticking it means, beside it. Without words
    /// beside it (a table's column says it), `label` names it.
    pub struct Checkbox {
        /// What ticking it means: the label.
        text: Option<Slot>,
        name: Option<String>,
        id: Option<String>,
        /// What it sends when ticked.
        #[default(String::from("on"))]
        value: String,
        #[default(false)]
        checked: bool,
        /// Its name for screen readers, without `text`.
        label: Option<String>,
        described_by: Option<String>,
        #[default(false)]
        disabled: bool,
        /// More attributes on the checkbox: `data-saved`, a script's hook.
        rest: Attrs,
    }
    html {
        @if let Some(text) = text {
            label class="check-line" {
                input type="checkbox" name=[name] id=[id] value=(value) checked[checked] aria-label=[label]
                    aria-describedby=[described_by] disabled[disabled] ..rest;
                span { (text) }
            }
        } @else {
            input type="checkbox" class="mk-checkbox" name=[name] id=[id] value=(value) checked[checked]
                aria-label=[label] aria-describedby=[described_by] disabled[disabled] ..rest;
        }
    }
}

pub(super) fn checkbox_examples() -> Vec<(&'static str, Checkbox)> {
    vec![
        (
            "With what it means",
            Checkbox::new()
                .text("I've written all 16 words down, in order, and put them somewhere safe."),
        ),
        ("Ticked", Checkbox::new().text("Use TLS").name("ssl").value("on").checked(true)),
        ("In a table", Checkbox::new().name("use_node").label("Use node.example.com")),
    ]
}

/// A checkbox's or radio button's state as saved (`data-saved`), for a
/// settings form to tell what changed when a refused save shows another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum Saved {
    On,
    Off,
}

impl From<bool> for Saved {
    fn from(on: bool) -> Self {
        if on { Saved::On } else { Saved::Off }
    }
}

component! {
    /// On or off, said as a switch: a checkbox with `role="switch"`, so it
    /// works and submits without JavaScript. An unticked checkbox sends
    /// nothing; `listed_in` names a field each switch adds its name to, so
    /// the form's handler can tell off from absent.
    pub struct Switch {
        name: String,
        id: String,
        #[default(false)]
        on: bool,
        described_by: Option<String>,
        /// What is saved, when a refused save shows another value.
        saved: Option<Saved>,
        listed_in: Option<String>,
        #[default(false)]
        disabled: bool,
    }
    html {
        @if let Some(listed_in) = listed_in {
            input type="hidden" name=(listed_in) value=(name);
        }
        label class="switch" {
            input type="checkbox" role="switch" name=(name) value="true" id=(id) checked[on]
                aria-describedby=[described_by] data-saved=[saved] disabled[disabled];
            span class="switch-track" aria-hidden="true" {}
            span class="switch-on" aria-hidden="true" { "On" }
            span class="switch-off" aria-hidden="true" { "Off" }
        }
    }
}

pub(super) fn switch_examples() -> Vec<(&'static str, Switch)> {
    vec![
        ("On", Switch::new("book.accept", "book-switch-on").on(true)),
        ("Off", Switch::new("book.notify", "book-switch-off")),
        ("Disabled", Switch::new("book.locked", "book-switch-disabled").disabled(true)),
    ]
}

/// One choice of [`RadioCards`].
#[derive(Clone, Debug, UiRecord)]
pub struct RadioCard {
    pub value: String,
    pub label: Slot,
    /// Before the label: a picture of the choice.
    pub picture: Option<Slot>,
    pub checked: bool,
    /// Whether this is the choice as saved, when the form marks changes.
    pub saved: Option<Saved>,
}

component! {
    /// One of a few choices, each a card: a radio button, maybe a picture,
    /// and its label. Its label is elsewhere (a `Setting` group's).
    pub struct RadioCards {
        name: String,
        cards: Vec<RadioCard>,
        /// Small cards, side by side: the theme's three.
        #[default(false)]
        compact: bool,
    }
    html {
        div class=join["radio-cards", "is-compact" if compact] {
            @for card in cards {
                label class="radio-card" {
                    input type="radio" name=(name) value=(card.value) checked[card.checked]
                        data-saved=[card.saved];
                    @if let Some(picture) = card.picture { (picture) }
                    (card.label)
                }
            }
        }
    }
}

pub(super) fn radio_cards_examples() -> Vec<(&'static str, RadioCards)> {
    let card = |value: &str, label: &str, checked| RadioCard {
        value: value.into(),
        label: label.into(),
        picture: None,
        checked,
        saved: None,
    };
    vec![(
        "A theme",
        RadioCards::new(
            "book_theme",
            vec![card("system", "System", true), card("light", "Light", false), card("dark", "Dark", false)],
        )
        .compact(true),
    )]
}

component! {
    /// A button showing only an icon: its name, required, is what screen
    /// readers say and what hovering shows.
    pub struct IconButton {
        /// What it does ("Send again").
        label: String,
        /// The icon: an inline SVG.
        icon: Slot,
        #[default(ButtonType::Button)]
        kind: ButtonType,
        #[default(false)]
        disabled: bool,
        rest: Attrs,
    }
    html {
        button type=(kind) class="mk-btn icon-only" aria-label=(label) title=(label) disabled[disabled] ..rest {
            (icon)
        }
    }
}

pub(super) fn icon_button_examples() -> Vec<(&'static str, IconButton)> {
    vec![(
        "Send again",
        IconButton::new("Send again", "↻"),
    )]
}

component! {
    /// A link showing only an icon, looking like an `IconButton`: its
    /// name, required, is what screen readers say and what hovering shows.
    pub struct IconLink {
        /// Where it goes, in words ("Refresh page").
        label: String,
        href: String,
        /// The icon: an inline SVG.
        icon: Slot,
        rest: Attrs,
    }
    html {
        a class="mk-btn icon-only" href=(href) aria-label=(label) title=(label) ..rest {
            (icon)
        }
    }
}

pub(super) fn icon_link_examples() -> Vec<(&'static str, IconLink)> {
    vec![("Refresh", IconLink::new("Refresh page", "#component-icon-link", "↻"))]
}

component! {
    /// A form's buttons in a row; a `spacer` pushes the ones after it to
    /// the right.
    pub struct FormActions {
        children: Slot,
        rest: Attrs,
    }
    html {
        div class="form-actions" ..rest { (children) }
    }
}

pub(super) fn form_actions_examples() -> Vec<(&'static str, FormActions)> {
    vec![(
        "Save and Cancel",
        FormActions::new(crate::Slot::many([
            crate::Button::new("Add webhook").variant(crate::ButtonVariant::Primary).kind(ButtonType::Submit).into(),
            crate::Slot::from(crate::LinkButton::new("Cancel", "#component-form-actions")),
        ])),
    )]
}
