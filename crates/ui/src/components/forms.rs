use crate::{component, Attrs, ControlSize, UiEnum};

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
        /// More attributes on the input: `minlength`, `inputmode`, `fx-*`.
        rest: Attrs,
    }
    html {
        div class="mk-field" {
            label for=(id) { (label) }
            input class="mk-input" id=(id) name=(name) type=(kind) value=[value] placeholder=[placeholder]
                autocomplete=[autocomplete] data-size=[size] required[required] readonly[readonly] disabled[disabled]
                aria-invalid="true" if error
                aria-describedby=join[{ (id) "-help" } if hint, { (id) "-error" } if error]
                ..rest;
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
