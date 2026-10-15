//! The components. Each one's CSS and behaviour is in web/components; its
//! examples, below, are what the stylebook shows and what the Solid
//! translation is checked against.

mod actions;
mod chrome;
mod copy;
mod feedback;
mod forms;
mod select;
mod settings;
mod steps;
mod tabs;

pub use actions::*;
pub use chrome::*;
pub use copy::*;
pub use feedback::*;
pub use forms::*;
pub use select::*;
pub use settings::*;
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
}

fn specimens<C: crate::Component>(examples: Vec<(&'static str, C)>) -> Specimens {
    Specimens {
        info: C::info(),
        examples: examples
            .into_iter()
            .map(|(title, c)| (title, Slot::from(c)))
            .collect(),
        partials: Vec::new(),
    }
}

impl Specimens {
    fn partial<C: crate::Component>(mut self, name: &'static str, component: C) -> Self {
        self.partials.push((name, Slot::from(component)));
        self
    }
}

/// Every component, with its examples.
pub fn catalogue() -> Vec<Specimens> {
    vec![
        specimens(actions::button_examples()),
        specimens(actions::link_button_examples()),
        specimens(feedback::alert_examples()),
        specimens(forms::field_examples()),
        specimens(select::select_examples())
            .partial("select-demo", select::select_demo())
            .partial("book-theme", select::book_theme()),
        specimens(steps::progress_steps_examples()),
        specimens(tabs::tabs_examples()),
        specimens(tabs::tab_links_examples()),
        specimens(copy::copy_value_examples()),
        specimens(settings::settings_form_examples())
            .partial(
                "page-form",
                settings::page_form(
                    "#page-forms",
                    "example",
                    "example-store-name",
                    "Save example settings",
                    "Example save changes",
                ),
            )
            .partial(
                "form-grouped",
                settings::page_form(
                    "./form-grouped.html",
                    "grouped-details",
                    "grouped-name",
                    "Save settings",
                    "Save changes",
                ),
            ),
        specimens(settings::settings_card_examples()),
        specimens(settings::setting_examples()),
        specimens(settings::save_bar_examples()),
        specimens(settings::toast_examples()),
        specimens(chrome::site_nav_examples()),
        specimens(chrome::menu_examples()),
        specimens(chrome::theme_toggle_examples()),
        specimens(chrome::status_dot_examples()),
    ]
}
