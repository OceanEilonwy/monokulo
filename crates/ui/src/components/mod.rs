//! The components. Each one's CSS and behaviour is in web/components; its
//! examples, below, are what the stylebook shows and what the Solid
//! translation is checked against.

mod actions;
mod chrome;
mod content;
mod copy;
mod data;
mod dialog;
mod forms;
mod items;
mod select;
mod settings;
mod sortable;
mod steps;
mod tabs;

pub use actions::*;
pub use chrome::*;
pub use content::*;
pub use copy::*;
pub use data::*;
pub use dialog::*;
pub use forms::*;
pub use items::*;
pub use select::*;
pub use settings::*;
pub use sortable::*;
pub use steps::*;
pub use tabs::*;

use crate::{ComponentInfo, Slot};

/// One component's specimens, in the order the stylebook shows them.
pub struct Specimens {
    pub info: &'static ComponentInfo,
    pub examples: Vec<(&'static str, Slot)>,
    /// Rendered only for hand-written stylebook pages to include
    /// (`<!-- example:generated/partials/NAME -->`), not shown as specimens.
    pub partials: Vec<(&'static str, Slot)>,
    /// What the stylebook shows in an example's frame before it, by the
    /// example's title: what a dialog needs to be opened by. Not part of
    /// the example, so not in its source or the Solid fixtures.
    pub leads: Vec<(&'static str, Slot)>,
}

fn specimens<C: crate::Component>(examples: Vec<(&'static str, C)>) -> Specimens {
    Specimens {
        info: C::info(),
        examples: examples
            .into_iter()
            .map(|(title, c)| (title, Slot::from(c)))
            .collect(),
        partials: Vec::new(),
        leads: Vec::new(),
    }
}

impl Specimens {
    fn partial<C: crate::Component>(mut self, name: &'static str, component: C) -> Self {
        self.partials.push((name, Slot::from(component)));
        self
    }

    fn lead<C: crate::Component>(mut self, example: &'static str, component: C) -> Self {
        assert!(
            self.examples.iter().any(|(title, _)| *title == example),
            "a lead for an example {} doesn't have: {example}",
            self.info.name
        );
        self.leads.push((example, Slot::from(component)));
        self
    }
}

use dialog::DIALOG_EXAMPLE;

/// Every component, with its examples.
pub fn catalogue() -> Vec<Specimens> {
    vec![
        specimens(actions::button_examples()),
        specimens(actions::link_button_examples()),
        specimens(actions::launch_tile_examples()),
        specimens(actions::segmented_control_examples()),
        specimens(content::alert_examples()),
        specimens(content::panel_examples()),
        specimens(content::heading_examples()),
        specimens(content::hint_examples()),
        specimens(content::facts_examples()),
        specimens(content::tag_examples()),
        specimens(content::network_badge_examples()),
        specimens(content::short_value_examples()),
        specimens(content::disclosure_examples()),
        specimens(content::help_tip_examples()),
        specimens(sortable::sortable_list_examples()),
        specimens(Vec::<(&str, sortable::SortableItem)>::new()),
        specimens(Vec::<(&str, sortable::SortHandle)>::new()),
        specimens(content::checks_examples()),
        specimens(content::timeline_examples()),
        specimens(content::empty_state_examples()),
        specimens(content::avatar_examples()),
        specimens(content::table_examples()),
        specimens(content::pagination_examples()),
        specimens(content::card_examples()),
        specimens(items::status_banner_examples()),
        specimens(items::item_card_examples()),
        specimens(items::dns_record_examples()),
        specimens(data::data_grid_examples()),
        // Shown in DataGrid's example: a cell alone isn't a table.
        specimens(Vec::<(&str, data::DataCell)>::new()),
        specimens(data::table_browser_examples()),
        // Shown in Table's examples: a row or a cell alone isn't a table.
        specimens(Vec::<(&str, content::Row)>::new()),
        specimens(Vec::<(&str, content::Cell)>::new()),
        specimens(dialog::dialog_examples()).lead(DIALOG_EXAMPLE, dialog::dialog_opener()),
        specimens(dialog::dialog_title_examples()),
        specimens(dialog::dialog_actions_examples()),
        specimens(dialog::dialog_page_examples()),
        specimens(forms::field_examples()),
        specimens(forms::input_examples()),
        specimens(forms::field_note_examples()),
        specimens(forms::locked_value_examples()),
        specimens(forms::text_area_examples()),
        specimens(forms::checkbox_examples()),
        specimens(forms::switch_examples()),
        specimens(forms::radio_cards_examples()),
        specimens(forms::rows_editor_examples()),
        specimens(forms::icon_button_examples()),
        specimens(forms::icon_link_examples()),
        specimens(forms::form_actions_examples()),
        specimens(select::select_examples()).partial("book-theme", select::book_theme()),
        specimens(steps::wizard_steps_examples()),
        specimens(steps::progress_steps_examples()),
        specimens(steps::countdown_examples()),
        specimens(steps::step_scale_examples()),
        specimens(tabs::tabs_examples()),
        specimens(tabs::tab_links_examples()),
        specimens(copy::copy_value_examples()),
        specimens(copy::grouped_value_examples()),
        specimens(copy::copy_block_examples()),
        specimens(settings::settings_form_examples()),
        specimens(settings::settings_card_examples()),
        specimens(settings::setting_examples()),
        specimens(settings::save_bar_examples()),
        specimens(settings::toast_examples()),
        specimens(chrome::site_nav_examples()),
        specimens(chrome::page_header_examples()),
        specimens(chrome::menu_examples()),
        specimens(chrome::menu_item_examples()),
        specimens(chrome::nav_menu_examples()),
        specimens(chrome::theme_toggle_examples()),
        specimens(chrome::status_dot_examples()),
    ]
}
