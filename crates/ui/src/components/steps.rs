use crate::{component, UiEnum, UiRecord};

/// Where a step stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum StepState {
    Done,
    Current,
    Next,
}

/// One segment of a progress strip.
#[derive(Clone, Debug, PartialEq, Eq, UiRecord)]
pub struct Step {
    /// The stage ("Wallet").
    pub label: String,
    /// Where it stands, under the label: "Complete", "Next", or the task
    /// in hand and its position ("Back up · step 2 of 3").
    pub detail: String,
    /// The stage's tasks done: its bar fills by these.
    pub done: usize,
    /// The stage's tasks; 1 for a stage with none of its own.
    pub tasks: usize,
    pub state: StepState,
}

component! {
    /// Progress through a flow (setup: Store, Wallet, Done), as flat
    /// segments, each with a bar counting its tasks done. It shows where
    /// you are; it isn't navigation.
    pub struct ProgressSteps {
        /// What the steps are of ("Setup progress").
        label: String,
        steps: Vec<Step>,
    }
    html {
        ol class="mk-progress-steps" data-design="strip" aria-label=(label) {
            @for step in steps {
                li class="done" if step.state == StepState::Done aria-current="step" if step.state == StepState::Current {
                    span { (step.label) small { (step.detail) } }
                    progress class="mk-step-progress" value=(step.done) max=(step.tasks)
                        aria-label={ (step.label) ": " (step.done) " of " (step.tasks) " tasks complete" } {}
                }
            }
        }
    }
}

pub(super) fn progress_steps_examples() -> Vec<(&'static str, ProgressSteps)> {
    let step = |label: &str, detail: &str, done, tasks, state| Step {
        label: label.into(),
        detail: detail.into(),
        done,
        tasks,
        state,
    };
    let setup = |wallet: Step, done: Step| {
        let store = step("Store", "Complete", 1, 1, StepState::Done);
        ProgressSteps::new("Setup progress", vec![store, wallet, done])
    };
    let next = || step("Done", "Next", 0, 1, StepState::Next);
    vec![
        (
            "Setup, backing up a new wallet",
            setup(
                step("Wallet", "Back up · step 2 of 3", 1, 3, StepState::Current),
                next(),
            ),
        ),
        (
            "Setup, bringing your own wallet",
            setup(
                step("Wallet", "Keys · step 2 of 2", 1, 2, StepState::Current),
                next(),
            ),
        ),
        (
            "Setup, checking the backup",
            setup(
                step("Wallet", "Check · step 3 of 3", 2, 3, StepState::Current),
                next(),
            ),
        ),
        (
            "Setup, finished",
            setup(
                step("Wallet", "Complete", 1, 1, StepState::Done),
                step("Done", "Complete", 1, 1, StepState::Current),
            ),
        ),
        (
            "Adding a wallet on its own",
            ProgressSteps::new(
                "Wallet progress",
                vec![step(
                    "Wallet",
                    "Keys · step 2 of 2",
                    1,
                    2,
                    StepState::Current,
                )],
            ),
        ),
    ]
}
