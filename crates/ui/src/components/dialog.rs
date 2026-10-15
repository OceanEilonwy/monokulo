//! Dialogs that ask before something that can't be undone (retire a wallet,
//! delete a webhook) or show more about one thing (a webhook delivery).
//!
//! Without JavaScript a dialog never opens: what opens it is a link to a
//! page holding the same content ([`DialogPage`]), and Cancel there is a
//! link back. Once the dialog's island attaches, those links open the
//! dialog instead (`data-opens-dialog` naming it), and Cancel and the ✕
//! (`data-closes-dialog`) close it.

use crate::{component, Attrs, Slot, UiEnum};

/// How wide a dialog is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum DialogSize {
    /// A question and its answers.
    Confirm,
    /// Room for a table or a long value.
    Wide,
}

/// When a form's save opens a dialog instead (`Dialog::confirms`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum ConfirmWhen {
    /// The field holds something other than what was saved.
    Changed,
    /// The field, saved with something, is now empty.
    Emptied,
}

component! {
    /// A modal dialog, opened by the links and buttons naming it in
    /// `data-opens-dialog`. It gives focus to its first field, and back to
    /// what opened it once it closes.
    pub struct Dialog {
        id: String,
        /// The id of its title (a `DialogTitle`).
        labelled_by: String,
        children: Slot,
        #[default(DialogSize::Confirm)]
        size: DialogSize,
        /// A form's field (its `name`) whose change the form's save asks
        /// about with this dialog first: the field holds what was saved in
        /// `data-saved`. The dialog's field marked `data-confirmed` gets
        /// what was typed.
        confirms: Option<String>,
        #[default(ConfirmWhen::Changed)]
        confirms_when: ConfirmWhen,
        /// A click on the backdrop closes it too.
        #[default(false)]
        light_dismiss: bool,
        rest: Attrs,
    }
    behaviour "dialog.behaviour.ts" {
        uses(id, confirms, confirms_when),
        ref root,
    }
    html {
        dialog id=(id) class=join["settings-dialog", "confirm-dialog" if size == DialogSize::Confirm]
            aria-labelledby=(labelled_by) closedby="any" if light_dismiss ..rest ref=root {
            (children)
        }
    }
}

pub(super) fn dialog_examples() -> Vec<(&'static str, Dialog)> {
    let body = |id: &str| {
        Slot::many([
            DialogTitle::new(format!("{id}-title"), "Retire this wallet?").into(),
            Slot::from(crate::Hint::new(
                "Takes it out of every list and deletes its keys from Monokulo.",
            )),
            DialogActions::new(Slot::many([
                crate::Button::new("Cancel")
                    .attr("data-closes-dialog", "")
                    .into(),
                Slot::from(
                    crate::Button::new("Retire wallet").variant(crate::ButtonVariant::Danger),
                ),
            ]))
            .into(),
        ])
    };
    vec![(
        "A question (the button opens it)",
        Dialog::new("book-dialog", "book-dialog-title", body("book-dialog")),
    )]
}

component! {
    /// A dialog's title: an `h2`, with a ✕ that closes the dialog. On the
    /// content's own page, the title is the page's (`PageHeader`).
    pub struct DialogTitle {
        id: String,
        children: Slot,
        /// Beside the title, before the ✕: a status.
        aside: Option<Slot>,
    }
    html {
        h2 id=(id) class="dialog-title" {
            (children)
            @if let Some(aside) = aside { span class="dialog-aside" { (aside) } }
            button type="button" class="dialog-x" aria-label="Close" data-closes-dialog { "×" }
        }
    }
}

pub(super) fn dialog_title_examples() -> Vec<(&'static str, DialogTitle)> {
    vec![
        (
            "A question",
            DialogTitle::new("book-title-dialog", "Delete this webhook?"),
        ),
        (
            "With a status",
            DialogTitle::new("book-title-status", "Delivery of order.paid")
                .aside(crate::Tag::new("retrying").tone(crate::TagTone::Slow)),
        ),
    ]
}

component! {
    /// A dialog's answers, at its foot on the right: Cancel, then the action.
    pub struct DialogActions {
        children: Slot,
    }
    html {
        div class="dialog-actions" { (children) }
    }
}

pub(super) fn dialog_actions_examples() -> Vec<(&'static str, DialogActions)> {
    vec![(
        "Cancel and an action",
        DialogActions::new(Slot::many([
            crate::LinkButton::new("Cancel", "#component-dialog-actions").into(),
            Slot::from(crate::Button::new("Delete webhook").variant(crate::ButtonVariant::Danger)),
        ])),
    )]
}

component! {
    /// A dialog's content as a page of its own: where its opener goes
    /// without JavaScript. The page's header holds its title.
    pub struct DialogPage {
        children: Slot,
        #[default(DialogSize::Confirm)]
        size: DialogSize,
    }
    html {
        div class=join["confirm-dialog-page" if size == DialogSize::Confirm, "wide-dialog-page" if size == DialogSize::Wide] {
            (children)
        }
    }
}

pub(super) fn dialog_page_examples() -> Vec<(&'static str, DialogPage)> {
    vec![(
        "A question",
        DialogPage::new(Slot::many([
            Slot::from(crate::Hint::new(
                "Takes it out of every list and deletes its keys from Monokulo.",
            )),
            DialogActions::new(Slot::many([
                crate::LinkButton::new("Cancel", "#component-dialog-page").into(),
                Slot::from(
                    crate::Button::new("Retire wallet").variant(crate::ButtonVariant::Danger),
                ),
            ]))
            .into(),
        ])),
    )]
}
