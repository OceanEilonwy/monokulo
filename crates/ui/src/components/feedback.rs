use crate::{component, Attrs, Slot, UiEnum};

/// What a message says about how something went. The words say it; the
/// colour only backs them up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum Tone {
    Success,
    Warning,
    Error,
}

component! {
    /// A message about the page or what just happened: a refused save, a
    /// node that's down. Without a tone it's neutral information.
    pub struct Alert {
        children: Slot,
        tone: Option<Tone>,
        rest: Attrs,
    }
    html {
        div class="mk-alert" data-tone=[tone] ..rest { (children) }
    }
}

pub(super) fn alert_examples() -> Vec<(&'static str, Alert)> {
    vec![
        (
            "Success",
            Alert::new("Saved. Your preferences are up to date.").tone(Tone::Success),
        ),
        (
            "Warning",
            Alert::new("Connection interrupted. Payment status may be delayed.")
                .tone(Tone::Warning),
        ),
        (
            "Error",
            Alert::new("Could not save. Check the highlighted fields and try again.")
                .tone(Tone::Error)
                .attr("role", "alert"),
        ),
        ("Neutral", Alert::new("Your account uses the system theme.")),
    ]
}
