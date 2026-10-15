//! A list whose order is the user's to set: fallbacks tried in turn.

use crate::{component, Attrs, Slot};

component! {
    /// A list whose items can be put in another order. Without JavaScript
    /// the page reorders it (each item's own Move up and Move down, posted
    /// to the server). Once its island attaches, an item moves by dragging
    /// its `SortHandle`, or with the up and down arrows on the handle; the
    /// items it passes slide out of its way. Items marked `fixed` (a blank
    /// row to add one) stay at the end.
    ///
    /// Each move sends `mk-reorder` from the list (`{item}`), for a page
    /// script to renumber what it names after the order; a drag that ends
    /// in a new order, or an arrow, sends `mk-reordered` (`{item}`).
    pub struct SortableList {
        /// What is ordered ("Stagenet nodes").
        label: String,
        /// `SortableItem`s.
        children: Slot,
        rest: Attrs,
    }
    behaviour "sortable-list.behaviour.ts" {
        uses(),
        ref root,
    }
    html {
        ol class="mk-sortable" aria-label=(label) ..rest ref=root { (children) }
    }
}

component! {
    /// One item of a `SortableList`: usually a `Disclosure` with the `Row`
    /// look, its summary holding the item's `SortHandle`.
    pub struct SortableItem {
        children: Slot,
        /// Stays at the end, and isn't moved: a blank row to add one.
        #[default(false)]
        fixed: bool,
        rest: Attrs,
    }
    html {
        li class="mk-sortable-item" data-sortable-fixed="" if fixed ..rest { (children) }
    }
}

component! {
    /// What drags a `SortableList`'s item, and takes its arrow keys. Shown
    /// only with JavaScript: without it the list's Move buttons reorder.
    pub struct SortHandle {
        /// What moving it does, for screen readers ("Reorder Primary: press
        /// the up or down arrow").
        label: String,
        rest: Attrs,
    }
    html {
        span class="mk-sort-handle" role="button" tabindex="0" title="Drag to reorder" aria-label=(label)
            data-sortable-handle="" data-only="script" ..rest { "\u{283F}" }
    }
}

pub(super) fn sortable_list_examples() -> Vec<(&'static str, SortableList)> {
    let item = |place: &str, address: &str| {
        Slot::from(SortableItem::new(
            crate::Disclosure::new(
                Slot::many([
                    Slot::from(SortHandle::new(format!(
                        "Reorder {place}: press the up or down arrow"
                    ))),
                    Slot::from(format!("{place} · {address}")),
                ]),
                "The node's address, and whether it uses TLS.",
            )
            .look(crate::DisclosureLook::Row),
        ))
    };
    vec![(
        "Nodes, tried in order",
        SortableList::new(
            "Example nodes",
            Slot::many([
                item("Primary", "node-a.example:18081"),
                item("Fallback 1", "node-b.example:18081"),
                item("Fallback 2", "node-c.example:18081"),
                Slot::from(
                    SortableItem::new(
                        crate::Disclosure::new("+ Add a node", "An address to try last.")
                            .look(crate::DisclosureLook::Row),
                    )
                    .fixed(true),
                ),
            ]),
        ),
    )]
}
