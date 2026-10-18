use crate::{component, Attrs, Slot, UiEnum};

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
    /// button over its top right copies it; with `comment` ("#"), a note
    /// after that marker is left out (the marker starting a line or after a
    /// space: `#` inside a word stays).
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

component! {
    /// A long value shown whole, for someone to check by eye against the
    /// copy they paste: in groups of a few characters, alternately strong
    /// and muted, the first and last group marked, and a Copy button under
    /// it. An address to pay; a key's fingerprint. Build it with
    /// [`GroupedValue::of`], which splits the value. A screen reader hears
    /// the value whole, never group by group. Without JavaScript a tap
    /// selects all of it; once its island attaches, the button copies it
    /// and says so, and the value stays on screen as it was.
    pub struct GroupedValue {
        /// The whole value: what is copied, and what a screen reader hears.
        value: String,
        /// `value` in order, split into groups.
        groups: Vec<String>,
        /// The Copy button's words: what it copies ("Copy address").
        #[default(String::from("Copy"))]
        copy_label: String,
        /// Under it: what to check ("Check your wallet shows 8FASi4 at the
        /// start and NoVKf at the end").
        hint: Option<Slot>,
        /// Shown, but not to be used any more (an address an order no
        /// longer takes payments at): struck through, faded, nothing to
        /// copy it with.
        #[default(false)]
        void: bool,
        rest: Attrs,
    }
    behaviour "grouped-value.behaviour.ts" {
        uses(value),
        /// The last copy went through: the button says so for a while.
        copied: bool,
        /// What the last copy did, for screen readers too.
        status: String,
        busy: bool,
        fn copy,
        ref shown,
    }
    html {
        div class="mk-grouped" data-void="" if void ..rest {
            span class="visually-hidden" { (value) }
            span class="mk-grouped-value mk-mono" aria-hidden="true" ref=shown {
                @for group in groups { span { (group) } }
            }
            @if enhanced {
                @if !void {
                    button class="mk-btn" type="button" data-copied="" if b.copied disabled[b.busy] on:click=copy {
                        svg class="mk-icon" viewBox="0 0 24 24" aria-hidden="true" {
                            @if b.copied { path d="M5 12l5 5L20 7" {} }
                            @else { rect x="8" y="8" width="12" height="13" rx="2" {} path d="M16 8V3H3v13h5" {} }
                        }
                        @if b.copied { "Copied" } @else { (copy_label) }
                    }
                }
                span class="visually-hidden" role="status" { (b.status) }
            }
            @if let Some(hint) = hint { p class="mk-grouped-hint" { (hint) } }
        }
    }
}

impl GroupedValue {
    /// `value` in groups of `size` characters, the last one what's left.
    pub fn of(value: impl Into<String>, size: usize) -> Self {
        let value = value.into();
        let chars: Vec<char> = value.chars().collect();
        let groups: Vec<String> = chars
            .chunks(size.max(1))
            .map(|group| group.iter().collect::<String>())
            .collect();
        Self::new(value, groups)
    }
}

const ADDRESS: &str = "8FASi45ub7Qe4ZE36UT5G6cU4ud8Fhhe4deS4F3cw9KTAb8dLcukC7edhDQ7cn5d4gEYkbUrMWeWQLGsCmrG6dLaYyNoVKf";

pub(super) fn grouped_value_examples() -> Vec<(&'static str, GroupedValue)> {
    vec![
        (
            "An address to pay, in sixes",
            GroupedValue::of(ADDRESS, 6)
                .copy_label("Copy address")
                .hint("Check your wallet shows 8FASi4 at the start and NoVKf at the end."),
        ),
        (
            "No longer to be used",
            GroupedValue::of(ADDRESS, 6).void(true),
        ),
    ]
}

pub(super) fn copy_block_examples() -> Vec<(&'static str, CopyBlock)> {
    vec![(
        "A command with a note",
        CopyBlock::new("umask 077\nopenssl rand -hex 32 > monokulo.key   # keep this file safe")
            .copy_label("Copy the key commands")
            .comment("#"),
    )]
}
