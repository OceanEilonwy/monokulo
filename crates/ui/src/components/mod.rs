//! The components. Each one's CSS and behaviour is in web/components; its
//! examples, below, are what the stylebook shows and what the Solid
//! translation is checked against.

mod actions;
mod feedback;
mod forms;
mod steps;

pub use actions::*;
pub use feedback::*;
pub use forms::*;
pub use steps::*;

use crate::{ComponentInfo, Slot};

/// One component's specimens, in the order the stylebook shows them.
pub struct Specimens {
    pub info: &'static ComponentInfo,
    pub examples: Vec<(&'static str, Slot)>,
}

fn specimens<C: crate::Component>(examples: Vec<(&'static str, C)>) -> Specimens {
    Specimens {
        info: C::info(),
        examples: examples
            .into_iter()
            .map(|(title, c)| (title, Slot::from(c)))
            .collect(),
    }
}

/// Every component, with its examples.
pub fn catalogue() -> Vec<Specimens> {
    vec![
        specimens(actions::button_examples()),
        specimens(actions::link_button_examples()),
        specimens(feedback::alert_examples()),
        specimens(forms::field_examples()),
        specimens(steps::progress_steps_examples()),
    ]
}
