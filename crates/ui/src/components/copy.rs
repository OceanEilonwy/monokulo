use crate::{component, UiEnum};

/// How a long value is shortened on screen. The whole value is always what
/// is copied, and what focus reveals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum Truncate {
    /// An ellipsis at the end, by CSS: the only one without JavaScript.
    End,
    /// The first `head` and last `tail` characters, an ellipsis between.
    Middle,
    /// The `segments` ranges (`start:length`, comma-separated; a negative
    /// start counts from the end), an ellipsis for each gap.
    Segments,
}

component! {
    /// A long value to copy: an address, an id, a key. Without JavaScript it
    /// is selectable text, ellipsised, that focus shows in full. Once its
    /// island attaches, a Copy button copies the whole value (or, where the
    /// clipboard is refused, selects it to copy by hand), and a shortened
    /// form can stand in for it on screen.
    pub struct CopyValue {
        /// The whole value: what is copied.
        value: String,
        /// The Copy button's accessible name: what it copies.
        #[default(String::from("Copy full value"))]
        copy_label: String,
        /// The id of what names the value, when something on the page does.
        labelled_by: Option<String>,
        #[default(Truncate::End)]
        truncate: Truncate,
        /// `Middle`'s characters kept at the start.
        #[default(8usize)]
        head: usize,
        /// `Middle`'s characters kept at the end.
        #[default(8usize)]
        tail: usize,
        /// `Segments`' ranges.
        segments: Option<String>,
    }
    behaviour "copy.behaviour.ts" {
        uses(value, truncate, segments, head, tail),
        /// The shortened value, when it is shorter.
        preview: Option<String>,
        /// What the last copy did, for screen readers too.
        status: String,
        busy: bool,
        fn copy,
        fn reveal,
        ref value,
    }
    html {
        div class="mk-copy" {
            span class="mk-copy-value mk-mono" data-copy-value tabindex="0" aria-labelledby=[labelled_by] ref=value { (value) }
            @if enhanced {
                @if let Some(preview) = b.preview {
                    span class="mk-copy-preview mk-mono" aria-hidden="true" on:click=reveal { (preview) }
                }
                button class="mk-btn" data-size="compact" type="button" aria-label=(copy_label) disabled[b.busy] on:click=copy { "Copy" }
                span class="mk-copy-status" role="status" { (b.status) }
            }
        }
    }
}

const REFERENCE: &str =
    "order_0123456789abcdefghijklmnopqrstuvwxyz_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789";

pub(super) fn copy_value_examples() -> Vec<(&'static str, CopyValue)> {
    vec![
        (
            "Ellipsised at the end",
            CopyValue::new(REFERENCE).copy_label("Copy payment reference"),
        ),
        (
            "Middle: 10 characters at each end",
            CopyValue::new(REFERENCE)
                .copy_label("Copy middle example")
                .truncate(Truncate::Middle)
                .head(10usize)
                .tail(10usize),
        ),
        (
            "Segments: prefix, internal id, suffix",
            CopyValue::new(REFERENCE)
                .copy_label("Copy segmented example")
                .truncate(Truncate::Segments)
                .segments("0:8,32:8,-8:8"),
        ),
        (
            "Short enough to show whole",
            CopyValue::new("48xQ7v3Rk").copy_label("Copy wallet id"),
        ),
    ]
}

component! {
    /// Lines to copy whole: a command, a config file, a key. Without
    /// JavaScript it is a `pre` to select. Once its island attaches, a Copy
    /// button over its top right copies it; with `comment` ("#"), what
    /// follows that marker on a line is left out (a note after a command).
    pub struct CopyBlock {
        code: String,
        /// The Copy button's accessible name: what it copies.
        #[default(String::from("Copy to clipboard"))]
        copy_label: String,
        /// A line comment's marker: what follows it isn't copied.
        comment: Option<String>,
    }
    behaviour "copy-block.behaviour.ts" {
        uses(comment),
        /// What the button says: Copy, or Copied for a moment after.
        label: String,
        /// What the last copy did, for screen readers.
        status: String,
        fn copy,
        ref code,
    }
    html {
        div class="mk-copy-block" {
            pre ref=code { code { (code) } }
            @if enhanced {
                button class="mk-btn" data-size="compact" type="button" aria-label=(copy_label) on:click=copy {
                    (b.label)
                }
                span class="visually-hidden" role="status" { (b.status) }
            }
        }
    }
}

pub(super) fn copy_block_examples() -> Vec<(&'static str, CopyBlock)> {
    vec![(
        "A command with a note",
        CopyBlock::new("umask 077\nopenssl rand -hex 32 > monokulo.key   # keep this file safe")
            .copy_label("Copy the key commands")
            .comment("#"),
    )]
}
