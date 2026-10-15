use crate::{component, Attrs, Slot, UiEnum};

/// How much a button stands out. Without one it is secondary: most buttons
/// are, and a page has at most one primary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum ButtonVariant {
    /// The page's main action, in the accent colour.
    Primary,
    /// An action that deletes or can't be undone.
    Danger,
    /// No border or fill, for actions beside other content.
    Quiet,
}

/// A control's size. Without one it is the normal size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum ControlSize {
    /// For dense rows: tables, card headers, toolbars.
    Compact,
    /// A larger target, for a counter's touch screen.
    Touch,
}

/// What a button does in its form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum ButtonType {
    /// Nothing by itself: a script or `fx-*` attributes act on it.
    Button,
    /// Submits its form.
    Submit,
    /// Resets its form.
    Reset,
}

component! {
    /// A button for an action on the page. One that goes somewhere is a
    /// `LinkButton`: a link, not a button.
    pub struct Button {
        /// What it says: the action, as a verb ("Save", "Delete store").
        children: Slot,
        variant: Option<ButtonVariant>,
        size: Option<ControlSize>,
        /// Defaults to `button`, so it never submits a form by accident.
        #[default(ButtonType::Button)]
        kind: ButtonType,
        /// Fills its container's width.
        #[default(false)]
        block: bool,
        #[default(false)]
        disabled: bool,
        /// Marks it working (a save in flight): the cursor shows progress.
        #[default(false)]
        busy: bool,
        rest: Attrs,
    }
    html {
        button class="mk-btn" type=(kind) data-variant=[variant] data-size=[size] data-block[block]
            disabled[disabled] aria-busy="true" if busy ..rest { (children) }
    }
}

pub(super) fn button_examples() -> Vec<(&'static str, Button)> {
    vec![
        (
            "Primary",
            Button::new("Save changes").variant(ButtonVariant::Primary),
        ),
        ("Secondary", Button::new("Cancel")),
        (
            "Danger",
            Button::new("Delete store").variant(ButtonVariant::Danger),
        ),
        (
            "Quiet",
            Button::new("Discard").variant(ButtonVariant::Quiet),
        ),
        (
            "Submits its form",
            Button::new("Log in")
                .variant(ButtonVariant::Primary)
                .kind(ButtonType::Submit),
        ),
        ("Compact", Button::new("Discard").size(ControlSize::Compact)),
        (
            "Touch",
            Button::new("Charge")
                .variant(ButtonVariant::Primary)
                .size(ControlSize::Touch)
                .block(true),
        ),
        ("Disabled", Button::new("Export orders").disabled(true)),
        (
            "Busy",
            Button::new("Saving…")
                .variant(ButtonVariant::Primary)
                .disabled(true)
                .busy(true),
        ),
    ]
}

component! {
    /// A link that looks like a button: for a call to action that goes to
    /// another page. It needs somewhere to go.
    pub struct LinkButton {
        children: Slot,
        href: String,
        variant: Option<ButtonVariant>,
        size: Option<ControlSize>,
        #[default(false)]
        block: bool,
        rest: Attrs,
    }
    html {
        a class="mk-btn" href=(href) data-variant=[variant] data-size=[size] data-block[block] ..rest { (children) }
    }
}

pub(super) fn link_button_examples() -> Vec<(&'static str, LinkButton)> {
    vec![
        (
            "Primary",
            LinkButton::new("Add a store", "#component-link-button")
                .variant(ButtonVariant::Primary),
        ),
        (
            "Secondary",
            LinkButton::new("Watch it live", "#component-link-button"),
        ),
        (
            "Compact",
            LinkButton::new("Discard changes", "#component-link-button").size(ControlSize::Compact),
        ),
    ]
}
